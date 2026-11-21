//! ClaudeTranslator composition root (v5 reference §5.1, §5.2, §5.8, §5.9).
//!
//! Start-up order, the hook → router → dispatcher → flow wiring, tray command handling on
//! the UI thread, the Settings hot-swap and shutdown. Logs carry operation and status
//! only, never captured or translated text.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use ct_app::dispatch::{AppState, Dispatcher, GestureAction, Snapshot};
use ct_app::flows::{
    Badge, ExplainView, FlowDeps, MSG_CLI_UNAVAILABLE, MSG_CORRECTION_FAILED, MSG_EXPLAIN_FAILED,
    MSG_TRANSLATE_FAILED, Notifier, alt_alt_flow, correction_replace_flow, correction_view_flow,
    ctrl_ctrl_flow,
};
use ct_app::logging;
use ct_app::ui::tray::{Tray, TrayCommand};
use ct_app::ui::win32::{message_box, message_box_blocking};
use ct_app::ui::{BadgeWindow, DictionaryWindow, ExplainWindow, SettingsWindow};
use ct_core::{
    AppConfig, Lang, LanguageDetector, PersonalDictionaryStore, RuleBasedDetector, SettingsStore,
    UserSettings, file_problems_warning, load_app_config_from_environment, normalize_targets,
    toggle_target, user_data_dir,
};
use ct_llm::{
    BuiltProvider, CatalogSource, ClaudeCliClient, ModelCatalog, ProviderOverrides, RealSpawner,
    RefreshOutcome, build_llm_provider, home_dir, refresh_catalog_and_warm_up,
};
use ct_win::chrome::{
    ChromeAdapter, READ_ONLY_STRATEGIES, REPLACE_STRATEGIES, RetryingInteraction, TextInteraction,
    UiaBackend, WindowsUiaBackend,
};
use ct_win::clipboard::{Clipboard, Win32Clipboard};
use ct_win::hotkey::{ChordRouter, Gesture, KeyboardHook};
use ct_win::keys::{
    KeySender, MODIFIER_RELEASE_TIMEOUT, Win32KeySender, wait_for_modifiers_released,
};
use ct_win::registry::apply_autostart;
use ct_win::single_instance::{ALREADY_RUNNING_MESSAGE, SINGLE_INSTANCE_NAME, SingleInstance};

const APP_TITLE: &str = "ClaudeTranslator";
const HOOK_FAILED_MESSAGE: &str = "키보드 단축키를 등록하지 못해 Alt+Alt, Ctrl+Ctrl을 사용할 수 없습니다. 트레이 메뉴와 Settings는 사용할 수 있습니다. 프로그램을 다시 시작해 보세요.";
const SETTINGS_FILE: &str = "settings.yaml";
const DICTIONARY_FILE: &str = "user.yaml";
const MODELS_CACHE_FILE: &str = "models_cache.json";
/// Tray events are drained on the UI thread at this period (ruling R6).
const TRAY_POLL: Duration = Duration::from_millis(50);
/// How long Exit waits for the CLI process to be closed before leaving anyway.
const CLOSE_WAIT: Duration = Duration::from_secs(3);
/// Autostart is written only by release builds (P7).
const IS_RELEASE: bool = cfg!(not(debug_assertions));

/// Settings values handed to the provider factory; the model `"default"` means "no
/// override" and is passed as `""`.
fn provider_overrides(s: &UserSettings) -> ProviderOverrides {
    let model = if s.claude_model == "default" {
        String::new()
    } else {
        s.claude_model.clone()
    };
    ProviderOverrides {
        cli_path: s.claude_cli_path.clone(),
        model,
        effort: s.claude_effort.clone(),
    }
}

/// Seconds (Settings) to the milliseconds the double-press detectors read (P1).
/// Clamped to 0.1..=2.0 s (R9); NaN falls back to the default 0.2 s.
fn interval_ms(seconds: f64) -> u64 {
    const DEFAULT_SECONDS: f64 = 0.2;
    let seconds = if seconds.is_nan() {
        DEFAULT_SECONDS
    } else {
        seconds.clamp(0.1, 2.0)
    };
    (seconds * 1000.0).round() as u64
}

/// Settings after a tray Target click, or `None` when the toggle changes nothing (the base
/// language, or the last remaining target).
fn toggled_settings(current: &UserSettings, lang: Lang) -> Option<UserSettings> {
    let base = current.base_language;
    let targets = toggle_target(base, &current.target_languages, lang);
    if targets == normalize_targets(base, &current.target_languages) {
        return None;
    }
    Some(UserSettings {
        target_languages: targets,
        ..current.clone()
    })
}

/// Runs the gesture flows on the dispatcher's action thread.
struct AppAction {
    detector: Box<dyn LanguageDetector>,
    replace: Box<dyn TextInteraction>,
    view_only: Box<dyn TextInteraction>,
    docs_or_slides: Box<dyn Fn() -> bool + Send + Sync>,
    /// Waits (bounded) for the gesture's modifier keys to be released.
    modifiers_released: Box<dyn Fn() -> bool + Send + Sync>,
    badge: Arc<dyn Badge>,
    notifier: Arc<dyn Notifier>,
    view: Arc<dyn ExplainView>,
}

impl GestureAction for AppAction {
    fn run(&self, gesture: Gesture, snapshot: Snapshot) {
        let deps = FlowDeps {
            settings: &snapshot.settings,
            entries: &snapshot.entries,
            detector: self.detector.as_ref(),
            llm: snapshot.llm.as_ref(),
        };
        match gesture {
            Gesture::AltAlt => alt_alt_flow(
                &deps,
                self.replace.as_ref(),
                self.docs_or_slides.as_ref(),
                self.badge.as_ref(),
                self.notifier.as_ref(),
            ),
            Gesture::CtrlCtrl => ctrl_ctrl_flow(&deps, self.view_only.as_ref(), self.view.as_ref()),
            Gesture::CtrlHeldAltAlt => correction_replace_flow(
                &deps,
                self.replace.as_ref(),
                self.modifiers_released.as_ref(),
                self.badge.as_ref(),
                self.notifier.as_ref(),
            ),
            Gesture::AltHeldCtrlCtrl => correction_view_flow(
                &deps,
                self.view_only.as_ref(),
                self.modifiers_released.as_ref(),
                self.view.as_ref(),
            ),
        }
    }

    /// Ruling R5: tell the user the run failed (the original text was not replaced).
    fn on_panic(&self, gesture: Gesture) {
        match gesture {
            Gesture::AltAlt => {
                self.badge.hide();
                self.notifier.notify(MSG_TRANSLATE_FAILED);
            }
            Gesture::CtrlHeldAltAlt => {
                self.badge.hide();
                self.notifier.notify(MSG_CORRECTION_FAILED);
            }
            Gesture::CtrlCtrl => self.view.show_error(MSG_EXPLAIN_FAILED),
            Gesture::AltHeldCtrlCtrl => self.view.show_error(MSG_CORRECTION_FAILED),
        }
    }
}

fn build_provider(
    config: &AppConfig,
    data_dir: &Path,
    settings: &UserSettings,
    catalog: &ModelCatalog,
) -> BuiltProvider {
    build_llm_provider(
        config,
        data_dir,
        &provider_overrides(settings),
        &catalog.get(),
        Arc::new(RealSpawner),
    )
}

/// The CLI-unavailable notice is shown at most once per run, however many warm-ups fail.
static CLI_NOTICE_SHOWN: AtomicBool = AtomicBool::new(false);

/// Held while a Settings-triggered catalog retry runs, so repeated Settings opens start at
/// most one at a time. Releases on drop (also if the thread panics).
struct InFlight(&'static AtomicBool);

impl InFlight {
    fn try_begin(flag: &'static AtomicBool) -> Option<Self> {
        (!flag.swap(true, Ordering::AcqRel)).then_some(Self(flag))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

static CATALOG_RETRY_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Boots the CLI and refreshes the model list on a background thread. Failures are
/// non-fatal; a CLI that cannot be started is reported once with a non-modal box.
fn warm_up_in_background(
    built: &BuiltProvider,
    catalog: Arc<ModelCatalog>,
    in_flight: Option<InFlight>,
) {
    let built = BuiltProvider {
        provider: Arc::clone(&built.provider),
        client: built.client.clone(),
    };
    let spawned = std::thread::Builder::new()
        .name("cli-warm-up".into())
        .spawn(move || {
            let _in_flight = in_flight;
            if refresh_catalog_and_warm_up(&built, &catalog) == RefreshOutcome::SpawnFailed
                && !CLI_NOTICE_SHOWN.swap(true, Ordering::AcqRel)
            {
                message_box(APP_TITLE, MSG_CLI_UNAVAILABLE);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not start the CLI warm-up thread");
    }
}

/// Settings shows the hardcoded fallback list only when no live or cached list exists, so
/// opening it is a good moment for one more refresh attempt.
fn should_retry_catalog(source: CatalogSource) -> bool {
    source == CatalogSource::Fallback
}

/// Closes a replaced CLI client off the UI thread (it waits for an in-flight turn).
fn close_in_background(client: Arc<ClaudeCliClient>) {
    let spawned = std::thread::Builder::new()
        .name("cli-close".into())
        .spawn(move || client.close());
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not start the CLI close thread");
    }
}

/// Closes the current CLI client at exit, waiting at most `CLOSE_WAIT`.
fn close_at_exit(client: Option<Arc<ClaudeCliClient>>) {
    let Some(client) = client else {
        return;
    };
    let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);
    let spawned = std::thread::Builder::new()
        .name("cli-close".into())
        .spawn(move || {
            client.close();
            let _ = done_tx.send(());
        });
    match spawned {
        Ok(_) => {
            if done_rx.recv_timeout(CLOSE_WAIT).is_err() {
                tracing::warn!("Claude CLI did not close in time; exiting anyway");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not start the CLI close thread"),
    }
}

/// Everything the UI thread needs to apply tray commands and Settings saves.
struct App {
    config: AppConfig,
    data_dir: PathBuf,
    exe: Option<PathBuf>,
    state: Arc<AppState>,
    settings_store: Arc<Mutex<SettingsStore>>,
    dictionary_store: Arc<Mutex<PersonalDictionaryStore>>,
    catalog: Arc<ModelCatalog>,
    client: RefCell<Option<Arc<ClaudeCliClient>>>,
    alt_interval_ms: Arc<AtomicU64>,
    ctrl_interval_ms: Arc<AtomicU64>,
    tray: Option<Tray>,
}

impl App {
    fn apply_autostart(&self, enabled: bool) {
        if let Some(exe) = &self.exe {
            apply_autostart(enabled, IS_RELEASE, exe);
        }
    }

    fn set_settings(&self, settings: &UserSettings) {
        *self
            .state
            .settings
            .write()
            .unwrap_or_else(PoisonError::into_inner) = settings.clone();
    }

    fn handle(self: &Rc<Self>, command: TrayCommand) {
        match command {
            TrayCommand::ToggleTarget(lang) => self.toggle_target(lang),
            TrayCommand::OpenDictionary => {
                let state = Arc::clone(&self.state);
                DictionaryWindow::open(Arc::clone(&self.dictionary_store), move |entries| {
                    tracing::info!(count = entries.len(), "personal dictionary applied");
                    *state
                        .entries
                        .write()
                        .unwrap_or_else(PoisonError::into_inner) = entries;
                });
            }
            TrayCommand::OpenSettings => {
                if should_retry_catalog(self.catalog.source())
                    && let Some(guard) = InFlight::try_begin(&CATALOG_RETRY_IN_FLIGHT)
                {
                    let current = BuiltProvider {
                        provider: Arc::clone(
                            &self
                                .state
                                .llm
                                .read()
                                .unwrap_or_else(PoisonError::into_inner),
                        ),
                        client: self.client.borrow().clone(),
                    };
                    warm_up_in_background(&current, Arc::clone(&self.catalog), Some(guard));
                }
                let app = Rc::clone(self);
                SettingsWindow::open(
                    Arc::clone(&self.settings_store),
                    Arc::clone(&self.catalog),
                    move |settings| app.on_settings_saved(settings),
                );
            }
            TrayCommand::SetPaused(paused) => {
                tracing::info!(paused, "pause toggled");
                self.state.paused.store(paused, Ordering::Release);
            }
            TrayCommand::Exit => {
                tracing::info!("exit requested");
                if let Err(e) = slint::quit_event_loop() {
                    tracing::error!(error = %e, "could not quit the event loop");
                }
            }
        }
    }

    /// Ruling R6: save the toggled targets, then refresh the tray. A refused toggle (or a
    /// failed save) leaves the tray showing the stored state, which `poll` re-applied.
    fn toggle_target(&self, lang: Lang) {
        let saved = {
            let mut store = self
                .settings_store
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(next) = toggled_settings(store.get(), lang) else {
                tracing::debug!(?lang, "target toggle refused");
                return;
            };
            if let Err(e) = store.update(next) {
                tracing::warn!(kind = ?e.kind(), "could not save the target languages");
                return;
            }
            store.get().clone()
        };
        tracing::info!(
            count = saved.target_languages.len(),
            "target languages saved"
        );
        self.set_settings(&saved);
        if let Some(tray) = &self.tray {
            tray.refresh(&saved);
        }
    }

    /// v5 §5.8 plus P1/P6: the store already holds `settings`.
    fn on_settings_saved(&self, settings: UserSettings) {
        self.set_settings(&settings);
        let built = build_provider(&self.config, &self.data_dir, &settings, &self.catalog);
        warm_up_in_background(&built, Arc::clone(&self.catalog), None);
        *self
            .state
            .llm
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::clone(&built.provider);
        if let Some(old) = self.client.replace(built.client) {
            close_in_background(old);
        }
        self.apply_autostart(settings.start_on_login);
        self.alt_interval_ms.store(
            interval_ms(settings.alt_alt_interval_seconds),
            Ordering::Release,
        );
        self.ctrl_interval_ms.store(
            interval_ms(settings.ctrl_ctrl_interval_seconds),
            Ordering::Release,
        );
        if let Some(tray) = &self.tray {
            tray.refresh(&settings);
        }
        tracing::info!("settings applied");
    }
}

fn main() {
    // P4: one instance per user session. The box must block: the process exits next.
    let instance = match SingleInstance::acquire(SINGLE_INSTANCE_NAME) {
        Ok(Some(instance)) => Ok(instance),
        Ok(None) => {
            message_box_blocking(APP_TITLE, ALREADY_RUNNING_MESSAGE);
            return;
        }
        Err(e) => Err(e),
    };

    let data_dir = user_data_dir(std::env::var("APPDATA").ok().as_deref(), &home_dir());
    let data_dir_error = std::fs::create_dir_all(&data_dir).err();
    let log = logging::init(&data_dir);
    tracing::info!(release = IS_RELEASE, "ClaudeTranslator starting");
    if let Some(e) = data_dir_error {
        tracing::error!(kind = ?e.kind(), "could not create the data folder");
    }
    // Keep the mutex for the whole run (dropped at the end of main).
    let _instance = match instance {
        Ok(instance) => Some(instance),
        Err(e) => {
            tracing::warn!(error = %e, "single-instance check failed; continuing");
            None
        }
    };

    let exe = std::env::current_exe().ok();
    let exe_dir = exe
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let config = load_app_config_from_environment(&exe_dir, &data_dir);
    // `.env` may have set LOG_LEVEL after the subscriber was installed.
    log.apply_env_level();
    let mut problems = Vec::new();
    let settings_store = SettingsStore::load(&data_dir.join(SETTINGS_FILE), &mut problems);
    let dictionary_store =
        PersonalDictionaryStore::load(&data_dir.join(DICTIONARY_FILE), &mut problems);
    let settings = settings_store.get().clone();
    let entries = dictionary_store.entries().to_vec();
    tracing::info!(
        provider = ?config.llm_provider,
        dictionary_entries = entries.len(),
        file_problems = problems.len(),
        "configuration loaded"
    );

    if let Some(exe) = &exe {
        apply_autostart(settings.start_on_login, IS_RELEASE, exe);
    }

    let catalog = Arc::new(ModelCatalog::new(Some(data_dir.join(MODELS_CACHE_FILE))));
    let built = build_provider(&config, &data_dir, &settings, &catalog);
    warm_up_in_background(&built, Arc::clone(&catalog), None);

    let state = Arc::new(AppState {
        settings: RwLock::new(settings.clone()),
        entries: RwLock::new(entries),
        llm: RwLock::new(Arc::clone(&built.provider)),
        paused: AtomicBool::new(false),
    });

    let keys = Arc::new(Win32KeySender);
    let clipboard: Arc<dyn Clipboard> = Arc::new(Win32Clipboard);
    let uia = Arc::new(WindowsUiaBackend::new(keys.clone(), Arc::clone(&clipboard)));
    let replace = RetryingInteraction::new(ChromeAdapter::new(
        uia.clone(),
        Arc::clone(&clipboard),
        &REPLACE_STRATEGIES,
    ));
    let view_only = RetryingInteraction::new(ChromeAdapter::new(
        uia.clone(),
        Arc::clone(&clipboard),
        &READ_ONLY_STRATEGIES,
    ));

    // The badge schedules its creation with invoke_from_event_loop, which needs an
    // initialised platform; no window exists yet, so select the backend explicitly.
    if let Err(e) = slint::BackendSelector::new().select() {
        tracing::error!(error = %e, "Slint backend initialisation failed");
    }
    let explain = Arc::new(ExplainWindow::new());
    let badge = Arc::new(BadgeWindow::create());
    let tray = match Tray::new(&settings) {
        Ok(tray) => Some(tray),
        Err(e) => {
            tracing::error!(error = %e, "tray icon could not be created");
            None
        }
    };

    let keys_for_wait = keys.clone();
    let action = AppAction {
        detector: Box::new(RuleBasedDetector),
        replace: Box::new(replace),
        view_only: Box::new(view_only),
        docs_or_slides: Box::new(move || uia.foreground_is_docs_or_slides()),
        modifiers_released: Box::new(move || {
            wait_for_modifiers_released(keys_for_wait.as_ref(), MODIFIER_RELEASE_TIMEOUT)
        }),
        badge,
        notifier: explain.clone(),
        view: explain,
    };
    let dispatcher = Dispatcher::new(Arc::clone(&state), Arc::new(action));

    let alt_interval_ms = Arc::new(AtomicU64::new(interval_ms(
        settings.alt_alt_interval_seconds,
    )));
    let ctrl_interval_ms = Arc::new(AtomicU64::new(interval_ms(
        settings.ctrl_ctrl_interval_seconds,
    )));
    let probe_keys = keys.clone();
    let mut router = ChordRouter::new(
        Arc::clone(&alt_interval_ms),
        Arc::clone(&ctrl_interval_ms),
        Box::new(move |m| Some(probe_keys.is_down(m))),
    );
    let hook = match KeyboardHook::start(Box::new(move |key, dir, at| {
        if let Some(gesture) = router.on_key(key, dir, at) {
            dispatcher.dispatch(gesture);
        }
    })) {
        Ok(hook) => {
            tracing::info!("keyboard hook installed");
            Some(hook)
        }
        Err(e) => {
            tracing::error!(error = %e, "keyboard hook installation failed");
            message_box(APP_TITLE, HOOK_FAILED_MESSAGE);
            None
        }
    };

    if let Some(warning) = file_problems_warning(&problems) {
        message_box(APP_TITLE, &warning);
    }

    let app = Rc::new(App {
        config,
        data_dir,
        exe,
        state,
        settings_store: Arc::new(Mutex::new(settings_store)),
        dictionary_store: Arc::new(Mutex::new(dictionary_store)),
        catalog,
        client: RefCell::new(built.client),
        alt_interval_ms,
        ctrl_interval_ms,
        tray,
    });
    let poll = slint::Timer::default();
    {
        let app = Rc::clone(&app);
        poll.start(slint::TimerMode::Repeated, TRAY_POLL, move || {
            let commands = match &app.tray {
                Some(tray) => tray.poll(),
                None => Vec::new(),
            };
            for command in commands {
                app.handle(command);
            }
        });
    }

    tracing::info!("event loop starting");
    if let Err(e) = slint::run_event_loop_until_quit() {
        tracing::error!(error = %e, "event loop failed");
    }

    // v5 §5.9: stop the hook first so no new gesture starts, then close the CLI.
    poll.stop();
    if let Some(hook) = hook {
        hook.stop();
    }
    close_at_exit(app.client.take());
    tracing::info!("ClaudeTranslator stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use ct_app::flows::{MSG_CORRECTION_REPLACE_NO_SELECTION, MSG_CORRECTION_VIEW_NO_SELECTION};
    use ct_core::{CorrectionStatus, DisplayMode, ExplanationResult};
    use ct_llm::MockProvider;
    use ct_win::chrome::{CapturedText, ReplaceError};

    type Log = Arc<Mutex<Vec<String>>>;

    #[test]
    fn in_flight_guard_allows_one_holder_and_releases_on_drop() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let first = InFlight::try_begin(&FLAG);
        assert!(first.is_some());
        assert!(InFlight::try_begin(&FLAG).is_none());
        drop(first);
        assert!(InFlight::try_begin(&FLAG).is_some());
    }

    #[test]
    fn catalog_is_retried_only_when_settings_would_show_the_fallback() {
        assert!(should_retry_catalog(CatalogSource::Fallback));
        assert!(!should_retry_catalog(CatalogSource::Cache));
        assert!(!should_retry_catalog(CatalogSource::Live));
    }

    fn push(log: &Log, s: impl Into<String>) {
        log.lock().unwrap().push(s.into());
    }

    struct Recorder(Log);

    impl Badge for Recorder {
        fn show(&self, _text: &str) {
            push(&self.0, "badge.show");
        }
        fn hide(&self) {
            push(&self.0, "badge.hide");
        }
    }

    impl Notifier for Recorder {
        fn notify(&self, message: &str) {
            push(&self.0, format!("notify:{message}"));
        }
    }

    impl ExplainView for Recorder {
        fn show_loading(&self, _text: &str) {
            push(&self.0, "view.loading");
        }
        fn show_error(&self, message: &str) {
            push(&self.0, format!("view.error:{message}"));
        }
        fn show_result(&self, _result: ExplanationResult, _mode: DisplayMode) {
            push(&self.0, "view.result");
        }
        fn show_correction(
            &self,
            _sentences: Vec<ct_core::CorrectionSentence>,
            _mode: DisplayMode,
        ) {
            push(&self.0, "view.correction");
        }
    }

    struct NoCapture(Log);

    impl TextInteraction for NoCapture {
        fn capture(&self) -> Option<CapturedText> {
            push(&self.0, "capture");
            None
        }
        fn replace(&self, _: &CapturedText, _: &str) -> Result<(), ReplaceError> {
            push(&self.0, "replace");
            Ok(())
        }
    }

    fn action(log: &Log) -> AppAction {
        let recorder = Arc::new(Recorder(log.clone()));
        AppAction {
            detector: Box::new(RuleBasedDetector),
            replace: Box::new(NoCapture(log.clone())),
            view_only: Box::new(NoCapture(log.clone())),
            docs_or_slides: Box::new(|| false),
            modifiers_released: Box::new(|| true),
            badge: recorder.clone(),
            notifier: recorder.clone(),
            view: recorder,
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            settings: UserSettings::default(),
            entries: Vec::new(),
            llm: Arc::new(MockProvider),
        }
    }

    fn take(log: &Log) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    #[test]
    fn on_panic_alt_family_hides_badge_and_notifies() {
        let log = Log::default();
        let a = action(&log);
        a.on_panic(Gesture::AltAlt);
        assert_eq!(
            take(&log),
            vec![
                "badge.hide".to_string(),
                format!("notify:{MSG_TRANSLATE_FAILED}")
            ]
        );
    }

    #[test]
    fn on_panic_ctrl_family_shows_explain_error() {
        let log = Log::default();
        let a = action(&log);
        a.on_panic(Gesture::CtrlCtrl);
        assert_eq!(take(&log), vec![format!("view.error:{MSG_EXPLAIN_FAILED}")]);
    }

    #[test]
    fn on_panic_correction_gestures_show_correction_failed() {
        let log = Log::default();
        let a = action(&log);
        a.on_panic(Gesture::CtrlHeldAltAlt);
        assert_eq!(
            take(&log),
            vec![
                "badge.hide".to_string(),
                format!("notify:{MSG_CORRECTION_FAILED}")
            ]
        );
        a.on_panic(Gesture::AltHeldCtrlCtrl);
        assert_eq!(
            take(&log),
            vec![format!("view.error:{MSG_CORRECTION_FAILED}")]
        );
    }

    #[test]
    fn correction_failed_constant_matches_core_message() {
        assert_eq!(
            MSG_CORRECTION_FAILED,
            CorrectionStatus::LlmFailed.user_message()
        );
    }

    #[test]
    fn run_routes_correction_gestures_to_correction_flows() {
        let log = Log::default();
        let a = action(&log);
        a.run(Gesture::CtrlHeldAltAlt, snapshot());
        assert_eq!(
            take(&log),
            vec![
                "badge.show".to_string(),
                "capture".to_string(),
                "badge.hide".to_string(),
                format!("notify:{MSG_CORRECTION_REPLACE_NO_SELECTION}")
            ]
        );
        a.run(Gesture::AltHeldCtrlCtrl, snapshot());
        assert_eq!(
            take(&log),
            vec![
                "capture".to_string(),
                format!("view.error:{MSG_CORRECTION_VIEW_NO_SELECTION}")
            ]
        );
    }

    #[test]
    fn run_routes_gestures_to_flows() {
        let log = Log::default();
        let a = action(&log);
        // Alt+Alt: badge around a capture; nothing captured outside Docs/Slides is silent.
        a.run(Gesture::AltAlt, snapshot());
        assert_eq!(take(&log), vec!["badge.show", "capture", "badge.hide"]);
        // Ctrl+Ctrl: capture first, then the "no selection" error in the view.
        a.run(Gesture::CtrlCtrl, snapshot());
        let got = take(&log);
        assert_eq!(got[0], "capture");
        assert!(got[1].starts_with("view.error:"), "{got:?}");
    }

    #[test]
    fn overrides_map_default_model_to_empty() {
        let mut s = UserSettings {
            claude_cli_path: r"C:\tools\claude.exe".into(),
            claude_model: "default".into(),
            claude_effort: "high".into(),
            ..Default::default()
        };
        let o = provider_overrides(&s);
        assert_eq!(o.cli_path, r"C:\tools\claude.exe");
        assert_eq!(o.model, "");
        assert_eq!(o.effort, "high");
        s.claude_model = "opus".into();
        assert_eq!(provider_overrides(&s).model, "opus");
    }

    #[test]
    fn interval_seconds_become_milliseconds() {
        assert_eq!(interval_ms(0.3), 300);
        assert_eq!(interval_ms(0.45), 450);
        assert_eq!(interval_ms(1.0), 1000);
    }

    #[test]
    fn interval_seconds_are_clamped_and_nan_uses_default() {
        assert_eq!(interval_ms(-1.0), 100);
        assert_eq!(interval_ms(0.0), 100);
        assert_eq!(interval_ms(0.05), 100);
        assert_eq!(interval_ms(2.0), 2000);
        assert_eq!(interval_ms(5.0), 2000);
        assert_eq!(interval_ms(f64::INFINITY), 2000);
        assert_eq!(interval_ms(f64::NEG_INFINITY), 100);
        assert_eq!(interval_ms(f64::NAN), 200);
    }

    #[test]
    fn toggled_settings_adds_removes_and_refuses_last() {
        let s = UserSettings {
            base_language: Lang::Ko,
            target_languages: vec![Lang::En],
            ..Default::default()
        };
        let added = toggled_settings(&s, Lang::Ja).unwrap();
        assert_eq!(added.target_languages, vec![Lang::En, Lang::Ja]);
        assert_eq!(added.base_language, Lang::Ko);
        let removed = toggled_settings(&added, Lang::En).unwrap();
        assert_eq!(removed.target_languages, vec![Lang::Ja]);
        // The last remaining target and the base language are refused.
        assert!(toggled_settings(&removed, Lang::Ja).is_none());
        assert!(toggled_settings(&removed, Lang::Ko).is_none());
    }
}
