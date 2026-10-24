//! Poll actual capture retirement without waiting for driver destructors on GPUI.
use std::{sync::mpsc, thread::JoinHandle};

pub(crate) struct Batch<E> {
    pub events: Vec<E>,
    pub retired: Option<Result<(), String>>,
}

pub(crate) fn poll<E>(worker: &mut Option<JoinHandle<()>>, receiver: Option<&mpsc::Receiver<E>>) -> Batch<E> {
    let mut events = receiver.map(|receiver| receiver.try_iter().collect::<Vec<_>>()).unwrap_or_default();
    let retired = if worker.as_ref().is_some_and(JoinHandle::is_finished) {
        // A terminal/replay event can arrive between the initial drain and the
        // completion check. Once finished, the thread can publish nothing more.
        if let Some(receiver) = receiver { events.extend(receiver.try_iter()); }
        Some(worker.take().unwrap().join().map_err(|panic| {
            let detail = panic.downcast_ref::<String>().map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("unknown panic");
            format!("Capture worker stopped unexpectedly: {detail}")
        }))
    } else { None };
    Batch { events, retired }
}

/// A normal return without a terminal payload is also a failed session. Consume
/// the payload marker so the completed session cannot be finalized twice.
pub(crate) fn outcome(terminal_seen: &mut bool, joined: Result<(), String>) -> Result<(), String> {
    let seen = std::mem::take(terminal_seen);
    joined?;
    if seen { Ok(()) } else { Err("Capture worker stopped without reporting its final result.".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[derive(Debug, PartialEq, Eq)]
    enum Event { Finished, ReplaySaved }
    struct GatedDrop { entered: mpsc::SyncSender<()>, release: mpsc::Receiver<()>, events: mpsc::SyncSender<Event> }
    impl Drop for GatedDrop {
        fn drop(&mut self) {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            self.events.send(Event::ReplaySaved).unwrap();
        }
    }
    fn await_retirement<E>(worker: &mut Option<JoinHandle<()>>, receiver: &mpsc::Receiver<E>) -> Batch<E> {
        let until = Instant::now() + Duration::from_secs(3);
        let mut events = Vec::new();
        loop {
            let batch = poll(worker, Some(receiver));
            events.extend(batch.events);
            if batch.retired.is_some() { return Batch { events, retired: batch.retired }; }
            assert!(Instant::now() < until, "Owned capture worker did not retire");
            std::thread::yield_now();
        }
    }
    #[test]
    fn terminal_before_blocked_destructor_keeps_handle_and_late_replay_until_actual_retirement() {
        let (tx, rx) = mpsc::sync_channel(8);
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let mut worker = Some(std::thread::spawn(move || {
            let _resource = GatedDrop { entered, release: gate, events: tx.clone() };
            tx.send(Event::Finished).unwrap();
        }));
        seen.recv_timeout(Duration::from_secs(3)).unwrap();
        let first = poll(&mut worker, Some(&rx));
        assert_eq!(first.events, [Event::Finished]);
        assert!(first.retired.is_none());
        assert!(worker.is_some(), "Early Finished released capture-start admission");
        #[cfg(windows)]
        let mut pending_restart = true;
        let mut terminal_seen = true;
        release.send(()).unwrap();
        let final_batch = await_retirement(&mut worker, &rx);
        assert_eq!(final_batch.events, [Event::ReplaySaved]);
        assert_eq!(outcome(&mut terminal_seen, final_batch.retired.unwrap()), Ok(()));
        assert!(!terminal_seen && worker.is_none());
        #[cfg(windows)]
        {
            assert!(crate::consume_recording_restart(&mut pending_restart, true));
            assert!(!crate::consume_recording_restart(&mut pending_restart, true));
        }
        assert!(poll(&mut worker, Some(&rx)).retired.is_none(), "Session retired twice");
    }
    #[test]
    fn missing_terminal_and_panicked_workers_retire_with_actionable_failure() {
        for panic in [false, true] {
            let (_tx, rx) = mpsc::sync_channel::<Event>(8);
            let mut worker = Some(std::thread::spawn(move || { assert!(!panic, "injected capture failure"); }));
            let batch = await_retirement(&mut worker, &rx);
            let error = outcome(&mut false, batch.retired.unwrap()).unwrap_err();
            assert!(error.contains(if panic { "injected capture failure" } else { "without reporting" }));
            assert!(worker.is_none());
        }
    }
    #[cfg(windows)]
    #[test]
    fn restart_is_consumed_once_only_after_actual_successful_retirement() {
        for healthy in [false, true] {
            let mut pending = true;
            let mut terminal_seen = healthy;
            let result = outcome(&mut terminal_seen, Ok(()));
            assert_eq!(crate::consume_recording_restart(&mut pending, result.is_ok()), healthy);
            assert!(!pending);
            assert!(!crate::consume_recording_restart(&mut pending, true));
        }
    }
}
