//! Google Sheets routing for the four hotkey flows (S-ref §8.1–§8.4, UI-ref §3.4, §5.3–§5.6).
//!
//! Every flow asks the router first (correction flows after the base check and modifier
//! wait). A cell selection runs a range job, an editing cell or a non-Sheets page goes to
//! the text flow, and any other Sheets state is refused. Logs carry operation, route,
//! status, cell counts and durations only, never cell text.

use std::cell::Cell;
use std::time::Instant;

use ct_core::{
    BatchFailure, BatchLimits, CellKey, CellText, SheetsConfig, batch::correct_cells,
    compose_cell_replacements, translate_cells,
};
use ct_sheets::{
    Route, SheetsInteraction, SheetsOutcome, SheetsRouting, SheetsStatus, TableResults,
    capture_for_table, correction_failure, is_google_sheets_url, replace_cells, route_of,
    table_from, translatable_cells, translation_failure,
};
use ct_win::chrome::ReplaceError;

use crate::flows::{
    Badge, BadgeGuard, ExplainView, FlowDeps, LOADING_TEXT, MSG_CLI_UNAVAILABLE,
    MSG_DIRECTION_UNKNOWN, Notifier, is_cli_start_failure,
};

pub const MSG_SHEETS_EDIT_TRANSLATE_FAILED: &str =
    "편집 중인 셀을 번역할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음).";
pub const MSG_SHEETS_EDIT_CORRECT_FAILED: &str =
    "편집 중인 셀을 교정할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음).";
pub const MSG_SHEETS_RANGE_REPLACE_DISABLED: &str = "Google Sheets 범위 교체(Alt+Alt)는 실제 Chrome에서 아직 검증되지 않아 기본으로 꺼져 있습니다. Ctrl+Ctrl로 번역을 확인하거나 셀 안의 글자를 선택한 뒤 Alt+Alt를 사용해주세요.";
pub const MSG_SHEETS_CORRECTION_RANGE_REPLACE_DISABLED: &str = "Google Sheets 범위 교체(Ctrl+Alt+Alt)는 실제 Chrome에서 아직 검증되지 않아 기본으로 꺼져 있습니다. Alt+Ctrl+Ctrl로 교정 결과를 확인하거나 셀 안의 글자를 선택한 뒤 Ctrl+Alt+Alt를 사용해주세요.";
pub const MSG_SHEETS_EDIT_TRANSLATE_FORMULA_PREFIX: &str =
    "번역 결과가 =, +로 시작해 수식으로 해석될 수 있어 교체하지 않았습니다.";
pub const MSG_SHEETS_EDIT_TRANSLATE_TARGET_CHANGED: &str =
    "번역 중 편집 중인 셀의 내용이나 선택이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.";
pub const MSG_SHEETS_EDIT_TRANSLATE_FULL_CELL_DISABLED: &str = "셀 전체 교체는 아직 검증되지 않아 꺼져 있습니다. 교체할 글자를 선택한 뒤 Alt+Alt를 눌러주세요.";
pub const MSG_SHEETS_EDIT_CORRECT_FORMULA_PREFIX: &str =
    "교정 결과가 =, +로 시작해 수식으로 해석될 수 있어 교체하지 않았습니다.";
pub const MSG_SHEETS_EDIT_CORRECT_TARGET_CHANGED: &str =
    "교정 중 편집 중인 셀의 내용이나 선택이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.";
pub const MSG_SHEETS_EDIT_CORRECT_FULL_CELL_DISABLED: &str = "셀 전체 교체는 아직 검증되지 않아 꺼져 있습니다. 교체할 글자를 선택한 뒤 Ctrl+Alt+Alt를 눌러주세요.";

/// Sheets ports and limits shared by the four flows.
pub struct SheetsDeps<'a> {
    pub routing: &'a dyn SheetsRouting,
    pub range: &'a dyn SheetsInteraction,
    pub config: &'a SheetsConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Job {
    Translate,
    Correct,
}

/// Where a flow goes after routing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Routed {
    /// Stop with this message.
    Refuse(String),
    /// Run the range job.
    Range,
    /// Run the text flow; `on_sheets` selects the Sheets cell-edit messages.
    Text { on_sheets: bool },
}

fn wrong_input_state() -> String {
    SheetsOutcome {
        status: SheetsStatus::WrongInputState,
        reason: None,
    }
    .user_message()
}

/// Routes a flow. `range_disabled` is the message for a replace flow when range replace is
/// switched off; view flows pass `None`.
pub(crate) fn route(
    s: &SheetsDeps,
    operation: &'static str,
    range_disabled: Option<&'static str>,
) -> Routed {
    let (url, route) = route_of(s.routing);
    tracing::info!(operation, route = ?route, "routed");
    match route {
        Route::SheetsUnsupported => Routed::Refuse(wrong_input_state()),
        Route::SheetsRange => match range_disabled {
            Some(m) if !s.config.range_replace_enabled => Routed::Refuse(m.to_string()),
            _ => Routed::Range,
        },
        Route::TextInteraction => Routed::Text {
            on_sheets: url.as_deref().is_some_and(is_google_sheets_url),
        },
    }
}

/// UI-ref §5.3/§5.5 message for a cell-edit replace error on Sheets; `None` keeps the
/// generic message. Never uses the error's `Display` (internal codes).
pub(crate) fn sheets_edit_replace_message(job: Job, e: &ReplaceError) -> Option<&'static str> {
    Some(match (job, e) {
        (Job::Translate, ReplaceError::FormulaPrefix) => MSG_SHEETS_EDIT_TRANSLATE_FORMULA_PREFIX,
        (Job::Translate, ReplaceError::ContextChanged) => MSG_SHEETS_EDIT_TRANSLATE_TARGET_CHANGED,
        (Job::Translate, ReplaceError::FullCellNotEnabled) => {
            MSG_SHEETS_EDIT_TRANSLATE_FULL_CELL_DISABLED
        }
        (Job::Correct, ReplaceError::FormulaPrefix) => MSG_SHEETS_EDIT_CORRECT_FORMULA_PREFIX,
        (Job::Correct, ReplaceError::ContextChanged) => MSG_SHEETS_EDIT_CORRECT_TARGET_CHANGED,
        (Job::Correct, ReplaceError::FullCellNotEnabled) => {
            MSG_SHEETS_EDIT_CORRECT_FULL_CELL_DISABLED
        }
        _ => return None,
    })
}

/// Runs a range replace job while the badge shows `badge_text`; a non-success outcome is
/// notified after the badge is hidden.
pub(crate) fn run_range_replace(
    d: &FlowDeps,
    s: &SheetsDeps,
    job: Job,
    started: Instant,
    badge: &dyn Badge,
    badge_text: &str,
    notifier: &dyn Notifier,
) {
    badge.show(badge_text);
    let notice = Cell::new(None);
    let outcome = {
        let _guard = BadgeGuard(badge);
        match job {
            Job::Translate => translate_range(d, s, started, &notice),
            Job::Correct => correct_range(d, s, started, &notice),
        }
    };
    // The notice only counts when the job actually ended in the transform's failure status.
    let notice = notice.get().filter(|_| {
        matches!(
            outcome.status,
            SheetsStatus::TranslationFailed | SheetsStatus::CorrectionFailed
        )
    });
    tracing::info!(
        operation = job_operation(job, false),
        status = ?outcome.status,
        notice = ?notice,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "done"
    );
    if let Some(notice) = notice {
        notifier.notify(notice.message());
    } else if outcome.status != SheetsStatus::Success {
        notifier.notify(&outcome.user_message());
    }
}

/// A batch failure that has its own message instead of the generic failure text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureNotice {
    /// The Claude CLI could not be started (plan 3 D2).
    CliDown,
    /// No safe translation direction (same message as the text flow).
    DirectionUnknown,
}

impl FailureNotice {
    fn message(self) -> &'static str {
        match self {
            FailureNotice::CliDown => MSG_CLI_UNAVAILABLE,
            FailureNotice::DirectionUnknown => MSG_DIRECTION_UNKNOWN,
        }
    }
}

/// A table job failure: the outcome and its special notice, if any.
type TableFailure = (SheetsOutcome, Option<FailureNotice>);

fn failure_notice(f: &BatchFailure) -> Option<FailureNotice> {
    match f {
        BatchFailure::Llm(m) if is_cli_start_failure(m) => Some(FailureNotice::CliDown),
        BatchFailure::DirectionUnknown => Some(FailureNotice::DirectionUnknown),
        _ => None,
    }
}

fn job_operation(job: Job, view: bool) -> &'static str {
    match (job, view) {
        (Job::Translate, false) => "sheets_range_translate",
        (Job::Correct, false) => "sheets_range_correct",
        (Job::Translate, true) => "sheets_table_translate",
        (Job::Correct, true) => "sheets_table_correct",
    }
}

fn translate_range(
    d: &FlowDeps,
    s: &SheetsDeps,
    started: Instant,
    notice: &Cell<Option<FailureNotice>>,
) -> SheetsOutcome {
    let st = d.settings;
    let limits = BatchLimits::from(s.config);
    let include = st.include_original_on_replace;
    let elapsed = || started.elapsed();
    let mut transform = |cells: &[CellText]| {
        let translated = translate_cells(
            cells,
            d.entries,
            d.detector,
            d.llm,
            st.base_language,
            &st.target_languages,
            st.translation_tone,
            include,
            &limits,
            &elapsed,
        )
        .map_err(|f| {
            notice.set(failure_notice(&f));
            translation_failure(&f)
        })?;
        compose_cell_replacements(cells, &translated, include, limits.max_cell_chars)
            .map_err(|f| translation_failure(&f))
    };
    replace_cells(
        s.range,
        s.config.max_range_cells,
        &mut transform,
        SheetsStatus::NoText,
        SheetsStatus::TranslationFailed,
        false,
    )
}

fn correct_range(
    d: &FlowDeps,
    s: &SheetsDeps,
    started: Instant,
    notice: &Cell<Option<FailureNotice>>,
) -> SheetsOutcome {
    let limits = BatchLimits::from(s.config);
    let elapsed = || started.elapsed();
    let mut transform = |cells: &[CellText]| {
        correct_cells(
            cells,
            d.entries,
            d.detector,
            d.llm,
            d.settings.korean_correction_mode,
            &limits,
            &elapsed,
        )
        .map_err(|f| {
            notice.set(failure_notice(&f));
            correction_failure(&f)
        })
    };
    replace_cells(
        s.range,
        s.config.max_range_cells,
        &mut transform,
        SheetsStatus::NoKoreanText,
        SheetsStatus::CorrectionFailed,
        true,
    )
}

/// Table view (UI-ref §3.4 `run_sheets`): capture first, loading only after a successful
/// capture, then the table or the outcome message.
pub(crate) fn run_table(
    d: &FlowDeps,
    s: &SheetsDeps,
    job: Job,
    started: Instant,
    view: &dyn ExplainView,
) {
    let operation = job_operation(job, true);
    let selection = match capture_for_table(s.range, s.config.max_range_cells) {
        Ok(sel) => sel,
        Err(outcome) => {
            tracing::info!(operation, status = ?outcome.status, "capture refused");
            view.show_error(&outcome.user_message());
            return;
        }
    };
    let loading = match job {
        Job::Translate => LOADING_TEXT,
        Job::Correct => d.settings.korean_correction_mode.loading_message(),
    };
    view.show_loading(loading);
    let cells: Vec<CellText> = translatable_cells(&selection.grid)
        .into_iter()
        .map(|(key, text)| CellText { key, text })
        .collect();
    let limits = BatchLimits::from(s.config);
    let elapsed = || started.elapsed();
    let st = d.settings;
    let results: Result<Vec<(CellKey, TableResults)>, TableFailure> = match job {
        Job::Translate if cells.is_empty() => Err((
            SheetsOutcome {
                status: SheetsStatus::NoText,
                reason: None,
            },
            None,
        )),
        Job::Translate => translate_cells(
            &cells,
            d.entries,
            d.detector,
            d.llm,
            st.base_language,
            &st.target_languages,
            st.translation_tone,
            false,
            &limits,
            &elapsed,
        )
        .map(|rows| {
            rows.into_iter()
                .map(|(key, texts)| {
                    let tagged = texts.into_iter().map(|(l, t)| (Some(l), t)).collect();
                    (key, tagged)
                })
                .collect()
        })
        .map_err(|f| (translation_failure(&f), failure_notice(&f))),
        Job::Correct => correct_cells(
            &cells,
            d.entries,
            d.detector,
            d.llm,
            st.korean_correction_mode,
            &limits,
            &elapsed,
        )
        .map(|rows| {
            rows.into_iter()
                .map(|(key, text)| (key, vec![(None, text)]))
                .collect()
        })
        .map_err(|f| (correction_failure(&f), failure_notice(&f))),
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match results {
        Ok(results) => {
            tracing::info!(
                operation,
                status = "success",
                cells = cells.len(),
                elapsed_ms,
                "done"
            );
            view.show_table(table_from(&selection, &results));
        }
        Err((outcome, notice)) => {
            tracing::info!(
                operation,
                status = ?outcome.status,
                notice = ?notice,
                cells = cells.len(),
                elapsed_ms,
                "done"
            );
            match notice {
                Some(notice) => view.show_error(notice.message()),
                None => view.show_error(&outcome.user_message()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::tests::{FakeBadge, FakeNotifier, FakeTi, Log, push, snapshot};
    use crate::flows::{
        ExplainView, FlowDeps, MSG_CORRECTION_REPLACE_NO_SELECTION, MSG_DIRECTION_UNKNOWN,
        MSG_REPLACE_FAILED, alt_alt_flow, correction_replace_flow, correction_view_flow,
        ctrl_ctrl_flow,
    };
    use ct_core::llm::{
        BatchCorrectionRequest, BatchCorrectionResult, BatchTranslationRequest,
        BatchTranslationResult, CorrectionRequest, CorrectionSentence, ExplanationRequest,
        LlmError, TranslationRequest, TranslationResult,
    };
    use ct_core::{
        DisplayMode, ExplanationResult, Lang, LlmProvider, RuleBasedDetector, SheetsConfig,
        UserSettings,
    };
    use ct_llm::CLI_START_FAILURE_MARKER;
    use ct_sheets::{
        CaptureError, SheetLocation, SheetSelection, SheetsInputState, SheetsInteraction,
        SheetsOutcome, SheetsRouting, SheetsStatus, SheetsTable, column_letters,
        extract_cf_html_fragment, parse_sheets_html,
    };
    use ct_win::chrome::ReplaceError;
    use std::sync::Mutex;

    const SHEETS_URL: &str = "https://docs.google.com/spreadsheets/d/abc/edit#gid=0";
    const GMAIL_URL: &str = "https://mail.google.com/mail/u/0/#inbox";

    fn wrong_input_state() -> String {
        SheetsOutcome {
            status: SheetsStatus::WrongInputState,
            reason: None,
        }
        .user_message()
    }

    /// Batch-aware fake: translations are upper-case language codes, corrections drop spaces.
    /// `cli_down`: every batch call fails like a CLI that could not be started.
    struct SheetsLlm {
        cli_down: bool,
    }

    fn cli_down_error() -> LlmError {
        LlmError(format!(
            "Claude CLI call failed: {CLI_START_FAILURE_MARKER}: Access is denied. (os error 5)"
        ))
    }

    fn upper(dests: &[Lang]) -> Vec<(Lang, String)> {
        dests
            .iter()
            .map(|l| (*l, l.code().to_uppercase()))
            .collect()
    }

    impl LlmProvider for SheetsLlm {
        fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, LlmError> {
            Ok(TranslationResult {
                translations: upper(&req.destinations),
            })
        }
        fn explain(&self, _: &ExplanationRequest) -> Result<ExplanationResult, LlmError> {
            Err(LlmError("unused".into()))
        }
        fn correct(&self, req: &CorrectionRequest) -> Result<String, LlmError> {
            Ok(req.text.replace(' ', ""))
        }
        fn correct_sentences(
            &self,
            _: &CorrectionRequest,
        ) -> Result<Vec<CorrectionSentence>, LlmError> {
            Err(LlmError("unused".into()))
        }
        fn translate_batch(
            &self,
            req: &BatchTranslationRequest,
        ) -> Result<BatchTranslationResult, LlmError> {
            if self.cli_down {
                return Err(cli_down_error());
            }
            Ok(BatchTranslationResult {
                items: req
                    .items
                    .iter()
                    .map(|i| (i.id.clone(), upper(&i.destinations)))
                    .collect(),
            })
        }
        fn correct_batch(
            &self,
            req: &BatchCorrectionRequest,
        ) -> Result<BatchCorrectionResult, LlmError> {
            if self.cli_down {
                return Err(cli_down_error());
            }
            Ok(BatchCorrectionResult {
                items: req
                    .items
                    .iter()
                    .map(|i| (i.id.clone(), i.text.replace(' ', "")))
                    .collect(),
            })
        }
    }

    struct FakeRouting {
        url: Option<&'static str>,
        state: Option<SheetsInputState>,
        log: Log,
    }

    impl SheetsRouting for FakeRouting {
        fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>) {
            push(&self.log, "route");
            (self.url.map(String::from), self.state)
        }
    }

    type Capture = Result<SheetSelection, CaptureError>;

    /// Small range port: rereads return the capture until something is written, then the
    /// grid parsed from the written CF_HTML.
    struct FakeRange {
        capture: Capture,
        written: Mutex<Option<String>>,
        log: Log,
    }

    impl FakeRange {
        fn written_grid(&self) -> Option<ct_sheets::SheetGrid> {
            let cf = self.written.lock().unwrap().clone()?;
            let fragment = extract_cf_html_fragment(&cf).unwrap();
            Some(parse_sheets_html(&fragment, None).unwrap())
        }
    }

    impl SheetsInteraction for FakeRange {
        fn capture_selection(&self) -> Result<SheetSelection, CaptureError> {
            push(&self.log, "sheets.capture");
            self.capture.clone()
        }
        fn reread_selection(&self) -> Result<SheetSelection, CaptureError> {
            let sel = self.capture.clone()?;
            Ok(match self.written_grid() {
                Some(grid) => SheetSelection { grid, ..sel },
                None => sel,
            })
        }
        fn can_edit(&self) -> bool {
            true
        }
        fn write_selection(&self, _: &SheetSelection, cf_html: &str) -> Result<(), String> {
            push(&self.log, "sheets.write");
            *self.written.lock().unwrap() = Some(cf_html.to_string());
            Ok(())
        }
        fn wait(&self, _: std::time::Duration) {}
    }

    /// Like the flows' FakeView, and keeps the last table for content checks.
    struct SpyView {
        log: Log,
        table: Mutex<Option<SheetsTable>>,
    }

    impl ExplainView for SpyView {
        fn show_loading(&self, t: &str) {
            push(&self.log, format!("loading:{t}"));
        }
        fn show_error(&self, m: &str) {
            push(&self.log, format!("error:{m}"));
        }
        fn show_result(&self, r: ExplanationResult, mode: DisplayMode) {
            push(&self.log, format!("result:{}:{mode:?}", r.sentences.len()));
        }
        fn show_correction(&self, s: Vec<CorrectionSentence>, mode: DisplayMode) {
            push(&self.log, format!("correction:{}:{mode:?}", s.len()));
        }
        fn show_table(&self, table: SheetsTable) {
            push(&self.log, format!("table:{}x{}", table.rows, table.cols));
            *self.table.lock().unwrap() = Some(table);
        }
    }

    fn grid_html(rows: &[&[&str]]) -> String {
        let mut h = String::from("<google-sheets-html-origin><table><tbody>");
        for row in rows {
            h.push_str("<tr>");
            for t in *row {
                h.push_str(&format!(
                    "<td data-sheets-value=\"{{&quot;1&quot;:2,&quot;2&quot;:&quot;{t}&quot;}}\">{t}</td>"
                ));
            }
            h.push_str("</tr>");
        }
        h.push_str("</tbody></table>");
        h
    }

    fn selection(rows: &[&[&str]]) -> SheetSelection {
        let address = format!("A1:{}{}", column_letters(rows[0].len() - 1), rows.len());
        SheetSelection {
            location: SheetLocation {
                document_id: "abc".into(),
                gid: Some("0".into()),
                range_address: Some(address),
            },
            grid: parse_sheets_html(&grid_html(rows), None).unwrap(),
            hidden_or_filtered: Some(false),
        }
    }

    #[derive(Clone, Copy)]
    enum G {
        AltAlt,
        CorrectionReplace,
        CtrlCtrl,
        CorrectionView,
    }

    struct Case {
        url: Option<&'static str>,
        state: Option<SheetsInputState>,
        capture: Capture,
        config: SheetsConfig,
        settings: UserSettings,
        text: Option<&'static str>,
        replace_err: Option<fn() -> ReplaceError>,
        released: bool,
        cli_down: bool,
    }

    struct Run {
        log: Vec<String>,
        written: Option<Vec<String>>,
        table: Option<SheetsTable>,
    }

    impl Case {
        fn base(url: Option<&'static str>, state: Option<SheetsInputState>) -> Self {
            Case {
                url,
                state,
                capture: Err(CaptureError { reason: None }),
                config: SheetsConfig::default(),
                settings: UserSettings {
                    target_languages: vec![Lang::En],
                    ..UserSettings::default()
                },
                text: None,
                replace_err: None,
                released: true,
                cli_down: false,
            }
        }
        fn range(rows: &[&[&str]]) -> Self {
            Case {
                capture: Ok(selection(rows)),
                ..Case::base(Some(SHEETS_URL), Some(SheetsInputState::CellSelection))
            }
        }
        fn editing(text: Option<&'static str>) -> Self {
            Case {
                text,
                ..Case::base(Some(SHEETS_URL), Some(SheetsInputState::CellEditing))
            }
        }

        fn run(&self, g: G) -> Run {
            let log: Log = Default::default();
            let routing = FakeRouting {
                url: self.url,
                state: self.state,
                log: log.clone(),
            };
            let range = FakeRange {
                capture: self.capture.clone(),
                written: Mutex::new(None),
                log: log.clone(),
            };
            let s = SheetsDeps {
                routing: &routing,
                range: &range,
                config: &self.config,
            };
            let mut ti = FakeTi::new(self.text, &log);
            ti.replace_err = self.replace_err;
            let llm = SheetsLlm {
                cli_down: self.cli_down,
            };
            let d = FlowDeps {
                settings: &self.settings,
                entries: &[],
                detector: &RuleBasedDetector,
                llm: &llm,
            };
            let w = log.clone();
            let released = self.released;
            let wait = move || {
                push(&w, "wait");
                released
            };
            let view = SpyView {
                log: log.clone(),
                table: Mutex::new(None),
            };
            let badge = FakeBadge(log.clone());
            let notifier = FakeNotifier(log.clone());
            match g {
                G::AltAlt => alt_alt_flow(&d, &s, &ti, &|| None, &badge, &notifier),
                G::CorrectionReplace => {
                    correction_replace_flow(&d, &s, &ti, &wait, &badge, &notifier)
                }
                G::CtrlCtrl => ctrl_ctrl_flow(&d, &s, &ti, &view),
                G::CorrectionView => correction_view_flow(&d, &s, &ti, &wait, &view),
            }
            Run {
                log: snapshot(&log),
                written: range
                    .written_grid()
                    .map(|g| g.cells.iter().map(|c| c.display_text.clone()).collect()),
                table: view.table.lock().unwrap().take(),
            }
        }
    }

    fn notify(m: &str) -> String {
        format!("notify:{m}")
    }

    #[test]
    fn alt_alt_range_success_is_silent_and_badge_wraps_work() {
        let r = Case::range(&[&["안녕", "Hello"]]).run(G::AltAlt);
        assert_eq!(
            r.log,
            [
                "route",
                "show:Translating...",
                "sheets.capture",
                "sheets.write",
                "hide"
            ]
        );
        assert_eq!(r.written, Some(vec!["EN".to_string(), "KO".to_string()]));
    }

    #[test]
    fn alt_alt_range_disabled_notifies_without_capture() {
        let mut case = Case::range(&[&["안녕"]]);
        case.config.range_replace_enabled = false;
        let r = case.run(G::AltAlt);
        assert_eq!(
            r.log,
            [
                "route".to_string(),
                notify(MSG_SHEETS_RANGE_REPLACE_DISABLED)
            ]
        );
        let r = case.run(G::CorrectionReplace);
        assert_eq!(
            r.log,
            [
                "wait".to_string(),
                "route".to_string(),
                notify(MSG_SHEETS_CORRECTION_RANGE_REPLACE_DISABLED)
            ]
        );
        // The view flows ignore the replace switch.
        let r = case.run(G::CtrlCtrl);
        assert_eq!(r.log.last().map(String::as_str), Some("table:1x1"));
    }

    #[test]
    fn alt_alt_unsupported_state_notifies_wrong_input_state() {
        let case = Case::base(Some(SHEETS_URL), Some(SheetsInputState::Other));
        let r = case.run(G::AltAlt);
        assert_eq!(r.log, ["route".to_string(), notify(&wrong_input_state())]);
        let r = case.run(G::CorrectionReplace);
        assert_eq!(
            r.log,
            [
                "wait".to_string(),
                "route".to_string(),
                notify(&wrong_input_state())
            ]
        );
        let r = case.run(G::CtrlCtrl);
        assert_eq!(
            r.log,
            [
                "route".to_string(),
                format!("error:{}", wrong_input_state())
            ]
        );
        let r = case.run(G::CorrectionView);
        assert_eq!(
            r.log,
            [
                "wait".to_string(),
                "route".to_string(),
                format!("error:{}", wrong_input_state())
            ]
        );
    }

    #[test]
    fn range_cell_too_large_notifies_and_never_writes() {
        let mut case = Case::range(&[&["안녕하세요"]]);
        case.settings.target_languages = vec![Lang::En, Lang::Vi, Lang::Ja];
        case.settings.include_original_on_replace = true;
        case.config.max_cell_chars = 10;
        let r = case.run(G::AltAlt);
        assert!(!r.log.contains(&"sheets.write".to_string()), "{:?}", r.log);
        assert_eq!(r.written, None);
        assert_eq!(
            r.log[r.log.len() - 2..],
            [
                "hide".to_string(),
                notify("셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요.")
            ]
        );
    }

    #[test]
    fn alt_alt_range_include_original_writes_original_first() {
        let mut case = Case::range(&[&["안녕"]]);
        case.settings.target_languages = vec![Lang::En, Lang::Ja];
        case.settings.include_original_on_replace = true;
        let r = case.run(G::AltAlt);
        assert_eq!(
            r.log.last().map(String::as_str),
            Some("hide"),
            "{:?}",
            r.log
        );
        assert_eq!(r.written, Some(vec!["안녕\n\nEN\n\nJA".to_string()]));
    }

    #[test]
    fn ctrl_ctrl_range_shows_loading_then_table() {
        let r = Case::range(&[&["안녕", "Hello"]]).run(G::CtrlCtrl);
        assert_eq!(
            r.log,
            [
                "route",
                "sheets.capture",
                "loading:Translating...",
                "table:1x2"
            ]
        );
        let table = r.table.unwrap();
        assert_eq!(table.cells[0].results, [(Some(Lang::En), "EN".to_string())]);
        assert_eq!(table.cells[1].results, [(Some(Lang::Ko), "KO".to_string())]);
    }

    #[test]
    fn ctrl_ctrl_range_without_text_cells_shows_no_text() {
        let r = Case::range(&[&[""]]).run(G::CtrlCtrl);
        assert_eq!(
            r.log.last().map(String::as_str),
            Some("error:번역할 텍스트가 없습니다.")
        );
    }

    #[test]
    fn ctrl_ctrl_range_capture_failure_shows_error_without_loading() {
        let mut case = Case::range(&[&["안녕"]]);
        case.capture = Err(CaptureError { reason: None });
        let r = case.run(G::CtrlCtrl);
        assert_eq!(
            r.log,
            [
                "route",
                "sheets.capture",
                "error:선택한 셀을 읽을 수 없습니다."
            ]
        );
    }

    #[test]
    fn correction_range_runs_after_base_check_and_modifier_wait() {
        let mut case = Case::range(&[&["안녕 하세요"]]);
        case.settings.base_language = Lang::En;
        let r = case.run(G::CorrectionReplace);
        assert_eq!(
            r.log,
            [notify(
                ct_core::CorrectionStatus::BaseNotKorean.user_message()
            )]
        );

        let mut case = Case::range(&[&["안녕 하세요"]]);
        case.released = false;
        let r = case.run(G::CorrectionReplace);
        assert_eq!(
            r.log,
            [
                "wait".to_string(),
                notify(MSG_CORRECTION_REPLACE_NO_SELECTION)
            ]
        );

        let r = Case::range(&[&["안녕 하세요"]]).run(G::CorrectionReplace);
        assert_eq!(
            r.log,
            [
                "wait",
                "route",
                "show:한국어 교정 중...",
                "sheets.capture",
                "sheets.write",
                "hide"
            ]
        );
        assert_eq!(r.written, Some(vec!["안녕하세요".to_string()]));
    }

    #[test]
    fn correction_range_nothing_korean_notifies_no_korean_text() {
        let r = Case::range(&[&["Hello"]]).run(G::CorrectionReplace);
        assert_eq!(r.written, None);
        assert_eq!(
            r.log.last(),
            Some(&notify("선택한 셀에 한국어 텍스트가 없습니다."))
        );
    }

    fn cli_down_case(rows: &[&[&str]]) -> Case {
        Case {
            cli_down: true,
            ..Case::range(rows)
        }
    }

    #[test]
    fn range_translate_cli_start_failure_shows_cli_notice() {
        let r = cli_down_case(&[&["안녕"]]).run(G::AltAlt);
        assert_eq!(r.written, None);
        assert_eq!(
            r.log[r.log.len() - 2..],
            ["hide".to_string(), notify(MSG_CLI_UNAVAILABLE)]
        );
    }

    #[test]
    fn range_correct_cli_start_failure_shows_cli_notice() {
        let r = cli_down_case(&[&["안녕 하세요"]]).run(G::CorrectionReplace);
        assert_eq!(r.written, None);
        assert_eq!(
            r.log[r.log.len() - 2..],
            ["hide".to_string(), notify(MSG_CLI_UNAVAILABLE)]
        );
    }

    #[test]
    fn table_translate_cli_start_failure_shows_cli_notice() {
        let r = cli_down_case(&[&["안녕"]]).run(G::CtrlCtrl);
        assert_eq!(r.table, None);
        assert_eq!(r.log.last(), Some(&format!("error:{MSG_CLI_UNAVAILABLE}")));
    }

    #[test]
    fn table_correct_cli_start_failure_shows_cli_notice() {
        let r = cli_down_case(&[&["안녕 하세요"]]).run(G::CorrectionView);
        assert_eq!(r.table, None);
        assert_eq!(r.log.last(), Some(&format!("error:{MSG_CLI_UNAVAILABLE}")));
    }

    /// No usable direction: the only target is the base language (like a cell whose
    /// languages leave nothing to translate into).
    fn direction_unknown_case(rows: &[&[&str]]) -> Case {
        let mut case = Case::range(rows);
        case.settings.target_languages = vec![Lang::Ko];
        case
    }

    #[test]
    fn range_translate_direction_unknown_shows_direction_message() {
        let r = direction_unknown_case(&[&["안녕"]]).run(G::AltAlt);
        assert_eq!(r.written, None);
        assert_eq!(
            r.log[r.log.len() - 2..],
            ["hide".to_string(), notify(MSG_DIRECTION_UNKNOWN)]
        );
    }

    #[test]
    fn table_translate_direction_unknown_shows_direction_message() {
        let r = direction_unknown_case(&[&["안녕"]]).run(G::CtrlCtrl);
        assert_eq!(r.table, None);
        assert_eq!(
            r.log.last(),
            Some(&format!("error:{MSG_DIRECTION_UNKNOWN}"))
        );
    }

    #[test]
    fn correction_view_range_without_korean_shows_no_korean_text() {
        let r = Case::range(&[&["Hello"]]).run(G::CorrectionView);
        assert_eq!(r.table, None);
        assert_eq!(
            r.log.last().map(String::as_str),
            Some("error:선택한 셀에 한국어 텍스트가 없습니다.")
        );
    }

    #[test]
    fn correction_view_range_shows_table_without_tags() {
        let r = Case::range(&[&["안녕 하세요", "Hello"]]).run(G::CorrectionView);
        assert_eq!(
            r.log,
            [
                "wait",
                "route",
                "sheets.capture",
                "loading:한국어 교정 중...",
                "table:1x2"
            ]
        );
        let table = r.table.unwrap();
        assert_eq!(table.cells[0].results, [(None, "안녕하세요".to_string())]);
        assert!(table.cells[1].results.is_empty());
    }

    #[test]
    fn sheets_editing_capture_failure_uses_sheets_message() {
        let case = Case::editing(None);
        let r = case.run(G::AltAlt);
        assert_eq!(
            r.log,
            [
                "route".to_string(),
                "show:Translating...".to_string(),
                "capture".to_string(),
                "hide".to_string(),
                notify(MSG_SHEETS_EDIT_TRANSLATE_FAILED)
            ]
        );
        let r = case.run(G::CorrectionReplace);
        assert_eq!(r.log.last(), Some(&notify(MSG_SHEETS_EDIT_CORRECT_FAILED)));
    }

    #[test]
    fn sheets_editing_formula_prefix_uses_sheets_message() {
        let mut case = Case::editing(Some("안녕 하세요"));
        case.replace_err = Some(|| ReplaceError::FormulaPrefix);
        let r = case.run(G::AltAlt);
        assert_eq!(
            r.log.last(),
            Some(&notify(MSG_SHEETS_EDIT_TRANSLATE_FORMULA_PREFIX))
        );
        let r = case.run(G::CorrectionReplace);
        assert_eq!(
            r.log.last(),
            Some(&notify(MSG_SHEETS_EDIT_CORRECT_FORMULA_PREFIX))
        );
    }

    #[test]
    fn sheets_edit_replace_errors_map_to_ui_ref_strings() {
        use Job::{Correct, Translate};
        type ErrorCase = (Job, fn() -> ReplaceError, &'static str);
        let cases: [ErrorCase; 6] = [
            (
                Translate,
                || ReplaceError::FormulaPrefix,
                "번역 결과가 =, +로 시작해 수식으로 해석될 수 있어 교체하지 않았습니다.",
            ),
            (
                Translate,
                || ReplaceError::ContextChanged,
                "번역 중 편집 중인 셀의 내용이나 선택이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.",
            ),
            (
                Translate,
                || ReplaceError::FullCellNotEnabled,
                "셀 전체 교체는 아직 검증되지 않아 꺼져 있습니다. 교체할 글자를 선택한 뒤 Alt+Alt를 눌러주세요.",
            ),
            (
                Correct,
                || ReplaceError::FormulaPrefix,
                "교정 결과가 =, +로 시작해 수식으로 해석될 수 있어 교체하지 않았습니다.",
            ),
            (
                Correct,
                || ReplaceError::ContextChanged,
                "교정 중 편집 중인 셀의 내용이나 선택이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.",
            ),
            (
                Correct,
                || ReplaceError::FullCellNotEnabled,
                "셀 전체 교체는 아직 검증되지 않아 꺼져 있습니다. 교체할 글자를 선택한 뒤 Ctrl+Alt+Alt를 눌러주세요.",
            ),
        ];
        for (job, err, message) in cases {
            assert_eq!(sheets_edit_replace_message(job, &err()), Some(message));
        }
        assert_eq!(
            sheets_edit_replace_message(Translate, &ReplaceError::NothingFocused),
            None
        );
        // On Sheets, other replace errors keep the generic message.
        let mut case = Case::editing(Some("안녕"));
        case.replace_err = Some(|| ReplaceError::NothingFocused);
        let r = case.run(G::AltAlt);
        assert_eq!(r.log.last(), Some(&notify(MSG_REPLACE_FAILED)));
    }

    #[test]
    fn ui_ref_constants_are_exact() {
        assert_eq!(
            MSG_SHEETS_EDIT_TRANSLATE_FAILED,
            "편집 중인 셀을 번역할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음)."
        );
        assert_eq!(
            MSG_SHEETS_EDIT_CORRECT_FAILED,
            "편집 중인 셀을 교정할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음)."
        );
        assert_eq!(
            MSG_SHEETS_RANGE_REPLACE_DISABLED,
            "Google Sheets 범위 교체(Alt+Alt)는 실제 Chrome에서 아직 검증되지 않아 기본으로 꺼져 있습니다. Ctrl+Ctrl로 번역을 확인하거나 셀 안의 글자를 선택한 뒤 Alt+Alt를 사용해주세요."
        );
        assert_eq!(
            MSG_SHEETS_CORRECTION_RANGE_REPLACE_DISABLED,
            "Google Sheets 범위 교체(Ctrl+Alt+Alt)는 실제 Chrome에서 아직 검증되지 않아 기본으로 꺼져 있습니다. Alt+Ctrl+Ctrl로 교정 결과를 확인하거나 셀 안의 글자를 선택한 뒤 Ctrl+Alt+Alt를 사용해주세요."
        );
    }

    #[test]
    fn non_sheets_page_keeps_existing_behaviour() {
        // Capture failure outside Sheets (and outside Docs/Slides) stays silent.
        let r = Case::base(Some(GMAIL_URL), None).run(G::AltAlt);
        assert_eq!(r.log, ["route", "show:Translating...", "capture", "hide"]);
        // Correction capture failure keeps the generic "select text" message.
        let r = Case::base(Some(GMAIL_URL), None).run(G::CorrectionReplace);
        assert_eq!(
            r.log.last(),
            Some(&notify(MSG_CORRECTION_REPLACE_NO_SELECTION))
        );
        // A Sheets-only replace error outside Sheets keeps the generic message.
        let mut case = Case::base(None, None);
        case.text = Some("안녕");
        case.replace_err = Some(|| ReplaceError::FormulaPrefix);
        let r = case.run(G::AltAlt);
        assert_eq!(r.log.last(), Some(&notify(MSG_REPLACE_FAILED)));
        // The range port is never touched off Sheets.
        assert!(!r.log.iter().any(|l| l.starts_with("sheets.")));
    }
}
