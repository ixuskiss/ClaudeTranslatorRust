//! Korean correction service (v5 `KoreanCorrectionService`): mask -> check Korean -> one LLM
//! call -> integrity check -> restore. Any failure leaves the original text untouched.

use crate::detector::LanguageDetector;
use crate::dictionary::{
    DictionaryEntry, MaskMatch, mask, restore_fragment, validate_integrity,
    validate_integrity_across,
};
use crate::lang::Lang;
use crate::llm::{CorrectionRequest, CorrectionSentence, LlmError, LlmProvider};
use crate::settings::CorrectionMode;
use crate::translate::DICTIONARY_INTEGRITY_MESSAGE;

const BASE_NOT_KOREAN_MESSAGE: &str = "한글 교정은 Base Language가 ko(한국어)일 때만 사용할 수 있습니다. 설정에서 Base Language를 ko로 바꿔주세요.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrectionStatus {
    Success,
    Unchanged,
    BaseNotKorean,
    NotKorean,
    LlmFailed,
    IntegrityFailed,
}

impl CorrectionStatus {
    pub fn user_message(self) -> &'static str {
        match self {
            CorrectionStatus::Success => "",
            CorrectionStatus::Unchanged => "수정할 부분이 없습니다.",
            CorrectionStatus::BaseNotKorean => BASE_NOT_KOREAN_MESSAGE,
            CorrectionStatus::NotKorean => {
                "선택한 문장의 주 언어가 한국어가 아니어서 교정하지 않았습니다."
            }
            CorrectionStatus::LlmFailed => "교정에 실패하여 원본을 변경하지 않았습니다.",
            CorrectionStatus::IntegrityFailed => DICTIONARY_INTEGRITY_MESSAGE,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            CorrectionStatus::Success => "success",
            CorrectionStatus::Unchanged => "unchanged",
            CorrectionStatus::BaseNotKorean => "base_not_korean",
            CorrectionStatus::NotKorean => "not_korean",
            CorrectionStatus::LlmFailed => "llm_failed",
            CorrectionStatus::IntegrityFailed => "integrity_failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CorrectionOutcome {
    pub status: CorrectionStatus,
    pub corrected_text: Option<String>,
    pub sentences: Option<Vec<CorrectionSentence>>,
    /// LLM error text; `Some` only when `LlmFailed` was caused by `Err(LlmError)`. Never logged.
    pub llm_error: Option<String>,
}

impl CorrectionOutcome {
    fn status_only(status: CorrectionStatus) -> Self {
        Self {
            status,
            corrected_text: None,
            sentences: None,
            llm_error: None,
        }
    }

    fn llm_failed(llm_error: Option<String>) -> Self {
        Self {
            llm_error,
            ..Self::status_only(CorrectionStatus::LlmFailed)
        }
    }
}

/// Logs operation, status, char count and duration only (never text).
fn log(operation: &str, outcome: &CorrectionOutcome, text: &str, started: std::time::Instant) {
    tracing::info!(
        operation,
        status = outcome.status.as_str(),
        chars = text.chars().count(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "correction"
    );
}

/// Base-language gate, then mask and Korean detection on the masked text.
fn prepare(
    text: &str,
    base: Lang,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
) -> Result<(String, Vec<MaskMatch>), CorrectionOutcome> {
    if base != Lang::Ko {
        return Err(CorrectionOutcome::status_only(
            CorrectionStatus::BaseNotKorean,
        ));
    }
    let masked = mask(text, entries);
    if detector.detect(&masked.masked_text) != Some(Lang::Ko) {
        return Err(CorrectionOutcome::status_only(CorrectionStatus::NotKorean));
    }
    Ok((masked.masked_text, masked.mapping))
}

pub fn correct_text(
    text: &str,
    base: Lang,
    mode: CorrectionMode,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
) -> CorrectionOutcome {
    let started = std::time::Instant::now();
    let outcome = run_single(text, base, mode, entries, detector, llm);
    log("correct", &outcome, text, started);
    outcome
}

pub fn correct_sentences(
    text: &str,
    base: Lang,
    mode: CorrectionMode,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
) -> CorrectionOutcome {
    let started = std::time::Instant::now();
    let outcome = run_sentences(text, base, mode, entries, detector, llm);
    log("correct_sentences", &outcome, text, started);
    outcome
}

fn run_single(
    text: &str,
    base: Lang,
    mode: CorrectionMode,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
) -> CorrectionOutcome {
    let (masked_text, mapping) = match prepare(text, base, entries, detector) {
        Ok(p) => p,
        Err(early) => return early,
    };
    let request = CorrectionRequest {
        text: masked_text,
        mode,
    };
    let llm_text = match llm.correct(&request) {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) => return CorrectionOutcome::llm_failed(None),
        Err(LlmError(m)) => return CorrectionOutcome::llm_failed(Some(m)),
    };
    // Exactly one of each known token, and no token the model invented.
    if !validate_integrity(&mapping, &llm_text)
        || !validate_integrity_across(&mapping, &[llm_text.as_str()])
    {
        return CorrectionOutcome::status_only(CorrectionStatus::IntegrityFailed);
    }
    let Ok(restored) = restore_fragment(&llm_text, &mapping, None) else {
        return CorrectionOutcome::status_only(CorrectionStatus::IntegrityFailed);
    };
    let restored = restored.trim();
    let stripped = text.trim();
    if restored == stripped {
        return CorrectionOutcome::status_only(CorrectionStatus::Unchanged);
    }
    let start = text.find(stripped).unwrap_or(0);
    let leading = &text[..start];
    let trailing = &text[start + stripped.len()..];
    CorrectionOutcome {
        status: CorrectionStatus::Success,
        corrected_text: Some(format!("{leading}{restored}{trailing}")),
        sentences: None,
        llm_error: None,
    }
}

fn run_sentences(
    text: &str,
    base: Lang,
    mode: CorrectionMode,
    entries: &[DictionaryEntry],
    detector: &dyn LanguageDetector,
    llm: &dyn LlmProvider,
) -> CorrectionOutcome {
    let (masked_text, mapping) = match prepare(text, base, entries, detector) {
        Ok(p) => p,
        Err(early) => return early,
    };
    let request = CorrectionRequest {
        text: masked_text,
        mode,
    };
    let sentences = match llm.correct_sentences(&request) {
        Ok(s) if s.iter().any(|x| !x.corrected.trim().is_empty()) => s,
        Ok(_) => return CorrectionOutcome::llm_failed(None),
        Err(LlmError(m)) => return CorrectionOutcome::llm_failed(Some(m)),
    };
    let originals: Vec<&str> = sentences.iter().map(|s| s.original.as_str()).collect();
    let corrected: Vec<&str> = sentences.iter().map(|s| s.corrected.as_str()).collect();
    if !validate_integrity_across(&mapping, &originals)
        || !validate_integrity_across(&mapping, &corrected)
    {
        return CorrectionOutcome::status_only(CorrectionStatus::IntegrityFailed);
    }
    let mut restored = Vec::with_capacity(sentences.len());
    for s in &sentences {
        let (Ok(original), Ok(corrected)) = (
            restore_fragment(&s.original, &mapping, None),
            restore_fragment(&s.corrected, &mapping, None),
        ) else {
            return CorrectionOutcome::status_only(CorrectionStatus::IntegrityFailed);
        };
        restored.push(CorrectionSentence {
            original,
            corrected,
        });
    }
    CorrectionOutcome {
        status: CorrectionStatus::Success,
        corrected_text: None,
        sentences: Some(restored),
        llm_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::RuleBasedDetector;
    use crate::dictionary::Action;
    use crate::test_support::FakeLlm;
    use std::collections::BTreeMap;

    fn keep(term: &str) -> DictionaryEntry {
        DictionaryEntry::new(term.to_string(), Action::Keep, BTreeMap::new()).unwrap()
    }

    fn translate_entry(term: &str, ko: &str) -> DictionaryEntry {
        let tr = BTreeMap::from([("ko".to_string(), ko.to_string())]);
        DictionaryEntry::new(term.to_string(), Action::Translate, tr).unwrap()
    }

    fn run(
        text: &str,
        base: Lang,
        mode: CorrectionMode,
        entries: &[DictionaryEntry],
        llm: &FakeLlm,
    ) -> CorrectionOutcome {
        correct_text(text, base, mode, entries, &RuleBasedDetector, llm)
    }

    fn run_ko(text: &str, entries: &[DictionaryEntry], llm: &FakeLlm) -> CorrectionOutcome {
        run(text, Lang::Ko, CorrectionMode::Spelling, entries, llm)
    }

    fn run_sentences_ko(
        text: &str,
        entries: &[DictionaryEntry],
        llm: &FakeLlm,
    ) -> CorrectionOutcome {
        correct_sentences(
            text,
            Lang::Ko,
            CorrectionMode::Spelling,
            entries,
            &RuleBasedDetector,
            llm,
        )
    }

    fn cs(original: &str, corrected: &str) -> CorrectionSentence {
        CorrectionSentence {
            original: original.into(),
            corrected: corrected.into(),
        }
    }

    const START_ERR: &str = "failed to start Claude CLI: x";

    #[test]
    fn llm_error_is_kept_on_llm_failed() {
        let llm = FakeLlm::new();
        llm.push_correct(Err(LlmError(START_ERR.into())));
        let out = run_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::LlmFailed);
        assert_eq!(out.llm_error.as_deref(), Some(START_ERR));

        let llm = FakeLlm::new();
        llm.push_correct_sentences(Err(LlmError(START_ERR.into())));
        let out = run_sentences_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::LlmFailed);
        assert_eq!(out.llm_error.as_deref(), Some(START_ERR));
    }

    #[test]
    fn empty_llm_result_has_no_llm_error() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("  \n".into()));
        let out = run_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::LlmFailed);
        assert_eq!(out.llm_error, None);

        let llm = FakeLlm::new();
        llm.push_correct_sentences(Ok(vec![cs("확인햇습니다", "  ")]));
        let out = run_sentences_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::LlmFailed);
        assert_eq!(out.llm_error, None);
    }

    #[test]
    fn non_llm_statuses_have_no_llm_error() {
        let llm = FakeLlm::new();
        let out = run(
            "확인햇습니다",
            Lang::En,
            CorrectionMode::Spelling,
            &[],
            &llm,
        );
        assert_eq!(out.status, CorrectionStatus::BaseNotKorean);
        assert_eq!(out.llm_error, None);

        let out = run_ko("Please check this.", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::NotKorean);
        assert_eq!(out.llm_error, None);

        llm.push_correct(Ok("확인햇습니다".into()));
        let out = run_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::Unchanged);
        assert_eq!(out.llm_error, None);

        llm.push_correct(Ok("확인했습니다".into()));
        let out = run_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::Success);
        assert_eq!(out.llm_error, None);
    }

    // v5 test_check_base_language + test_base_not_korean_blocks_without_calling_llm
    #[test]
    fn base_not_korean_blocks_without_calling_llm() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("x".into()));
        let out = run(
            "확인햇습니다",
            Lang::En,
            CorrectionMode::Spelling,
            &[],
            &llm,
        );
        assert_eq!(out.status, CorrectionStatus::BaseNotKorean);
        assert_eq!(llm.correct_calls(), 0);
        assert_eq!(
            out.status.user_message(),
            "한글 교정은 Base Language가 ko(한국어)일 때만 사용할 수 있습니다. 설정에서 Base Language를 ko로 바꿔주세요."
        );
    }

    #[test]
    fn base_ko_is_not_blocked() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("확인했습니다".into()));
        let out = run_ko("확인햇습니다", &[], &llm);
        assert_ne!(out.status, CorrectionStatus::BaseNotKorean);
    }

    #[test]
    fn english_text_is_not_korean() {
        let llm = FakeLlm::new();
        let out = run_ko("Please check this.", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::NotKorean);
        assert_eq!(llm.correct_calls(), 0);
        assert_eq!(
            out.status.user_message(),
            "선택한 문장의 주 언어가 한국어가 아니어서 교정하지 않았습니다."
        );
    }

    #[test]
    fn blank_text_is_not_korean() {
        let llm = FakeLlm::new();
        let out = run_ko("  \n", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::NotKorean);
        assert_eq!(llm.correct_calls(), 0);
    }

    #[test]
    fn mixed_korean_english_with_dictionary_is_korean() {
        let entries = vec![
            keep("DA"),
            keep("CLT"),
            keep("infrastructure"),
            translate_entry("logical model", "논리 모델"),
        ];
        let text = "DA에서 logical model 확인하고 CLT infrastructure에 적용해주세요.";
        let llm = FakeLlm::new();
        llm.push_correct(Ok("⟦0⟧에서 ⟦1⟧ 확인하고 ⟦2⟧ ⟦3⟧에 적용해 주세요.".into()));
        let out = run_ko(text, &entries, &llm);
        assert_eq!(out.status, CorrectionStatus::Success);
        assert_eq!(
            out.corrected_text.as_deref(),
            Some("DA에서 logical model 확인하고 CLT infrastructure에 적용해 주세요.")
        );
    }

    #[test]
    fn success_returns_corrected_text() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("확인했습니다".into()));
        let out = run_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::Success);
        assert_eq!(out.corrected_text.as_deref(), Some("확인했습니다"));
        assert_eq!(out.status.user_message(), "");
    }

    #[test]
    fn identical_result_is_unchanged() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("확인했습니다".into()));
        let out = run_ko("확인했습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::Unchanged);
        assert_eq!(out.corrected_text, None);
        assert_eq!(out.status.user_message(), "수정할 부분이 없습니다.");
    }

    #[test]
    fn whitespace_only_difference_is_unchanged() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("확인했습니다".into()));
        let out = run_ko("확인했습니다\n", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::Unchanged);
    }

    #[test]
    fn preserves_original_outer_whitespace() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("확인했습니다".into()));
        let out = run_ko("  확인햇습니다\n", &[], &llm);
        assert_eq!(out.corrected_text.as_deref(), Some("  확인했습니다\n"));
    }

    #[test]
    fn preserves_whitespace_when_llm_adds_its_own() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("\n 교정문 \n".into()));
        let out = run_ko("  원문 \n", &[], &llm);
        assert_eq!(out.corrected_text.as_deref(), Some("  교정문 \n"));
    }

    #[test]
    fn translate_term_keeps_original_form() {
        let entries = vec![translate_entry("LM", "논리 모델")];
        let llm = FakeLlm::new();
        llm.push_correct(Ok("⟦0⟧ 확인했다".into()));
        let out = run_ko("LM 확인햇다", &entries, &llm);
        assert_eq!(out.corrected_text.as_deref(), Some("LM 확인했다"));
        assert_eq!(llm.correct_requests()[0].text, "⟦0⟧ 확인햇다");
    }

    #[test]
    fn dropped_token_is_integrity_failed() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("확인했다".into()));
        let out = run_ko("LM 확인햇다", &[keep("LM")], &llm);
        assert_eq!(out.status, CorrectionStatus::IntegrityFailed);
        assert_eq!(out.status.user_message(), DICTIONARY_INTEGRITY_MESSAGE);
        assert_eq!(out.corrected_text, None);
    }

    #[test]
    fn invented_token_is_integrity_failed() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("⟦0⟧ ⟦7⟧ 확인했다".into()));
        let out = run_ko("LM 확인햇다", &[keep("LM")], &llm);
        assert_eq!(out.status, CorrectionStatus::IntegrityFailed);
        assert_eq!(out.corrected_text, None);
    }

    #[test]
    fn invented_token_without_dictionary_is_integrity_failed() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("⟦0⟧ 확인했다".into()));
        let out = run_ko("확인햇다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::IntegrityFailed);
    }

    #[test]
    fn llm_error_is_llm_failed() {
        let llm = FakeLlm::new();
        llm.push_correct(Err(LlmError("boom".into())));
        let out = run_ko("확인햇습니다", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::LlmFailed);
        assert_eq!(
            out.status.user_message(),
            "교정에 실패하여 원본을 변경하지 않았습니다."
        );
    }

    #[test]
    fn mode_is_passed_to_the_llm() {
        let llm = FakeLlm::new();
        llm.push_correct(Ok("확인했습니다".into()))
            .push_correct(Ok("확인했습니다".into()));
        run(
            "확인햇습니다",
            Lang::Ko,
            CorrectionMode::Spelling,
            &[],
            &llm,
        );
        run("확인햇습니다", Lang::Ko, CorrectionMode::Polish, &[], &llm);
        let modes: Vec<_> = llm.correct_requests().iter().map(|r| r.mode).collect();
        assert_eq!(
            modes,
            vec![CorrectionMode::Spelling, CorrectionMode::Polish]
        );
    }

    // v5 test_modes.py (unknown mode strings are normalized when settings are parsed).
    #[test]
    fn loading_message_per_mode() {
        assert_eq!(
            CorrectionMode::Spelling.loading_message(),
            "한국어 교정 중..."
        );
        assert_eq!(
            CorrectionMode::Polish.loading_message(),
            "한국어 수정 중..."
        );
    }

    #[test]
    fn empty_llm_result_is_llm_failed() {
        for empty in ["", "  \n"] {
            let llm = FakeLlm::new();
            llm.push_correct(Ok(empty.into()));
            let out = run_ko("확인햇습니다", &[], &llm);
            assert_eq!(out.status, CorrectionStatus::LlmFailed);
            assert_eq!(out.corrected_text, None);
        }
    }

    #[test]
    fn unscripted_llm_is_llm_failed_for_both_methods() {
        let llm = FakeLlm::new();
        assert_eq!(
            run_ko("확인햇다", &[], &llm).status,
            CorrectionStatus::LlmFailed
        );
        assert_eq!(
            run_sentences_ko("확인햇다", &[], &llm).status,
            CorrectionStatus::LlmFailed
        );
    }

    #[test]
    fn sentences_success_even_when_unchanged() {
        let entries = vec![translate_entry("LM", "논리 모델")];
        let llm = FakeLlm::new();
        llm.push_correct_sentences(Ok(vec![cs("⟦0⟧ 확인했다.", "⟦0⟧ 확인했다.")]));
        let out = run_sentences_ko("LM 확인했다.", &entries, &llm);
        assert_eq!(out.status, CorrectionStatus::Success);
        assert_eq!(
            out.sentences,
            Some(vec![cs("LM 확인했다.", "LM 확인했다.")])
        );
        assert_eq!(out.corrected_text, None);
    }

    #[test]
    fn sentences_integrity_failed_on_dropped_token() {
        let llm = FakeLlm::new();
        llm.push_correct_sentences(Ok(vec![cs("⟦0⟧ 확인햇다.", "확인했다.")]));
        let out = run_sentences_ko("LM 확인햇다.", &[keep("LM")], &llm);
        assert_eq!(out.status, CorrectionStatus::IntegrityFailed);
        assert_eq!(out.sentences, None);
    }

    #[test]
    fn sentences_integrity_failed_on_originals_column() {
        let llm = FakeLlm::new();
        llm.push_correct_sentences(Ok(vec![cs("확인햇다.", "⟦0⟧ 확인했다.")]));
        let out = run_sentences_ko("LM 확인햇다.", &[keep("LM")], &llm);
        assert_eq!(out.status, CorrectionStatus::IntegrityFailed);
    }

    #[test]
    fn sentences_llm_error_is_llm_failed() {
        let llm = FakeLlm::new();
        llm.push_correct_sentences(Err(LlmError("x".into())));
        let out = run_sentences_ko("확인햇다.", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::LlmFailed);
    }

    #[test]
    fn sentences_not_korean_and_base_gate() {
        let llm = FakeLlm::new();
        let out = run_sentences_ko("hello there", &[], &llm);
        assert_eq!(out.status, CorrectionStatus::NotKorean);
        let out = correct_sentences(
            "확인햇다.",
            Lang::En,
            CorrectionMode::Spelling,
            &[],
            &RuleBasedDetector,
            &llm,
        );
        assert_eq!(out.status, CorrectionStatus::BaseNotKorean);
        assert_eq!(llm.correct_sentences_calls(), 0);
    }

    #[test]
    fn sentences_empty_or_blank_is_llm_failed() {
        for bad in [vec![], vec![cs("a", " ")]] {
            let llm = FakeLlm::new();
            llm.push_correct_sentences(Ok(bad));
            let out = run_sentences_ko("확인햇다.", &[], &llm);
            assert_eq!(out.status, CorrectionStatus::LlmFailed);
            assert_eq!(out.sentences, None);
        }
    }

    #[test]
    fn sentences_mode_is_passed() {
        let llm = FakeLlm::new();
        llm.push_correct_sentences(Ok(vec![cs("가.", "나.")]));
        correct_sentences(
            "확인햇다.",
            Lang::Ko,
            CorrectionMode::Polish,
            &[],
            &RuleBasedDetector,
            &llm,
        );
        assert_eq!(
            llm.correct_sentences_requests()[0].mode,
            CorrectionMode::Polish
        );
    }
}
