//! 사전 용어를 `⟦N⟧` 토큰으로 가렸다가 복원한다. 무결성이 의심되면 항상 실패 쪽으로 간다.

use std::collections::HashSet;

use super::model::{Action, DictionaryEntry};
use crate::lang::Lang;

const OPEN: char = '⟦';
const CLOSE: char = '⟧';

fn placeholder(index: usize) -> String {
    format!("{OPEN}{index}{CLOSE}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskMatch {
    pub placeholder: String,
    pub entry: DictionaryEntry,
    pub original_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskResult {
    pub masked_text: String,
    pub mapping: Vec<MaskMatch>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PlaceholderIntegrityError(pub String);

/// 소문자 변환이 정확히 한 글자일 때만 바꾼다. 위치가 원문과 1:1로 맞는다.
fn lower_char(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn lower_chars(s: &str) -> Vec<char> {
    s.chars().map(lower_char).collect()
}

fn candidate_forms(entries: &[DictionaryEntry]) -> Vec<(Vec<char>, &DictionaryEntry)> {
    let mut seen: HashSet<Vec<char>> = HashSet::new();
    let mut candidates: Vec<(Vec<char>, &DictionaryEntry)> = Vec::new();
    for entry in entries {
        let mut forms: Vec<&str> = vec![entry.term.as_str()];
        if entry.action == Action::Translate {
            forms.extend(entry.translations.values().map(String::as_str));
        }
        for form in forms {
            if form.is_empty() {
                continue;
            }
            let lowered = lower_chars(form);
            if !seen.insert(lowered.clone()) {
                continue;
            }
            candidates.push((lowered, entry));
        }
    }
    // 안정 정렬: 길이가 같으면 목록 순서가 이긴다.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0.len()));
    candidates
}

fn boundary_ok(lowered: &[char], start: usize, form: &[char]) -> bool {
    let (Some(first), Some(last)) = (form.first(), form.last()) else {
        return false;
    };
    if first.is_ascii_alphanumeric() && start > 0 && lowered[start - 1].is_ascii_alphanumeric() {
        return false;
    }
    let end = start + form.len();
    !(last.is_ascii_alphanumeric() && end < lowered.len() && lowered[end].is_ascii_alphanumeric())
}

fn translated_form(m: &MaskMatch, target: &str) -> Result<String, PlaceholderIntegrityError> {
    if let Some(t) = m.entry.translations.get(target) {
        return Ok(t.clone());
    }
    if lower_chars(&m.original_text) != lower_chars(&m.entry.term) {
        return Ok(m.entry.term.clone());
    }
    Err(PlaceholderIntegrityError(format!(
        "No translation available for term '{}' in language '{}'",
        m.entry.term, target
    )))
}

pub fn mask(text: &str, entries: &[DictionaryEntry]) -> MaskResult {
    let candidates = candidate_forms(entries);
    let chars: Vec<char> = text.chars().collect();
    let lowered: Vec<char> = chars.iter().copied().map(lower_char).collect();

    let mut mapping: Vec<MaskMatch> = Vec::new();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let found = candidates.iter().find(|(form, _)| {
            lowered.len() - i >= form.len()
                && lowered[i..i + form.len()] == form[..]
                && boundary_ok(&lowered, i, form)
        });
        if let Some((form, entry)) = found {
            let ph = placeholder(mapping.len());
            out.push_str(&ph);
            mapping.push(MaskMatch {
                placeholder: ph,
                entry: (*entry).clone(),
                original_text: chars[i..i + form.len()].iter().collect(),
            });
            i += form.len();
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    MaskResult {
        masked_text: out,
        mapping,
    }
}

pub fn validate_integrity(mapping: &[MaskMatch], text: &str) -> bool {
    mapping
        .iter()
        .all(|m| text.matches(m.placeholder.as_str()).count() == 1)
}

/// `⟦` + 숫자 1개 이상 + `⟧` 토큰을 (시작 바이트, 끝 바이트)로 훑는다.
fn scan_tokens(text: &str) -> Vec<(usize, usize)> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    let mut digits = 0usize;
    for (pos, c) in text.char_indices() {
        if c == OPEN {
            start = Some(pos);
            digits = 0;
        } else if c.is_ascii_digit() && start.is_some() {
            digits += 1;
        } else if c == CLOSE && digits > 0 {
            if let Some(s) = start.take() {
                tokens.push((s, pos + c.len_utf8()));
            }
            digits = 0;
        } else {
            start = None;
            digits = 0;
        }
    }
    tokens
}

/// Shifts every placeholder owned by `result` from `⟦i⟧` to `⟦i+offset⟧` in one pass (renamed
/// tokens are never renamed again), so several masked texts can share one placeholder space.
pub fn renumber(result: MaskResult, offset: usize) -> MaskResult {
    if offset == 0 {
        return result;
    }
    let text = &result.masked_text;
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (s, e) in scan_tokens(text) {
        out.push_str(&text[last..s]);
        let token = &text[s..e];
        match result.mapping.iter().position(|m| m.placeholder == token) {
            Some(i) => out.push_str(&placeholder(i + offset)),
            None => out.push_str(token),
        }
        last = e;
    }
    out.push_str(&text[last..]);
    let mapping = result
        .mapping
        .into_iter()
        .enumerate()
        .map(|(i, m)| MaskMatch {
            placeholder: placeholder(i + offset),
            ..m
        })
        .collect();
    MaskResult {
        masked_text: out,
        mapping,
    }
}

pub fn restore(
    masked: &str,
    mapping: &[MaskMatch],
    target: Lang,
) -> Result<String, PlaceholderIntegrityError> {
    if !validate_integrity(mapping, masked) {
        return Err(PlaceholderIntegrityError(
            "Placeholder integrity check failed: masked text does not contain each \
             dictionary placeholder exactly once."
                .to_string(),
        ));
    }
    let mut out = String::new();
    let mut last = 0;
    for (s, e) in scan_tokens(masked) {
        out.push_str(&masked[last..s]);
        let token = &masked[s..e];
        match mapping.iter().find(|m| m.placeholder == token) {
            Some(m) if m.entry.action == Action::Keep => out.push_str(&m.original_text),
            Some(m) => out.push_str(&translated_form(m, target.code())?),
            None => out.push_str(token),
        }
        last = e;
    }
    out.push_str(&masked[last..]);
    Ok(out)
}

pub fn validate_integrity_across(mapping: &[MaskMatch], texts: &[&str]) -> bool {
    let joined: String = texts.concat();
    let mut found: Vec<&str> = scan_tokens(&joined)
        .into_iter()
        .map(|(s, e)| &joined[s..e])
        .collect();
    let mut expected: Vec<&str> = mapping.iter().map(|m| m.placeholder.as_str()).collect();
    let unique = expected.iter().collect::<HashSet<_>>().len() == expected.len();
    found.sort_unstable();
    expected.sort_unstable();
    found == expected && unique
}

pub fn restore_fragment(
    masked: &str,
    mapping: &[MaskMatch],
    target: Option<Lang>,
) -> Result<String, PlaceholderIntegrityError> {
    let mut out = String::new();
    let mut last = 0;
    for (s, e) in scan_tokens(masked) {
        out.push_str(&masked[last..s]);
        let token = &masked[s..e];
        let Some(m) = mapping.iter().find(|m| m.placeholder == token) else {
            return Err(PlaceholderIntegrityError(format!(
                "Unknown placeholder in text: {token}"
            )));
        };
        match target {
            Some(lang) if m.entry.action == Action::Translate => {
                out.push_str(&translated_form(m, lang.code())?)
            }
            _ => out.push_str(&m.original_text),
        }
        last = e;
    }
    out.push_str(&masked[last..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn keep(term: &str) -> DictionaryEntry {
        DictionaryEntry::new(term.into(), Action::Keep, BTreeMap::new()).unwrap()
    }

    fn tr(term: &str, pairs: &[(&str, &str)]) -> DictionaryEntry {
        let t = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        DictionaryEntry::new(term.into(), Action::Translate, t).unwrap()
    }

    fn lm() -> DictionaryEntry {
        tr(
            "logical model",
            &[
                ("ko", "논리 모델"),
                ("en", "logical model"),
                ("vi", "mô hình logic"),
                ("ja", "論理モデル"),
            ],
        )
    }

    fn infra() -> DictionaryEntry {
        tr(
            "infrastructure",
            &[
                ("ko", "인프라"),
                ("en", "infrastructure"),
                ("vi", "hạ tầng"),
                ("ja", "インフラ"),
            ],
        )
    }

    fn lm_short() -> DictionaryEntry {
        tr(
            "logical model",
            &[("ko", "논리 모델"), ("en", "logical model")],
        )
    }

    #[test]
    fn mask_replaces_keep_term_with_unique_placeholder() {
        let r = mask("Please check DA for details.", &[keep("DA")]);
        assert!(!r.masked_text.contains("DA"));
        assert_eq!(r.mapping.len(), 1);
        assert_eq!(r.mapping[0].entry.term, "DA");
        assert_eq!(r.mapping[0].original_text, "DA");
    }

    #[test]
    fn restore_preserves_original_casing_for_keep_term() {
        let r = mask("please check da again", &[keep("DA")]);
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::En).unwrap(),
            "please check da again"
        );
    }

    #[test]
    fn restore_substitutes_translate_term_with_target_language() {
        let r = mask("logical model must be reviewed", &[lm()]);
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::Ko).unwrap(),
            "논리 모델 must be reviewed"
        );
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::Ja).unwrap(),
            "論理モデル must be reviewed"
        );
    }

    #[test]
    fn longest_match_wins_for_overlapping_terms() {
        let r = mask("check logical model now", &[lm(), keep("model")]);
        assert_eq!(r.mapping.len(), 1);
        assert_eq!(r.mapping[0].entry.term, "logical model");
    }

    #[test]
    fn repeated_term_produces_distinct_placeholders() {
        let r = mask("DA said DA is done", &[keep("DA")]);
        assert_eq!(r.mapping.len(), 2);
        assert_eq!(
            r.masked_text
                .matches(r.mapping[0].placeholder.as_str())
                .count(),
            1
        );
        assert_ne!(r.mapping[0].placeholder, r.mapping[1].placeholder);
    }

    #[test]
    fn term_adjacent_to_punctuation_is_matched_cleanly() {
        let r = mask(
            "DA, CLT infrastructure 적용해주세요.",
            &[keep("DA"), keep("CLT"), infra()],
        );
        assert_eq!(r.mapping.len(), 3);
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::Ko).unwrap(),
            "DA, CLT 인프라 적용해주세요."
        );
    }

    #[test]
    fn mixed_korean_english_dictionary_protected_sentence_round_trip() {
        let text = "DA에서 logical model 확인하고 CLT infrastructure에 적용해주세요.";
        let entries = [keep("DA"), keep("CLT"), lm(), infra()];
        let r = mask(text, &entries);
        for e in &entries {
            assert!(!r.masked_text.contains(&e.term));
        }
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::Ko).unwrap(),
            "DA에서 논리 모델 확인하고 CLT 인프라에 적용해주세요."
        );
    }

    #[test]
    fn validate_integrity_passes_when_all_placeholders_present_once() {
        let r = mask("DA and CLT", &[keep("DA"), keep("CLT")]);
        let t = format!(
            "{} 그리고 {}",
            r.mapping[0].placeholder, r.mapping[1].placeholder
        );
        assert!(validate_integrity(&r.mapping, &t));
    }

    #[test]
    fn validate_integrity_fails_when_placeholder_missing() {
        let r = mask("DA and CLT", &[keep("DA"), keep("CLT")]);
        let t = format!("only {} present", r.mapping[0].placeholder);
        assert!(!validate_integrity(&r.mapping, &t));
    }

    #[test]
    fn validate_integrity_fails_when_placeholder_duplicated() {
        let r = mask("DA and CLT", &[keep("DA"), keep("CLT")]);
        let p = |i: usize| r.mapping[i].placeholder.clone();
        let t = format!("{} {} {}", p(0), p(0), p(1));
        assert!(!validate_integrity(&r.mapping, &t));
    }

    #[test]
    fn restore_raises_on_corrupted_placeholders_instead_of_guessing() {
        let r = mask("DA and CLT", &[keep("DA"), keep("CLT")]);
        let corrupted = format!("only {} present", r.mapping[0].placeholder);
        assert!(restore(&corrupted, &r.mapping, Lang::En).is_err());
    }

    #[test]
    fn restore_raises_when_translate_term_missing_target_language() {
        let e = tr("foo", &[("en", "foo")]);
        let r = mask("foo bar", &[e]);
        assert!(restore(&r.masked_text, &r.mapping, Lang::Vi).is_err());
    }

    #[test]
    fn token_may_move_between_sentences() {
        let m = mask("DA 확인. logical model 적용.", &[keep("DA"), lm_short()]);
        assert!(validate_integrity_across(
            &m.mapping,
            &["Check ⟦0⟧ and ⟦1⟧.", "Apply."]
        ));
    }

    #[test]
    fn across_rejects_duplicate_and_missing_and_foreign() {
        let m = mask("DA 확인. logical model 적용.", &[keep("DA"), lm_short()]);
        assert!(!validate_integrity_across(&m.mapping, &["⟦0⟧ ⟦0⟧", "⟦1⟧"]));
        assert!(!validate_integrity_across(&m.mapping, &["⟦0⟧"]));
        assert!(!validate_integrity_across(&m.mapping, &["⟦0⟧ ⟦1⟧ ⟦7⟧"]));
    }

    #[test]
    fn restore_fragment_translates_only_present_tokens() {
        let m = mask("DA 확인. logical model 적용.", &[keep("DA"), lm_short()]);
        assert_eq!(
            restore_fragment("Use ⟦1⟧.", &m.mapping, Some(Lang::Ko)).unwrap(),
            "Use 논리 모델."
        );
        assert_eq!(
            restore_fragment("⟦0⟧ ok", &m.mapping, Some(Lang::En)).unwrap(),
            "DA ok"
        );
    }

    #[test]
    fn restore_fragment_none_language_keeps_written_form() {
        let m = mask("Logical Model 확인", &[lm_short()]);
        assert_eq!(
            restore_fragment("⟦0⟧ 확인함", &m.mapping, None).unwrap(),
            "Logical Model 확인함"
        );
    }

    #[test]
    fn restore_fragment_unknown_token_raises() {
        let m = mask("DA", &[keep("DA")]);
        assert!(restore_fragment("⟦3⟧", &m.mapping, Some(Lang::En)).is_err());
    }

    #[test]
    fn korean_translation_form_is_masked_and_restored_to_english() {
        let r = mask("DA에서 논리 모델 확인", &[keep("DA"), lm()]);
        assert!(!r.masked_text.contains("논리 모델"));
        assert_eq!(r.mapping[1].original_text, "논리 모델");
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::En).unwrap(),
            "DA에서 logical model 확인"
        );
    }

    #[test]
    fn english_term_still_restores_to_korean() {
        let r = mask("check the logical model", &[lm()]);
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::Ko).unwrap(),
            "check the 논리 모델"
        );
    }

    #[test]
    fn translation_form_match_is_case_insensitive_and_keeps_written_form() {
        let e = tr("논리 모델", &[("en", "logical model")]);
        let r = mask("Logical Model 확인", &[e]);
        assert_eq!(r.mapping[0].original_text, "Logical Model");
        assert_eq!(
            restore_fragment(&r.masked_text, &r.mapping, None).unwrap(),
            "Logical Model 확인"
        );
    }

    #[test]
    fn repeated_korean_form_gets_distinct_placeholders() {
        let r = mask("논리 모델과 논리 모델", &[lm()]);
        let ph: Vec<&str> = r.mapping.iter().map(|m| m.placeholder.as_str()).collect();
        assert_eq!(ph, ["⟦0⟧", "⟦1⟧"]);
    }

    #[test]
    fn longest_form_wins_across_entries() {
        let short = tr("model", &[("ko", "모델")]);
        let r = mask("논리 모델", &[short, lm()]);
        assert_eq!(r.mapping.len(), 1);
        assert_eq!(r.mapping[0].entry, lm());
    }

    #[test]
    fn same_form_in_two_entries_earlier_entry_wins() {
        let user_keep = keep("논리 모델");
        let r = mask("논리 모델 확인", &[user_keep.clone(), lm()]);
        assert_eq!(r.mapping[0].entry, user_keep);
    }

    #[test]
    fn missing_target_falls_back_to_term_when_matched_by_translation() {
        let e = tr("logical model", &[("ko", "논리 모델")]);
        let r = mask("논리 모델 확인", &[e]);
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::En).unwrap(),
            "logical model 확인"
        );
    }

    #[test]
    fn missing_target_still_raises_when_matched_by_term() {
        let e = tr("logical model", &[("ko", "논리 모델")]);
        let r = mask("logical model", &[e]);
        assert!(restore(&r.masked_text, &r.mapping, Lang::Vi).is_err());
    }

    // v5 test_non_string_form_in_entry_is_ignored_without_error 대응:
    // Rust 타입상 비문자열 표기는 없으므로 빈 문자열 표기가 무시되는지 확인한다.
    #[test]
    fn empty_form_in_entry_is_ignored_without_error() {
        let e = tr("logical model", &[("ko", "논리 모델"), ("ja", "")]);
        let r = mask("논리 모델 확인", &[e]);
        assert_eq!(r.mapping.len(), 1);
    }

    fn test_word() -> DictionaryEntry {
        tr("테스트", &[("en", "test")])
    }

    #[test]
    fn latin_form_is_not_masked_inside_words() {
        let r = mask("the latest contest results", &[test_word()]);
        assert!(r.mapping.is_empty());
        assert_eq!(r.masked_text, "the latest contest results");
    }

    #[test]
    fn latin_form_is_masked_as_whole_word() {
        let r = mask("run the test now", &[test_word()]);
        assert_eq!(r.mapping.len(), 1);
        assert_eq!(r.mapping[0].original_text, "test");
    }

    #[test]
    fn keep_term_respects_ascii_boundary_but_not_hangul() {
        assert!(mask("DATA", &[keep("DA")]).mapping.is_empty());
        assert_eq!(mask("DA에서 확인", &[keep("DA")]).mapping.len(), 1);
        assert_eq!(mask("(DA)", &[keep("DA")]).mapping.len(), 1);
    }

    #[test]
    fn hangul_particles_attach_to_translate_form() {
        let r = mask("논리 모델을 확인", &[lm()]);
        assert_eq!(r.mapping.len(), 1);
        assert_eq!(r.mapping[0].original_text, "논리 모델");
    }

    #[test]
    fn shorter_candidate_tried_when_longer_fails_boundary() {
        let r = mask("DAXY and DA 에서", &[keep("DAX"), keep("DA")]);
        let o: Vec<&str> = r.mapping.iter().map(|m| m.original_text.as_str()).collect();
        assert_eq!(o, ["DA"]);
    }

    #[test]
    fn mask_handles_chars_whose_lowercase_changes_length() {
        let e = [keep("DA")];
        let r = mask("İstanbul DA에서", &e);
        assert_eq!(r.masked_text, "İstanbul ⟦0⟧에서");
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::En).unwrap(),
            "İstanbul DA에서"
        );
    }

    #[test]
    fn mask_handles_emoji_and_non_bmp() {
        let r = mask("👍 DA ok", &[keep("DA")]);
        assert_eq!(r.masked_text, "👍 ⟦0⟧ ok");
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::En).unwrap(),
            "👍 DA ok"
        );
    }

    #[test]
    fn renumber_shifts_placeholders_in_one_pass() {
        let r = renumber(mask("DA B", &[keep("DA"), keep("B")]), 1);
        assert_eq!(r.masked_text, "⟦1⟧ ⟦2⟧");
        assert_eq!(r.mapping[0].placeholder, "⟦1⟧");
        assert_eq!(r.mapping[1].placeholder, "⟦2⟧");
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::En).unwrap(),
            "DA B"
        );
    }

    #[test]
    fn restore_is_single_pass_and_never_reexpands_replacements() {
        // KEEP 원문이 토큰 모양을 포함해도 복원 결과에서 다시 치환되지 않는다.
        let k = keep("⟦1⟧x");
        let r = mask("⟦1⟧x and B", &[k, keep("B")]);
        assert_eq!(r.mapping.len(), 2);
        assert_eq!(
            restore(&r.masked_text, &r.mapping, Lang::En).unwrap(),
            "⟦1⟧x and B"
        );
    }
}
