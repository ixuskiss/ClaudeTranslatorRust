//! Multi-target translation service: mask -> detect -> plan -> one LLM call -> per-language
//! integrity check -> restore. Any failure is an error so the original text is never replaced.

use crate::compose::compose_replacement;
use crate::detector::LanguageDetector;
use crate::dictionary::{
    DictionaryEntry, MaskMatch, mask, restore, validate_integrity, validate_integrity_across,
};
use crate::lang::Lang;
use crate::llm::{LlmProvider, TranslationRequest};
use crate::plan::{TranslationPlan, resolve_plan};
use crate::settings::Tone;

pub const DICTIONARY_INTEGRITY_MESSAGE: &str = "사전 용어 처리에 실패했습니다. 다시 시도해주세요.";

#[derive(Clone, Debug, PartialEq)]
pub struct Translated {
    pub source: Lang,
    pub translations: Vec<(Lang, String)>,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum TranslateError {
    #[error("Could not safely determine translation direction")]
    DirectionUnknown,
    #[error("{0}")]
    Llm(String),
    #[error("Dictionary placeholder integrity check failed")]
    Integrity,
}

impl TranslateError {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            TranslateError::DirectionUnknown => "direction_unknown",
            TranslateError::Llm(_) => "llm",
            TranslateError::Integrity => "integrity",
        }
    }
}

impl Translated {
    /// Replacement text: optionally the original first, then each translation, blank-line separated.
    pub fn replacement_text(&self, original: &str, include_original: bool) -> String {
        let blocks: Vec<String> = self.translations.iter().map(|(_, t)| t.clone()).collect();
        compose_replacement(include_original.then_some(original), &blocks)
    }
}

/// Masked text, its mapping and the resolved plan shared by translation and explanation.
pub(crate) struct Prepared {
    pub masked_text: String,
    pub mapping: Vec<MaskMatch>,
    pub plan: TranslationPlan,
}

pub(crate) fn prepare(
    text: &str,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    base: Lang,
    targets: &[Lang],
) -> Result<Prepared, TranslateError> {
    if text.trim().is_empty() {
        return Err(TranslateError::DirectionUnknown);
    }
    let masked = mask(text, entries);
    let detected = detector.detect(&masked.masked_text);
    let plan = resolve_plan(detected, base, targets)
        .filter(|p| !p.destinations.is_empty())
        .ok_or(TranslateError::DirectionUnknown)?;
    Ok(Prepared {
        masked_text: masked.masked_text,
        mapping: masked.mapping,
        plan,
    })
}

/// True when `returned` equals `expected` exactly (same languages, same order, same length).
pub(crate) fn languages_match(returned: impl Iterator<Item = Lang>, expected: &[Lang]) -> bool {
    returned.eq(expected.iter().copied())
}

pub(crate) fn language_mismatch() -> TranslateError {
    TranslateError::Llm("LLM response languages do not match the requested languages".to_string())
}

/// Logs operation, char count, duration and error kind only (never text).
pub(crate) fn log_outcome<T>(
    operation: &str,
    chars: usize,
    started: std::time::Instant,
    result: &Result<T, TranslateError>,
) {
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(_) => tracing::info!(operation, chars, elapsed_ms, "ok"),
        Err(e) => tracing::warn!(
            operation,
            chars,
            elapsed_ms,
            error_kind = e.kind(),
            "failed"
        ),
    }
}

pub fn translate_text(
    text: &str,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    base: Lang,
    targets: &[Lang],
    tone: Tone,
) -> Result<Translated, TranslateError> {
    let started = std::time::Instant::now();
    let result = run_translate(text, entries, detector, llm, base, targets, tone);
    log_outcome("translate", text.chars().count(), started, &result);
    result
}

fn run_translate(
    text: &str,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    base: Lang,
    targets: &[Lang],
    tone: Tone,
) -> Result<Translated, TranslateError> {
    let prepared = prepare(text, entries, detector, base, targets)?;
    let source = prepared.plan.source;
    let request = TranslationRequest {
        text: prepared.masked_text,
        source,
        destinations: prepared.plan.destinations,
        tone,
    };
    let response = llm
        .translate(&request)
        .map_err(|e| TranslateError::Llm(e.0))?;
    if !languages_match(
        response.translations.iter().map(|(l, _)| *l),
        &request.destinations,
    ) {
        return Err(language_mismatch());
    }
    let mut translations = Vec::with_capacity(response.translations.len());
    for (lang, translated) in response.translations {
        let intact = validate_integrity(&prepared.mapping, &translated)
            && validate_integrity_across(&prepared.mapping, &[translated.as_str()]);
        if !intact {
            return Err(TranslateError::Integrity);
        }
        let restored =
            restore(&translated, &prepared.mapping, lang).map_err(|_| TranslateError::Integrity)?;
        translations.push((lang, restored));
    }
    Ok(Translated {
        source,
        translations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::RuleBasedDetector;
    use crate::dictionary::Action;
    use crate::llm::{LlmError, TranslationResult};
    use crate::test_support::FakeLlm;
    use std::collections::BTreeMap;

    fn logical_model() -> DictionaryEntry {
        let tr: BTreeMap<String, String> = [
            ("ko", "논리 모델"),
            ("en", "logical model"),
            ("ja", "論理モデル"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        DictionaryEntry::new("logical model".to_string(), Action::Translate, tr).unwrap()
    }

    fn ok(items: &[(Lang, &str)]) -> Result<TranslationResult, LlmError> {
        Ok(TranslationResult {
            translations: items.iter().map(|(l, s)| (*l, s.to_string())).collect(),
        })
    }

    fn run(text: &str, llm: &FakeLlm, targets: &[Lang]) -> Result<Translated, TranslateError> {
        translate_text(
            text,
            &[logical_model()],
            &RuleBasedDetector,
            llm,
            Lang::Ko,
            targets,
            Tone::Neutral,
        )
    }

    #[test]
    fn translate_requests_all_targets_in_one_call_and_restores_per_language() {
        let llm = FakeLlm::new();
        llm.push_translate(ok(&[(Lang::En, "check ⟦0⟧"), (Lang::Ja, "⟦0⟧を確認")]));
        let out = run("논리 모델 확인", &llm, &[Lang::En, Lang::Ja]).unwrap();
        assert_eq!(llm.translate_calls(), 1);
        let req = &llm.translate_requests()[0];
        assert_eq!(req.destinations, vec![Lang::En, Lang::Ja]);
        assert_eq!(req.text, "⟦0⟧ 확인");
        assert_eq!(out.source, Lang::Ko);
        assert_eq!(
            out.translations,
            vec![
                (Lang::En, "check logical model".to_string()),
                (Lang::Ja, "論理モデルを確認".to_string())
            ]
        );
    }

    #[test]
    fn translate_target_language_input_goes_to_base_only() {
        let llm = FakeLlm::new();
        llm.push_translate(ok(&[(Lang::Ko, "안녕하세요")]));
        let out = run("Hello", &llm, &[Lang::En, Lang::Ja]).unwrap();
        assert_eq!(llm.translate_requests()[0].destinations, vec![Lang::Ko]);
        assert_eq!(out.source, Lang::En);
    }

    #[test]
    fn translate_fails_whole_when_one_language_breaks_tokens() {
        let llm = FakeLlm::new();
        llm.push_translate(ok(&[(Lang::En, "check ⟦0⟧"), (Lang::Ja, "確認")]));
        let out = run("논리 모델 확인", &llm, &[Lang::En, Lang::Ja]);
        assert_eq!(out, Err(TranslateError::Integrity));
    }

    #[test]
    fn translate_rejects_invented_extra_token() {
        let llm = FakeLlm::new();
        llm.push_translate(ok(&[(Lang::En, "check ⟦0⟧ ⟦9⟧")]));
        let out = run("논리 모델 확인", &llm, &[Lang::En]);
        assert_eq!(out, Err(TranslateError::Integrity));
    }

    #[test]
    fn translate_blank_text_fails_without_calling_llm() {
        let llm = FakeLlm::new();
        let out = run("   ", &llm, &[Lang::En]);
        assert_eq!(out, Err(TranslateError::DirectionUnknown));
        assert_eq!(llm.translate_calls(), 0);
    }

    #[test]
    fn translate_empty_targets_fails_without_calling_llm() {
        let llm = FakeLlm::new();
        let out = run("논리 모델 확인", &llm, &[]);
        assert_eq!(out, Err(TranslateError::DirectionUnknown));
        assert_eq!(llm.translate_calls(), 0);
    }

    #[test]
    fn translate_ambiguous_or_foreign_language_fails_without_calling_llm() {
        let llm = FakeLlm::new();
        assert_eq!(
            run("12345 !!!", &llm, &[Lang::En]),
            Err(TranslateError::DirectionUnknown)
        );
        assert_eq!(
            run("Vui lòng kiểm tra mô hình logic hôm nay", &llm, &[Lang::En]),
            Err(TranslateError::DirectionUnknown)
        );
        assert_eq!(llm.translate_calls(), 0);
    }

    #[test]
    fn translate_llm_error_is_llm_variant() {
        let llm = FakeLlm::new();
        llm.push_translate(Err(LlmError("upstream unavailable".into())));
        let out = run("논리 모델 확인", &llm, &[Lang::En]);
        assert_eq!(out, Err(TranslateError::Llm("upstream unavailable".into())));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn translate_rejects_language_list_mismatch() {
        let mismatch = Err(language_mismatch());
        let cases: Vec<(Vec<(Lang, &str)>, Vec<Lang>)> = vec![
            (vec![(Lang::En, "a")], vec![Lang::En, Lang::Ja]),
            (vec![], vec![Lang::En]),
            (
                vec![(Lang::Ja, "b"), (Lang::En, "a")],
                vec![Lang::En, Lang::Ja],
            ),
            (
                vec![(Lang::En, "a"), (Lang::En, "a")],
                vec![Lang::En, Lang::Ja],
            ),
            (vec![(Lang::En, "a"), (Lang::Vi, "c")], vec![Lang::En]),
        ];
        for (returned, targets) in cases {
            let llm = FakeLlm::new();
            llm.push_translate(ok(&returned));
            assert_eq!(run("논리 모델 확인", &llm, &targets), mismatch);
        }
    }

    #[test]
    fn translate_forwards_tone() {
        let llm = FakeLlm::new();
        llm.push_translate(ok(&[(Lang::En, "hi")]));
        translate_text(
            "안녕하세요",
            &[],
            &RuleBasedDetector,
            &llm,
            Lang::Ko,
            &[Lang::En],
            Tone::Formal,
        )
        .unwrap();
        assert_eq!(llm.translate_requests()[0].tone, Tone::Formal);
    }

    #[test]
    fn replacement_text_with_and_without_original() {
        let t = Translated {
            source: Lang::Ko,
            translations: vec![(Lang::En, "A".into()), (Lang::Ja, "B".into())],
        };
        assert_eq!(t.replacement_text("원문", true), "원문\n\nA\n\nB");
        assert_eq!(t.replacement_text("원문", false), "A\n\nB");
    }
}
