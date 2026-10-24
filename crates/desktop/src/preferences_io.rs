//! One storage owner for preferences.json. Only the Library layout is optimistic;
//! other callers retain their existing synchronous durability/rollback boundary.
use crate::preferences::Preferences;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutAck {
    pub revision: u64,
    pub result: Result<(), String>,
}

struct Barrier {
    preferences: Preferences,
    reply: mpsc::SyncSender<Result<(), String>>,
}

struct State {
    durable: Preferences,
    list_view: bool,
    layout_revision: u64,
    durable_layout_revision: u64,
    pending_layout: bool,
    barrier: Option<Barrier>,
    completion: Option<LayoutAck>,
    active: bool,
    last_write_error: Option<String>,
    closed: bool,
}

pub struct PreferenceWriter {
    state: Arc<(Mutex<State>, Condvar)>,
    // A synchronous caller may wait, but can never append an unbounded queue.
    barrier_admission: Mutex<()>,
}

impl PreferenceWriter {
    pub fn start(
        initial: Preferences,
        mut write: impl FnMut(&Preferences) -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        let state = Arc::new((Mutex::new(State {
            list_view: initial.list_view,
            durable: initial,
            layout_revision: 0,
            durable_layout_revision: 0,
            pending_layout: false,
            barrier: None,
            completion: None,
            active: false,
            last_write_error: None,
            closed: false,
        }), Condvar::new()));
        let worker_state = Arc::clone(&state);
        std::thread::Builder::new().name("rcr-preferences".into()).spawn(move || {
            let (lock, wake) = &*worker_state;
            loop {
                let (candidate, revision, reply) = {
                    let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
                    while state.barrier.is_none() && !state.pending_layout && !state.closed {
                        state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                    }
                    if state.closed && state.barrier.is_none() && !state.pending_layout { return; }
                    let (mut candidate, reply) = if let Some(barrier) = state.barrier.take() {
                        (barrier.preferences, Some(barrier.reply))
                    } else {
                        (state.durable.clone(), None)
                    };
                    // A barrier's potentially older whole snapshot has no authority
                    // over this independently submitted, typed layout intent.
                    candidate.list_view = state.list_view;
                    state.pending_layout = false;
                    state.active = true;
                    (candidate, state.layout_revision, reply)
                };
                // No UI-owned lock or admission is held across filesystem work.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(&candidate)))
                    .unwrap_or_else(|_| Err("Preferences writer panicked before acknowledging settings; retry to save".into()));
                {
                    let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
                    if result.is_ok() {
                        state.durable = candidate;
                        state.durable_layout_revision = revision;
                    }
                    state.last_write_error = result.as_ref().err().cloned();
                    if revision > 0 {
                        // A failed unrelated barrier cannot make an already durable
                        // layout appear unsaved. A successful retry supersedes any
                        // earlier failed completion for this same intent revision.
                        let layout_result = if revision <= state.durable_layout_revision {
                            Ok(())
                        } else { result.clone() };
                        state.completion = Some(LayoutAck { revision, result: layout_result });
                    }
                    state.active = false;
                    wake.notify_all();
                }
                if let Some(reply) = reply { let _ = reply.send(result); }
            }
        }).map_err(|error| format!("Cannot start preferences writer: {error}"))?;
        Ok(Self { state, barrier_admission: Mutex::new(()) })
    }

    /// This field-only intent never serializes or touches a source drive on UI.
    /// Repeated toggles replace one pending value, rather than queuing snapshots.
    pub fn set_list_view(&self, list_view: bool) -> Result<u64, String> {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed { return Err("Preferences writer stopped".into()); }
        let revision = state.layout_revision.checked_add(1)
            .ok_or_else(|| "Preferences revision limit reached; restart to save layout".to_owned())?;
        state.list_view = list_view;
        state.layout_revision = revision;
        state.pending_layout = true;
        wake.notify_one();
        Ok(revision)
    }

    pub fn save_barrier(&self, preferences: &Preferences) -> Result<(), String> {
        let _admission = self.barrier_admission.lock().unwrap_or_else(|error| error.into_inner());
        let (reply, receipt) = mpsc::sync_channel(1);
        let (lock, wake) = &*self.state;
        {
            let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
            if state.closed { return Err("Preferences writer stopped".into()); }
            state.barrier = Some(Barrier { preferences: preferences.clone(), reply });
            wake.notify_one();
        }
        receipt.recv().map_err(|_| "Preferences writer stopped before acknowledging settings".to_owned())?
    }

    pub fn take_layout_ack(&self) -> Option<LayoutAck> {
        self.state.0.lock().unwrap_or_else(|error| error.into_inner()).completion.take()
    }

    /// Called only after the UI event loop exits. Drain already admitted work;
    /// never retry a failure, start another worker, or join a blocked OS write.
    pub fn finish_pending(&self, timeout: Duration) -> Result<(), String> {
        let until = Instant::now() + timeout;
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
        let had_work = state.active || state.pending_layout || state.barrier.is_some();
        state.closed = true;
        wake.notify_all();
        if !had_work { return Ok(()); }
        while state.active || state.pending_layout || state.barrier.is_some() {
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Timed out waiting for the pending preferences write; the selected Library view may not have been saved".into());
            }
            let (next, _) = wake.wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next;
        }
        state.last_write_error.clone().map_or(Ok(()), Err)
    }
}

impl Drop for PreferenceWriter {
    fn drop(&mut self) {
        let (lock, wake) = &*self.state;
        lock.lock().unwrap_or_else(|error| error.into_inner()).closed = true;
        wake.notify_one();
        // No join: an active OS write keeps its ownership until it completes.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::{Duration, Instant}};

    fn wait_for_barrier(writer: &PreferenceWriter) {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if writer.state.0.lock().unwrap().barrier.is_some() { return; }
            assert!(Instant::now() < until, "Sync barrier was not admitted");
            std::thread::yield_now();
        }
    }

    fn wait_for_ack(writer: &PreferenceWriter, revision: u64) -> LayoutAck {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(ack) = writer.take_layout_ack()
                && ack.revision == revision { return ack; }
            assert!(Instant::now() < until, "Layout did not receive its actual write acknowledgement");
            std::thread::yield_now();
        }
    }

    #[test]
    fn blocked_layout_coalesces_and_stale_barrier_preserves_latest_field_intent() {
        let (entered, started) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let (written, writes) = mpsc::channel();
        let mut first = true;
        let initial = Preferences::default();
        let writer = Arc::new(PreferenceWriter::start(initial.clone(), move |candidate| {
            if first { first = false; entered.send(()).unwrap(); gate.recv().unwrap(); }
            written.send(candidate.clone()).unwrap();
            Ok(())
        }).unwrap());
        writer.set_list_view(true).unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        for index in 0..500 { writer.set_list_view(index % 2 == 0).unwrap(); }
        let latest = writer.set_list_view(true).unwrap();
        let mut stale = initial;
        stale.fps = 30;
        stale.favorites.insert(PathBuf::from("fixture-highlight.mkv"));
        stale.microphone = true;
        let barrier_writer = Arc::clone(&writer);
        let barrier = std::thread::spawn(move || barrier_writer.save_barrier(&stale));
        wait_for_barrier(&writer);
        // Exactly one active write, one barrier and one coalesced field intent.
        assert!(writer.state.0.lock().unwrap().pending_layout);
        release.send(()).unwrap();
        assert_eq!(barrier.join().unwrap(), Ok(()));
        let first = writes.recv_timeout(Duration::from_secs(3)).unwrap();
        let final_write = writes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(first.list_view && final_write.list_view);
        assert_eq!(first.fps, 60);
        assert_eq!(final_write.fps, 30);
        assert!(final_write.microphone);
        assert!(final_write.favorites.contains(&PathBuf::from("fixture-highlight.mkv")));
        assert!(writes.try_recv().is_err(), "Intermediate toggles escaped coalescing");
        assert_eq!(wait_for_ack(&writer, latest).result, Ok(()));
    }

    #[test]
    fn successful_barrier_acknowledges_same_revision_after_failed_layout_write() {
        let (written, writes) = mpsc::channel();
        let mut first = true;
        let initial = Preferences::default();
        let writer = PreferenceWriter::start(initial.clone(), move |candidate| {
            written.send(candidate.list_view).unwrap();
            if first { first = false; Err("Synthetic disk-full".into()) } else { Ok(()) }
        }).unwrap();
        let revision = writer.set_list_view(true).unwrap();
        assert_eq!(wait_for_ack(&writer, revision).result, Err("Synthetic disk-full".into()));
        let mut unrelated = initial;
        unrelated.bitrate = 50_000_000;
        writer.save_barrier(&unrelated).unwrap();
        assert_eq!(wait_for_ack(&writer, revision).result, Ok(()));
        assert!(writes.recv().unwrap() && writes.recv().unwrap());
        let state = writer.state.0.lock().unwrap();
        assert_eq!(state.durable.bitrate, 50_000_000);
        assert!(state.durable.list_view);
    }

    #[test]
    fn failed_barrier_never_leaks_rolled_back_fields_into_pending_layout() {
        let (entered, started) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let (written, writes) = mpsc::channel();
        let initial = Preferences::default();
        let mut first = true;
        let writer = Arc::new(PreferenceWriter::start(initial.clone(), move |candidate| {
            written.send(candidate.clone()).unwrap();
            if first {
                first = false;
                entered.send(()).unwrap(); gate.recv().unwrap();
                Err("Synthetic shortcut persistence failure".into())
            } else { Ok(()) }
        }).unwrap());
        let mut proposed = initial.clone();
        proposed.hotkeys[0] = "Ctrl+Alt+F8".into();
        let barrier_writer = Arc::clone(&writer);
        let barrier = std::thread::spawn(move || barrier_writer.save_barrier(&proposed));
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let revision = writer.set_list_view(true).unwrap();
        release.send(()).unwrap();
        assert!(barrier.join().unwrap().is_err());
        assert_eq!(wait_for_ack(&writer, revision).result, Ok(()));
        assert_eq!(writes.recv().unwrap().hotkeys[0], "Ctrl+Alt+F8");
        let layout = writes.recv().unwrap();
        assert_eq!(layout.hotkeys, initial.hotkeys);
        assert!(layout.list_view);
        assert_eq!(writer.state.0.lock().unwrap().durable.hotkeys, initial.hotkeys);
    }

    #[test]
    fn failed_unrelated_barrier_cannot_mark_durable_layout_unsaved() {
        let mut first = true;
        let initial = Preferences::default();
        let writer = PreferenceWriter::start(initial.clone(), move |_| {
            if first { first = false; Ok(()) } else { Err("Synthetic folder save failure".into()) }
        }).unwrap();
        let revision = writer.set_list_view(true).unwrap();
        assert_eq!(wait_for_ack(&writer, revision).result, Ok(()));
        let mut folder_change = initial;
        folder_change.folder = "new-folder".into();
        assert!(writer.save_barrier(&folder_change).is_err());
        assert_eq!(wait_for_ack(&writer, revision).result, Ok(()));
        assert_eq!(writer.state.0.lock().unwrap().durable.folder, PathBuf::from("recordings"));
    }

    #[test]
    fn shutdown_drains_only_admitted_latest_layout_and_refuses_new_intents() {
        let (entered, started) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let (written, writes) = mpsc::channel();
        let mut first = true;
        let writer = Arc::new(PreferenceWriter::start(Preferences::default(), move |candidate| {
            if first { first = false; entered.send(()).unwrap(); gate.recv().unwrap(); }
            written.send(candidate.list_view).unwrap(); Ok(())
        }).unwrap());
        writer.set_list_view(true).unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        writer.set_list_view(false).unwrap();
        let finishing = Arc::clone(&writer);
        let done = std::thread::spawn(move || finishing.finish_pending(Duration::from_secs(3)));
        let until = Instant::now() + Duration::from_secs(3);
        while !writer.state.0.lock().unwrap().closed {
            assert!(Instant::now() < until); std::thread::yield_now();
        }
        assert!(writer.set_list_view(true).is_err());
        release.send(()).unwrap();
        assert_eq!(done.join().unwrap(), Ok(()));
        assert!(writes.recv_timeout(Duration::from_secs(3)).unwrap());
        assert!(!writes.recv_timeout(Duration::from_secs(3)).unwrap());
        assert!(writes.try_recv().is_err());
    }

    #[test]
    fn shutdown_timeout_does_not_release_or_replace_blocked_write() {
        let (entered, started) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let (written, writes) = mpsc::channel();
        let writer = PreferenceWriter::start(Preferences::default(), move |_| {
            entered.send(()).unwrap(); gate.recv().unwrap();
            written.send(()).unwrap(); Ok(())
        }).unwrap();
        writer.set_list_view(true).unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(writer.finish_pending(Duration::from_millis(10)).unwrap_err().contains("Timed out"));
        assert!(writer.state.0.lock().unwrap().active);
        assert!(writes.try_recv().is_err());
        assert!(writer.set_list_view(false).is_err());
        release.send(()).unwrap();
        writes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(writer.finish_pending(Duration::from_secs(3)), Ok(()));
    }

    #[test]
    fn idle_shutdown_never_retries_failure_and_revision_overflow_keeps_intent() {
        let (written, writes) = mpsc::channel();
        let writer = PreferenceWriter::start(Preferences::default(), move |_| {
            written.send(()).unwrap(); Err("Synthetic disk-full".into())
        }).unwrap();
        let revision = writer.set_list_view(true).unwrap();
        assert!(wait_for_ack(&writer, revision).result.is_err());
        writes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(writer.finish_pending(Duration::from_secs(3)), Ok(()));
        assert!(writes.try_recv().is_err(), "Shutdown retried a completed failed write");

        let writer = PreferenceWriter::start(Preferences::default(), |_| Ok(())).unwrap();
        writer.state.0.lock().unwrap().layout_revision = u64::MAX;
        assert!(writer.set_list_view(true).is_err());
        let state = writer.state.0.lock().unwrap();
        assert!(!state.list_view && !state.pending_layout);
    }

    #[test]
    fn writer_panic_acknowledges_failure_then_barrier_and_layout_can_recover() {
        let (written, writes) = mpsc::channel();
        let mut first = true;
        let initial = Preferences::default();
        let writer = PreferenceWriter::start(initial.clone(), move |candidate| {
            written.send(candidate.clone()).unwrap();
            if first { first = false; panic!("Injected preferences writer panic"); }
            Ok(())
        }).unwrap();
        let revision = writer.set_list_view(true).unwrap();
        assert!(wait_for_ack(&writer, revision).result.unwrap_err().contains("panicked"));
        {
            let state = writer.state.0.lock().unwrap();
            assert!(!state.active && !state.closed);
            assert!(!state.durable.list_view);
        }
        let mut proposed = initial;
        proposed.fps = 30;
        writer.save_barrier(&proposed).unwrap();
        assert_eq!(wait_for_ack(&writer, revision).result, Ok(()));
        let final_revision = writer.set_list_view(false).unwrap();
        assert_eq!(wait_for_ack(&writer, final_revision).result, Ok(()));
        let failed = writes.recv_timeout(Duration::from_secs(3)).unwrap();
        let barrier = writes.recv_timeout(Duration::from_secs(3)).unwrap();
        let layout = writes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(failed.list_view && barrier.list_view && !layout.list_view);
        assert_eq!(barrier.fps, 30);
        assert_eq!(layout.fps, 30);
        assert_eq!(writer.finish_pending(Duration::from_secs(3)), Ok(()));
    }
}
