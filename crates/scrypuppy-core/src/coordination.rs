//! Shared capture serialization and suppression of internal clipboard writes.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Default)]
pub struct CaptureCoordinator {
    gate: Arc<Mutex<bool>>,
    ignored_sequences: Arc<Mutex<VecDeque<u32>>>,
    monitor_suppressions: Arc<AtomicUsize>,
}

impl CaptureCoordinator {
    /// Wait until the prior capture is committed or abandoned. The guard releases
    /// the slot even when a capture returns early with an error.
    pub fn begin(&self) -> Result<CaptureSession, String> {
        loop {
            let mut gate = self
                .gate
                .lock()
                .map_err(|_| "Nao foi possivel controlar o estado da captura.".to_string())?;
            if !*gate {
                *gate = true;
                return Ok(CaptureSession {
                    gate: self.gate.clone(),
                });
            }
            drop(gate);
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn mark_internal_sequence(&self, sequence: u32) {
        if let Ok(mut values) = self.ignored_sequences.lock() {
            values.retain(|value| *value != sequence);
            values.push_back(sequence);
            while values.len() > 64 {
                values.pop_front();
            }
        }
    }

    pub fn consume_internal_sequence(&self, sequence: u32) -> bool {
        self.ignored_sequences
            .lock()
            .map(|mut values| {
                values
                    .iter()
                    .position(|value| *value == sequence)
                    .and_then(|index| values.remove(index))
                    .is_some()
            })
            .unwrap_or(false)
    }

    pub fn suppress_monitor(&self) -> ClipboardMonitorSuppression {
        self.monitor_suppressions.fetch_add(1, Ordering::AcqRel);
        ClipboardMonitorSuppression {
            count: self.monitor_suppressions.clone(),
        }
    }

    pub fn monitor_is_suppressed(&self) -> bool {
        self.monitor_suppressions.load(Ordering::Acquire) > 0
    }
}

pub struct CaptureSession {
    gate: Arc<Mutex<bool>>,
}
impl Drop for CaptureSession {
    fn drop(&mut self) {
        if let Ok(mut gate) = self.gate.lock() {
            *gate = false;
        }
    }
}

pub struct ClipboardMonitorSuppression {
    count: Arc<AtomicUsize>,
}
impl Drop for ClipboardMonitorSuppression {
    fn drop(&mut self) {
        let previous = self.count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "clipboard monitor suppression underflow");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suppression_is_scoped_nestable_and_sequence_is_consumed_once() {
        let coordinator = CaptureCoordinator::default();
        coordinator.mark_internal_sequence(42);
        assert!(coordinator.consume_internal_sequence(42));
        assert!(!coordinator.consume_internal_sequence(42));
        let first = coordinator.suppress_monitor();
        assert!(coordinator.monitor_is_suppressed());
        {
            let _second = coordinator.suppress_monitor();
        }
        assert!(coordinator.monitor_is_suppressed());
        drop(first);
        assert!(!coordinator.monitor_is_suppressed());
    }
    #[test]
    fn capture_guard_releases_after_early_drop() {
        let coordinator = CaptureCoordinator::default();
        let first = coordinator.begin().unwrap();
        let other = coordinator.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _guard = other.begin().unwrap();
            send.send(()).unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }
}
