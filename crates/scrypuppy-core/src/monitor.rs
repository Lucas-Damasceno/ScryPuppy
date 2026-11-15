//! Native clipboard sequence notifications, independent of the UI runtime.
//! The callback runs on the listener thread; persistence belongs on a worker.
use std::sync::{
    atomic::{AtomicBool, AtomicIsize, Ordering},
    Arc,
};

pub struct ClipboardMonitorHandle {
    stop: AtomicBool,
    window: AtomicIsize,
}

impl ClipboardMonitorHandle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: AtomicBool::new(false),
            window: AtomicIsize::new(0),
        })
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::{
                Foundation::{HWND, LPARAM, WPARAM},
                UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE},
            };
            let raw = self.window.load(Ordering::Acquire);
            if raw != 0 {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(raw as *mut std::ffi::c_void)),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        }
    }
}

#[derive(Debug)]
struct SequenceTracker {
    last: u32,
}
impl SequenceTracker {
    fn new(last: u32) -> Self {
        Self { last }
    }
    fn observe(&mut self, sequence: u32) -> bool {
        if sequence == 0 || sequence == self.last {
            return false;
        }
        self.last = sequence;
        true
    }
}

/// Start a message-only listener. The handle must be shut down by the owner.
#[cfg(target_os = "windows")]
pub fn start<F>(handle: Arc<ClipboardMonitorHandle>, callback: F) -> std::io::Result<()>
where
    F: FnMut(u32) + Send + 'static,
{
    std::thread::Builder::new()
        .name("scryppy-clipboard-monitor".into())
        .spawn(move || monitor_loop(handle, callback))
        .map(|_| ())
}

#[cfg(not(target_os = "windows"))]
pub fn start<F>(_handle: Arc<ClipboardMonitorHandle>, _callback: F) -> std::io::Result<()>
where
    F: FnMut(u32) + Send + 'static,
{
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Clipboard monitor is Windows-only",
    ))
}

#[cfg(target_os = "windows")]
fn monitor_loop<F>(handle: Arc<ClipboardMonitorHandle>, mut callback: F)
where
    F: FnMut(u32),
{
    use windows::core::w;
    use windows::Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::DataExchange::{
            AddClipboardFormatListener, GetClipboardSequenceNumber, RemoveClipboardFormatListener,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            RegisterClassW, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_MESSAGE, MSG,
            WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE, WM_CLOSE, WM_DESTROY, WNDCLASSW,
        },
    };
    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
    let class_name = w!("ScryppyClipboardMonitor");
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: HINSTANCE::default(),
        lpszClassName: class_name,
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassW(&class);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            class_name,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(HINSTANCE::default()),
            None,
        ) else {
            return;
        };
        handle.window.store(hwnd.0 as isize, Ordering::Release);
        if AddClipboardFormatListener(hwnd).is_err() {
            let _ = DestroyWindow(hwnd);
            handle.window.store(0, Ordering::Release);
            return;
        }
        let mut tracker = SequenceTracker::new(GetClipboardSequenceNumber());
        let mut message = MSG::default();
        while !handle.is_stopped() {
            if GetMessageW(&mut message, None, 0, 0).0 <= 0 {
                break;
            }
            if message.message == WM_CLIPBOARDUPDATE
                && tracker.observe(GetClipboardSequenceNumber())
            {
                callback(tracker.last);
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = DestroyWindow(hwnd);
        handle.window.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignores_initial_zero_and_duplicate_sequences() {
        let mut tracker = SequenceTracker::new(9);
        assert!(!tracker.observe(0));
        assert!(!tracker.observe(9));
        assert!(tracker.observe(10));
        assert!(!tracker.observe(10));
        assert!(tracker.observe(9));
    }
    #[test]
    fn shutdown_is_idempotent_before_start() {
        let handle = ClipboardMonitorHandle::new();
        assert!(!handle.is_stopped());
        handle.shutdown();
        handle.shutdown();
        assert!(handle.is_stopped());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_listener_starts_and_stops_without_modifying_clipboard() {
        let handle = ClipboardMonitorHandle::new();
        start(handle.clone(), |_| {}).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while handle.window.load(Ordering::Acquire) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let started = handle.window.load(Ordering::Acquire) != 0;
        handle.shutdown();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while handle.window.load(Ordering::Acquire) != 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(started, "listener window did not start");
        assert_eq!(
            handle.window.load(Ordering::Acquire),
            0,
            "listener window did not close"
        );
    }
}
