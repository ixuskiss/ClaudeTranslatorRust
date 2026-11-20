//! Hotkey dispatcher: per-family re-entrancy guards, pause, settings snapshot, panic isolation.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use ct_core::{DictionaryEntry, LlmProvider, UserSettings};
use ct_win::hotkey::Gesture;

/// Shared mutable application state; the dispatcher snapshots it at trigger time.
pub struct AppState {
    pub settings: RwLock<UserSettings>,
    pub entries: RwLock<Vec<DictionaryEntry>>,
    pub llm: RwLock<Arc<dyn LlmProvider>>,
    pub paused: AtomicBool,
}

/// Immutable view of the state at the moment a gesture was triggered.
pub struct Snapshot {
    pub settings: UserSettings,
    pub entries: Vec<DictionaryEntry>,
    pub llm: Arc<dyn LlmProvider>,
}

pub trait GestureAction: Send + Sync {
    fn run(&self, gesture: Gesture, snapshot: Snapshot);

    /// Called (inside its own `catch_unwind`) after `run` panicked, before the guard is released.
    fn on_panic(&self, _gesture: Gesture) {}
}

#[derive(Clone, Copy)]
enum Family {
    Alt,
    Ctrl,
}

impl Family {
    fn of(gesture: Gesture) -> Self {
        match gesture {
            Gesture::AltAlt | Gesture::CtrlHeldAltAlt => Self::Alt,
            Gesture::CtrlCtrl | Gesture::AltHeldCtrlCtrl => Self::Ctrl,
        }
    }
}

/// Releases a family guard on drop (also during unwinding).
struct GuardRelease(Arc<AtomicBool>);

impl Drop for GuardRelease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub struct Dispatcher {
    state: Arc<AppState>,
    action: Arc<dyn GestureAction>,
    alt_busy: Arc<AtomicBool>,
    ctrl_busy: Arc<AtomicBool>,
}

impl Dispatcher {
    pub fn new(state: Arc<AppState>, action: Arc<dyn GestureAction>) -> Self {
        Self {
            state,
            action,
            alt_busy: Arc::new(AtomicBool::new(false)),
            ctrl_busy: Arc::new(AtomicBool::new(false)),
        }
    }

    fn guard(&self, family: Family) -> &Arc<AtomicBool> {
        match family {
            Family::Alt => &self.alt_busy,
            Family::Ctrl => &self.ctrl_busy,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            settings: self
                .state
                .settings
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            entries: self
                .state
                .entries
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            llm: Arc::clone(
                &self
                    .state
                    .llm
                    .read()
                    .unwrap_or_else(PoisonError::into_inner),
            ),
        }
    }

    /// Called from the hook thread. Ignored while paused; dropped (not queued) while the same
    /// family is running; otherwise runs the action on a new thread with a trigger-time snapshot.
    pub fn dispatch(&self, gesture: Gesture) {
        if self.state.paused.load(Ordering::Acquire) {
            return;
        }
        let guard = self.guard(Family::of(gesture));
        if guard
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::debug!(?gesture, "dropped: same family still running");
            return;
        }
        let release = GuardRelease(Arc::clone(guard));
        let snapshot = self.snapshot();
        let action = Arc::clone(&self.action);
        let spawned = std::thread::Builder::new()
            .name("gesture-action".into())
            .spawn(move || {
                let _release = release;
                if catch_unwind(AssertUnwindSafe(|| action.run(gesture, snapshot))).is_err() {
                    tracing::error!(?gesture, "gesture action panicked");
                    if catch_unwind(AssertUnwindSafe(|| action.on_panic(gesture))).is_err() {
                        tracing::error!(?gesture, "on_panic handler panicked");
                    }
                }
            });
        if let Err(error) = spawned {
            // The closure (and its guard release) was dropped with the failed spawn.
            tracing::error!(?gesture, %error, "failed to spawn gesture thread");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, Sender, unbounded};
    use ct_core::{
        CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationResult, LlmError,
        TranslationRequest, TranslationResult,
    };
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    struct NullLlm;
    impl LlmProvider for NullLlm {
        fn translate(&self, _: &TranslationRequest) -> Result<TranslationResult, LlmError> {
            Err(LlmError("null".into()))
        }
        fn explain(&self, _: &ExplanationRequest) -> Result<ExplanationResult, LlmError> {
            Err(LlmError("null".into()))
        }
        fn correct(&self, _: &CorrectionRequest) -> Result<String, LlmError> {
            Err(LlmError("null".into()))
        }
        fn correct_sentences(
            &self,
            _: &CorrectionRequest,
        ) -> Result<Vec<CorrectionSentence>, LlmError> {
            Err(LlmError("null".into()))
        }
    }

    /// Reports `started`, waits for `release`, then optionally panics; reports `finished`.
    struct TestAction {
        started: Sender<(Gesture, Snapshot)>,
        release: Receiver<()>,
        panic_gestures: Vec<Gesture>,
        panics_seen: AtomicUsize,
        finished: Sender<Gesture>,
    }

    impl GestureAction for TestAction {
        fn run(&self, gesture: Gesture, snapshot: Snapshot) {
            self.started.send((gesture, snapshot)).unwrap();
            self.release.recv().unwrap();
            if self.panic_gestures.contains(&gesture) {
                panic!("boom");
            }
            self.finished.send(gesture).unwrap();
        }
        fn on_panic(&self, _gesture: Gesture) {
            self.panics_seen.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Harness {
        dispatcher: Dispatcher,
        state: Arc<AppState>,
        action: Arc<TestAction>,
        started: Receiver<(Gesture, Snapshot)>,
        release: Sender<()>,
        finished: Receiver<Gesture>,
    }

    fn harness(panic_gestures: Vec<Gesture>) -> Harness {
        let state = Arc::new(AppState {
            settings: RwLock::new(UserSettings::default()),
            entries: RwLock::new(Vec::new()),
            llm: RwLock::new(Arc::new(NullLlm)),
            paused: AtomicBool::new(false),
        });
        let (started_tx, started) = unbounded();
        let (release, release_rx) = unbounded();
        let (finished_tx, finished) = unbounded();
        let action = Arc::new(TestAction {
            started: started_tx,
            release: release_rx,
            panic_gestures,
            panics_seen: AtomicUsize::new(0),
            finished: finished_tx,
        });
        let dispatcher = Dispatcher::new(Arc::clone(&state), action.clone());
        Harness {
            dispatcher,
            state,
            action,
            started,
            release,
            finished,
        }
    }

    const WAIT: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(150);

    /// Waits until the family guard for `gesture` is free again.
    fn wait_released(h: &Harness, gesture: Gesture) {
        let guard = h.dispatcher.guard(Family::of(gesture));
        let deadline = std::time::Instant::now() + WAIT;
        while guard.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline, "guard never released");
            std::thread::yield_now();
        }
    }

    #[test]
    fn same_family_while_running_is_dropped() {
        let h = harness(vec![]);
        h.dispatcher.dispatch(Gesture::AltAlt);
        h.started.recv_timeout(WAIT).unwrap();
        // Same family (both Alt gestures) while running: dropped, not queued.
        h.dispatcher.dispatch(Gesture::AltAlt);
        h.dispatcher.dispatch(Gesture::CtrlHeldAltAlt);
        assert!(h.started.recv_timeout(SHORT).is_err());
        h.release.send(()).unwrap();
        assert_eq!(h.finished.recv_timeout(WAIT).unwrap(), Gesture::AltAlt);
        wait_released(&h, Gesture::AltAlt);
        // Nothing was queued behind it.
        assert!(h.started.recv_timeout(SHORT).is_err());
        // And the family works again afterwards.
        h.dispatcher.dispatch(Gesture::AltAlt);
        h.started.recv_timeout(WAIT).unwrap();
        h.release.send(()).unwrap();
    }

    #[test]
    fn alt_and_ctrl_families_run_concurrently() {
        let h = harness(vec![]);
        h.dispatcher.dispatch(Gesture::AltAlt);
        h.dispatcher.dispatch(Gesture::CtrlCtrl);
        // Both are inside run() at the same time (neither released yet).
        let a = h.started.recv_timeout(WAIT).unwrap().0;
        let b = h.started.recv_timeout(WAIT).unwrap().0;
        assert!(
            (a == Gesture::AltAlt && b == Gesture::CtrlCtrl)
                || (a == Gesture::CtrlCtrl && b == Gesture::AltAlt)
        );
        // The Ctrl-family chord is still blocked by the running Ctrl action.
        h.dispatcher.dispatch(Gesture::AltHeldCtrlCtrl);
        assert!(h.started.recv_timeout(SHORT).is_err());
        h.release.send(()).unwrap();
        h.release.send(()).unwrap();
    }

    #[test]
    fn paused_drops_gesture() {
        let h = harness(vec![]);
        h.state.paused.store(true, Ordering::Release);
        h.dispatcher.dispatch(Gesture::AltAlt);
        assert!(h.started.recv_timeout(SHORT).is_err());
        h.state.paused.store(false, Ordering::Release);
        h.dispatcher.dispatch(Gesture::AltAlt);
        h.started.recv_timeout(WAIT).unwrap();
        h.release.send(()).unwrap();
    }

    #[test]
    fn panic_in_action_releases_guard() {
        let h = harness(vec![Gesture::AltAlt]);
        h.dispatcher.dispatch(Gesture::AltAlt);
        h.started.recv_timeout(WAIT).unwrap();
        h.release.send(()).unwrap();
        wait_released(&h, Gesture::AltAlt);
        // Next dispatch runs.
        h.dispatcher.dispatch(Gesture::CtrlHeldAltAlt);
        let (g, _) = h.started.recv_timeout(WAIT).unwrap();
        assert_eq!(g, Gesture::CtrlHeldAltAlt);
        h.release.send(()).unwrap();
        assert_eq!(
            h.finished.recv_timeout(WAIT).unwrap(),
            Gesture::CtrlHeldAltAlt
        );
    }

    #[test]
    fn on_panic_called_once_on_panic_and_not_on_success() {
        let h = harness(vec![Gesture::AltAlt]);
        h.dispatcher.dispatch(Gesture::CtrlCtrl);
        h.started.recv_timeout(WAIT).unwrap();
        h.release.send(()).unwrap();
        h.finished.recv_timeout(WAIT).unwrap();
        wait_released(&h, Gesture::CtrlCtrl);
        assert_eq!(h.action.panics_seen.load(Ordering::SeqCst), 0);

        h.dispatcher.dispatch(Gesture::AltAlt);
        h.started.recv_timeout(WAIT).unwrap();
        h.release.send(()).unwrap();
        wait_released(&h, Gesture::AltAlt);
        assert_eq!(h.action.panics_seen.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dispatch_snapshots_settings_at_trigger() {
        let h = harness(vec![]);
        let before = h.state.settings.read().unwrap().clone();
        h.dispatcher.dispatch(Gesture::AltAlt);
        let (_, snapshot) = h.started.recv_timeout(WAIT).unwrap();
        // Change settings while the action is running.
        h.state.settings.write().unwrap().claude_model = "opus".to_string();
        assert_ne!(*h.state.settings.read().unwrap(), before);
        assert_eq!(snapshot.settings, before);
        h.release.send(()).unwrap();
    }
}
