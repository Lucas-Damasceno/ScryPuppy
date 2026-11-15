use super::{
    collect_active_window, open_conn, persist_clipboard_payload, read_clipboard_payload, AppState,
    CaptureOrigin, ClipboardPayload, INBOX_CONTEXT_ID,
};
pub use scrypuppy_core::monitor::ClipboardMonitorHandle;
use std::{
    collections::VecDeque,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};
use tauri::AppHandle;

pub fn start(app: AppHandle, state: AppState, handle: Arc<ClipboardMonitorHandle>) {
    #[cfg(target_os = "windows")]
    {
        let (sender, receiver) = mpsc::channel::<MonitorEvent>();
        let worker_state = state.clone();
        let worker_handle = handle.clone();
        thread::Builder::new()
            .name("scryppy-clipboard-worker".into())
            .spawn(move || worker_loop(app, worker_state, receiver, worker_handle))
            .expect("clipboard worker thread should start");
        scrypuppy_core::monitor::start(handle, move |sequence| {
            if state.consume_ignored_clipboard_sequence(sequence)
                || state.clipboard_monitor_is_suppressed()
            {
                return;
            }
            let active_window = collect_active_window();
            if let Ok(Some(payload)) = read_clipboard_payload(&state) {
                if payload
                    .plain_text()
                    .is_some_and(super::is_internal_clipboard_marker)
                {
                    return;
                }
                let _ = sender.send(MonitorEvent {
                    sequence,
                    payload,
                    active_window,
                });
            }
        })
        .expect("clipboard monitor thread should start");
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, state, handle);
    }
}

#[derive(Debug)]
struct MonitorEvent {
    sequence: u32,
    payload: ClipboardPayload,
    active_window: super::ActiveWindowMetadata,
}

fn worker_loop(
    app: AppHandle,
    state: AppState,
    receiver: mpsc::Receiver<MonitorEvent>,
    stop: Arc<ClipboardMonitorHandle>,
) {
    let mut processed_sequences = VecDeque::new();
    let mut last_diagnostic = Instant::now() - Duration::from_secs(10);

    while !stop.is_stopped() {
        let Ok(event) = receiver.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        if processed_sequences.contains(&event.sequence) {
            continue;
        }
        processed_sequences.push_back(event.sequence);
        while processed_sequences.len() > 64 {
            processed_sequences.pop_front();
        }

        let enabled = open_conn(&state)
            .and_then(|conn| super::settings_from_conn(&conn, &state))
            .map(|settings| settings.clipboard_monitor_enabled)
            .unwrap_or(false);
        if !enabled {
            continue;
        }

        if persist_clipboard_payload(
            &app,
            state.clone(),
            event.payload,
            event.active_window,
            CaptureOrigin::ClipboardMonitor,
            "capture",
            INBOX_CONTEXT_ID,
        )
        .is_err()
        {
            report_diagnostic(&mut last_diagnostic, "capture event ignored");
        }
    }
}

fn report_diagnostic(last: &mut Instant, reason: &str) {
    if last.elapsed() >= Duration::from_secs(5) {
        eprintln!("Clipboard monitor diagnostic: {reason}");
        *last = Instant::now();
    }
}
