//! Range jobs: safe replace sequence and table-view helpers (pure logic over a port).
//! Logs carry statuses, counts and durations only, never cell text.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ct_core::{BatchFailure, CellText, Lang};

use crate::cf_html::build_cf_html;
use crate::model::{CellKey, SheetSelection};
use crate::outcome::{
    OutcomeReason, SheetsOutcome, SheetsStatus, SheetsTable, TableCell, UnsupportedReason,
};
use crate::payload::build_replacement_html;
use crate::support::{SheetsMode, check_range_support, translatable_cells};
use crate::verify::{same_target, written_as_expected};

/// A capture that failed; `reason` is set when the cause is an unsupported selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureError {
    pub reason: Option<UnsupportedReason>,
}

/// OS access needed by the range jobs (implemented by ct-win, faked in tests).
pub trait SheetsInteraction: Send + Sync {
    fn capture_selection(&self) -> Result<SheetSelection, CaptureError>;
    fn reread_selection(&self) -> Result<SheetSelection, CaptureError> {
        self.capture_selection()
    }
    fn can_edit(&self) -> bool;
    fn write_selection(&self, selection: &SheetSelection, cf_html: &str) -> Result<(), String>;
    /// The Name Box address read without sending keys; `None` = unknown.
    fn current_address(&self) -> Option<String> {
        None
    }
    /// Sends Esc to clear Sheets' copy marquee (grid selection mode only).
    fn clear_copy_marquee(&self) {}
    /// Pause between verify attempts (injectable so tests don't sleep).
    fn wait(&self, d: Duration) {
        std::thread::sleep(d)
    }
}

/// Delays before each post-write verify attempt: Sheets can take seconds to apply a paste.
pub const VERIFY_DELAYS: [Duration; 4] = [
    Duration::from_millis(300),
    Duration::from_millis(700),
    Duration::from_millis(1000),
    Duration::from_millis(1500),
];

/// Result columns of one cell: `(language, text)` per requested column.
pub type TableResults = Vec<(Option<Lang>, String)>;

pub type Transform<'a> =
    dyn FnMut(&[CellText]) -> Result<Vec<(CellKey, String)>, SheetsOutcome> + 'a;

fn outcome(status: SheetsStatus) -> SheetsOutcome {
    SheetsOutcome {
        status,
        reason: None,
    }
}

fn unsupported(reason: UnsupportedReason) -> SheetsOutcome {
    SheetsOutcome {
        status: SheetsStatus::UnsupportedRange,
        reason: Some(OutcomeReason::Unsupported(reason)),
    }
}

fn capture_outcome(e: CaptureError) -> SheetsOutcome {
    match e.reason {
        Some(reason) => unsupported(reason),
        None => outcome(SheetsStatus::CaptureFailed),
    }
}

/// Replace the translatable cells of the current selection, following the
/// 10-step safety sequence. Nothing is written unless every check passes, and a
/// write is never retried. `skip_unchanged` drops results equal to the original
/// text and yields `NoChanges` when nothing remains (correction).
pub fn replace_cells(
    ia: &dyn SheetsInteraction,
    max_range_cells: usize,
    transform: &mut Transform,
    no_text: SheetsStatus,
    failure: SheetsStatus,
    skip_unchanged: bool,
) -> SheetsOutcome {
    let started = Instant::now();
    let result = replace_cells_inner(
        ia,
        max_range_cells,
        transform,
        no_text,
        failure,
        skip_unchanged,
    );
    tracing::info!(
        status = ?result.status,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "sheets replace finished"
    );
    result
}

fn replace_cells_inner(
    ia: &dyn SheetsInteraction,
    max_range_cells: usize,
    transform: &mut Transform,
    no_text: SheetsStatus,
    failure: SheetsStatus,
    skip_unchanged: bool,
) -> SheetsOutcome {
    // 1. capture
    let selection = match ia.capture_selection() {
        Ok(s) => s,
        Err(e) => return capture_outcome(e),
    };
    // 2. support
    if let Some(reason) = check_range_support(&selection, SheetsMode::Replace, max_range_cells) {
        return unsupported(reason);
    }
    // 3. editable
    if !ia.can_edit() {
        return unsupported(UnsupportedReason::UnidentifiedTarget);
    }
    // 4. text cells
    let cells = translatable_cells(&selection.grid);
    if cells.is_empty() {
        return outcome(no_text);
    }
    let texts: Vec<CellText> = cells
        .into_iter()
        .map(|(key, text)| CellText { key, text })
        .collect();
    // 5. transform (already an outcome on failure)
    let mut translations = match transform(&texts) {
        Ok(t) => t,
        Err(o) => return o,
    };
    if skip_unchanged {
        let originals: HashMap<CellKey, &str> =
            texts.iter().map(|c| (c.key, c.text.as_str())).collect();
        translations.retain(|(key, text)| originals.get(key).copied() != Some(text.as_str()));
    }
    if translations.is_empty() {
        return outcome(SheetsStatus::NoChanges);
    }
    // 6. target unchanged
    match ia.reread_selection() {
        Ok(current) if same_target(&selection, &current) => {}
        _ => return outcome(SheetsStatus::TargetChanged),
    }
    // 7. payload
    let Ok(fragment) = build_replacement_html(&selection.grid, &translations) else {
        return outcome(failure);
    };
    // 8. write (never retried)
    if ia
        .write_selection(&selection, &build_cf_html(&fragment))
        .is_err()
    {
        return outcome(SheetsStatus::WriteFailed);
    }
    // 9. verify (retried with delays; the write itself is never repeated)
    verify_written(ia, &selection, &translations)
}

/// Step 9-10: reread until the write shows up. If the user has moved to another
/// cell, stop without a reread (a Ctrl+C there would copy their cell) and report
/// success: the single paste was delivered, the left range cannot be verified.
fn verify_written(
    ia: &dyn SheetsInteraction,
    selection: &SheetSelection,
    translations: &[(CellKey, String)],
) -> SheetsOutcome {
    let target = selection.location.range_address.as_deref();
    let moved = || {
        tracing::info!("verification skipped: selection moved");
        outcome(SheetsStatus::Success)
    };
    for (attempt, delay) in VERIFY_DELAYS.iter().enumerate() {
        ia.wait(*delay);
        if let Some(address) = ia.current_address()
            && Some(address.as_str()) != target
        {
            return moved();
        }
        match ia.reread_selection() {
            Ok(after) if after.location.range_address.as_deref() != target => return moved(),
            Ok(after) if written_as_expected(&selection.grid, translations, &after.grid) => {
                ia.clear_copy_marquee();
                tracing::info!(
                    cells = selection.grid.cells.len(),
                    translated = translations.len(),
                    attempt = attempt + 1,
                    "sheets replace verified"
                );
                return outcome(SheetsStatus::Success);
            }
            _ => {}
        }
    }
    outcome(SheetsStatus::WriteUnverified)
}

/// Capture for the table view: no `can_edit` check; links and rich text allowed.
pub fn capture_for_table(
    ia: &dyn SheetsInteraction,
    max_range_cells: usize,
) -> Result<SheetSelection, SheetsOutcome> {
    let selection = ia.capture_selection().map_err(capture_outcome)?;
    match check_range_support(&selection, SheetsMode::Explain, max_range_cells) {
        Some(reason) => Err(unsupported(reason)),
        None => Ok(selection),
    }
}

/// One `TableCell` for every grid cell in `(row, col)` order; cells without a
/// result get an empty `results`.
pub fn table_from(selection: &SheetSelection, results: &[(CellKey, TableResults)]) -> SheetsTable {
    let by_key: HashMap<CellKey, &TableResults> = results.iter().map(|(k, v)| (*k, v)).collect();
    let mut cells: Vec<TableCell> = selection
        .grid
        .cells
        .iter()
        .map(|c| TableCell {
            row: c.row,
            col: c.col,
            original: c.display_text.clone(),
            results: by_key
                .get(&(c.row, c.col))
                .map(|v| (*v).clone())
                .unwrap_or_default(),
        })
        .collect();
    cells.sort_by_key(|c| (c.row, c.col));
    SheetsTable {
        location: selection.location.clone(),
        rows: selection.grid.rows,
        cols: selection.grid.cols,
        cells,
    }
}

fn failure_reason(f: &BatchFailure) -> Option<OutcomeReason> {
    matches!(f, BatchFailure::CellTooLarge).then_some(OutcomeReason::CellTooLarge)
}

pub fn translation_failure(f: &BatchFailure) -> SheetsOutcome {
    SheetsOutcome {
        status: SheetsStatus::TranslationFailed,
        reason: failure_reason(f),
    }
}

pub fn correction_failure(f: &BatchFailure) -> SheetsOutcome {
    if matches!(f, BatchFailure::NoKoreanText) {
        return outcome(SheetsStatus::NoKoreanText);
    }
    SheetsOutcome {
        status: SheetsStatus::CorrectionFailed,
        reason: failure_reason(f),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use ct_core::{BatchFailure, CellKey, CellText};

    use super::*;
    use crate::cf_html::extract_cf_html_fragment;
    use crate::html::parse_sheets_html;
    use crate::model::{SheetLocation, SheetSelection};
    use crate::outcome::{OutcomeReason, SheetsOutcome, SheetsStatus, UnsupportedReason};
    use crate::payload::build_replacement_html;

    const FIXTURE: &str = include_str!("../tests/fixtures/mixed_2x3.html");

    fn selection(address: &str) -> SheetSelection {
        SheetSelection {
            location: SheetLocation {
                document_id: "doc".into(),
                gid: Some("0".into()),
                range_address: Some(address.into()),
            },
            grid: parse_sheets_html(FIXTURE, None).unwrap(),
            hidden_or_filtered: Some(false),
        }
    }

    fn after_write(sel: &SheetSelection, rep: &[(CellKey, String)]) -> SheetSelection {
        let html = build_replacement_html(&sel.grid, rep).unwrap();
        SheetSelection {
            grid: parse_sheets_html(&html, None).unwrap(),
            ..sel.clone()
        }
    }

    type Capture = Result<SheetSelection, CaptureError>;

    struct Fake {
        capture: Capture,
        rereads: Mutex<VecDeque<Capture>>,
        can_edit: bool,
        write_error: Option<String>,
        writes: Mutex<Vec<String>>,
        reread_calls: Mutex<usize>,
        /// Name Box addresses returned by `current_address`, in order (None when empty).
        addresses: Mutex<VecDeque<Option<String>>>,
        waits: Mutex<Vec<Duration>>,
        marquee_clears: Mutex<usize>,
    }

    impl Fake {
        fn new(capture: Capture, rereads: Vec<Capture>) -> Self {
            Self {
                capture,
                rereads: Mutex::new(rereads.into()),
                can_edit: true,
                write_error: None,
                writes: Mutex::new(Vec::new()),
                reread_calls: Mutex::new(0),
                addresses: Mutex::new(VecDeque::new()),
                waits: Mutex::new(Vec::new()),
                marquee_clears: Mutex::new(0),
            }
        }
        fn with_addresses(self, addresses: Vec<Option<&str>>) -> Self {
            *self.addresses.lock().unwrap() =
                addresses.into_iter().map(|a| a.map(String::from)).collect();
            self
        }
        fn marquee_clears(&self) -> usize {
            *self.marquee_clears.lock().unwrap()
        }
        fn waits(&self) -> Vec<Duration> {
            self.waits.lock().unwrap().clone()
        }
        fn writes(&self) -> usize {
            self.writes.lock().unwrap().len()
        }
        fn rereads_done(&self) -> usize {
            *self.reread_calls.lock().unwrap()
        }
    }

    impl SheetsInteraction for Fake {
        fn capture_selection(&self) -> Result<SheetSelection, CaptureError> {
            self.capture.clone()
        }
        fn reread_selection(&self) -> Result<SheetSelection, CaptureError> {
            *self.reread_calls.lock().unwrap() += 1;
            self.rereads
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(CaptureError { reason: None }))
        }
        fn can_edit(&self) -> bool {
            self.can_edit
        }
        fn write_selection(&self, _s: &SheetSelection, cf_html: &str) -> Result<(), String> {
            if let Some(e) = &self.write_error {
                return Err(e.clone());
            }
            self.writes.lock().unwrap().push(cf_html.to_string());
            Ok(())
        }
        fn current_address(&self) -> Option<String> {
            self.addresses.lock().unwrap().pop_front().flatten()
        }
        fn clear_copy_marquee(&self) {
            *self.marquee_clears.lock().unwrap() += 1;
        }
        fn wait(&self, d: Duration) {
            self.waits.lock().unwrap().push(d);
        }
    }

    fn expected() -> Vec<(CellKey, String)> {
        vec![
            ((0, 0), "[en] 안녕하세요".into()),
            ((1, 2), "[en] 합계".into()),
        ]
    }

    fn ok_transform(cells: &[CellText]) -> Result<Vec<(CellKey, String)>, SheetsOutcome> {
        Ok(cells
            .iter()
            .map(|c| (c.key, format!("[en] {}", c.text)))
            .collect())
    }

    fn run(fake: &Fake, skip: bool, t: &mut Transform) -> SheetsOutcome {
        replace_cells(
            fake,
            1000,
            t,
            SheetsStatus::NoText,
            SheetsStatus::TranslationFailed,
            skip,
        )
    }

    fn correct(fake: &Fake, t: &mut Transform) -> SheetsOutcome {
        replace_cells(
            fake,
            1000,
            t,
            SheetsStatus::NoKoreanText,
            SheetsStatus::CorrectionFailed,
            true,
        )
    }

    #[test]
    fn replace_writes_once_and_succeeds() {
        let sel = selection("A1:C2");
        let fake = Fake::new(
            Ok(sel.clone()),
            vec![Ok(sel.clone()), Ok(after_write(&sel, &expected()))],
        );
        let mut seen = Vec::new();
        let mut t = |c: &[CellText]| {
            seen = c.to_vec();
            ok_transform(c)
        };
        let out = run(&fake, false, &mut t);
        assert_eq!(out.status, SheetsStatus::Success);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].key, (0, 0));
        assert_eq!(fake.writes(), 1);
        assert_eq!(fake.rereads_done(), 2);
        assert_eq!(fake.marquee_clears(), 1);
        let frag = extract_cf_html_fragment(&fake.writes.lock().unwrap()[0]).unwrap();
        assert!(frag.contains("[en] 안녕하세요") && frag.contains("[en] 합계"));
    }

    #[test]
    fn capture_reason_maps_to_unsupported_range() {
        let mut t = ok_transform;
        let f = Fake::new(
            Err(CaptureError {
                reason: Some(UnsupportedReason::MultipleRanges),
            }),
            vec![],
        );
        let out = run(&f, false, &mut t);
        assert_eq!(out.status, SheetsStatus::UnsupportedRange);
        assert_eq!(
            out.reason,
            Some(OutcomeReason::Unsupported(
                UnsupportedReason::MultipleRanges
            ))
        );
    }

    #[test]
    fn parse_error_is_capture_failed() {
        let mut t = ok_transform;
        let f = Fake::new(Err(CaptureError { reason: None }), vec![]);
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::CaptureFailed);
    }

    #[test]
    fn unsupported_range_and_cannot_edit() {
        let mut t = ok_transform;
        let mut sel = selection("A1:C2");
        sel.hidden_or_filtered = Some(true);
        let f = Fake::new(Ok(sel), vec![]);
        let out = run(&f, false, &mut t);
        assert_eq!(out.status, SheetsStatus::UnsupportedRange);
        assert_eq!(
            out.reason,
            Some(OutcomeReason::Unsupported(
                UnsupportedReason::HiddenOrFiltered
            ))
        );

        let mut f = Fake::new(Ok(selection("A1:C2")), vec![]);
        f.can_edit = false;
        let out = run(&f, false, &mut t);
        assert_eq!(out.status, SheetsStatus::UnsupportedRange);
        assert_eq!(
            out.reason,
            Some(OutcomeReason::Unsupported(
                UnsupportedReason::UnidentifiedTarget
            ))
        );
    }

    #[test]
    fn no_text_skips_transform() {
        let mut sel = selection("A1:C2");
        for c in &mut sel.grid.cells {
            c.display_text = " ".into();
        }
        let f = Fake::new(Ok(sel), vec![]);
        let mut called = false;
        let mut t = |_: &[CellText]| {
            called = true;
            Ok(vec![])
        };
        let out = run(&f, false, &mut t);
        assert_eq!(out.status, SheetsStatus::NoText);
        assert!(!called);
    }

    #[test]
    fn transform_error_maps_to_failure_status() {
        // D6: transform errors are already outcomes and are returned as they are.
        let f = Fake::new(Ok(selection("A1:C2")), vec![]);
        let mut t = |_: &[CellText]| Err(translation_failure(&BatchFailure::CellTooLarge));
        let out = run(&f, false, &mut t);
        assert_eq!(out.status, SheetsStatus::TranslationFailed);
        assert_eq!(out.reason, Some(OutcomeReason::CellTooLarge));
        assert_eq!(f.writes(), 0);
        assert_eq!(f.rereads_done(), 0);
    }

    #[test]
    fn reread_differs_is_target_changed_and_never_writes() {
        let mut t = ok_transform;
        let f = Fake::new(Ok(selection("A1:C2")), vec![Ok(selection("A1:C3"))]);
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::TargetChanged);
        assert_eq!(f.writes(), 0);

        let f = Fake::new(
            Ok(selection("A1:C2")),
            vec![Err(CaptureError { reason: None })],
        );
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::TargetChanged);
        assert_eq!(f.writes(), 0);
    }

    #[test]
    fn payload_error_reports_failure_status_not_write_failed() {
        let sel = selection("A1:C2");
        let f = Fake::new(Ok(sel.clone()), vec![Ok(sel)]);
        // (0,2) is a number cell: the payload builder refuses it.
        let mut t = |_: &[CellText]| Ok(vec![((0, 2), "x".to_string())]);
        let out = run(&f, false, &mut t);
        assert_eq!(out.status, SheetsStatus::TranslationFailed);
        assert_eq!(f.writes(), 0);
    }

    #[test]
    fn write_error_is_write_failed_without_retry() {
        let sel = selection("A1:C2");
        let mut f = Fake::new(Ok(sel.clone()), vec![Ok(sel)]);
        f.write_error = Some("paste blocked".into());
        let mut t = ok_transform;
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::WriteFailed);
        assert_eq!(f.rereads_done(), 1);
    }

    #[test]
    fn verify_mismatch_is_write_unverified() {
        let sel = selection("A1:C2");
        // Sheet ignored the paste: every after-read equals the original.
        let f = Fake::new(
            Ok(sel.clone()),
            vec![
                Ok(sel.clone()),
                Ok(sel.clone()),
                Ok(sel.clone()),
                Ok(sel.clone()),
                Ok(sel.clone()),
            ],
        );
        let mut t = ok_transform;
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::WriteUnverified);
        assert_eq!(f.writes(), 1);
        // 1 target check + 4 verify attempts, one wait before each attempt.
        assert_eq!(f.rereads_done(), 5);
        assert_eq!(f.waits(), VERIFY_DELAYS.to_vec());
        assert_eq!(f.marquee_clears(), 0);

        let f = Fake::new(
            Ok(sel.clone()),
            vec![Ok(sel), Err(CaptureError { reason: None })],
        );
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::WriteUnverified);
        assert_eq!(f.writes(), 1);
        assert_eq!(f.rereads_done(), 5);
    }

    #[test]
    fn verify_moved_before_first_reread_succeeds_without_reread() {
        let sel = selection("A1:C2");
        let f = Fake::new(Ok(sel.clone()), vec![Ok(sel.clone())]).with_addresses(vec![Some("D5")]);
        let mut t = ok_transform;
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::Success);
        assert_eq!(f.writes(), 1);
        // Only the pre-write target check; no Ctrl+C on the moved-to cell.
        assert_eq!(f.rereads_done(), 1);
        assert_eq!(f.marquee_clears(), 0);
        assert_eq!(f.waits(), vec![VERIFY_DELAYS[0]]);
    }

    #[test]
    fn verify_mismatch_then_match_succeeds_and_clears_marquee_once() {
        let sel = selection("A1:C2");
        let f = Fake::new(
            Ok(sel.clone()),
            vec![
                Ok(sel.clone()),
                Ok(sel.clone()),
                Ok(after_write(&sel, &expected())),
            ],
        )
        .with_addresses(vec![Some("A1:C2"), None]);
        let mut t = ok_transform;
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::Success);
        assert_eq!(f.writes(), 1);
        assert_eq!(f.rereads_done(), 3);
        assert_eq!(f.marquee_clears(), 1);
        assert_eq!(f.waits(), VERIFY_DELAYS[..2].to_vec());
    }

    #[test]
    fn verify_reread_at_other_address_succeeds_without_marquee_clear() {
        let sel = selection("A1:C2");
        let f = Fake::new(Ok(sel.clone()), vec![Ok(sel.clone()), Ok(selection("D5"))]);
        let mut t = ok_transform;
        assert_eq!(run(&f, false, &mut t).status, SheetsStatus::Success);
        assert_eq!(f.writes(), 1);
        assert_eq!(f.rereads_done(), 2);
        assert_eq!(f.marquee_clears(), 0);
    }

    #[test]
    fn correction_nothing_changed_is_no_changes_and_never_writes() {
        let f = Fake::new(Ok(selection("A1:C2")), vec![]);
        let mut t = |_: &[CellText]| Ok(vec![]);
        assert_eq!(correct(&f, &mut t).status, SheetsStatus::NoChanges);
        assert_eq!(f.writes(), 0);

        // Results identical to the originals are dropped too.
        let mut t = |c: &[CellText]| Ok(c.iter().map(|x| (x.key, x.text.clone())).collect());
        assert_eq!(correct(&f, &mut t).status, SheetsStatus::NoChanges);
        assert_eq!(f.writes(), 0);
        assert_eq!(f.rereads_done(), 0);
    }

    #[test]
    fn skip_unchanged_writes_only_changed_cells() {
        let sel = selection("A1:C2");
        let rep = vec![((1, 2), "합계!".to_string())];
        let f = Fake::new(
            Ok(sel.clone()),
            vec![Ok(sel.clone()), Ok(after_write(&sel, &rep))],
        );
        let mut t = |_: &[CellText]| {
            Ok(vec![
                ((0, 0), "안녕하세요".to_string()),
                ((1, 2), "합계!".to_string()),
            ])
        };
        assert_eq!(correct(&f, &mut t).status, SheetsStatus::Success);
        let frag = extract_cf_html_fragment(&f.writes.lock().unwrap()[0]).unwrap();
        assert!(frag.contains("합계!"));
    }

    #[test]
    fn failure_mappers() {
        let o = translation_failure(&BatchFailure::Timeout);
        assert_eq!(
            (o.status, o.reason),
            (SheetsStatus::TranslationFailed, None)
        );
        let o = correction_failure(&BatchFailure::NoKoreanText);
        assert_eq!((o.status, o.reason), (SheetsStatus::NoKoreanText, None));
        let o = correction_failure(&BatchFailure::CellTooLarge);
        assert_eq!(
            (o.status, o.reason),
            (
                SheetsStatus::CorrectionFailed,
                Some(OutcomeReason::CellTooLarge)
            )
        );
        let o = correction_failure(&BatchFailure::Llm("x".into()));
        assert_eq!((o.status, o.reason), (SheetsStatus::CorrectionFailed, None));
    }

    #[test]
    fn capture_for_table_skips_can_edit_and_maps_errors() {
        let mut f = Fake::new(Ok(selection("A1:C2")), vec![]);
        f.can_edit = false;
        assert!(capture_for_table(&f, 1000).is_ok());
        let f = Fake::new(Err(CaptureError { reason: None }), vec![]);
        assert_eq!(
            capture_for_table(&f, 1000).unwrap_err().status,
            SheetsStatus::CaptureFailed
        );
        let f = Fake::new(Ok(selection("A1:C2")), vec![]);
        assert_eq!(
            capture_for_table(&f, 2).unwrap_err().status,
            SheetsStatus::UnsupportedRange
        );
    }

    #[test]
    fn table_keeps_every_cell_in_order() {
        let sel = selection("A1:C2");
        let results = vec![((1usize, 2usize), vec![(None, "합계!".to_string())])];
        let t = table_from(&sel, &results);
        assert_eq!((t.rows, t.cols), (2, 3));
        let order: Vec<(usize, usize)> = t.cells.iter().map(|c| (c.row, c.col)).collect();
        assert_eq!(order, vec![(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]);
        assert_eq!(t.cells[2].original, "42");
        assert!(t.cells[0].results.is_empty());
        assert_eq!(t.cells[5].original, "합계");
        assert_eq!(t.cells[5].results, vec![(None, "합계!".to_string())]);
        assert_eq!(t.location, sel.location);
    }
}
