//! Deterministic provider for tests and pipeline development: tags the text with the
//! destination language and leaves everything else (including placeholders) untouched.

use ct_core::{
    CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationResult,
    ExplanationSentence, Lang, LlmError, LlmProvider, TranslationRequest, TranslationResult,
};

pub struct MockProvider;

/// Splits after `.`, `?` or `!` followed by whitespace (the whitespace is dropped).
fn split_sentences(text: &str) -> Vec<String> {
    let text = text.trim();
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev_is_end = false;
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch.is_whitespace() && prev_is_end {
            if i > start {
                out.push(text[start..i].to_string());
            }
            let mut next = i + ch.len_utf8();
            while let Some(&(j, c)) = chars.peek() {
                if c.is_whitespace() {
                    next = j + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            start = next;
            prev_is_end = false;
        } else {
            prev_is_end = matches!(ch, '.' | '?' | '!');
        }
    }
    if start < text.len() {
        out.push(text[start..].to_string());
    }
    out
}

fn tag(destinations: &[Lang], text: &str) -> Vec<(Lang, String)> {
    destinations
        .iter()
        .map(|l| (*l, format!("[{}] {}", l.code(), text)))
        .collect()
}

impl LlmProvider for MockProvider {
    fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, LlmError> {
        Ok(TranslationResult {
            translations: tag(&req.destinations, &req.text),
        })
    }

    fn explain(&self, req: &ExplanationRequest) -> Result<ExplanationResult, LlmError> {
        let sentences = split_sentences(&req.text)
            .into_iter()
            .map(|s| ExplanationSentence {
                translations: tag(&req.destinations, &s),
                original: s,
            })
            .collect();
        Ok(ExplanationResult { sentences })
    }

    fn correct(&self, req: &CorrectionRequest) -> Result<String, LlmError> {
        Ok(format!("[교정] {}", req.text))
    }

    fn correct_sentences(
        &self,
        req: &CorrectionRequest,
    ) -> Result<Vec<CorrectionSentence>, LlmError> {
        Ok(split_sentences(&req.text)
            .into_iter()
            .map(|s| CorrectionSentence {
                corrected: format!("[교정] {s}"),
                original: s,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ct_core::{CorrectionMode, Tone};

    fn req(text: &str, dests: Vec<Lang>) -> TranslationRequest {
        TranslationRequest {
            text: text.into(),
            source: Lang::Ko,
            destinations: dests,
            tone: Tone::Neutral,
        }
    }

    #[test]
    fn mock_translates_each_destination_in_order() {
        let r = MockProvider
            .translate(&req("x", vec![Lang::En, Lang::Ja]))
            .unwrap();
        assert_eq!(
            r.translations,
            vec![
                (Lang::En, "[en] x".to_string()),
                (Lang::Ja, "[ja] x".to_string())
            ]
        );
    }

    #[test]
    fn translate_preserves_placeholders() {
        let r = MockProvider
            .translate(&req("⟦0⟧ said ⟦1⟧ is done", vec![Lang::Ko]))
            .unwrap();
        assert_eq!(r.translations[0].1, "[ko] ⟦0⟧ said ⟦1⟧ is done");
    }

    #[test]
    fn explain_returns_structured_sentence_breakdown() {
        let r = MockProvider
            .explain(&req("hello world. how are you?", vec![Lang::Ko]))
            .unwrap();
        assert_eq!(r.sentences.len(), 2);
        assert_eq!(r.sentences[0].original, "hello world.");
        assert_eq!(
            r.sentences[0].translations,
            vec![(Lang::Ko, "[ko] hello world.".to_string())]
        );
    }

    #[test]
    fn mock_explain_splits_sentences_without_regex_lookbehind() {
        let r = MockProvider
            .explain(&req("A. B? C", vec![Lang::En]))
            .unwrap();
        let originals: Vec<_> = r.sentences.iter().map(|s| s.original.as_str()).collect();
        assert_eq!(originals, ["A.", "B?", "C"]);
    }

    #[test]
    fn correct_tags_text() {
        let req = CorrectionRequest {
            text: "가나".into(),
            mode: CorrectionMode::Spelling,
        };
        assert_eq!(MockProvider.correct(&req).unwrap(), "[교정] 가나");
    }

    #[test]
    fn correct_sentences_splits_and_tags() {
        let req = CorrectionRequest {
            text: "첫째. 둘째!".into(),
            mode: CorrectionMode::Polish,
        };
        let r = MockProvider.correct_sentences(&req).unwrap();
        let pairs: Vec<_> = r
            .iter()
            .map(|s| (s.original.as_str(), s.corrected.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("첫째.", "[교정] 첫째."), ("둘째!", "[교정] 둘째!")]
        );
    }
}
