//! Settings window (v5 §4.4, spec §5.2). `SettingsForm` is the pure, unit-tested view model;
//! `SettingsWindow` binds it to the Slint `SettingsWin` on the UI thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ct_core::{
    CorrectionMode, DisplayMode, Lang, SettingsStore, Tone, UserSettings, move_target,
    normalize_targets, with_base,
};
use ct_llm::{
    CatalogSource, CliLaunch, ListenerId, ModelCatalog, ModelInfo, home_dir, resolve_claude_cli,
    which_on_path,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::win32::{HWND_RETRIES, bring_to_front, hwnd_of, message_box, wait_for_hwnd};
use super::{SettingsWin, TargetRow};

pub const SHORTCUT_GUIDE: &[&str] = &[
    "Alt+Alt — 번역 후 교체",
    "Ctrl+Ctrl — 번역 결과를 창으로 보기 (원문 변경 없음)",
    "Ctrl을 누른 채 Alt+Alt — 한글 교정 후 교체",
    "Alt를 누른 채 Ctrl+Ctrl — 한글 교정 결과를 창으로 보기 (원문 변경 없음)",
    "한글 교정은 Base Language가 ko이고 선택한 문장의 주 언어가 한국어일 때만 동작합니다.",
    "Google Docs에서 Alt+Alt, Ctrl+Alt+Alt 기능을 사용하시려면 반드시 블록을 설정하셔야 합니다.",
    "Target 언어가 여러 개면 정한 순서대로 모두 번역합니다.",
];
pub const INCLUDE_ORIGINAL_LABEL: &str = "Alt+Alt 교체 시 원문 포함";
pub const INCLUDE_ORIGINAL_DESC: &str = "켜면 입력한 원문 아래에 빈 줄을 두고 번역문을 넣습니다.";
pub const NO_TARGET_MESSAGE: &str = "Target 언어를 하나 이상 선택하세요.";
pub const START_ON_LOGIN_LABEL: &str = "Start ClaudeTranslator when I log in";
pub const FALLBACK_HINT: &str =
    "기본 목록 표시 중 — Claude CLI에서 목록을 불러오면 자동으로 갱신됩니다";
pub const EFFORT_UNSUPPORTED_HINT: &str = "이 모델은 Effort를 지원하지 않습니다";
pub const HAIKU_WARNING_TITLE: &str = "모델 선택";
pub const HAIKU_WARNING_TEXT: &str = "길거나 복잡한 문장에서 성능이 저하될 수 있습니다.";

/// Label of the `""` (no override) entry of the Model and Effort combos.
const AUTO_LABEL: &str = "기본값 (자동)";
const HAIKU: &str = "haiku";
/// Levels offered for a model the catalog does not know.
const STANDARD_EFFORT: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Base Language combo order (Settings order, unlike the tray).
pub const BASE_ITEMS: [Lang; 4] = Lang::ALL;
pub const TONE_ITEMS: [(&str, Tone); 4] = [
    ("일반 (기본값)", Tone::Neutral),
    ("구어체", Tone::Casual),
    ("정중하게 (회사 공식 메일 등)", Tone::Formal),
    ("쉬운 단어와 문장", Tone::Plain),
];
pub const DISPLAY_MODE_ITEMS: [(&str, DisplayMode); 2] = [
    (
        "원문 전체 · 번역 전체 보기 (기본값)",
        DisplayMode::WholeText,
    ),
    ("문장별로 보기", DisplayMode::PerSentence),
];
pub const CORRECTION_ITEMS: [(&str, CorrectionMode); 2] = [
    ("오탈자,띄어쓰기 교정", CorrectionMode::Spelling),
    ("오탈자,띄어쓰기 교정,문장 다듬기", CorrectionMode::Polish),
];

const INTERVAL_MIN: f64 = 0.1;
const INTERVAL_MAX: f64 = 2.0;
const INTERVAL_STEP: f64 = 0.1;

/// One step of the interval spin box (0.1 .. 2.0, step 0.1), rounded to 2 decimals.
pub fn step_interval(value: f64, up: bool) -> f64 {
    let next = if up {
        value + INTERVAL_STEP
    } else {
        value - INTERVAL_STEP
    };
    ((next * 100.0).round() / 100.0).clamp(INTERVAL_MIN, INTERVAL_MAX)
}

/// Spin box text (Qt double spin box default: 2 decimals).
pub fn format_interval(value: f64) -> String {
    format!("{value:.2}")
}

/// Detected launch as one copyable command line: program, then any prefix args
/// (e.g. `node.exe C:\npm\...\cli.js`), each quoted when it contains a space.
pub fn describe_launch(launch: &CliLaunch) -> String {
    let quote = |s: &str| {
        if s.contains(' ') {
            format!("\"{s}\"")
        } else {
            s.to_string()
        }
    };
    std::iter::once(launch.program.to_string_lossy().into_owned())
        .chain(launch.prefix_args.iter().cloned())
        .map(|p| quote(&p))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Placeholder of the Claude CLI path field.
pub fn cli_placeholder(detected: &str) -> String {
    format!("자동 감지: {detected}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComboItem {
    pub label: String,
    pub value: String,
}

fn combo(label: &str, value: &str) -> ComboItem {
    ComboItem {
        label: label.to_string(),
        value: value.to_string(),
    }
}

/// `low -> Low`, `xhigh -> X-High`; unknown levels are capitalized.
fn effort_label(level: &str) -> String {
    match level {
        "low" => "Low".to_string(),
        "medium" => "Medium".to_string(),
        "high" => "High".to_string(),
        "xhigh" => "X-High".to_string(),
        "max" => "Max".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        }
    }
}

/// Pure view model: the settings being edited. Model `""` means "CLI default".
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsForm {
    pub settings: UserSettings,
}

impl SettingsForm {
    /// Normalizes the stored targets, maps the model `default` to `""` and drops a saved
    /// effort the selected model does not offer.
    pub fn new(mut settings: UserSettings, models: &[ModelInfo]) -> Self {
        settings.target_languages =
            normalize_targets(settings.base_language, &settings.target_languages);
        if settings.claude_model == "default" {
            settings.claude_model.clear();
        }
        let mut form = SettingsForm { settings };
        form.reconcile_effort(models);
        form
    }

    /// Every language except the base: checked ones first in output order with their
    /// 1-based number, then unchecked ones in `Lang::ALL` order.
    pub fn target_rows(&self) -> Vec<(Lang, bool, Option<usize>)> {
        let base = self.settings.base_language;
        let targets = &self.settings.target_languages;
        let checked = targets
            .iter()
            .enumerate()
            .map(|(i, &lang)| (lang, true, Some(i + 1)));
        let unchecked = Lang::ALL
            .into_iter()
            .filter(|l| *l != base && !targets.contains(l))
            .map(|lang| (lang, false, None));
        checked.chain(unchecked).collect()
    }

    /// `with_base` semantics: the base leaves the targets; an empty result falls back.
    pub fn set_base(&mut self, base: Lang) {
        self.settings = with_base(&self.settings, base);
    }

    /// Unlike the tray, Settings may uncheck the last target (Save is then disabled).
    pub fn toggle(&mut self, lang: Lang) {
        if lang == self.settings.base_language {
            return;
        }
        let targets = &mut self.settings.target_languages;
        if let Some(i) = targets.iter().position(|l| *l == lang) {
            targets.remove(i);
        } else {
            targets.push(lang);
        }
    }

    pub fn move_up(&mut self, lang: Lang) {
        self.settings.target_languages = move_target(&self.settings.target_languages, lang, true);
    }

    pub fn move_down(&mut self, lang: Lang) {
        self.settings.target_languages = move_target(&self.settings.target_languages, lang, false);
    }

    pub fn can_save(&self) -> bool {
        !self.settings.target_languages.is_empty()
    }

    /// `default` -> `기본값 (자동)`/`""`, others `display_name`/`value`. A selected model
    /// missing from the list is appended as `(목록에 없음) {v}` (never silently swapped).
    pub fn model_items(&self, models: &[ModelInfo]) -> Vec<ComboItem> {
        let mut items: Vec<ComboItem> = models
            .iter()
            .map(|m| {
                if m.value == "default" {
                    combo(AUTO_LABEL, "")
                } else {
                    combo(&m.display_name, &m.value)
                }
            })
            .collect();
        let selected = self.settings.claude_model.as_str();
        if !items.iter().any(|i| i.value == selected) {
            if selected.is_empty() {
                items.insert(0, combo(AUTO_LABEL, ""));
            } else {
                items.push(combo(&format!("(목록에 없음) {selected}"), selected));
            }
        }
        items
    }

    /// Index of the selected model in [`Self::model_items`].
    pub fn model_index(&self, models: &[ModelInfo]) -> usize {
        self.model_items(models)
            .iter()
            .position(|i| i.value == self.settings.claude_model)
            .unwrap_or(0)
    }

    /// Effort levels of the selected model: `""` looks up `default`; an unknown model
    /// offers the five standard levels.
    fn levels(&self, models: &[ModelInfo]) -> Vec<String> {
        let key = match self.settings.claude_model.as_str() {
            "" => "default",
            other => other,
        };
        match models.iter().find(|m| m.value == key) {
            Some(m) => m.effort_levels.clone(),
            None => STANDARD_EFFORT.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn effort_items(&self, models: &[ModelInfo]) -> Vec<ComboItem> {
        std::iter::once(combo(AUTO_LABEL, ""))
            .chain(
                self.levels(models)
                    .iter()
                    .map(|level| combo(&effort_label(level), level)),
            )
            .collect()
    }

    /// Index of the selected effort in [`Self::effort_items`].
    pub fn effort_index(&self, models: &[ModelInfo]) -> usize {
        self.effort_items(models)
            .iter()
            .position(|i| i.value == self.settings.claude_effort)
            .unwrap_or(0)
    }

    /// The Effort combo is enabled iff the selected model has levels (haiku has none).
    pub fn effort_supported(&self, models: &[ModelInfo]) -> bool {
        !self.levels(models).is_empty()
    }

    /// Keeps the effort if still offered, else `""`.
    fn reconcile_effort(&mut self, models: &[ModelInfo]) {
        let effort = &self.settings.claude_effort;
        if !effort.is_empty() && !self.levels(models).iter().any(|l| l == effort) {
            self.settings.claude_effort.clear();
        }
    }

    /// A user-driven model change. Returns true when the haiku warning must be shown
    /// (the user switched to haiku).
    pub fn select_model(&mut self, value: &str, models: &[ModelInfo]) -> bool {
        let changed = self.settings.claude_model != value;
        self.settings.claude_model = value.to_string();
        self.reconcile_effort(models);
        changed && value == HAIKU
    }

    pub fn select_effort(&mut self, value: &str) {
        self.settings.claude_effort = value.to_string();
    }

    /// The catalog changed: the model selection is kept (unlisted if it vanished) and an
    /// effort the new list no longer offers resets to `""`. Never asks for a warning.
    pub fn refresh_models(&mut self, models: &[ModelInfo]) {
        self.reconcile_effort(models);
    }
}

// ---------------------------------------------------------------------------------------
// Slint window (UI thread only)
// ---------------------------------------------------------------------------------------

/// Called with the saved (normalized) settings after a successful Save.
type OnSaved = Box<dyn Fn(UserSettings)>;

struct Open {
    win: SettingsWin,
    form: RefCell<SettingsForm>,
    /// Catalog list the combos were last built from.
    models: RefCell<Vec<ModelInfo>>,
    /// Items currently shown, so a combo index maps to the value the user saw.
    model_items: RefCell<Vec<ComboItem>>,
    effort_items: RefCell<Vec<ComboItem>>,
    catalog: Arc<ModelCatalog>,
    listener: ListenerId,
    store: Arc<Mutex<SettingsStore>>,
    on_saved: OnSaved,
}

thread_local! {
    /// The one open Settings window (UI thread).
    static CURRENT: RefCell<Option<Rc<Open>>> = const { RefCell::new(None) };
}

fn current() -> Option<Rc<Open>> {
    CURRENT.with(|c| c.borrow().clone())
}

fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

fn index_of<T: PartialEq>(items: &[(&str, T)], value: &T) -> i32 {
    items.iter().position(|(_, v)| v == value).unwrap_or(0) as i32
}

fn pick<T: Copy>(items: &[(&str, T)], index: i32, fallback: T) -> T {
    usize::try_from(index)
        .ok()
        .and_then(|i| items.get(i))
        .map_or(fallback, |(_, v)| *v)
}

impl Open {
    fn refresh_targets(&self) {
        let form = self.form.borrow();
        let rows = form.target_rows();
        let checked = form.settings.target_languages.len();
        let model: Vec<TargetRow> = rows
            .iter()
            .map(|&(lang, is_checked, number)| TargetRow {
                label: match number {
                    Some(n) => format!("{} ({n})", lang.code()).into(),
                    None => lang.code().into(),
                },
                checked: is_checked,
                can_up: number.is_some_and(|n| n > 1),
                can_down: number.is_some_and(|n| n < checked),
            })
            .collect();
        self.win.set_targets(ModelRc::new(VecModel::from(model)));
        self.win.set_can_save(form.can_save());
    }

    fn refresh_effort(&self) {
        let form = self.form.borrow();
        let models = self.models.borrow();
        let items = form.effort_items(&models);
        self.win
            .set_effort_items(strings(items.iter().map(|i| i.label.clone())));
        self.win.set_effort_index(form.effort_index(&models) as i32);
        self.win.set_effort_enabled(form.effort_supported(&models));
        *self.effort_items.borrow_mut() = items;
    }

    /// Rebuilds the Model and Effort combos from the catalog (programmatic index changes
    /// do not fire `selected`, so no haiku warning).
    fn refresh_models(&self) {
        let models = self.catalog.get();
        let source = self.catalog.source();
        tracing::info!(?source, count = models.len(), "settings: model list");
        self.form.borrow_mut().refresh_models(&models);
        let items = self.form.borrow().model_items(&models);
        let index = self.form.borrow().model_index(&models);
        self.win
            .set_model_items(strings(items.iter().map(|i| i.label.clone())));
        self.win.set_model_index(index as i32);
        self.win
            .set_fallback_hint_visible(source == CatalogSource::Fallback);
        *self.model_items.borrow_mut() = items;
        *self.models.borrow_mut() = models;
        self.refresh_effort();
    }

    fn row_lang(&self, row: i32) -> Option<Lang> {
        let rows = self.form.borrow().target_rows();
        usize::try_from(row)
            .ok()
            .and_then(|i| rows.get(i))
            .map(|r| r.0)
    }

    fn on_base_selected(&self, index: i32) {
        if let Some(&base) = usize::try_from(index).ok().and_then(|i| BASE_ITEMS.get(i)) {
            self.form.borrow_mut().set_base(base);
            self.refresh_targets();
        }
    }

    fn on_target_toggled(&self, row: i32) {
        if let Some(lang) = self.row_lang(row) {
            self.form.borrow_mut().toggle(lang);
        }
        // Always re-render: the checkbox flipped itself and must reflect the form.
        self.refresh_targets();
    }

    fn on_target_moved(&self, row: i32, up: bool) {
        if let Some(lang) = self.row_lang(row) {
            let mut form = self.form.borrow_mut();
            if up {
                form.move_up(lang);
            } else {
                form.move_down(lang);
            }
        }
        self.refresh_targets();
    }

    fn on_model_selected(&self, index: i32) {
        let value = usize::try_from(index)
            .ok()
            .and_then(|i| self.model_items.borrow().get(i).map(|m| m.value.clone()));
        let Some(value) = value else {
            return;
        };
        let warn = {
            let models = self.models.borrow();
            self.form.borrow_mut().select_model(&value, &models)
        };
        self.refresh_effort();
        if warn {
            message_box(HAIKU_WARNING_TITLE, HAIKU_WARNING_TEXT);
        }
    }

    fn on_effort_selected(&self, index: i32) {
        let value = usize::try_from(index)
            .ok()
            .and_then(|i| self.effort_items.borrow().get(i).map(|m| m.value.clone()));
        if let Some(value) = value {
            self.form.borrow_mut().select_effort(&value);
        }
    }

    fn on_step(&self, alt_alt: bool, up: bool) {
        let mut form = self.form.borrow_mut();
        if alt_alt {
            let v = step_interval(form.settings.alt_alt_interval_seconds, up);
            form.settings.alt_alt_interval_seconds = v;
            self.win.set_alt_alt_interval(format_interval(v).into());
        } else {
            let v = step_interval(form.settings.ctrl_ctrl_interval_seconds, up);
            form.settings.ctrl_ctrl_interval_seconds = v;
            self.win.set_ctrl_ctrl_interval(format_interval(v).into());
        }
    }

    /// Form + the controls Rust does not track live -> the settings to save.
    fn collect(&self) -> UserSettings {
        let w = &self.win;
        let mut s = self.form.borrow().settings.clone();
        s.include_original_on_replace = w.get_include_original();
        s.start_on_login = w.get_start_on_login();
        s.claude_cli_path = w.get_cli_path().trim().to_string();
        s.translation_tone = pick(&TONE_ITEMS, w.get_tone_index(), Tone::Neutral);
        s.ctrl_ctrl_display_mode = pick(
            &DISPLAY_MODE_ITEMS,
            w.get_display_index(),
            DisplayMode::WholeText,
        );
        s.korean_correction_mode = pick(
            &CORRECTION_ITEMS,
            w.get_correction_index(),
            CorrectionMode::Spelling,
        );
        s
    }

    /// `store.update` -> `on_saved` -> close. A failed write keeps the window open.
    fn save(&self) {
        if !self.form.borrow().can_save() {
            return;
        }
        let settings = self.collect();
        let saved = {
            let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
            match store.update(settings) {
                Ok(()) => Some(store.get().clone()),
                Err(e) => {
                    tracing::error!(error = %e, "settings: save failed");
                    None
                }
            }
        };
        if let Some(saved) = saved {
            tracing::info!("settings: saved");
            (self.on_saved)(saved);
            close_current();
        }
    }
}

/// Hides and forgets the open window and unregisters its catalog listener. The component
/// is dropped on a later event-loop turn, never inside its own callback.
fn close_current() {
    let Some(open) = CURRENT.with(|c| c.borrow_mut().take()) else {
        return;
    };
    open.catalog.remove_listener(open.listener);
    if let Err(e) = open.win.hide() {
        tracing::warn!(error = %e, "settings: hide failed");
    }
    slint::Timer::single_shot(Duration::ZERO, move || drop(open));
}

/// The Settings window. Only one is open at a time; everything runs on the UI thread.
pub struct SettingsWindow;

impl SettingsWindow {
    /// Opens the window, or brings the already open one to the front. Save writes
    /// `store.update(settings)`, then calls `on_saved` with the stored (normalized)
    /// settings, then closes. Closing without Save discards the edits. Must be called on
    /// the UI thread (inside the Slint event loop).
    pub fn open(
        store: Arc<Mutex<SettingsStore>>,
        catalog: Arc<ModelCatalog>,
        on_saved: impl Fn(UserSettings) + 'static,
    ) {
        if let Some(open) = current() {
            match hwnd_of(open.win.window()) {
                Ok(hwnd) => bring_to_front(hwnd),
                Err(e) => tracing::warn!(error = %e, "settings: re-open focus failed"),
            }
            return;
        }
        if let Err(e) = Self::create(store, catalog, Box::new(on_saved)) {
            tracing::error!(error = %e, "settings: open failed");
        }
    }

    fn create(
        store: Arc<Mutex<SettingsStore>>,
        catalog: Arc<ModelCatalog>,
        on_saved: OnSaved,
    ) -> Result<(), String> {
        let win = SettingsWin::new().map_err(|e| e.to_string())?;
        // Register before reading the catalog so a list landing in between is not missed
        // (the listener re-reads the catalog). It runs on a background thread: hop to the
        // UI thread and refresh whichever window is open then.
        let listener = catalog.add_listener(Box::new(|| {
            let queued = slint::invoke_from_event_loop(|| {
                if let Some(open) = current() {
                    open.refresh_models();
                }
            });
            if let Err(e) = queued {
                tracing::warn!(error = %e, "settings: catalog refresh not queued");
            }
        }));

        let settings = store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get()
            .clone();
        let models = catalog.get();
        let form = SettingsForm::new(settings, &models);
        let s = &form.settings;
        let detected = describe_launch(&resolve_claude_cli(
            &s.claude_cli_path,
            &which_on_path,
            &home_dir(),
        ));

        win.set_base_items(strings(BASE_ITEMS.iter().map(|l| l.code().to_string())));
        win.set_base_index(
            BASE_ITEMS
                .iter()
                .position(|l| *l == s.base_language)
                .unwrap_or(0) as i32,
        );
        win.set_include_original(s.include_original_on_replace);
        win.set_include_original_label(INCLUDE_ORIGINAL_LABEL.into());
        win.set_include_original_description(INCLUDE_ORIGINAL_DESC.into());
        win.set_start_on_login(s.start_on_login);
        win.set_start_on_login_label(START_ON_LOGIN_LABEL.into());
        win.set_cli_path(s.claude_cli_path.as_str().into());
        win.set_cli_placeholder(cli_placeholder(&detected).into());
        win.set_fallback_hint(FALLBACK_HINT.into());
        win.set_effort_hint(EFFORT_UNSUPPORTED_HINT.into());
        win.set_tone_items(strings(TONE_ITEMS.iter().map(|(l, _)| l.to_string())));
        win.set_tone_index(index_of(&TONE_ITEMS, &s.translation_tone));
        win.set_ctrl_ctrl_interval(format_interval(s.ctrl_ctrl_interval_seconds).into());
        win.set_display_items(strings(
            DISPLAY_MODE_ITEMS.iter().map(|(l, _)| l.to_string()),
        ));
        win.set_display_index(index_of(&DISPLAY_MODE_ITEMS, &s.ctrl_ctrl_display_mode));
        win.set_alt_alt_interval(format_interval(s.alt_alt_interval_seconds).into());
        win.set_correction_items(strings(CORRECTION_ITEMS.iter().map(|(l, _)| l.to_string())));
        win.set_correction_index(index_of(&CORRECTION_ITEMS, &s.korean_correction_mode));
        win.set_shortcut_guide(SHORTCUT_GUIDE.join("\n").into());
        win.set_no_target_message(NO_TARGET_MESSAGE.into());

        let open = Rc::new(Open {
            win,
            form: RefCell::new(form),
            models: RefCell::new(Vec::new()),
            model_items: RefCell::new(Vec::new()),
            effort_items: RefCell::new(Vec::new()),
            catalog,
            listener,
            store,
            on_saved,
        });
        open.refresh_targets();
        open.refresh_models();
        wire(&open);

        CURRENT.with(|c| *c.borrow_mut() = Some(open.clone()));
        if let Err(e) = open.win.show() {
            close_current();
            return Err(e.to_string());
        }
        let weak = open.win.as_weak();
        wait_for_hwnd(
            move || {
                let win = weak
                    .upgrade()
                    .ok_or_else(|| "settings window closed".to_string())?;
                hwnd_of(win.window())
            },
            HWND_RETRIES,
            |hwnd| match hwnd {
                Ok(hwnd) => bring_to_front(hwnd),
                Err(e) => tracing::warn!(error = %e, "settings: no HWND to focus"),
            },
        );
        Ok(())
    }
}

/// Connects the Slint callbacks. Handlers look the window up through `CURRENT`, so the
/// component holds no strong reference to its own state.
fn wire(open: &Open) {
    fn with(f: impl Fn(&Open)) {
        if let Some(open) = current() {
            f(&open);
        }
    }
    let w = &open.win;
    w.on_base_selected(|i| with(|o| o.on_base_selected(i)));
    w.on_target_toggled(|i| with(|o| o.on_target_toggled(i)));
    w.on_target_moved(|i, up| with(|o| o.on_target_moved(i, up)));
    w.on_model_selected(|i| with(|o| o.on_model_selected(i)));
    w.on_effort_selected(|i| with(|o| o.on_effort_selected(i)));
    w.on_ctrl_ctrl_step(|up| with(|o| o.on_step(false, up)));
    w.on_alt_alt_step(|up| with(|o| o.on_step(true, up)));
    w.on_save(|| with(|o| o.save()));
    w.window().on_close_requested(|| {
        close_current();
        slint::CloseRequestResponse::HideWindow
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ct_llm::fallback_models;

    fn info(value: &str, display: &str, levels: &[&str]) -> ModelInfo {
        ModelInfo {
            value: value.to_string(),
            display_name: display.to_string(),
            description: String::new(),
            effort_levels: levels.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// v5 `make_catalog`: the fallback list plus `claude-opus-6` (low, high).
    fn catalog_models() -> Vec<ModelInfo> {
        let mut models = fallback_models();
        models.push(info("claude-opus-6", "Opus 6", &["low", "high"]));
        models
    }

    fn live(extra: Vec<ModelInfo>) -> Vec<ModelInfo> {
        let mut models = vec![info(
            "default",
            "Default (recommended)",
            &["low", "medium", "high", "xhigh", "max"],
        )];
        models.extend(extra);
        models
    }

    fn form_with(model: &str, effort: &str, models: &[ModelInfo]) -> SettingsForm {
        SettingsForm::new(
            UserSettings {
                claude_model: model.to_string(),
                claude_effort: effort.to_string(),
                ..UserSettings::default()
            },
            models,
        )
    }

    fn values(items: &[ComboItem]) -> Vec<&str> {
        items.iter().map(|i| i.value.as_str()).collect()
    }

    fn labels(items: &[ComboItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    fn targets_form(base: Lang, targets: &[Lang]) -> SettingsForm {
        SettingsForm::new(
            UserSettings {
                base_language: base,
                target_languages: targets.to_vec(),
                ..UserSettings::default()
            },
            &fallback_models(),
        )
    }

    // --- targets ---

    #[test]
    fn target_rows_checked_first_with_numbers() {
        let form = targets_form(Lang::Ko, &[Lang::Ja, Lang::En]);
        assert_eq!(
            form.target_rows(),
            vec![
                (Lang::Ja, true, Some(1)),
                (Lang::En, true, Some(2)),
                (Lang::Vi, false, None),
            ]
        );
    }

    #[test]
    fn target_rows_follow_toggle_and_move() {
        let mut form = targets_form(Lang::Ko, &[Lang::En]);
        form.toggle(Lang::Ja);
        form.toggle(Lang::Vi);
        assert_eq!(
            form.settings.target_languages,
            vec![Lang::En, Lang::Ja, Lang::Vi]
        );
        form.move_up(Lang::Vi);
        assert_eq!(
            form.settings.target_languages,
            vec![Lang::En, Lang::Vi, Lang::Ja]
        );
        form.move_down(Lang::En);
        assert_eq!(
            form.settings.target_languages,
            vec![Lang::Vi, Lang::En, Lang::Ja]
        );
        form.move_up(Lang::Vi); // already first: no-op
        form.move_down(Lang::Ja); // already last: no-op
        form.toggle(Lang::En);
        assert_eq!(
            form.target_rows(),
            vec![
                (Lang::Vi, true, Some(1)),
                (Lang::Ja, true, Some(2)),
                (Lang::En, false, None),
            ]
        );
    }

    #[test]
    fn toggling_the_base_is_ignored() {
        let mut form = targets_form(Lang::Ko, &[Lang::En]);
        form.toggle(Lang::Ko);
        assert_eq!(form.settings.target_languages, vec![Lang::En]);
    }

    #[test]
    fn set_base_removes_it_from_targets_and_falls_back() {
        let mut form = targets_form(Lang::Ko, &[Lang::En, Lang::Ja]);
        form.set_base(Lang::En);
        assert_eq!(form.settings.base_language, Lang::En);
        assert_eq!(form.settings.target_languages, vec![Lang::Ja]);
        assert_eq!(
            form.target_rows(),
            vec![
                (Lang::Ja, true, Some(1)),
                (Lang::Ko, false, None),
                (Lang::Vi, false, None),
            ]
        );

        // Only target becomes the base -> falls back to the first non-base language.
        let mut form = targets_form(Lang::Ko, &[Lang::En]);
        form.set_base(Lang::En);
        assert_eq!(form.settings.target_languages, vec![Lang::Ko]);

        // Everything unchecked in the form, then a base change -> one is checked again.
        let mut form = targets_form(Lang::Ko, &[Lang::En]);
        form.toggle(Lang::En);
        form.set_base(Lang::Vi);
        assert_eq!(form.settings.target_languages, vec![Lang::Ko]);
    }

    #[test]
    fn cannot_save_without_target() {
        let mut form = targets_form(Lang::Ko, &[Lang::En]);
        assert!(form.can_save());
        form.toggle(Lang::En);
        assert!(form.settings.target_languages.is_empty());
        assert!(!form.can_save());
        form.toggle(Lang::Ja);
        assert!(form.can_save());
    }

    // --- model / effort (v5 test_settings_dialog.py) ---

    #[test]
    fn model_items_list_catalog_with_default_first() {
        let form = form_with("sonnet", "low", &catalog_models());
        let items = form.model_items(&catalog_models());
        assert_eq!(
            items[0],
            ComboItem {
                label: "기본값 (자동)".into(),
                value: String::new()
            }
        );
        assert!(items.contains(&ComboItem {
            label: "Opus 6".into(),
            value: "claude-opus-6".into()
        }));
        assert!(items.contains(&ComboItem {
            label: "Sonnet".into(),
            value: "sonnet".into()
        }));
    }

    #[test]
    fn model_default_maps_to_empty_value() {
        let models = fallback_models();
        let form = form_with("default", "low", &models);
        assert_eq!(form.settings.claude_model, "");
        let items = form.model_items(&models);
        assert_eq!(values(&items), vec!["", "opus", "sonnet", "haiku"]);
        assert_eq!(form.model_index(&models), 0);
    }

    #[test]
    fn saved_model_is_selected_and_defaults_to_sonnet() {
        let models = fallback_models();
        let form = form_with("opus", "low", &models);
        assert_eq!(form.model_index(&models), 1);
        let form = SettingsForm::new(UserSettings::default(), &models);
        assert_eq!(form.model_index(&models), 2);
        assert_eq!(form.settings.claude_effort, "low");
        assert_eq!(
            form.effort_items(&models)[form.effort_index(&models)].value,
            "low"
        );
    }

    #[test]
    fn unknown_saved_model_is_appended_not_swapped() {
        let models = catalog_models();
        let form = form_with("retired-model", "high", &models);
        let items = form.model_items(&models);
        let last = &items[items.len() - 1];
        assert_eq!(last.label, "(목록에 없음) retired-model");
        assert_eq!(last.value, "retired-model");
        assert_eq!(form.model_index(&models), items.len() - 1);
        // Unknown model -> the five standard levels, saved effort kept.
        assert!(form.effort_supported(&models));
        assert_eq!(form.effort_items(&models).len(), 6);
        assert_eq!(form.settings.claude_model, "retired-model");
        assert_eq!(form.settings.claude_effort, "high");
    }

    #[test]
    fn effort_items_offer_only_levels_of_selected_model() {
        let models = catalog_models();
        let mut form = form_with("sonnet", "low", &models);
        assert_eq!(
            labels(&form.effort_items(&models)),
            vec!["기본값 (자동)", "Low", "Medium", "High", "X-High", "Max"]
        );
        form.select_model("claude-opus-6", &models);
        assert_eq!(values(&form.effort_items(&models)), vec!["", "low", "high"]);
    }

    #[test]
    fn unknown_effort_level_is_capitalized() {
        let models = vec![info("x", "X", &["turbo", "ULTRA"])];
        let form = form_with("x", "", &models);
        assert_eq!(
            labels(&form.effort_items(&models)),
            vec!["기본값 (자동)", "Turbo", "Ultra"]
        );
    }

    #[test]
    fn default_model_uses_the_default_entry_levels() {
        let models = vec![
            info("default", "Default", &["low"]),
            info("opus", "Opus", &[]),
        ];
        let form = form_with("", "", &models);
        assert_eq!(values(&form.effort_items(&models)), vec!["", "low"]);
    }

    #[test]
    fn haiku_has_no_effort() {
        let models = catalog_models();
        let mut form = form_with("sonnet", "high", &models);
        let warn = form.select_model("haiku", &models);
        assert!(warn, "user picking haiku asks for the warning");
        assert!(!form.effort_supported(&models));
        assert_eq!(labels(&form.effort_items(&models)), vec!["기본값 (자동)"]);
        assert_eq!(form.settings.claude_model, "haiku");
        assert_eq!(form.settings.claude_effort, "");

        form.select_model("sonnet", &models);
        assert!(form.effort_supported(&models));
    }

    #[test]
    fn non_haiku_selection_asks_for_no_warning() {
        let models = catalog_models();
        let mut form = form_with("sonnet", "low", &models);
        assert!(!form.select_model("opus", &models));
        assert!(!form.select_model("", &models));
    }

    #[test]
    fn opening_with_haiku_keeps_it_with_effort_unsupported() {
        let models = fallback_models();
        let form = form_with("haiku", "", &models);
        assert_eq!(form.settings.claude_model, "haiku");
        assert!(!form.effort_supported(&models));
    }

    #[test]
    fn unsupported_saved_effort_resets_on_model_switch() {
        let models = catalog_models();
        let mut form = form_with("sonnet", "xhigh", &models);
        assert_eq!(form.settings.claude_effort, "xhigh");
        form.select_model("claude-opus-6", &models);
        assert_eq!(form.settings.claude_effort, "");
        assert_eq!(form.effort_index(&models), 0);
    }

    #[test]
    fn supported_saved_effort_is_kept_on_model_switch() {
        let models = catalog_models();
        let mut form = form_with("sonnet", "high", &models);
        form.select_model("claude-opus-6", &models);
        assert_eq!(form.settings.claude_effort, "high");
        assert_eq!(form.effort_index(&models), 2);
    }

    #[test]
    fn selecting_back_to_auto_clears_the_override() {
        let models = fallback_models();
        let mut form = form_with("opus", "low", &models);
        form.select_model("", &models);
        assert_eq!(form.settings.claude_model, "");
    }

    #[test]
    fn refresh_switches_to_live_list() {
        let fallback = fallback_models();
        let mut form = form_with("", "low", &fallback);
        let new = live(vec![
            info("opus", "Opus 5.5", &["low", "high"]),
            info("haiku", "Haiku 4.5", &[]),
        ]);
        form.refresh_models(&new);
        assert_eq!(
            labels(&form.model_items(&new)),
            vec!["기본값 (자동)", "Opus 5.5", "Haiku 4.5"]
        );
    }

    #[test]
    fn refresh_keeps_current_model_and_effort() {
        let fallback = fallback_models();
        let mut form = form_with("sonnet", "low", &fallback);
        form.select_model("opus", &fallback);
        form.select_effort("high");
        let new = live(vec![
            info("opus", "Opus 5.5", &["low", "high"]),
            info("haiku", "Haiku 4.5", &[]),
        ]);
        form.refresh_models(&new);
        assert_eq!(form.settings.claude_model, "opus");
        assert_eq!(form.settings.claude_effort, "high");
        assert_eq!(
            form.model_items(&new)[form.model_index(&new)].label,
            "Opus 5.5"
        );
    }

    #[test]
    fn refresh_resets_effort_the_new_list_no_longer_offers() {
        let fallback = fallback_models();
        let mut form = form_with("sonnet", "xhigh", &fallback);
        let new = live(vec![info("sonnet", "Sonnet 5.5", &["low", "high"])]);
        form.refresh_models(&new);
        assert_eq!(form.settings.claude_model, "sonnet");
        assert_eq!(form.settings.claude_effort, "");
    }

    #[test]
    fn refresh_keeps_selected_model_missing_from_new_list_as_unlisted() {
        let fallback = fallback_models();
        let mut form = form_with("opus", "low", &fallback);
        let new = live(vec![info("haiku", "Haiku 4.5", &[])]);
        form.refresh_models(&new);
        let items = form.model_items(&new);
        let selected = &items[form.model_index(&new)];
        assert_eq!(selected.label, "(목록에 없음) opus");
        assert_eq!(selected.value, "opus");
    }

    #[test]
    fn refresh_can_turn_effort_support_off() {
        let fallback = fallback_models();
        let mut form = form_with("sonnet", "low", &fallback);
        assert!(form.effort_supported(&fallback));
        let new = live(vec![info("sonnet", "Sonnet 5.5", &[])]);
        form.refresh_models(&new);
        assert!(!form.effort_supported(&new));
        assert_eq!(form.settings.claude_effort, "");
    }

    // --- other controls ---

    #[test]
    fn interval_steps_by_tenths_within_range() {
        assert_eq!(step_interval(0.2, true), 0.3);
        assert_eq!(step_interval(0.2, false), 0.1);
        assert_eq!(step_interval(0.1, false), 0.1);
        assert_eq!(step_interval(2.0, true), 2.0);
        assert_eq!(step_interval(0.25, true), 0.35);
        assert_eq!(format_interval(0.2), "0.20");
    }

    #[test]
    fn combo_lists_are_verbatim() {
        assert_eq!(
            TONE_ITEMS.map(|(l, t)| (l, t.as_str())),
            [
                ("일반 (기본값)", "neutral"),
                ("구어체", "casual"),
                ("정중하게 (회사 공식 메일 등)", "formal"),
                ("쉬운 단어와 문장", "plain"),
            ]
        );
        assert_eq!(
            DISPLAY_MODE_ITEMS.map(|(l, m)| (l, m.as_str())),
            [
                ("원문 전체 · 번역 전체 보기 (기본값)", "whole_text"),
                ("문장별로 보기", "per_sentence"),
            ]
        );
        assert_eq!(
            CORRECTION_ITEMS.map(|(l, m)| (l, m.as_str())),
            [
                ("오탈자,띄어쓰기 교정", "spelling"),
                ("오탈자,띄어쓰기 교정,문장 다듬기", "polish"),
            ]
        );
    }

    #[test]
    fn shortcut_guide_has_v5_lines_plus_multi_target_line() {
        assert_eq!(SHORTCUT_GUIDE.len(), 7);
        assert_eq!(SHORTCUT_GUIDE[0], "Alt+Alt — 번역 후 교체");
        assert_eq!(
            SHORTCUT_GUIDE[5],
            "Google Docs에서 Alt+Alt, Ctrl+Alt+Alt 기능을 사용하시려면 반드시 블록을 설정하셔야 합니다."
        );
        assert_eq!(
            SHORTCUT_GUIDE[6],
            "Target 언어가 여러 개면 정한 순서대로 모두 번역합니다."
        );
    }

    #[test]
    fn cli_placeholder_names_the_detected_path() {
        assert_eq!(cli_placeholder("claude"), "자동 감지: claude");
    }

    #[test]
    fn describe_launch_shows_program_and_prefix_args() {
        let l = CliLaunch {
            program: r"C:\nodejs\node.exe".into(),
            prefix_args: vec![r"C:\Users\a b\npm\cli.js".into()],
        };
        assert_eq!(
            describe_launch(&l),
            r#"C:\nodejs\node.exe "C:\Users\a b\npm\cli.js""#
        );
        let l = CliLaunch {
            program: "claude".into(),
            prefix_args: vec![],
        };
        assert_eq!(describe_launch(&l), "claude");
    }
}
