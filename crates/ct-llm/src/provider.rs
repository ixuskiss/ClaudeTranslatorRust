//! `LlmProvider` backed by a Claude CLI transport: build prompt, ask, validate the JSON reply.

use std::sync::Arc;

use ct_core::llm::{
    CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationResult,
    ExplanationSentence, LlmError, LlmProvider, TranslationRequest, TranslationResult,
    parse_json_response, parse_language_map,
};
use serde_json::Value;

use crate::client::CliError;
use crate::prompts::{
    correction_prompt, explanation_prompt, sentence_correction_prompt, translation_prompt,
};

/// Sends one prompt and returns the CLI's stream-json `result` event.
pub trait CliTransport: Send + Sync {
    fn ask(&self, prompt: &str) -> Result<Value, CliError>;
}

pub struct ClaudeCliProvider {
    transport: Arc<dyn CliTransport>,
    model: String,
}

fn err(msg: &str) -> LlmError {
    LlmError(msg.to_string())
}

impl ClaudeCliProvider {
    pub fn new(transport: Arc<dyn CliTransport>, model: String) -> Self {
        Self { transport, model }
    }

    fn ask_json(&self, prompt: &str) -> Result<Value, LlmError> {
        let event = self
            .transport
            .ask(prompt)
            .map_err(|e| LlmError(format!("Claude CLI call failed: {e}")))?;

        if event
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let detail = match event.get("result") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => "None".to_string(),
                Some(other) => other.to_string(),
            };
            return Err(LlmError(format!("Claude CLI returned an error: {detail}")));
        }

        let raw = event.get("result").and_then(Value::as_str).unwrap_or("");
        if raw.trim().is_empty() {
            return Err(err("Claude CLI returned an empty response"));
        }
        parse_json_response(raw)
    }
}

impl LlmProvider for ClaudeCliProvider {
    fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, LlmError> {
        let parsed = self.ask_json(&translation_prompt(req, &self.model))?;
        let map = parsed
            .get("translations")
            .ok_or_else(|| err("Claude CLI translation response missing 'translations'"))?;
        let translations = parse_language_map(map, &req.destinations)?;
        Ok(TranslationResult { translations })
    }

    fn explain(&self, req: &ExplanationRequest) -> Result<ExplanationResult, LlmError> {
        let parsed = self.ask_json(&explanation_prompt(req))?;
        let malformed = || err("Claude CLI returned a malformed explanation response");
        let items = parsed
            .get("sentences")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?;
        let mut sentences = Vec::with_capacity(items.len());
        for item in items {
            let original = item
                .get("original")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            let map = item.get("translations").ok_or_else(malformed)?;
            sentences.push(ExplanationSentence {
                original: original.to_string(),
                translations: parse_language_map(map, &req.destinations)?,
            });
        }
        Ok(ExplanationResult { sentences })
    }

    fn correct(&self, req: &CorrectionRequest) -> Result<String, LlmError> {
        let parsed = self.ask_json(&correction_prompt(req, &self.model))?;
        parsed
            .get("corrected")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| err("Claude CLI correction response missing 'corrected'"))
    }

    fn correct_sentences(
        &self,
        req: &CorrectionRequest,
    ) -> Result<Vec<CorrectionSentence>, LlmError> {
        let parsed = self.ask_json(&sentence_correction_prompt(req))?;
        let malformed = || err("Claude CLI returned a malformed correction response");
        let items = parsed
            .get("sentences")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?;
        items
            .iter()
            .map(|item| {
                let original = item.get("original").and_then(Value::as_str);
                let corrected = item.get("corrected").and_then(Value::as_str);
                match (original, corrected) {
                    (Some(o), Some(c)) => Ok(CorrectionSentence {
                        original: o.to_string(),
                        corrected: c.to_string(),
                    }),
                    _ => Err(malformed()),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ct_core::lang::Lang;
    use ct_core::settings::{CorrectionMode, Tone};
    use serde_json::json;
    use std::sync::Mutex;

    struct FakeTransport {
        reply: Result<Value, CliError>,
        prompts: Mutex<Vec<String>>,
    }

    impl CliTransport for FakeTransport {
        fn ask(&self, prompt: &str) -> Result<Value, CliError> {
            if let Ok(mut p) = self.prompts.lock() {
                p.push(prompt.to_string());
            }
            self.reply.clone()
        }
    }

    fn fake(reply: Result<Value, CliError>) -> Arc<FakeTransport> {
        Arc::new(FakeTransport {
            reply,
            prompts: Mutex::new(Vec::new()),
        })
    }

    fn result(text: &str) -> Result<Value, CliError> {
        Ok(json!({"type": "result", "is_error": false, "result": text}))
    }

    fn provider(reply: Result<Value, CliError>) -> ClaudeCliProvider {
        ClaudeCliProvider::new(fake(reply), String::new())
    }

    fn treq(dests: &[Lang]) -> TranslationRequest {
        TranslationRequest {
            text: "hello".into(),
            source: Lang::Ko,
            destinations: dests.to_vec(),
            tone: Tone::Neutral,
        }
    }

    fn creq() -> CorrectionRequest {
        CorrectionRequest {
            text: "x".into(),
            mode: CorrectionMode::Spelling,
        }
    }

    fn msg<T: std::fmt::Debug>(r: Result<T, LlmError>) -> String {
        match r {
            Err(LlmError(m)) => m,
            Ok(v) => panic!("expected error, got {v:?}"),
        }
    }

    #[test]
    fn translate_parses_language_map_in_order() {
        let p = provider(result(r#"{"translations":{"en":"Hi","ja":"やあ"}}"#));
        let r = p.translate(&treq(&[Lang::En, Lang::Ja])).unwrap();
        assert_eq!(
            r.translations,
            vec![(Lang::En, "Hi".into()), (Lang::Ja, "やあ".into())]
        );
    }

    #[test]
    fn translate_strips_markdown_fence() {
        let p = provider(result("```json\n{\"translations\":{\"en\":\"Hi\"}}\n```"));
        assert!(p.translate(&treq(&[Lang::En])).is_ok());
    }

    #[test]
    fn translate_rejects_missing_language() {
        let p = provider(result(r#"{"translations":{"en":"Hi"}}"#));
        assert!(p.translate(&treq(&[Lang::En, Lang::Ja])).is_err());
    }

    #[test]
    fn translate_rejects_missing_translations_key() {
        let p = provider(result(r#"{"other":1}"#));
        assert_eq!(
            msg(p.translate(&treq(&[Lang::En]))),
            "Claude CLI translation response missing 'translations'"
        );
    }

    #[test]
    fn is_error_result_is_error() {
        let p = provider(Ok(json!({"is_error": true, "result": "boom"})));
        assert_eq!(
            msg(p.translate(&treq(&[Lang::En]))),
            "Claude CLI returned an error: boom"
        );
    }

    #[test]
    fn empty_result_is_error() {
        let p = provider(result("   "));
        assert_eq!(
            msg(p.translate(&treq(&[Lang::En]))),
            "Claude CLI returned an empty response"
        );
    }

    #[test]
    fn malformed_json_is_error() {
        let p = provider(result("not json"));
        assert_eq!(
            msg(p.translate(&treq(&[Lang::En]))),
            "Claude CLI returned a malformed JSON response"
        );
    }

    #[test]
    fn transport_failure_is_wrapped() {
        let p = provider(Err(CliError::Timeout));
        assert_eq!(
            msg(p.translate(&treq(&[Lang::En]))),
            "Claude CLI call failed: Claude CLI timed out"
        );
    }

    #[test]
    fn explain_parses_sentences_with_language_maps() {
        let p = provider(result(
            r#"{"sentences":[{"original":"a","translations":{"en":"A","ja":"あ"}},{"original":"b","translations":{"en":"B","ja":"び"}}]}"#,
        ));
        let r = p.explain(&treq(&[Lang::En, Lang::Ja])).unwrap();
        assert_eq!(r.sentences.len(), 2);
        assert_eq!(r.sentences[0].original, "a");
        assert_eq!(
            r.sentences[1].translations,
            vec![(Lang::En, "B".into()), (Lang::Ja, "び".into())]
        );
    }

    #[test]
    fn explain_rejects_sentence_missing_language() {
        let p = provider(result(
            r#"{"sentences":[{"original":"a","translations":{"en":"A"}}]}"#,
        ));
        assert!(p.explain(&treq(&[Lang::En, Lang::Ja])).is_err());
    }

    #[test]
    fn explain_rejects_missing_sentences_list() {
        let p = provider(result(r#"{"sentences":"x"}"#));
        assert_eq!(
            msg(p.explain(&treq(&[Lang::En]))),
            "Claude CLI returned a malformed explanation response"
        );
    }

    #[test]
    fn correct_parses_and_rejects_empty() {
        let p = provider(result(r#"{"corrected":"fixed"}"#));
        assert_eq!(p.correct(&creq()).unwrap(), "fixed");
        let p = provider(result(r#"{"corrected":"  "}"#));
        assert_eq!(
            msg(p.correct(&creq())),
            "Claude CLI correction response missing 'corrected'"
        );
        let p = provider(result(r#"{"x":1}"#));
        assert!(p.correct(&creq()).is_err());
    }

    #[test]
    fn correct_sentences_parses_pairs() {
        let p = provider(result(
            r#"{"sentences":[{"original":"a","corrected":"b"}]}"#,
        ));
        assert_eq!(
            p.correct_sentences(&creq()).unwrap(),
            vec![CorrectionSentence {
                original: "a".into(),
                corrected: "b".into()
            }]
        );
    }

    #[test]
    fn correct_sentences_rejects_malformed() {
        for body in [
            r#"{"x":1}"#,
            r#"{"sentences":[{"original":"a"}]}"#,
            r#"{"sentences":[{"original":1,"corrected":"b"}]}"#,
            r#"{"sentences":[{"original":"a","corrected":null}]}"#,
        ] {
            let p = provider(result(body));
            assert_eq!(
                msg(p.correct_sentences(&creq())),
                "Claude CLI returned a malformed correction response"
            );
        }
    }

    #[test]
    fn haiku_model_uses_soft_formatting() {
        let t = fake(result(r#"{"translations":{"en":"Hi"}}"#));
        let p = ClaudeCliProvider::new(t.clone(), "haiku".into());
        p.translate(&treq(&[Lang::En])).unwrap();
        let prompts = t.prompts.lock().unwrap();
        assert!(prompts[0].contains("Keep the same line breaks."));
    }
}
