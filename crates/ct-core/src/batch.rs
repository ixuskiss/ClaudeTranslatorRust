//! Multi-target batch translation for spreadsheet ranges: mask every cell with job-unique
//! placeholders, give each cell its own destinations, translate in size-limited batches and
//! verify per cell. All-or-nothing: any failure returns no translations.

use crate::compose::compose_replacement;
use crate::config::SheetsConfig;
use crate::detector::LanguageDetector;
use crate::dictionary::{
    DictionaryEntry, MaskMatch, mask, renumber, restore, restore_fragment, validate_integrity,
    validate_integrity_across,
};
use crate::lang::Lang;
use crate::llm::{
    BatchCorrectionItem, BatchCorrectionRequest, BatchItem, BatchTranslationRequest, LlmProvider,
};
use crate::plan::resolve_plan;
use crate::settings::{CorrectionMode, Tone};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// (row, column), zero-based.
pub type CellKey = (usize, usize);

/// Translations of one cell, in destination order.
pub type LangTexts = Vec<(Lang, String)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellText {
    pub key: CellKey,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchLimits {
    pub max_cells: usize,
    pub max_chars: usize,
    pub max_cell_chars: usize,
    pub job_timeout: Duration,
}

impl From<&SheetsConfig> for BatchLimits {
    fn from(c: &SheetsConfig) -> Self {
        Self {
            max_cells: c.batch_max_cells,
            max_chars: c.batch_max_chars,
            max_cell_chars: c.max_cell_chars,
            job_timeout: c.job_timeout,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchFailure {
    CellTooLarge,
    DirectionUnknown,
    Llm(String),
    Integrity,
    Timeout,
    NoKoreanText,
}

fn plan_batches(items: Vec<(usize, usize)>, limits: &BatchLimits) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut chars = 0usize;
    for (index, size) in items {
        if !current.is_empty()
            && (current.len() >= limits.max_cells || chars.saturating_add(size) > limits.max_chars)
        {
            batches.push(std::mem::take(&mut current));
            chars = 0;
        }
        current.push(index);
        chars = chars.saturating_add(size);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn ids_mismatch() -> BatchFailure {
    BatchFailure::Llm("response ids do not match request".to_string())
}

/// Returns `texts` reordered to `destinations`, or `None` unless both hold exactly the same
/// languages with no duplicates and no blank text.
fn align_languages(destinations: &[Lang], texts: Vec<(Lang, String)>) -> Option<LangTexts> {
    if texts.len() != destinations.len() {
        return None;
    }
    let mut out = Vec::with_capacity(destinations.len());
    for lang in destinations {
        let mut matching = texts.iter().filter(|(l, _)| l == lang);
        let (_, text) = matching.next()?;
        if matching.next().is_some() || text.trim().is_empty() {
            return None;
        }
        out.push((*lang, text.clone()));
    }
    Some(out)
}

/// Runs planned batches in order: the job timeout is checked before each one, every response
/// must carry exactly the requested ids, and `accept` validates one item's payload (`None` is
/// an id mismatch). Returns the accepted payloads in item order.
fn run_planned<T, U>(
    planned: Vec<Vec<usize>>,
    ids: &[&str],
    limits: &BatchLimits,
    elapsed: &dyn Fn() -> Duration,
    batch_count: &mut usize,
    mut send: impl FnMut(&[usize]) -> Result<Vec<(String, T)>, BatchFailure>,
    accept: impl Fn(usize, T) -> Option<U>,
) -> Result<Vec<U>, BatchFailure> {
    let mut raw: Vec<Option<U>> = ids.iter().map(|_| None).collect();
    for batch in planned {
        if elapsed() > limits.job_timeout {
            return Err(BatchFailure::Timeout);
        }
        *batch_count += 1;
        let items = send(&batch)?;
        if items.len() != batch.len() {
            return Err(ids_mismatch());
        }
        let mut by_id: HashMap<String, T> = HashMap::new();
        for (id, payload) in items {
            if by_id.insert(id, payload).is_some() {
                return Err(ids_mismatch());
            }
        }
        for &i in &batch {
            let payload = by_id.remove(ids[i]).ok_or_else(ids_mismatch)?;
            raw[i] = Some(accept(i, payload).ok_or_else(ids_mismatch)?);
        }
    }
    raw.into_iter()
        .map(|r| r.ok_or_else(ids_mismatch))
        .collect()
}

struct Prepared {
    key: CellKey,
    mapping: Vec<MaskMatch>,
    item: BatchItem,
    size: usize,
}

fn prepare_cells(
    cells: &[CellText],
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    base: Lang,
    targets: &[Lang],
    include_original: bool,
    max_cell_chars: usize,
) -> Result<Vec<Prepared>, BatchFailure> {
    // Base and duplicate targets are dropped; nothing left means no safe direction.
    let mut filtered: Vec<Lang> = Vec::with_capacity(targets.len());
    for &l in targets {
        if l != base && !filtered.contains(&l) {
            filtered.push(l);
        }
    }
    let mut prepared = Vec::with_capacity(cells.len());
    let mut offset = 0usize;
    for (index, cell) in cells.iter().enumerate() {
        let masked = renumber(mask(&cell.text, entries), offset);
        offset += masked.mapping.len();
        let detected = detector.detect(&masked.masked_text);
        let plan = resolve_plan(detected, base, &filtered)
            .filter(|p| !p.destinations.is_empty())
            .ok_or(BatchFailure::DirectionUnknown)?;
        let chars = masked.masked_text.chars().count();
        if chars > max_cell_chars {
            return Err(BatchFailure::CellTooLarge);
        }
        let size = chars * (plan.destinations.len() + usize::from(include_original));
        prepared.push(Prepared {
            key: cell.key,
            mapping: masked.mapping,
            item: BatchItem {
                id: format!("c{index}"),
                text: masked.masked_text,
                source: plan.source,
                destinations: plan.destinations,
            },
            size,
        });
    }
    Ok(prepared)
}

#[allow(clippy::too_many_arguments)]
pub fn translate_cells(
    cells: &[CellText],
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    base: Lang,
    targets: &[Lang],
    tone: Tone,
    include_original: bool,
    limits: &BatchLimits,
    elapsed: &dyn Fn() -> Duration,
) -> Result<Vec<(CellKey, LangTexts)>, BatchFailure> {
    if cells.is_empty() {
        return Ok(Vec::new());
    }
    let started = Instant::now();
    let mut batches = 0usize;
    let result = run_translate_cells(
        cells,
        entries,
        detector,
        llm,
        base,
        targets,
        tone,
        include_original,
        limits,
        elapsed,
        &mut batches,
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(
            operation = "batch_translate",
            cells = cells.len(),
            batches,
            elapsed_ms,
            "ok"
        ),
        Err(e) => tracing::warn!(
            operation = "batch_translate",
            cells = cells.len(),
            batches,
            elapsed_ms,
            failure = failure_kind(e),
            "failed"
        ),
    }
    result
}

fn failure_kind(f: &BatchFailure) -> &'static str {
    match f {
        BatchFailure::CellTooLarge => "cell_too_large",
        BatchFailure::DirectionUnknown => "direction_unknown",
        BatchFailure::Llm(_) => "llm",
        BatchFailure::Integrity => "integrity",
        BatchFailure::Timeout => "timeout",
        BatchFailure::NoKoreanText => "no_korean_text",
    }
}

#[allow(clippy::too_many_arguments)]
fn run_translate_cells(
    cells: &[CellText],
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    base: Lang,
    targets: &[Lang],
    tone: Tone,
    include_original: bool,
    limits: &BatchLimits,
    elapsed: &dyn Fn() -> Duration,
    batch_count: &mut usize,
) -> Result<Vec<(CellKey, LangTexts)>, BatchFailure> {
    let prepared = prepare_cells(
        cells,
        entries,
        detector,
        base,
        targets,
        include_original,
        limits.max_cell_chars,
    )?;
    let planned = plan_batches(
        prepared.iter().map(|p| p.size).enumerate().collect(),
        limits,
    );
    let ids: Vec<&str> = prepared.iter().map(|p| p.item.id.as_str()).collect();
    let raw = run_planned(
        planned,
        &ids,
        limits,
        elapsed,
        batch_count,
        |batch| {
            let request = BatchTranslationRequest {
                items: batch.iter().map(|&i| prepared[i].item.clone()).collect(),
                tone,
            };
            llm.translate_batch(&request)
                .map(|r| r.items)
                .map_err(|e| BatchFailure::Llm(e.0))
        },
        |i, texts| align_languages(&prepared[i].item.destinations, texts),
    )?;
    let mut out = Vec::with_capacity(prepared.len());
    for (p, texts) in prepared.iter().zip(raw) {
        let mut restored = Vec::with_capacity(texts.len());
        for (lang, text) in texts {
            let intact = validate_integrity(&p.mapping, &text)
                && validate_integrity_across(&p.mapping, &[text.as_str()]);
            if !intact {
                return Err(BatchFailure::Integrity);
            }
            let value = restore(&text, &p.mapping, lang).map_err(|_| BatchFailure::Integrity)?;
            restored.push((lang, value));
        }
        out.push((p.key, restored));
    }
    Ok(out)
}

pub fn compose_cell_replacements(
    cells: &[CellText],
    translated: &[(CellKey, LangTexts)],
    include_original: bool,
    max_cell_chars: usize,
) -> Result<Vec<(CellKey, String)>, BatchFailure> {
    let originals: HashMap<CellKey, &str> =
        cells.iter().map(|c| (c.key, c.text.as_str())).collect();
    let mut out = Vec::with_capacity(translated.len());
    for (key, texts) in translated {
        let original = originals.get(key).ok_or_else(ids_mismatch)?;
        let blocks: Vec<String> = texts.iter().map(|(_, t)| t.clone()).collect();
        let composed = compose_replacement(include_original.then_some(*original), &blocks);
        if composed.chars().count() > max_cell_chars {
            return Err(BatchFailure::CellTooLarge);
        }
        out.push((*key, composed));
    }
    Ok(out)
}

/// Equal ignoring outer whitespace keeps the original; otherwise the original's outer
/// whitespace wraps the trimmed correction.
fn merge_whitespace(original: &str, restored: &str) -> String {
    let core = restored.trim();
    if original.trim() == core {
        return original.to_string();
    }
    let lead_len = original.len() - original.trim_start().len();
    let trail_start = original.trim_end().len().max(lead_len);
    format!(
        "{}{core}{}",
        &original[..lead_len],
        &original[trail_start..]
    )
}

struct PreparedCorrection {
    key: CellKey,
    original: String,
    mapping: Vec<MaskMatch>,
    item: BatchCorrectionItem,
}

/// Corrects Korean cells in size-limited batches. Only Korean cells are sent (ids `c{n}` count
/// sent cells) and returned, unchanged ones with their original text. All-or-nothing.
pub fn correct_cells(
    cells: &[CellText],
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    mode: CorrectionMode,
    limits: &BatchLimits,
    elapsed: &dyn Fn() -> Duration,
) -> Result<Vec<(CellKey, String)>, BatchFailure> {
    let started = Instant::now();
    let mut batches = 0usize;
    let result = run_correct_cells(
        cells,
        entries,
        detector,
        llm,
        mode,
        limits,
        elapsed,
        &mut batches,
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(
            operation = "batch_correct",
            cells = cells.len(),
            batches,
            elapsed_ms,
            "ok"
        ),
        Err(e) => tracing::warn!(
            operation = "batch_correct",
            cells = cells.len(),
            batches,
            elapsed_ms,
            failure = failure_kind(e),
            "failed"
        ),
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn run_correct_cells(
    cells: &[CellText],
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    mode: CorrectionMode,
    limits: &BatchLimits,
    elapsed: &dyn Fn() -> Duration,
    batch_count: &mut usize,
) -> Result<Vec<(CellKey, String)>, BatchFailure> {
    let mut prepared: Vec<PreparedCorrection> = Vec::new();
    let mut offset = 0usize;
    for cell in cells {
        let masked = mask(&cell.text, entries);
        if detector.detect(&masked.masked_text) != Some(Lang::Ko) {
            continue;
        }
        let masked = renumber(masked, offset);
        offset += masked.mapping.len();
        if masked.masked_text.chars().count() > limits.max_cell_chars {
            return Err(BatchFailure::CellTooLarge);
        }
        prepared.push(PreparedCorrection {
            key: cell.key,
            original: cell.text.clone(),
            mapping: masked.mapping,
            item: BatchCorrectionItem {
                id: format!("c{}", prepared.len()),
                text: masked.masked_text,
            },
        });
    }
    if prepared.is_empty() {
        return Err(BatchFailure::NoKoreanText);
    }
    let planned = plan_batches(
        prepared
            .iter()
            .map(|p| p.item.text.chars().count())
            .enumerate()
            .collect(),
        limits,
    );
    let ids: Vec<&str> = prepared.iter().map(|p| p.item.id.as_str()).collect();
    let texts = run_planned(
        planned,
        &ids,
        limits,
        elapsed,
        batch_count,
        |batch| {
            let request = BatchCorrectionRequest {
                items: batch.iter().map(|&i| prepared[i].item.clone()).collect(),
                mode,
            };
            llm.correct_batch(&request)
                .map(|r| r.items)
                .map_err(|e| BatchFailure::Llm(e.0))
        },
        |_, text: String| Some(text),
    )?;
    let mut out = Vec::with_capacity(prepared.len());
    for (p, text) in prepared.iter().zip(texts) {
        if text.trim().is_empty() {
            return Err(BatchFailure::Llm("empty correction result".to_string()));
        }
        let intact = validate_integrity(&p.mapping, &text)
            && validate_integrity_across(&p.mapping, &[text.as_str()]);
        if !intact {
            return Err(BatchFailure::Integrity);
        }
        let restored =
            restore_fragment(&text, &p.mapping, None).map_err(|_| BatchFailure::Integrity)?;
        if restored.trim().is_empty() {
            return Err(BatchFailure::Llm("empty correction result".to_string()));
        }
        out.push((p.key, merge_whitespace(&p.original, &restored)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::RuleBasedDetector;
    use crate::dictionary::Action;
    use crate::llm::{
        BatchCorrectionResult, BatchTranslationRequest, BatchTranslationResult, LlmError,
    };
    use crate::settings::CorrectionMode;
    use crate::test_support::FakeLlm;
    use std::cell::Cell;

    fn limits() -> BatchLimits {
        BatchLimits {
            max_cells: 50,
            max_chars: 20_000,
            max_cell_chars: 5_000,
            job_timeout: Duration::from_secs(300),
        }
    }
    fn small() -> BatchLimits {
        BatchLimits {
            max_cells: 4,
            max_chars: 10_000,
            max_cell_chars: 5_000,
            job_timeout: Duration::from_secs(60),
        }
    }
    fn keep(term: &str) -> DictionaryEntry {
        DictionaryEntry::new(term.into(), Action::Keep, Default::default()).unwrap()
    }
    fn logical_model() -> DictionaryEntry {
        let tr = [
            ("ko", "논리 모델"),
            ("en", "logical model"),
            ("vi", "mô hình logic"),
            ("ja", "論理モデル"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        DictionaryEntry::new("logical model".into(), Action::Translate, tr).unwrap()
    }
    fn cell(r: usize, c: usize, t: &str) -> CellText {
        CellText {
            key: (r, c),
            text: t.into(),
        }
    }
    fn echo() -> FakeLlm {
        let f = FakeLlm::new();
        f.set_batch_responder(FakeLlm::echo_batch_responder());
        f
    }
    fn no_time() -> Duration {
        Duration::ZERO
    }
    type Out = Result<Vec<(CellKey, Vec<(Lang, String)>)>, BatchFailure>;
    fn run(
        cells: &[CellText],
        entries: &[DictionaryEntry],
        llm: &FakeLlm,
        targets: &[Lang],
        include_original: bool,
        lim: &BatchLimits,
    ) -> Out {
        translate_cells(
            cells,
            entries,
            &RuleBasedDetector,
            llm,
            Lang::Ko,
            targets,
            Tone::Neutral,
            include_original,
            lim,
            &no_time,
        )
    }
    fn reply(items: &[(&str, &[(Lang, &str)])]) -> Result<BatchTranslationResult, LlmError> {
        Ok(BatchTranslationResult {
            items: items
                .iter()
                .map(|(id, t)| {
                    (
                        id.to_string(),
                        t.iter().map(|(l, s)| (*l, s.to_string())).collect(),
                    )
                })
                .collect(),
        })
    }
    fn s(l: Lang, t: &str) -> (Lang, String) {
        (l, t.to_string())
    }

    #[test]
    fn mixed_language_cells_get_per_item_destinations() {
        let llm = echo();
        let out = run(
            &[cell(0, 0, "안녕하세요"), cell(0, 1, "Hello")],
            &[],
            &llm,
            &[Lang::En, Lang::Ja],
            false,
            &limits(),
        )
        .unwrap();
        let req = &llm.translate_batch_requests()[0];
        assert_eq!(req.items[0].id, "c0");
        assert_eq!(req.items[0].source, Lang::Ko);
        assert_eq!(req.items[0].destinations, vec![Lang::En, Lang::Ja]);
        assert_eq!(req.items[1].id, "c1");
        assert_eq!(req.items[1].source, Lang::En);
        assert_eq!(req.items[1].destinations, vec![Lang::Ko]);
        assert_eq!(
            out,
            vec![
                (
                    (0, 0),
                    vec![
                        s(Lang::En, "[en] 안녕하세요"),
                        s(Lang::Ja, "[ja] 안녕하세요")
                    ]
                ),
                ((0, 1), vec![s(Lang::Ko, "[ko] Hello")]),
            ]
        );
    }

    #[test]
    fn translate_term_restored_in_each_destination_language() {
        let llm = echo();
        let out = run(
            &[cell(0, 0, "logical model 확인")],
            &[logical_model()],
            &llm,
            &[Lang::Vi, Lang::Ja],
            false,
            &limits(),
        )
        .unwrap();
        assert_eq!(
            out[0].1,
            vec![
                s(Lang::Vi, "[vi] mô hình logic 확인"),
                s(Lang::Ja, "[ja] 論理モデル 확인")
            ]
        );
    }

    #[test]
    fn translate_term_uses_base_language_translation_for_reverse_direction() {
        let llm = echo();
        let out = run(
            &[cell(0, 0, "check the logical model")],
            &[logical_model()],
            &llm,
            &[Lang::En],
            false,
            &limits(),
        )
        .unwrap();
        assert_eq!(out[0].1, vec![s(Lang::Ko, "[ko] check the 논리 모델")]);
    }

    #[test]
    fn keep_term_survives_in_each_cell_with_unique_placeholders() {
        let llm = echo();
        let out = run(
            &[cell(0, 0, "DA 확인"), cell(1, 0, "CLT 적용")],
            &[keep("DA"), keep("CLT")],
            &llm,
            &[Lang::En],
            false,
            &limits(),
        )
        .unwrap();
        let sent: Vec<String> = llm.translate_batch_requests()[0]
            .items
            .iter()
            .map(|i| i.text.clone())
            .collect();
        assert_eq!(sent, vec!["⟦0⟧ 확인", "⟦1⟧ 적용"]);
        assert_eq!(out[0].1, vec![s(Lang::En, "[en] DA 확인")]);
        assert_eq!(out[1].1, vec![s(Lang::En, "[en] CLT 적용")]);
    }

    #[test]
    fn large_range_splits_into_multiple_llm_calls_not_per_cell() {
        let llm = echo();
        let cells: Vec<CellText> = (0..10).map(|r| cell(r, 0, "안녕")).collect();
        let out = run(&cells, &[], &llm, &[Lang::En], false, &small()).unwrap();
        assert_eq!(out.len(), 10);
        assert_eq!(llm.translate_batch_calls(), 3);
        assert_eq!(llm.translate_calls(), 0);
        let reqs = llm.translate_batch_requests();
        let ids: Vec<&str> = reqs[1].items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["c4", "c5", "c6", "c7"]);
    }

    #[test]
    fn batch_size_counts_destinations_and_original() {
        let cells: Vec<CellText> = (0..3).map(|r| cell(r, 0, "안녕하세")).collect();
        let lim = BatchLimits {
            max_cells: 50,
            max_chars: 10,
            max_cell_chars: 5_000,
            job_timeout: Duration::from_secs(60),
        };
        // 4 chars x (2 targets + original) = 12 > 10: one item per batch.
        let llm = echo();
        run(&cells, &[], &llm, &[Lang::En, Lang::Ja], true, &lim).unwrap();
        assert_eq!(llm.translate_batch_calls(), 3);
        // 4 x 2 = 8: a second item (16) does not fit either.
        let llm = echo();
        run(&cells, &[], &llm, &[Lang::En, Lang::Ja], false, &lim).unwrap();
        assert_eq!(llm.translate_batch_calls(), 3);
        // One target, no original: 4 each, two fit (8), the third starts a new batch.
        let llm = echo();
        run(&cells, &[], &llm, &[Lang::En], false, &lim).unwrap();
        assert_eq!(llm.translate_batch_calls(), 2);
    }

    #[test]
    fn second_batch_failure_returns_llm_error() {
        let llm = echo();
        llm.push_translate_batch(Ok(BatchTranslationResult {
            items: (0..4)
                .map(|i| (format!("c{i}"), vec![s(Lang::En, "x")]))
                .collect(),
        }));
        llm.push_translate_batch(Err(LlmError("boom".into())));
        let cells: Vec<CellText> = (0..10).map(|r| cell(r, 0, "안녕")).collect();
        let out = run(&cells, &[], &llm, &[Lang::En], false, &small());
        assert_eq!(out, Err(BatchFailure::Llm("boom".into())));
    }

    #[test]
    fn missing_or_extra_or_duplicate_ids_fail_whole_job() {
        let cells = [cell(0, 0, "안녕"), cell(0, 1, "잘가")];
        type Items<'a> = Vec<(&'a str, &'a [(Lang, &'a str)])>;
        let cases: Vec<Items> = vec![
            vec![("c0", &[(Lang::En, "only first")])],
            vec![
                ("c0", &[(Lang::En, "a")]),
                ("c1", &[(Lang::En, "b")]),
                ("c2", &[(Lang::En, "c")]),
            ],
            vec![("c0", &[(Lang::En, "a")]), ("c0", &[(Lang::En, "b")])],
        ];
        for items in cases {
            let llm = FakeLlm::new();
            llm.push_translate_batch(reply(&items));
            let out = run(&cells, &[], &llm, &[Lang::En], false, &limits());
            assert_eq!(
                out,
                Err(BatchFailure::Llm(
                    "response ids do not match request".into()
                ))
            );
        }
    }

    #[test]
    fn language_keys_must_match_the_items_destinations() {
        for texts in [
            vec![(Lang::En, "a")],
            vec![(Lang::En, "a"), (Lang::Vi, "b")],
            vec![(Lang::En, "a"), (Lang::Ja, "b"), (Lang::Vi, "c")],
            vec![(Lang::En, "a"), (Lang::En, "b")],
        ] {
            let llm = FakeLlm::new();
            llm.push_translate_batch(reply(&[("c0", &texts)]));
            let out = run(
                &[cell(0, 0, "안녕")],
                &[],
                &llm,
                &[Lang::En, Lang::Ja],
                false,
                &limits(),
            );
            assert_eq!(
                out,
                Err(BatchFailure::Llm(
                    "response ids do not match request".into()
                ))
            );
        }
    }

    #[test]
    fn response_languages_are_returned_in_destination_order() {
        let llm = FakeLlm::new();
        llm.push_translate_batch(reply(&[("c0", &[(Lang::Ja, "J"), (Lang::En, "E")])]));
        let out = run(
            &[cell(0, 0, "안녕")],
            &[],
            &llm,
            &[Lang::En, Lang::Ja],
            false,
            &limits(),
        )
        .unwrap();
        assert_eq!(out[0].1, vec![s(Lang::En, "E"), s(Lang::Ja, "J")]);
    }

    #[test]
    fn cross_cell_placeholder_swap_fails_whole_job() {
        let llm = FakeLlm::new();
        llm.push_translate_batch(reply(&[
            ("c0", &[(Lang::En, "check")]),
            ("c1", &[(Lang::En, "⟦0⟧ ⟦0⟧ apply")]),
        ]));
        let out = run(
            &[cell(0, 0, "DA 확인"), cell(0, 1, "CLT 적용")],
            &[keep("DA"), keep("CLT")],
            &llm,
            &[Lang::En],
            false,
            &limits(),
        );
        assert_eq!(out, Err(BatchFailure::Integrity));
    }

    #[test]
    fn true_cross_cell_swap_is_detected_because_placeholders_are_job_unique() {
        let llm = FakeLlm::new();
        llm.set_batch_responder(Box::new(|req: &BatchTranslationRequest| {
            Ok(BatchTranslationResult {
                items: vec![
                    ("c0".into(), vec![(Lang::En, req.items[1].text.clone())]),
                    ("c1".into(), vec![(Lang::En, req.items[0].text.clone())]),
                ],
            })
        }));
        let out = run(
            &[cell(0, 0, "DA 확인"), cell(0, 1, "CLT 적용")],
            &[keep("DA"), keep("CLT")],
            &llm,
            &[Lang::En],
            false,
            &limits(),
        );
        assert_eq!(out, Err(BatchFailure::Integrity));
    }

    #[test]
    fn placeholder_not_belonging_to_cell_fails_whole_job() {
        let llm = FakeLlm::new();
        llm.push_translate_batch(reply(&[("c0", &[(Lang::En, "check ⟦0⟧ ⟦7⟧")])]));
        let out = run(
            &[cell(0, 0, "DA 확인")],
            &[keep("DA")],
            &llm,
            &[Lang::En],
            false,
            &limits(),
        );
        assert_eq!(out, Err(BatchFailure::Integrity));
    }

    #[test]
    fn undetectable_cell_fails_without_llm_call() {
        let llm = echo();
        let out = run(
            &[cell(0, 0, "안녕"), cell(0, 1, "12345")],
            &[],
            &llm,
            &[Lang::En],
            false,
            &limits(),
        );
        assert_eq!(out, Err(BatchFailure::DirectionUnknown));
        assert_eq!(llm.translate_batch_calls(), 0);
    }

    #[test]
    fn no_usable_target_is_direction_unknown() {
        let llm = echo();
        let out = run(
            &[cell(0, 0, "안녕")],
            &[],
            &llm,
            &[Lang::Ko],
            false,
            &limits(),
        );
        assert_eq!(out, Err(BatchFailure::DirectionUnknown));
        assert_eq!(llm.translate_batch_calls(), 0);
    }

    #[test]
    fn cell_over_max_cell_chars_fails_before_llm() {
        let llm = echo();
        let lim = BatchLimits {
            max_cells: 10,
            max_chars: 100,
            max_cell_chars: 5,
            job_timeout: Duration::from_secs(60),
        };
        let out = run(
            &[cell(0, 0, "안녕하세요요요")],
            &[],
            &llm,
            &[Lang::En],
            false,
            &lim,
        );
        assert_eq!(out, Err(BatchFailure::CellTooLarge));
        assert_eq!(llm.translate_batch_calls(), 0);
    }

    #[test]
    fn job_timeout_checked_before_each_batch() {
        let llm = echo();
        let lim = BatchLimits {
            max_cells: 1,
            max_chars: 10_000,
            max_cell_chars: 5_000,
            job_timeout: Duration::from_secs(150),
        };
        let ticks = Cell::new(0u64);
        let elapsed = || {
            ticks.set(ticks.get() + 100);
            Duration::from_secs(ticks.get())
        };
        let cells: Vec<CellText> = (0..3).map(|r| cell(r, 0, "안녕")).collect();
        let out = translate_cells(
            &cells,
            &[],
            &RuleBasedDetector,
            &llm,
            Lang::Ko,
            &[Lang::En],
            Tone::Neutral,
            false,
            &lim,
            &elapsed,
        );
        assert_eq!(out, Err(BatchFailure::Timeout));
        assert_eq!(llm.translate_batch_calls(), 1);
    }

    #[test]
    fn no_cells_means_no_llm_call() {
        let llm = echo();
        let out = run(&[], &[], &llm, &[Lang::En], false, &limits()).unwrap();
        assert!(out.is_empty());
        assert_eq!(llm.translate_batch_calls(), 0);
    }

    #[test]
    fn plan_batches_respects_cell_and_char_limits_and_order() {
        let lim = BatchLimits {
            max_cells: 2,
            max_chars: 10,
            max_cell_chars: 50,
            job_timeout: Duration::from_secs(1),
        };
        let sizes = [4usize, 4, 4, 1];
        let batches = plan_batches(sizes.iter().copied().enumerate().collect(), &lim);
        assert_eq!(batches, vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn default_provider_batch_methods_are_unsupported() {
        use crate::llm::{BatchCorrectionRequest, LlmProvider};
        struct Bare;
        impl LlmProvider for Bare {
            fn translate(
                &self,
                _: &crate::llm::TranslationRequest,
            ) -> Result<crate::llm::TranslationResult, LlmError> {
                Err(LlmError("x".into()))
            }
            fn explain(
                &self,
                _: &crate::llm::ExplanationRequest,
            ) -> Result<crate::llm::ExplanationResult, LlmError> {
                Err(LlmError("x".into()))
            }
            fn correct(&self, _: &crate::llm::CorrectionRequest) -> Result<String, LlmError> {
                Err(LlmError("x".into()))
            }
            fn correct_sentences(
                &self,
                _: &crate::llm::CorrectionRequest,
            ) -> Result<Vec<crate::llm::CorrectionSentence>, LlmError> {
                Err(LlmError("x".into()))
            }
        }
        let t = Bare.translate_batch(&BatchTranslationRequest {
            items: vec![],
            tone: Tone::Neutral,
        });
        assert_eq!(
            t,
            Err(LlmError(
                "batch translation not supported by this provider".into()
            ))
        );
        let c = Bare.correct_batch(&BatchCorrectionRequest {
            items: vec![],
            mode: CorrectionMode::Spelling,
        });
        assert_eq!(
            c,
            Err(LlmError("correction not supported by this provider".into()))
        );
    }

    fn echo_corr() -> FakeLlm {
        let f = FakeLlm::new();
        f.set_correct_responder(FakeLlm::echo_correct_responder());
        f
    }
    fn fixed(items: &[(&str, &str)]) -> FakeLlm {
        let f = FakeLlm::new();
        f.push_correct_batch(Ok(BatchCorrectionResult {
            items: items
                .iter()
                .map(|(i, t)| (i.to_string(), t.to_string()))
                .collect(),
        }));
        f
    }
    fn correct(
        cells: &[CellText],
        entries: &[DictionaryEntry],
        llm: &FakeLlm,
        lim: &BatchLimits,
    ) -> Result<Vec<(CellKey, String)>, BatchFailure> {
        correct_cells(
            cells,
            entries,
            &RuleBasedDetector,
            llm,
            CorrectionMode::Spelling,
            lim,
            &no_time,
        )
    }

    #[test]
    fn correction_sends_and_returns_only_korean_cells() {
        let llm = echo_corr();
        let out = correct(
            &[cell(0, 0, "확인햇다"), cell(0, 1, "Hello")],
            &[],
            &llm,
            &limits(),
        )
        .unwrap();
        let reqs = llm.correct_batch_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].items.len(), 1);
        assert_eq!(reqs[0].items[0].id, "c0");
        assert_eq!(reqs[0].mode, CorrectionMode::Spelling);
        assert_eq!(out, vec![((0, 0), "[교정] 확인햇다".to_string())]);
    }

    #[test]
    fn correction_ids_count_sent_cells_only() {
        let llm = echo_corr();
        correct(
            &[
                cell(0, 0, "Hello"),
                cell(0, 1, "확인햇다"),
                cell(0, 2, "안녕"),
            ],
            &[],
            &llm,
            &limits(),
        )
        .unwrap();
        let ids: Vec<String> = llm.correct_batch_requests()[0]
            .items
            .iter()
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(ids, vec!["c0", "c1"]);
    }

    #[test]
    fn correction_without_korean_cells_is_no_korean_text() {
        let llm = echo_corr();
        let out = correct(&[cell(0, 0, "Hello")], &[], &llm, &limits());
        assert_eq!(out, Err(BatchFailure::NoKoreanText));
        assert_eq!(llm.correct_batch_calls(), 0);
        let out = correct(&[], &[], &llm, &limits());
        assert_eq!(out, Err(BatchFailure::NoKoreanText));
    }

    #[test]
    fn correction_keeps_translate_terms_as_written() {
        let llm = echo_corr();
        let out = correct(
            &[cell(0, 0, "logical model 확인")],
            &[logical_model()],
            &llm,
            &limits(),
        )
        .unwrap();
        assert_eq!(out, vec![((0, 0), "[교정] logical model 확인".to_string())]);
    }

    #[test]
    fn correction_token_moved_to_other_cell_is_integrity_failure() {
        let llm = fixed(&[("c0", "확인"), ("c1", "⟦0⟧ 확인")]);
        let out = correct(
            &[cell(0, 0, "DA 확인"), cell(0, 1, "확인햇다")],
            &[keep("DA")],
            &llm,
            &limits(),
        );
        assert_eq!(out, Err(BatchFailure::Integrity));
    }

    #[test]
    fn correction_preserves_outer_whitespace_per_cell() {
        let llm = fixed(&[("c0", "  확인했다  ")]);
        let out = correct(&[cell(0, 0, " 확인햇다\n")], &[], &llm, &limits()).unwrap();
        assert_eq!(out, vec![((0, 0), " 확인했다\n".to_string())]);
    }

    #[test]
    fn correction_equal_ignoring_outer_whitespace_keeps_original() {
        let llm = fixed(&[("c0", "확인")]);
        let out = correct(&[cell(0, 0, "  확인\t")], &[], &llm, &limits()).unwrap();
        assert_eq!(out, vec![((0, 0), "  확인\t".to_string())]);
    }

    #[test]
    fn correction_blank_result_is_llm_failure() {
        let llm = fixed(&[("c0", "   ")]);
        let out = correct(&[cell(0, 0, "확인햇다")], &[], &llm, &limits());
        assert!(matches!(out, Err(BatchFailure::Llm(_))));
    }

    #[test]
    fn correction_llm_error_and_cell_too_large_fail() {
        let llm = FakeLlm::new();
        llm.push_correct_batch(Err(LlmError("boom".into())));
        let out = correct(&[cell(0, 0, "확인햇다")], &[], &llm, &limits());
        assert_eq!(out, Err(BatchFailure::Llm("boom".into())));
        let tiny = BatchLimits {
            max_cells: 5,
            max_chars: 100,
            max_cell_chars: 3,
            job_timeout: Duration::from_secs(60),
        };
        let llm = echo_corr();
        let out = correct(&[cell(0, 0, "확인햇다")], &[], &llm, &tiny);
        assert_eq!(out, Err(BatchFailure::CellTooLarge));
        assert_eq!(llm.correct_batch_calls(), 0);
    }

    #[test]
    fn correction_id_mismatch_and_timeout_reuse_batch_checks() {
        let llm = fixed(&[("c0", "a")]);
        let out = correct(
            &[cell(0, 0, "확인햇다"), cell(0, 1, "안녕")],
            &[],
            &llm,
            &limits(),
        );
        assert_eq!(
            out,
            Err(BatchFailure::Llm(
                "response ids do not match request".into()
            ))
        );
        let lim = BatchLimits {
            max_cells: 1,
            max_chars: 10_000,
            max_cell_chars: 5_000,
            job_timeout: Duration::from_secs(150),
        };
        let ticks = Cell::new(0u64);
        let elapsed = || {
            ticks.set(ticks.get() + 100);
            Duration::from_secs(ticks.get())
        };
        let llm = echo_corr();
        let cells: Vec<CellText> = (0..3).map(|r| cell(r, 0, "안녕")).collect();
        let out = correct_cells(
            &cells,
            &[],
            &RuleBasedDetector,
            &llm,
            CorrectionMode::Spelling,
            &lim,
            &elapsed,
        );
        assert_eq!(out, Err(BatchFailure::Timeout));
        assert_eq!(llm.correct_batch_calls(), 1);
    }

    #[test]
    fn correction_batch_size_ignores_destinations() {
        // 4 chars each, max_chars 8: two per batch, the third starts a new one.
        let lim = BatchLimits {
            max_cells: 50,
            max_chars: 8,
            max_cell_chars: 5_000,
            job_timeout: Duration::from_secs(60),
        };
        let llm = echo_corr();
        let cells: Vec<CellText> = (0..3).map(|r| cell(r, 0, "안녕하세")).collect();
        correct(&cells, &[], &llm, &lim).unwrap();
        assert_eq!(llm.correct_batch_calls(), 2);
    }

    #[test]
    fn merge_whitespace_handles_multibyte_and_edges() {
        assert_eq!(
            merge_whitespace("\n 확인햇다 \t", "확인했다"),
            "\n 확인했다 \t"
        );
        assert_eq!(merge_whitespace("확인햇다", " 확인했다 "), "확인했다");
    }

    #[test]
    fn limits_from_sheets_config() {
        let l = BatchLimits::from(&SheetsConfig::default());
        assert_eq!(
            (l.max_cells, l.max_chars, l.max_cell_chars, l.job_timeout),
            (30, 5000, 5000, Duration::from_secs(240))
        );
    }

    #[test]
    fn compose_cell_replacements_joins_blocks_with_blank_line() {
        let cells = [cell(0, 0, "원문")];
        let tr = vec![((0, 0), vec![s(Lang::En, "EN"), s(Lang::Ja, "JA")])];
        assert_eq!(
            compose_cell_replacements(&cells, &tr, true, 100).unwrap(),
            vec![((0, 0), "원문\n\nEN\n\nJA".to_string())]
        );
        assert_eq!(
            compose_cell_replacements(&cells, &tr, false, 100).unwrap(),
            vec![((0, 0), "EN\n\nJA".to_string())]
        );
    }

    #[test]
    fn assembled_cell_over_limit_is_cell_too_large() {
        let cells = [cell(0, 0, "원문입니다")];
        let tr = vec![((0, 0), vec![s(Lang::En, "english"), s(Lang::Ja, "日本語")])];
        // "원문입니다\n\nenglish\n\n日本語" = 5+2+7+2+3 = 19 chars
        assert_eq!(
            compose_cell_replacements(&cells, &tr, true, 18),
            Err(BatchFailure::CellTooLarge)
        );
        assert!(compose_cell_replacements(&cells, &tr, true, 19).is_ok());
        assert!(compose_cell_replacements(&cells, &tr, false, 18).is_ok());
    }

    #[test]
    fn compose_cell_replacements_unknown_key_is_llm_failure() {
        let tr = vec![((9, 9), vec![s(Lang::En, "x")])];
        assert!(matches!(
            compose_cell_replacements(&[cell(0, 0, "a")], &tr, false, 100),
            Err(BatchFailure::Llm(_))
        ));
    }
}
