//! LLM port: request/response types, the `LlmProvider` trait and JSON reply parsing.

use crate::lang::Lang;
use crate::settings::{CorrectionMode, Tone};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct TranslationRequest {
    pub text: String,
    pub source: Lang,
    pub destinations: Vec<Lang>,
    pub tone: Tone,
}

/// Translations in the order of the request's `destinations`.
#[derive(Clone, Debug, PartialEq)]
pub struct TranslationResult {
    pub translations: Vec<(Lang, String)>,
}

pub type ExplanationRequest = TranslationRequest;

#[derive(Clone, Debug, PartialEq)]
pub struct ExplanationSentence {
    pub original: String,
    pub translations: Vec<(Lang, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExplanationResult {
    pub sentences: Vec<ExplanationSentence>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CorrectionRequest {
    pub text: String,
    pub mode: CorrectionMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CorrectionSentence {
    pub original: String,
    pub corrected: String,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct LlmError(pub String);

/// The only way services talk to an LLM; implemented by `ct-llm`.
pub trait LlmProvider: Send + Sync {
    fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, LlmError>;
    fn explain(&self, req: &ExplanationRequest) -> Result<ExplanationResult, LlmError>;
    fn correct(&self, req: &CorrectionRequest) -> Result<String, LlmError>;
    fn correct_sentences(
        &self,
        req: &CorrectionRequest,
    ) -> Result<Vec<CorrectionSentence>, LlmError>;
}

const MALFORMED: &str = "Claude CLI returned a malformed JSON response";

fn malformed() -> LlmError {
    LlmError(MALFORMED.to_string())
}

/// Strips a surrounding markdown code fence (```` ```json ````) if the whole text is fenced.
fn strip_markdown_json_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return text;
    };
    let Some(inner) = rest.strip_suffix("```") else {
        return text;
    };
    inner.strip_prefix("json").unwrap_or(inner).trim()
}

/// Parses a model reply that should be a single JSON object: strict (optional fence) first,
/// then the first `{` from the left at which a JSON object parses.
pub fn parse_json_response(raw: &str) -> Result<Value, LlmError> {
    if let Ok(value) = serde_json::from_str::<Value>(strip_markdown_json_fence(raw)) {
        return Ok(value);
    }
    for (pos, _) in raw.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&raw[pos..]).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next()
            && value.is_object()
        {
            return Ok(value);
        }
    }
    Err(malformed())
}

/// Validates a `{lang_code: text}` object against `destinations` and returns it in that order.
pub fn parse_language_map(
    value: &Value,
    destinations: &[Lang],
) -> Result<Vec<(Lang, String)>, LlmError> {
    let map = value.as_object().ok_or_else(malformed)?;
    if map.len() != destinations.len() {
        return Err(malformed());
    }
    let mut out = Vec::with_capacity(destinations.len());
    for lang in destinations {
        let text = map
            .get(lang.code())
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(malformed)?;
        out.push((*lang, text.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fence_unchanged_when_absent() {
        assert_eq!(strip_markdown_json_fence(r#"{"a": 1}"#), r#"{"a": 1}"#);
    }

    #[test]
    fn fence_json_labeled() {
        assert_eq!(
            strip_markdown_json_fence("```json\n{\"a\": 1}\n```"),
            r#"{"a": 1}"#
        );
    }

    #[test]
    fn fence_bare() {
        assert_eq!(
            strip_markdown_json_fence("```\n{\"a\": 1}\n```"),
            r#"{"a": 1}"#
        );
    }

    #[test]
    fn fence_with_surrounding_whitespace() {
        assert_eq!(
            strip_markdown_json_fence("  \n```json\n{\"a\": 1}\n```\n  "),
            r#"{"a": 1}"#
        );
    }

    #[test]
    fn parse_plain() {
        assert_eq!(parse_json_response(r#"{"a": 1}"#).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn parse_fenced() {
        assert_eq!(
            parse_json_response("```json\n{\"a\": 1}\n```").unwrap(),
            json!({"a": 1})
        );
    }

    #[test]
    fn parse_commentary_before_json() {
        let raw = "\"확인 햇습니다\" → \"확인했습니다\" (오탈자)\n\n{\"corrected\": \"⟦0⟧에서 확인했습니다\"}";
        assert_eq!(
            parse_json_response(raw).unwrap(),
            json!({"corrected": "⟦0⟧에서 확인했습니다"})
        );
    }

    #[test]
    fn parse_commentary_before_fenced_block() {
        let raw = "수정했습니다.\n```json\n{\"a\": 1}\n```";
        assert_eq!(parse_json_response(raw).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn parse_trailing_commentary() {
        assert_eq!(
            parse_json_response("{\"a\": 1}\n\n위와 같이 고쳤습니다.").unwrap(),
            json!({"a": 1})
        );
    }

    #[test]
    fn parse_brace_inside_commentary_before_json() {
        let raw = "{오탈자} 를 고쳤습니다 {x\n{\"corrected\": \"안녕하세요\"}";
        assert_eq!(
            parse_json_response(raw).unwrap(),
            json!({"corrected": "안녕하세요"})
        );
    }

    #[test]
    fn parse_no_json_fails() {
        let err = parse_json_response("죄송합니다, 처리할 수 없습니다.").unwrap_err();
        assert_eq!(err.0, "Claude CLI returned a malformed JSON response");
    }

    #[test]
    fn parse_non_object_fails() {
        assert!(parse_json_response("[1, 2] 그리고 설명").is_err());
    }

    #[test]
    fn language_map_returns_in_destination_order() {
        let v = json!({"ja": "こんにちは", "en": "Hello"});
        assert_eq!(
            parse_language_map(&v, &[Lang::En, Lang::Ja]).unwrap(),
            vec![(Lang::En, "Hello".into()), (Lang::Ja, "こんにちは".into())]
        );
    }

    #[test]
    fn language_map_rejects_missing_extra_empty_and_non_string() {
        for v in [
            json!({"en": "Hi"}),
            json!({"en": "Hi", "ja": "x", "vi": "y"}),
            json!({"en": " ", "ja": "x"}),
            json!({"en": 1, "ja": "x"}),
            json!("en"),
            json!({"EN": "Hi", "ja": "x"}),
        ] {
            assert!(parse_language_map(&v, &[Lang::En, Lang::Ja]).is_err());
        }
    }
}
