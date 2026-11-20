//! Alt+Alt (translate and replace) and Ctrl+Ctrl (translate and explain) flows.
//!
//! The original text is replaced only after a successful translation, and any failure
//! leaves it untouched. Logs carry operation, duration and error kind only, never text.

use std::time::Instant;

use ct_core::{
    DICTIONARY_INTEGRITY_MESSAGE, DictionaryEntry, DisplayMode, ExplanationResult,
    LanguageDetector, LlmProvider, TranslateError, UserSettings, explain_text, translate_text,
};
use ct_llm::CLI_START_FAILURE_MARKER;
use ct_win::chrome::{ReplaceError, TextInteraction};

pub const MSG_DIRECTION_UNKNOWN: &str = "번역 방향을 판단할 수 없어 원문을 변경하지 않았습니다.";
pub const MSG_TRANSLATE_FAILED: &str = "번역에 실패하여 원본을 변경하지 않았습니다.";
pub const MSG_REPLACE_FAILED: &str = "교체 대상이 바뀌어 원문을 변경하지 않았습니다.";
pub const MSG_ALT_NO_SELECTION_DOCS: &str = "텍스트를 선택한 후 Alt+Alt를 눌러주세요.";
pub const MSG_CTRL_NO_SELECTION: &str = "텍스트를 선택한 후 Ctrl+Ctrl을 실행해주세요.";
pub const MSG_EXPLAIN_FAILED: &str = "Could not translate/explain the selected text";
pub const MSG_CLI_UNAVAILABLE: &str =
    "Claude CLI를 실행할 수 없습니다. Settings의 Claude CLI 경로를 확인하세요.";

const LOADING_TEXT: &str = "Translating...";

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
}

pub struct FlowDeps<'a> {
    pub settings: &'a UserSettings,
    pub entries: &'a [DictionaryEntry],
    pub detector: &'a dyn LanguageDetector,
    pub llm: &'a dyn LlmProvider,
}

/// Hides the badge on every exit path, including panics.
struct BadgeGuard<'a>(&'a dyn Badge);

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
    }
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
    docs_or_slides: &dyn Fn() -> bool,
) -> Option<&'static str> {
    let Some(captured) = ti.capture() else {
        tracing::info!(operation = "alt_alt", status = "capture_failed");
        return docs_or_slides().then_some(MSG_ALT_NO_SELECTION_DOCS);
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
            Some(MSG_REPLACE_FAILED)
        }
    }
}

pub fn alt_alt_flow(
    d: &FlowDeps,
    ti: &dyn TextInteraction,
    docs_or_slides: &dyn Fn() -> bool,
    badge: &dyn Badge,
    notifier: &dyn Notifier,
) {
    let started = Instant::now();
    badge.show(LOADING_TEXT);
    let message = {
        let _guard = BadgeGuard(badge);
        alt_alt_work(d, ti, docs_or_slides)
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

pub fn ctrl_ctrl_flow(d: &FlowDeps, ti: &dyn TextInteraction, view: &dyn ExplainView) {
    let started = Instant::now();
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

#[cfg(test)]
mod tests {
    use super::*;
    use ct_core::llm::{
        CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationSentence, LlmError,
        TranslationRequest, TranslationResult,
    };
    use ct_core::{Action, Lang, RuleBasedDetector};
    use ct_win::chrome::{CapturedText, EditingContext};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    fn push(log: &Log, s: impl Into<String>) {
        log.lock().unwrap().push(s.into());
    }

    fn snapshot(log: &Log) -> Vec<String> {
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

    struct FakeTi {
        text: Option<String>,
        replace_result: Result<(), ReplaceError>,
        log: Log,
    }

    impl FakeTi {
        fn new(text: Option<&str>, log: &Log) -> Self {
            FakeTi {
                text: text.map(String::from),
                replace_result: Ok(()),
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
            match &self.replace_result {
                Ok(()) => Ok(()),
                Err(_) => Err(ReplaceError::ContextChanged),
            }
        }
    }

    struct FakeBadge(Log);
    impl Badge for FakeBadge {
        fn show(&self, text: &str) {
            push(&self.0, format!("show:{text}"));
        }
        fn hide(&self) {
            push(&self.0, "hide");
        }
    }

    struct FakeNotifier(Log);
    impl Notifier for FakeNotifier {
        fn notify(&self, m: &str) {
            push(&self.0, format!("notify:{m}"));
        }
    }

    struct FakeView(Log);
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
                ti.replace_result = Err(ReplaceError::ContextChanged);
            }
            let llm = FakeLlm(self.mode);
            let d = FlowDeps {
                settings: s,
                entries: &self.entries,
                detector: &RuleBasedDetector,
                llm: &llm,
            };
            let docs = self.docs;
            alt_alt_flow(
                &d,
                &ti,
                &move || docs,
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
        ctrl_ctrl_flow(&d, &ti, &FakeView(log.clone()));
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
}
