//! Multi-target explanation service. Never replaces text; any failure is an error.

use crate::detector::LanguageDetector;
use crate::dictionary::{
    DictionaryEntry, MaskMatch, PlaceholderIntegrityError, restore_fragment,
    validate_integrity_across,
};
use crate::lang::Lang;
use crate::llm::{ExplanationRequest, ExplanationResult, ExplanationSentence, LlmProvider};
use crate::settings::Tone;
use crate::translate::{TranslateError, log_outcome, prepare};

pub fn explain_text(
    text: &str,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    base: Lang,
    targets: &[Lang],
    tone: Tone,
) -> Result<ExplanationResult, TranslateError> {
    let started = std::time::Instant::now();
    let result = run_explain(text, entries, detector, llm, base, targets, tone);
    log_outcome("explain", text.chars().count(), started, &result);
    result
}

fn run_explain(
    text: &str,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
    base: Lang,
    targets: &[Lang],
    tone: Tone,
) -> Result<ExplanationResult, TranslateError> {
    let prepared = prepare(text, entries, detector, base, targets)?;
    let request = ExplanationRequest {
        text: prepared.masked_text,
        source: prepared.plan.source,
        destinations: prepared.plan.destinations,
        tone,
    };
    let response = llm
        .explain(&request)
        .map_err(|e| TranslateError::Llm(e.0))?;
    restore_sentence_columns(response, &prepared.mapping).map_err(|_| TranslateError::Integrity)
}

fn integrity_error() -> PlaceholderIntegrityError {
    PlaceholderIntegrityError(
        "Placeholder integrity check failed across explanation columns.".to_string(),
    )
}

/// Validates each whole column (originals, and each language) across all sentences, then restores:
/// originals to the user's written form, each language column to that language's form.
pub fn restore_sentence_columns(
    result: ExplanationResult,
    mapping: &[MaskMatch],
) -> Result<ExplanationResult, PlaceholderIntegrityError> {
    let originals: Vec<&str> = result
        .sentences
        .iter()
        .map(|s| s.original.as_str())
        .collect();
    if !validate_integrity_across(mapping, &originals) {
        return Err(integrity_error());
    }
    let mut langs: Vec<Lang> = Vec::new();
    for sentence in &result.sentences {
        for (lang, _) in &sentence.translations {
            if !langs.contains(lang) {
                langs.push(*lang);
            }
        }
    }
    for lang in &langs {
        let column: Vec<&str> = result
            .sentences
            .iter()
            .flat_map(|s| s.translations.iter())
            .filter(|(l, _)| l == lang)
            .map(|(_, t)| t.as_str())
            .collect();
        if !validate_integrity_across(mapping, &column) {
            return Err(integrity_error());
        }
    }
    let mut sentences = Vec::with_capacity(result.sentences.len());
    for sentence in &result.sentences {
        let original = restore_fragment(&sentence.original, mapping, None)?;
        let mut translations = Vec::with_capacity(sentence.translations.len());
        for (lang, t) in &sentence.translations {
            translations.push((*lang, restore_fragment(t, mapping, Some(*lang))?));
        }
        sentences.push(ExplanationSentence {
            original,
            translations,
        });
    }
    Ok(ExplanationResult { sentences })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::RuleBasedDetector;
    use crate::dictionary::Action;
    use crate::llm::LlmError;
    use crate::test_support::FakeLlm;
    use std::collections::BTreeMap;

    fn entries() -> Vec<DictionaryEntry> {
        let tr: BTreeMap<String, String> = [
            ("ko", "논리 모델"),
            ("en", "logical model"),
            ("ja", "論理モデル"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        vec![
            DictionaryEntry::new("logical model".to_string(), Action::Translate, tr).unwrap(),
            DictionaryEntry::new("DA".to_string(), Action::Keep, BTreeMap::new()).unwrap(),
        ]
    }

    fn sentence(original: &str, items: &[(Lang, &str)]) -> ExplanationSentence {
        ExplanationSentence {
            original: original.into(),
            translations: items.iter().map(|(l, s)| (*l, s.to_string())).collect(),
        }
    }

    fn run(
        text: &str,
        llm: &FakeLlm,
        targets: &[Lang],
    ) -> Result<ExplanationResult, TranslateError> {
        explain_text(
            text,
            &entries(),
            &RuleBasedDetector,
            llm,
            Lang::Ko,
            targets,
            Tone::Neutral,
        )
    }

    #[test]
    fn explain_restores_originals_to_written_form_and_each_language_column() {
        let llm = FakeLlm::new();
        llm.push_explain(Ok(ExplanationResult {
            sentences: vec![sentence(
                "⟦0⟧ 확인",
                &[(Lang::En, "check ⟦0⟧"), (Lang::Ja, "⟦0⟧を確認")],
            )],
        }));
        let out = run("논리 모델 확인", &llm, &[Lang::En, Lang::Ja]).unwrap();
        assert_eq!(llm.explain_calls(), 1);
        assert_eq!(llm.explain_requests()[0].text, "⟦0⟧ 확인");
        assert_eq!(
            llm.explain_requests()[0].destinations,
            vec![Lang::En, Lang::Ja]
        );
        assert_eq!(out.sentences[0].original, "논리 모델 확인");
        assert_eq!(
            out.sentences[0].translations,
            vec![
                (Lang::En, "check logical model".to_string()),
                (Lang::Ja, "論理モデルを確認".to_string())
            ]
        );
    }

    #[test]
    fn explain_rejects_token_missing_in_any_language_column() {
        let llm = FakeLlm::new();
        llm.push_explain(Ok(ExplanationResult {
            sentences: vec![sentence(
                "⟦0⟧ 확인",
                &[(Lang::En, "check ⟦0⟧"), (Lang::Ja, "確認")],
            )],
        }));
        let out = run("논리 모델 확인", &llm, &[Lang::En, Lang::Ja]);
        assert_eq!(out, Err(TranslateError::Integrity));
    }

    #[test]
    fn explain_rejects_invented_token() {
        let llm = FakeLlm::new();
        llm.push_explain(Ok(ExplanationResult {
            sentences: vec![sentence("⟦0⟧ 확인", &[(Lang::En, "check ⟦0⟧ ⟦9⟧")])],
        }));
        let out = run("논리 모델 확인", &llm, &[Lang::En]);
        assert_eq!(out, Err(TranslateError::Integrity));
    }

    #[test]
    fn explain_allows_token_to_move_between_sentences() {
        let llm = FakeLlm::new();
        llm.push_explain(Ok(ExplanationResult {
            sentences: vec![
                sentence(
                    "⟦0⟧ 확인",
                    &[(Lang::En, "check ⟦0⟧ ⟦1⟧"), (Lang::Ja, "⟦0⟧")],
                ),
                sentence("⟦1⟧", &[(Lang::En, "done"), (Lang::Ja, "⟦1⟧ 確認")]),
            ],
        }));
        let out = run("논리 모델 확인 DA", &llm, &[Lang::En, Lang::Ja]).unwrap();
        assert_eq!(out.sentences[0].translations[0].1, "check logical model DA");
        assert_eq!(out.sentences[1].original, "DA");
        assert_eq!(out.sentences[1].translations[1].1, "DA 確認");
    }

    #[test]
    fn explain_blank_empty_targets_and_dictionary_only_fail_without_llm() {
        let llm = FakeLlm::new();
        assert_eq!(
            run("  ", &llm, &[Lang::En]),
            Err(TranslateError::DirectionUnknown)
        );
        assert_eq!(
            run("논리 모델 확인", &llm, &[]),
            Err(TranslateError::DirectionUnknown)
        );
        assert_eq!(
            run("논리 모델", &llm, &[Lang::En]),
            Err(TranslateError::DirectionUnknown)
        );
        assert_eq!(llm.explain_calls(), 0);
    }

    #[test]
    fn explain_llm_error_is_llm_variant() {
        let llm = FakeLlm::new();
        llm.push_explain(Err(LlmError("upstream unavailable".into())));
        let out = run("논리 모델 확인", &llm, &[Lang::En]);
        assert_eq!(out, Err(TranslateError::Llm("upstream unavailable".into())));
    }
}
