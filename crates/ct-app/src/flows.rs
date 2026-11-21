//! Alt+Alt (translate and replace) and Ctrl+Ctrl (translate and explain) flows, plus the
//! Korean correction flows. Each flow routes Google Sheets first (`sheets_flows`).
//!
//! The original text is replaced only after a successful translation, and any failure
//! leaves it untouched. Logs carry operation, duration and error kind only, never text.

use std::time::Instant;

use ct_core::{
    CorrectionOutcome, CorrectionSentence, CorrectionStatus, DICTIONARY_INTEGRITY_MESSAGE,
    DictionaryEntry, DisplayMode, ExplanationResult, Lang, LanguageDetector, LlmProvider,
    TranslateError, UserSettings, correct_sentences, correct_text, explain_text, translate_text,
};
use ct_llm::CLI_START_FAILURE_MARKER;
use ct_sheets::SheetsTable;
use ct_win::chrome::{ReplaceError, TextInteraction};

use crate::sheets_flows::{
    Job, MSG_SHEETS_CORRECTION_RANGE_REPLACE_DISABLED, MSG_SHEETS_EDIT_CORRECT_FAILED,
    MSG_SHEETS_EDIT_TRANSLATE_FAILED, MSG_SHEETS_RANGE_REPLACE_DISABLED, Routed, SheetsDeps, route,
    run_range_replace, run_table, sheets_edit_replace_message,
};

pub const MSG_DIRECTION_UNKNOWN: &str = "번역 방향을 판단할 수 없어 원문을 변경하지 않았습니다.";
pub const MSG_TRANSLATE_FAILED: &str = "번역에 실패하여 원본을 변경하지 않았습니다.";
pub const MSG_REPLACE_FAILED: &str = "교체 대상이 바뀌어 원문을 변경하지 않았습니다.";
pub const MSG_ALT_NO_SELECTION_DOCS: &str = "텍스트를 선택한 후 Alt+Alt를 눌러주세요.";
pub const MSG_CTRL_NO_SELECTION: &str = "텍스트를 선택한 후 Ctrl+Ctrl을 실행해주세요.";
pub const MSG_EXPLAIN_FAILED: &str = "Could not translate/explain the selected text";
pub const MSG_CLI_UNAVAILABLE: &str =
    "Claude CLI를 실행할 수 없습니다. Settings의 Claude CLI 경로를 확인하세요.";
pub const MSG_CORRECTION_REPLACE_NO_SELECTION: &str =
    "텍스트를 선택한 후 Ctrl+Alt+Alt를 눌러주세요.";
pub const MSG_CORRECTION_VIEW_NO_SELECTION: &str =
    "텍스트를 선택한 후 Alt+Ctrl+Ctrl을 실행해주세요.";
pub const MSG_CORRECTION_FAILED: &str = "교정에 실패하여 원본을 변경하지 않았습니다.";

pub(crate) const LOADING_TEXT: &str = "Translating...";

/// Shows an error in the explanation window ("Error: {message}").
pub trait Notifier: Send + Sync {
    fn notify(&self, message: &str);
}

pub trait Badge: Send + Sync {
    fn show(&self, text: &str);
    fn hide(&self);
}

pub trait ExplainView: Send + Sync {
    fn show_loading(&self, text: &str);
    fn show_error(&self, message: &str);
    fn show_result(&self, result: ExplanationResult, mode: DisplayMode);
    fn show_correction(&self, sentences: Vec<CorrectionSentence>, mode: DisplayMode);
    /// Google Sheets range result (translation or correction table).
    fn show_table(&self, table: SheetsTable);
}

pub struct FlowDeps<'a> {
    pub settings: &'a UserSettings,
    pub entries: &'a [DictionaryEntry],
    pub detector: &'a dyn LanguageDetector,
    pub llm: &'a dyn LlmProvider,
}

/// Hides the badge on every exit path, including panics.
pub(crate) struct BadgeGuard<'a>(pub(crate) &'a dyn Badge);

impl Drop for BadgeGuard<'_> {
    fn drop(&mut self) {
        self.0.hide();
    }
}

/// Log-safe variant name (never the inner message, which may echo text).
fn translate_error_kind(e: &TranslateError) -> &'static str {
    match e {
        TranslateError::DirectionUnknown => "direction_unknown",
        TranslateError::Llm(_) => "llm_failed",
        TranslateError::Integrity => "integrity",
    }
}

/// Log-safe variant name (never the inner message).
fn replace_error_kind(e: &ReplaceError) -> &'static str {
    match e {
        ReplaceError::NothingFocused => "nothing_focused",
        ReplaceError::Unsupported => "unsupported",
        ReplaceError::ContextChanged => "context_changed",
        ReplaceError::Paste(_) => "paste",
        ReplaceError::FormulaPrefix => "formula_prefix",
        ReplaceError::FullCellNotEnabled => "full_cell_not_enabled",
    }
}

/// User text for a failed text-path replace: the Sheets cell-edit message when one applies,
/// otherwise the generic one.
fn replace_failure_message(job: Job, on_sheets: bool, e: &ReplaceError) -> &'static str {
    on_sheets
        .then(|| sheets_edit_replace_message(job, e))
        .flatten()
        .unwrap_or(MSG_REPLACE_FAILED)
}

/// True when an LLM error message says the CLI process could not be started
/// (`CliError::Spawn`, wrapped by the provider as "Claude CLI call failed: failed to start
/// Claude CLI: ...").
pub fn is_cli_start_failure(message: &str) -> bool {
    message.contains(CLI_START_FAILURE_MARKER)
}

fn translate_error_message(e: &TranslateError) -> &'static str {
    match e {
        TranslateError::DirectionUnknown => MSG_DIRECTION_UNKNOWN,
        TranslateError::Llm(m) if is_cli_start_failure(m) => MSG_CLI_UNAVAILABLE,
        TranslateError::Llm(_) => MSG_TRANSLATE_FAILED,
        TranslateError::Integrity => DICTIONARY_INTEGRITY_MESSAGE,
    }
}

/// Work done while the badge is visible; the user message (if any) is sent after the
/// badge is hidden.
fn alt_alt_work(
    d: &FlowDeps,
    ti: &dyn TextInteraction,
    no_capture_message: &dyn Fn() -> Option<&'static str>,
    on_sheets: bool,
) -> Option<&'static str> {
    let Some(captured) = ti.capture() else {
        tracing::info!(operation = "alt_alt", status = "capture_failed");
        return no_capture_message();
    };
    let s = d.settings;
    let translated = match translate_text(
        &captured.context.text,
        d.entries,
        d.detector,
        d.llm,
        s.base_language,
        &s.target_languages,
        s.translation_tone,
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                operation = "alt_alt",
                status = "translate_failed",
                error = translate_error_kind(&e)
            );
            return Some(translate_error_message(&e));
        }
    };
    let replacement =
        translated.replacement_text(&captured.context.text, s.include_original_on_replace);
    match ti.replace(&captured, &replacement) {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(
                operation = "alt_alt",
                status = "replace_failed",
                error = replace_error_kind(&e)
            );
            Some(replace_failure_message(Job::Translate, on_sheets, &e))
        }
    }
}

/// Alt+Alt. `no_capture_message` gives the message for a failed capture outside Sheets
/// (e.g. the Docs/Slides hint, or `None` to stay silent).
pub fn alt_alt_flow(
    d: &FlowDeps,
    sheets: &SheetsDeps,
    ti: &dyn TextInteraction,
    no_capture_message: &dyn Fn() -> Option<&'static str>,
    badge: &dyn Badge,
    notifier: &dyn Notifier,
) {
    let started = Instant::now();
    let on_sheets = match route(sheets, "alt_alt", Some(MSG_SHEETS_RANGE_REPLACE_DISABLED)) {
        Routed::Refuse(m) => {
            notifier.notify(&m);
            return;
        }
        Routed::Range => {
            run_range_replace(
                d,
                sheets,
                Job::Translate,
                started,
                badge,
                LOADING_TEXT,
                notifier,
            );
            return;
        }
        Routed::Text { on_sheets } => on_sheets,
    };
    let no_capture = || {
        if on_sheets {
            Some(MSG_SHEETS_EDIT_TRANSLATE_FAILED)
        } else {
            no_capture_message()
        }
    };
    badge.show(LOADING_TEXT);
    let message = {
        let _guard = BadgeGuard(badge);
        alt_alt_work(d, ti, &no_capture, on_sheets)
    };
    tracing::info!(
        operation = "alt_alt",
        elapsed_ms = started.elapsed().as_millis() as u64,
        notified = message.is_some(),
        "done"
    );
    if let Some(m) = message {
        notifier.notify(m);
    }
}

pub fn ctrl_ctrl_flow(
    d: &FlowDeps,
    sheets: &SheetsDeps,
    ti: &dyn TextInteraction,
    view: &dyn ExplainView,
) {
    let started = Instant::now();
    match route(sheets, "ctrl_ctrl", None) {
        Routed::Refuse(m) => {
            view.show_error(&m);
            return;
        }
        Routed::Range => {
            run_table(d, sheets, Job::Translate, started, view);
            return;
        }
        Routed::Text { .. } => {}
    }
    // Capture first: showing the window before capture would steal focus from the target.
    let Some(captured) = ti.capture() else {
        tracing::info!(operation = "ctrl_ctrl", status = "capture_failed");
        view.show_error(MSG_CTRL_NO_SELECTION);
        return;
    };
    view.show_loading(LOADING_TEXT);
    let s = d.settings;
    let result = explain_text(
        &captured.context.text,
        d.entries,
        d.detector,
        d.llm,
        s.base_language,
        &s.target_languages,
        s.translation_tone,
    );
    tracing::info!(
        operation = "ctrl_ctrl",
        elapsed_ms = started.elapsed().as_millis() as u64,
        ok = result.is_ok(),
        error = result.as_ref().err().map(translate_error_kind),
        "done"
    );
    match result {
        Ok(r) => view.show_result(r, s.ctrl_ctrl_display_mode),
        Err(TranslateError::Integrity) => view.show_error(DICTIONARY_INTEGRITY_MESSAGE),
        Err(TranslateError::Llm(m)) if is_cli_start_failure(&m) => {
            view.show_error(MSG_CLI_UNAVAILABLE)
        }
        Err(_) => view.show_error(MSG_EXPLAIN_FAILED),
    }
}

/// User-facing text for a non-success correction outcome.
fn correction_message(o: &CorrectionOutcome) -> &'static str {
    match (&o.status, &o.llm_error) {
        (CorrectionStatus::LlmFailed, Some(m)) if is_cli_start_failure(m) => MSG_CLI_UNAVAILABLE,
        _ => o.status.user_message(),
    }
}

/// Work done while the badge is visible; the user message (if any) is sent after the
/// badge is hidden.
fn correction_replace_work(
    d: &FlowDeps,
    ti: &dyn TextInteraction,
    on_sheets: bool,
) -> Option<&'static str> {
    let Some(captured) = ti.capture() else {
        tracing::info!(operation = "correction_replace", status = "capture_failed");
        return Some(if on_sheets {
            MSG_SHEETS_EDIT_CORRECT_FAILED
        } else {
            MSG_CORRECTION_REPLACE_NO_SELECTION
        });
    };
    let s = d.settings;
    let outcome = correct_text(
        &captured.context.text,
        s.base_language,
        s.korean_correction_mode,
        d.entries,
        d.detector,
        d.llm,
    );
    let corrected = match (&outcome.status, outcome.corrected_text.as_deref()) {
        (CorrectionStatus::Success, Some(c)) => c,
        _ => return Some(correction_message(&outcome)),
    };
    match ti.replace(&captured, corrected) {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(
                operation = "correction_replace",
                status = "replace_failed",
                error = replace_error_kind(&e)
            );
            Some(replace_failure_message(Job::Correct, on_sheets, &e))
        }
    }
}

/// Ctrl+Alt+Alt: correct the selection and replace it. `modifiers_released` waits (up to
/// 2 seconds) until Ctrl and Alt are both up and reports whether they were. Sheets routing
/// comes after the base check and the wait (v5 order).
pub fn correction_replace_flow(
    d: &FlowDeps,
    sheets: &SheetsDeps,
    ti: &dyn TextInteraction,
    modifiers_released: &dyn Fn() -> bool,
    badge: &dyn Badge,
    notifier: &dyn Notifier,
) {
    if d.settings.base_language != Lang::Ko {
        notifier.notify(CorrectionStatus::BaseNotKorean.user_message());
        return;
    }
    if !modifiers_released() {
        tracing::info!(operation = "correction_replace", status = "modifiers_held");
        notifier.notify(MSG_CORRECTION_REPLACE_NO_SELECTION);
        return;
    }
    // Timed from here so the modifier wait does not eat the batch job budget.
    let started = Instant::now();
    let loading = d.settings.korean_correction_mode.loading_message();
    let on_sheets = match route(
        sheets,
        "correction_replace",
        Some(MSG_SHEETS_CORRECTION_RANGE_REPLACE_DISABLED),
    ) {
        Routed::Refuse(m) => {
            notifier.notify(&m);
            return;
        }
        Routed::Range => {
            run_range_replace(d, sheets, Job::Correct, started, badge, loading, notifier);
            return;
        }
        Routed::Text { on_sheets } => on_sheets,
    };
    badge.show(loading);
    let message = {
        let _guard = BadgeGuard(badge);
        correction_replace_work(d, ti, on_sheets)
    };
    tracing::info!(
        operation = "correction_replace",
        elapsed_ms = started.elapsed().as_millis() as u64,
        notified = message.is_some(),
        "done"
    );
    if let Some(m) = message {
        notifier.notify(m);
    }
}

/// Alt+Ctrl+Ctrl: correct the selection and show the result in the explanation window.
/// Sheets routing comes after the base check and the wait (v5 order).
pub fn correction_view_flow(
    d: &FlowDeps,
    sheets: &SheetsDeps,
    ti: &dyn TextInteraction,
    modifiers_released: &dyn Fn() -> bool,
    view: &dyn ExplainView,
) {
    let s = d.settings;
    if s.base_language != Lang::Ko {
        view.show_error(CorrectionStatus::BaseNotKorean.user_message());
        return;
    }
    if !modifiers_released() {
        tracing::info!(operation = "correction_view", status = "modifiers_held");
        view.show_error(MSG_CORRECTION_VIEW_NO_SELECTION);
        return;
    }
    // Timed from here so the modifier wait does not eat the batch job budget.
    let started = Instant::now();
    match route(sheets, "correction_view", None) {
        Routed::Refuse(m) => {
            view.show_error(&m);
            return;
        }
        Routed::Range => {
            run_table(d, sheets, Job::Correct, started, view);
            return;
        }
        Routed::Text { .. } => {}
    }
    // Capture first: showing the window before capture would steal focus from the target.
    let Some(captured) = ti.capture() else {
        tracing::info!(operation = "correction_view", status = "capture_failed");
        view.show_error(MSG_CORRECTION_VIEW_NO_SELECTION);
        return;
    };
    view.show_loading(s.korean_correction_mode.loading_message());
    let outcome = correct_sentences(
        &captured.context.text,
        s.base_language,
        s.korean_correction_mode,
        d.entries,
        d.detector,
        d.llm,
    );
    tracing::info!(
        operation = "correction_view",
        elapsed_ms = started.elapsed().as_millis() as u64,
        status = ?outcome.status,
        "done"
    );
    match (outcome.status, outcome.sentences.clone()) {
        (CorrectionStatus::Success, Some(sentences)) => {
            view.show_correction(sentences, s.ctrl_ctrl_display_mode)
        }
        _ => view.show_error(correction_message(&outcome)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sheets_flows::SheetsDeps;
    use ct_core::SheetsConfig;
    use ct_core::llm::{
        CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationSentence, LlmError,
        TranslationRequest, TranslationResult,
    };
    use ct_core::{Action, Lang, RuleBasedDetector};
    use ct_sheets::{SheetsInteraction, SheetsRouting, SheetsTable};
    use ct_win::chrome::{CapturedText, EditingContext};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    pub(crate) type Log = Arc<Mutex<Vec<String>>>;

    pub(crate) fn push(log: &Log, s: impl Into<String>) {
        log.lock().unwrap().push(s.into());
    }

    pub(crate) fn snapshot(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    fn upper(dests: &[Lang]) -> Vec<(Lang, String)> {
        dests
            .iter()
            .map(|l| (*l, l.code().to_uppercase()))
            .collect()
    }

    const SPAWN_ERR: &str =
        "Claude CLI call failed: failed to start Claude CLI: Access is denied. (os error 5)";

    #[test]
    fn cli_start_failure_is_recognised_by_stable_substring() {
        assert!(is_cli_start_failure(SPAWN_ERR));
        assert!(is_cli_start_failure("failed to start Claude CLI: x"));
        assert!(!is_cli_start_failure(
            "Claude CLI call failed: Claude CLI timed out"
        ));
        assert!(!is_cli_start_failure("boom"));
        assert!(!is_cli_start_failure(""));
    }

    #[derive(Clone, Copy)]
    enum LlmMode {
        Ok,
        Fail,
        /// Fails like a CLI that could not be started (blocked cmd.exe, missing file).
        SpawnFail,
        /// Returns a language set that differs from the request.
        BadLangs,
        /// Returns text without dictionary placeholders.
        DropsPlaceholders,
    }

    struct FakeLlm(LlmMode);

    impl LlmProvider for FakeLlm {
        fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, LlmError> {
            match self.0 {
                LlmMode::Fail => Err(LlmError("boom".into())),
                LlmMode::SpawnFail => Err(LlmError(SPAWN_ERR.into())),
                LlmMode::BadLangs => Ok(TranslationResult {
                    translations: vec![(Lang::Vi, "x".into())],
                }),
                LlmMode::DropsPlaceholders => Ok(TranslationResult {
                    translations: req
                        .destinations
                        .iter()
                        .map(|l| (*l, "gone".to_string()))
                        .collect(),
                }),
                LlmMode::Ok => Ok(TranslationResult {
                    translations: upper(&req.destinations),
                }),
            }
        }
        fn explain(&self, req: &ExplanationRequest) -> Result<ExplanationResult, LlmError> {
            let translations = match self.0 {
                LlmMode::Fail => return Err(LlmError("boom".into())),
                LlmMode::SpawnFail => return Err(LlmError(SPAWN_ERR.into())),
                LlmMode::DropsPlaceholders => req
                    .destinations
                    .iter()
                    .map(|l| (*l, "gone".to_string()))
                    .collect(),
                _ => upper(&req.destinations),
            };
            Ok(ExplanationResult {
                sentences: vec![ExplanationSentence {
                    original: req.text.clone(),
                    translations,
                }],
            })
        }
        fn correct(&self, _: &CorrectionRequest) -> Result<String, LlmError> {
            Err(LlmError("unused".into()))
        }
        fn correct_sentences(
            &self,
            _: &CorrectionRequest,
        ) -> Result<Vec<CorrectionSentence>, LlmError> {
            Err(LlmError("unused".into()))
        }
    }

    pub(crate) struct FakeTi {
        text: Option<String>,
        pub(crate) replace_err: Option<fn() -> ReplaceError>,
        log: Log,
    }

    impl FakeTi {
        pub(crate) fn new(text: Option<&str>, log: &Log) -> Self {
            FakeTi {
                text: text.map(String::from),
                replace_err: None,
                log: log.clone(),
            }
        }
    }

    impl TextInteraction for FakeTi {
        fn capture(&self) -> Option<CapturedText> {
            push(&self.log, "capture");
            self.text.as_ref().map(|t| CapturedText {
                context: EditingContext {
                    app: "chrome",
                    field_id: "f",
                    has_selection: true,
                    text: t.clone(),
                },
            })
        }
        fn replace(&self, _: &CapturedText, new_text: &str) -> Result<(), ReplaceError> {
            push(&self.log, format!("replace:{new_text}"));
            match self.replace_err {
                None => Ok(()),
                Some(err) => Err(err()),
            }
        }
    }

    pub(crate) struct FakeBadge(pub(crate) Log);
    impl Badge for FakeBadge {
        fn show(&self, text: &str) {
            push(&self.0, format!("show:{text}"));
        }
        fn hide(&self) {
            push(&self.0, "hide");
        }
    }

    pub(crate) struct FakeNotifier(pub(crate) Log);
    impl Notifier for FakeNotifier {
        fn notify(&self, m: &str) {
            push(&self.0, format!("notify:{m}"));
        }
    }

    pub(crate) struct FakeView(pub(crate) Log);
    impl ExplainView for FakeView {
        fn show_loading(&self, t: &str) {
            push(&self.0, format!("loading:{t}"));
        }
        fn show_error(&self, m: &str) {
            push(&self.0, format!("error:{m}"));
        }
        fn show_result(&self, r: ExplanationResult, mode: DisplayMode) {
            push(&self.0, format!("result:{}:{mode:?}", r.sentences.len()));
        }
        fn show_correction(&self, sentences: Vec<CorrectionSentence>, mode: DisplayMode) {
            push(&self.0, format!("correction:{}:{mode:?}", sentences.len()));
        }
        fn show_table(&self, table: SheetsTable) {
            push(&self.0, format!("table:{}x{}", table.rows, table.cols));
        }
    }

    /// Routing that always answers "not Sheets" and a range port that is never reached.
    pub(crate) struct NoSheets;

    impl SheetsRouting for NoSheets {
        fn url_and_state(&self) -> (Option<String>, Option<ct_sheets::SheetsInputState>) {
            (None, None)
        }
    }

    impl SheetsInteraction for NoSheets {
        fn capture_selection(&self) -> Result<ct_sheets::SheetSelection, ct_sheets::CaptureError> {
            panic!("range port used off Sheets")
        }
        fn can_edit(&self) -> bool {
            false
        }
        fn write_selection(&self, _: &ct_sheets::SheetSelection, _: &str) -> Result<(), String> {
            panic!("range port used off Sheets")
        }
    }

    fn no_sheets(config: &SheetsConfig) -> SheetsDeps<'_> {
        SheetsDeps {
            routing: &NoSheets,
            range: &NoSheets,
            config,
        }
    }

    fn settings(targets: &[Lang], include: bool) -> UserSettings {
        UserSettings {
            target_languages: targets.to_vec(),
            include_original_on_replace: include,
            ..UserSettings::default()
        }
    }

    fn dict() -> Vec<DictionaryEntry> {
        let tr: BTreeMap<String, String> = [("ko", "논리 모델"), ("en", "logical model")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        vec![DictionaryEntry::new("논리 모델".to_string(), Action::Translate, tr).unwrap()]
    }

    struct Alt {
        mode: LlmMode,
        text: Option<&'static str>,
        docs: bool,
        replace_fails: bool,
        entries: Vec<DictionaryEntry>,
    }

    impl Alt {
        fn new(text: Option<&'static str>) -> Self {
            Alt {
                mode: LlmMode::Ok,
                text,
                docs: false,
                replace_fails: false,
                entries: vec![],
            }
        }
        fn run(&self, s: &UserSettings) -> Vec<String> {
            let log: Log = Default::default();
            let mut ti = FakeTi::new(self.text, &log);
            if self.replace_fails {
                ti.replace_err = Some(|| ReplaceError::ContextChanged);
            }
            let llm = FakeLlm(self.mode);
            let d = FlowDeps {
                settings: s,
                entries: &self.entries,
                detector: &RuleBasedDetector,
                llm: &llm,
            };
            let docs = self.docs;
            let cfg = SheetsConfig::default();
            alt_alt_flow(
                &d,
                &no_sheets(&cfg),
                &ti,
                &move || docs.then_some(MSG_ALT_NO_SELECTION_DOCS),
                &FakeBadge(log.clone()),
                &FakeNotifier(log.clone()),
            );
            snapshot(&log)
        }
    }

    fn run_ctrl(
        s: &UserSettings,
        mode: LlmMode,
        text: Option<&str>,
        entries: &[DictionaryEntry],
    ) -> Vec<String> {
        let log: Log = Default::default();
        let ti = FakeTi::new(text, &log);
        let llm = FakeLlm(mode);
        let d = FlowDeps {
            settings: s,
            entries,
            detector: &RuleBasedDetector,
            llm: &llm,
        };
        let cfg = SheetsConfig::default();
        ctrl_ctrl_flow(&d, &no_sheets(&cfg), &ti, &FakeView(log.clone()));
        snapshot(&log)
    }

    fn no_replace(log: &[String]) -> bool {
        !log.iter().any(|l| l.starts_with("replace"))
    }

    #[test]
    fn error_kinds_are_variant_names_without_inner_text() {
        assert_eq!(
            translate_error_kind(&TranslateError::DirectionUnknown),
            "direction_unknown"
        );
        assert_eq!(
            translate_error_kind(&TranslateError::Llm("secret text".into())),
            "llm_failed"
        );
        assert_eq!(
            translate_error_kind(&TranslateError::Integrity),
            "integrity"
        );
        assert_eq!(
            replace_error_kind(&ReplaceError::NothingFocused),
            "nothing_focused"
        );
        assert_eq!(
            replace_error_kind(&ReplaceError::Unsupported),
            "unsupported"
        );
        assert_eq!(
            replace_error_kind(&ReplaceError::ContextChanged),
            "context_changed"
        );
        assert_eq!(
            replace_error_kind(&ReplaceError::Paste("secret text".into())),
            "paste"
        );
    }

    #[test]
    fn alt_alt_replaces_with_targets_in_order() {
        let s = settings(&[Lang::Ja, Lang::En], false);
        let log = Alt::new(Some("안녕")).run(&s);
        assert!(log.contains(&"replace:JA\n\nEN".to_string()), "{log:?}");
    }

    #[test]
    fn alt_alt_includes_original_when_enabled() {
        let s = settings(&[Lang::En, Lang::Ja], true);
        let log = Alt::new(Some("원문")).run(&s);
        assert!(
            log.contains(&"replace:원문\n\nEN\n\nJA".to_string()),
            "{log:?}"
        );
    }

    #[test]
    fn alt_alt_target_input_replaces_with_original_and_base_only() {
        let s = settings(&[Lang::En, Lang::Ja], true);
        let log = Alt::new(Some("Hello")).run(&s);
        assert!(log.contains(&"replace:Hello\n\nKO".to_string()), "{log:?}");
    }

    #[test]
    fn alt_alt_badge_shown_before_capture_and_hidden_after() {
        let s = settings(&[Lang::En], false);
        let log = Alt::new(Some("안녕")).run(&s);
        assert_eq!(
            log,
            vec!["show:Translating...", "capture", "replace:EN", "hide"]
        );
    }

    #[test]
    fn alt_alt_capture_failure_silent_outside_docs_slides() {
        let s = settings(&[Lang::En], false);
        let log = Alt::new(None).run(&s);
        assert_eq!(log, vec!["show:Translating...", "capture", "hide"]);
    }

    #[test]
    fn alt_alt_capture_failure_on_docs_notifies() {
        let s = settings(&[Lang::En], false);
        let mut alt = Alt::new(None);
        alt.docs = true;
        let log = alt.run(&s);
        assert_eq!(
            log,
            vec![
                "show:Translating...".to_string(),
                "capture".to_string(),
                "hide".to_string(),
                format!("notify:{MSG_ALT_NO_SELECTION_DOCS}"),
            ]
        );
    }

    #[test]
    fn alt_alt_translate_failures_notify_and_never_replace() {
        let s = settings(&[Lang::En], false);
        let cases: Vec<(Alt, &str)> = vec![
            (Alt::new(Some("1234")), MSG_DIRECTION_UNKNOWN),
            (
                Alt {
                    mode: LlmMode::Fail,
                    ..Alt::new(Some("안녕"))
                },
                MSG_TRANSLATE_FAILED,
            ),
            (
                Alt {
                    mode: LlmMode::SpawnFail,
                    ..Alt::new(Some("안녕"))
                },
                MSG_CLI_UNAVAILABLE,
            ),
            (
                Alt {
                    mode: LlmMode::BadLangs,
                    ..Alt::new(Some("안녕"))
                },
                MSG_TRANSLATE_FAILED,
            ),
            (
                Alt {
                    mode: LlmMode::DropsPlaceholders,
                    entries: dict(),
                    ..Alt::new(Some("논리 모델 안녕"))
                },
                DICTIONARY_INTEGRITY_MESSAGE,
            ),
        ];
        for (alt, message) in cases {
            let log = alt.run(&s);
            assert!(no_replace(&log), "{log:?}");
            assert_eq!(log.last(), Some(&format!("notify:{message}")), "{log:?}");
            assert!(log.contains(&"hide".to_string()));
        }
    }

    #[test]
    fn alt_alt_replace_failure_notifies() {
        let s = settings(&[Lang::En], false);
        let mut alt = Alt::new(Some("안녕"));
        alt.replace_fails = true;
        let log = alt.run(&s);
        assert_eq!(log.last(), Some(&format!("notify:{MSG_REPLACE_FAILED}")));
        assert!(log.contains(&"hide".to_string()));
    }

    #[test]
    fn ctrl_ctrl_shows_loading_only_after_capture() {
        let s = settings(&[Lang::En], false);
        let log = run_ctrl(&s, LlmMode::Ok, Some("안녕"), &[]);
        assert_eq!(log[0], "capture");
        assert_eq!(log[1], "loading:Translating...");
    }

    #[test]
    fn ctrl_ctrl_capture_failure_shows_no_selection() {
        let s = settings(&[Lang::En], false);
        let log = run_ctrl(&s, LlmMode::Ok, None, &[]);
        assert_eq!(
            log,
            vec![
                "capture".to_string(),
                format!("error:{MSG_CTRL_NO_SELECTION}")
            ]
        );
    }

    #[test]
    fn ctrl_ctrl_integrity_and_failure_messages() {
        let s = settings(&[Lang::En], false);
        let log = run_ctrl(&s, LlmMode::Fail, Some("안녕"), &[]);
        assert_eq!(log.last(), Some(&format!("error:{MSG_EXPLAIN_FAILED}")));
        let log = run_ctrl(&s, LlmMode::SpawnFail, Some("안녕"), &[]);
        assert_eq!(log.last(), Some(&format!("error:{MSG_CLI_UNAVAILABLE}")));
        let log = run_ctrl(&s, LlmMode::Ok, Some("1234"), &[]);
        assert_eq!(log.last(), Some(&format!("error:{MSG_EXPLAIN_FAILED}")));
        let log = run_ctrl(
            &s,
            LlmMode::DropsPlaceholders,
            Some("논리 모델 안녕"),
            &dict(),
        );
        assert_eq!(
            log.last(),
            Some(&format!("error:{DICTIONARY_INTEGRITY_MESSAGE}")),
            "{log:?}"
        );
    }

    #[test]
    fn ctrl_ctrl_result_passes_display_mode() {
        let mut s = settings(&[Lang::En], false);
        s.ctrl_ctrl_display_mode = DisplayMode::PerSentence;
        let log = run_ctrl(&s, LlmMode::Ok, Some("안녕"), &[]);
        assert_eq!(log.last().map(String::as_str), Some("result:1:PerSentence"));
    }

    // ---- Korean correction flows ----

    struct CorrLlm {
        single: Result<String, String>,
        sentences: Result<Vec<CorrectionSentence>, String>,
    }

    impl CorrLlm {
        fn ok() -> Self {
            CorrLlm {
                single: Ok("안녕하세요".into()),
                sentences: Ok(vec![CorrectionSentence {
                    original: "안녕 하세요".into(),
                    corrected: "안녕하세요".into(),
                }]),
            }
        }
        fn failing(msg: &str) -> Self {
            CorrLlm {
                single: Err(msg.into()),
                sentences: Err(msg.into()),
            }
        }
    }

    impl LlmProvider for CorrLlm {
        fn translate(&self, _: &TranslationRequest) -> Result<TranslationResult, LlmError> {
            Err(LlmError("unused".into()))
        }
        fn explain(&self, _: &ExplanationRequest) -> Result<ExplanationResult, LlmError> {
            Err(LlmError("unused".into()))
        }
        fn correct(&self, _: &CorrectionRequest) -> Result<String, LlmError> {
            self.single.clone().map_err(LlmError)
        }
        fn correct_sentences(
            &self,
            _: &CorrectionRequest,
        ) -> Result<Vec<CorrectionSentence>, LlmError> {
            self.sentences.clone().map_err(LlmError)
        }
    }

    fn run_replace(
        s: &UserSettings,
        llm: &CorrLlm,
        text: Option<&str>,
        replace_fails: bool,
        released: &dyn Fn() -> bool,
    ) -> Vec<String> {
        let log: Log = Default::default();
        let mut ti = FakeTi::new(text, &log);
        if replace_fails {
            ti.replace_err = Some(|| ReplaceError::ContextChanged);
        }
        let d = FlowDeps {
            settings: s,
            entries: &[],
            detector: &RuleBasedDetector,
            llm,
        };
        let cfg = SheetsConfig::default();
        correction_replace_flow(
            &d,
            &no_sheets(&cfg),
            &ti,
            released,
            &FakeBadge(log.clone()),
            &FakeNotifier(log.clone()),
        );
        snapshot(&log)
    }

    fn run_view(
        s: &UserSettings,
        llm: &CorrLlm,
        text: Option<&str>,
        released: &dyn Fn() -> bool,
    ) -> Vec<String> {
        let log: Log = Default::default();
        let ti = FakeTi::new(text, &log);
        let d = FlowDeps {
            settings: s,
            entries: &[],
            detector: &RuleBasedDetector,
            llm,
        };
        let cfg = SheetsConfig::default();
        correction_view_flow(&d, &no_sheets(&cfg), &ti, released, &FakeView(log.clone()));
        snapshot(&log)
    }

    fn last(log: &[String]) -> Option<&str> {
        log.last().map(String::as_str)
    }

    #[test]
    fn replace_success_replaces_with_corrected_text_inside_badge() {
        let log = run_replace(
            &UserSettings::default(),
            &CorrLlm::ok(),
            Some("안녕 하세요"),
            false,
            &|| true,
        );
        assert_eq!(
            log,
            [
                "show:한국어 교정 중...",
                "capture",
                "replace:안녕하세요",
                "hide"
            ]
        );
    }

    #[test]
    fn replace_polish_mode_shows_polish_loading_text() {
        let s = UserSettings {
            korean_correction_mode: ct_core::CorrectionMode::Polish,
            ..UserSettings::default()
        };
        let log = run_replace(&s, &CorrLlm::ok(), Some("안녕 하세요"), false, &|| {
            true
        });
        assert_eq!(log[0], "show:한국어 수정 중...");
    }

    #[test]
    fn replace_ignores_include_original_option() {
        let s = UserSettings {
            include_original_on_replace: true,
            ..UserSettings::default()
        };
        let log = run_replace(&s, &CorrLlm::ok(), Some("안녕 하세요"), false, &|| {
            true
        });
        assert!(log.contains(&"replace:안녕하세요".to_string()));
    }

    #[test]
    fn replace_waits_for_modifiers_and_never_captures_on_timeout() {
        let log = run_replace(
            &UserSettings::default(),
            &CorrLlm::ok(),
            Some("안녕 하세요"),
            false,
            &|| false,
        );
        assert_eq!(
            log,
            [format!("notify:{MSG_CORRECTION_REPLACE_NO_SELECTION}")]
        );
    }

    #[test]
    fn replace_base_not_korean_notifies_without_capture() {
        let s = UserSettings {
            base_language: Lang::En,
            ..UserSettings::default()
        };
        let log: Log = Default::default();
        let w = log.clone();
        let released = move || {
            push(&w, "wait");
            true
        };
        let ti = FakeTi::new(Some("안녕 하세요"), &log);
        let llm = CorrLlm::ok();
        let d = FlowDeps {
            settings: &s,
            entries: &[],
            detector: &RuleBasedDetector,
            llm: &llm,
        };
        let cfg = SheetsConfig::default();
        correction_replace_flow(
            &d,
            &no_sheets(&cfg),
            &ti,
            &released,
            &FakeBadge(log.clone()),
            &FakeNotifier(log.clone()),
        );
        assert_eq!(
            snapshot(&log),
            [format!(
                "notify:{}",
                ct_core::CorrectionStatus::BaseNotKorean.user_message()
            )]
        );
    }

    #[test]
    fn replace_capture_failure_notifies_after_badge_hidden() {
        let log = run_replace(
            &UserSettings::default(),
            &CorrLlm::ok(),
            None,
            false,
            &|| true,
        );
        assert_eq!(
            log,
            [
                "show:한국어 교정 중...".to_string(),
                "capture".to_string(),
                "hide".to_string(),
                format!("notify:{MSG_CORRECTION_REPLACE_NO_SELECTION}"),
            ]
        );
    }

    #[test]
    fn replace_unchanged_notifies_after_badge_hidden_and_never_replaces() {
        let llm = CorrLlm {
            single: Ok("안녕 하세요".into()),
            ..CorrLlm::ok()
        };
        let log = run_replace(
            &UserSettings::default(),
            &llm,
            Some("안녕 하세요"),
            false,
            &|| true,
        );
        assert!(no_replace(&log));
        assert_eq!(
            log[log.len() - 2..],
            ["hide", "notify:수정할 부분이 없습니다."]
        );
    }

    #[test]
    fn replace_not_korean_text_notifies() {
        let log = run_replace(
            &UserSettings::default(),
            &CorrLlm::ok(),
            Some("hello world"),
            false,
            &|| true,
        );
        assert!(no_replace(&log));
        assert_eq!(
            last(&log),
            Some("notify:선택한 문장의 주 언어가 한국어가 아니어서 교정하지 않았습니다.")
        );
    }

    #[test]
    fn replace_llm_failure_notifies_correction_failed() {
        let log = run_replace(
            &UserSettings::default(),
            &CorrLlm::failing("boom"),
            Some("안녕 하세요"),
            false,
            &|| true,
        );
        assert!(no_replace(&log));
        assert_eq!(
            last(&log),
            Some("notify:교정에 실패하여 원본을 변경하지 않았습니다.")
        );
    }

    #[test]
    fn replace_cli_start_failure_shows_cli_notice() {
        let log = run_replace(
            &UserSettings::default(),
            &CorrLlm::failing(SPAWN_ERR),
            Some("안녕 하세요"),
            false,
            &|| true,
        );
        assert_eq!(
            last(&log),
            Some(format!("notify:{MSG_CLI_UNAVAILABLE}").as_str())
        );
    }

    #[test]
    fn replace_failure_notifies_replace_failed() {
        let log = run_replace(
            &UserSettings::default(),
            &CorrLlm::ok(),
            Some("안녕 하세요"),
            true,
            &|| true,
        );
        assert_eq!(
            last(&log),
            Some(format!("notify:{MSG_REPLACE_FAILED}").as_str())
        );
    }

    #[test]
    fn view_success_shows_loading_then_correction() {
        let log = run_view(
            &UserSettings::default(),
            &CorrLlm::ok(),
            Some("안녕 하세요"),
            &|| true,
        );
        assert_eq!(
            log,
            [
                "capture",
                "loading:한국어 교정 중...",
                "correction:1:WholeText"
            ]
        );
    }

    #[test]
    fn view_uses_display_mode_setting() {
        let s = UserSettings {
            ctrl_ctrl_display_mode: DisplayMode::PerSentence,
            ..UserSettings::default()
        };
        let log = run_view(&s, &CorrLlm::ok(), Some("안녕 하세요"), &|| true);
        assert_eq!(last(&log), Some("correction:1:PerSentence"));
    }

    #[test]
    fn view_timeout_shows_no_selection_and_never_captures() {
        let log = run_view(
            &UserSettings::default(),
            &CorrLlm::ok(),
            Some("안녕 하세요"),
            &|| false,
        );
        assert_eq!(log, [format!("error:{MSG_CORRECTION_VIEW_NO_SELECTION}")]);
    }

    #[test]
    fn view_base_not_korean_shows_error_without_capture() {
        let s = UserSettings {
            base_language: Lang::En,
            ..UserSettings::default()
        };
        let log = run_view(&s, &CorrLlm::ok(), Some("안녕 하세요"), &|| true);
        assert_eq!(
            log,
            [format!(
                "error:{}",
                ct_core::CorrectionStatus::BaseNotKorean.user_message()
            )]
        );
    }

    #[test]
    fn view_capture_failure_shows_error_without_loading() {
        let log = run_view(&UserSettings::default(), &CorrLlm::ok(), None, &|| true);
        assert_eq!(
            log,
            [
                "capture".to_string(),
                format!("error:{MSG_CORRECTION_VIEW_NO_SELECTION}")
            ]
        );
    }

    #[test]
    fn view_llm_failure_shows_correction_failed() {
        let log = run_view(
            &UserSettings::default(),
            &CorrLlm::failing("boom"),
            Some("안녕 하세요"),
            &|| true,
        );
        assert_eq!(
            last(&log),
            Some("error:교정에 실패하여 원본을 변경하지 않았습니다.")
        );
    }

    #[test]
    fn view_cli_start_failure_shows_cli_notice() {
        let log = run_view(
            &UserSettings::default(),
            &CorrLlm::failing(SPAWN_ERR),
            Some("안녕 하세요"),
            &|| true,
        );
        assert_eq!(
            last(&log),
            Some(format!("error:{MSG_CLI_UNAVAILABLE}").as_str())
        );
    }
}
