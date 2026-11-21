//! `LlmProvider` backed by a Claude CLI transport: build prompt, ask, validate the JSON reply.

use std::sync::Arc;

use std::collections::HashMap;

use ct_core::Lang;
use ct_core::llm::{
    BatchCorrectionRequest, BatchCorrectionResult, BatchTranslationRequest, BatchTranslationResult,
    CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationResult,
    ExplanationSentence, LlmError, LlmProvider, TranslationRequest, TranslationResult,
    parse_json_response, parse_language_map,
};
use serde_json::Value;

use crate::client::CliError;
use crate::prompts::{
    batch_correction_prompt, batch_translation_prompt, correction_prompt, explanation_prompt,
    sentence_correction_prompt, translation_prompt,
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

fn batch_malformed() -> LlmError {
    err("Claude CLI batch response has a malformed entry")
}

/// The `list_key` array of a batch reply, every element required to be a JSON object.
fn batch_entries<'a>(parsed: &'a Value, list_key: &str) -> Result<&'a [Value], LlmError> {
    let list = parsed
        .get(list_key)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            LlmError(format!(
                "Claude CLI batch response missing '{list_key}' list"
            ))
        })?;
    if list.iter().any(|e| !e.is_object()) {
        return Err(batch_malformed());
    }
    Ok(list)
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

    fn translate_batch(
        &self,
        req: &BatchTranslationRequest,
    ) -> Result<BatchTranslationResult, LlmError> {
        let parsed = self.ask_json(&batch_translation_prompt(req, &self.model))?;
        let entries = batch_entries(&parsed, "translations")?;
        let mut by_id: HashMap<&str, Vec<(Lang, String)>> = HashMap::new();
        for entry in entries {
            let (id, texts) = entry
                .get("id")
                .and_then(Value::as_str)
                .zip(entry.get("texts").and_then(Value::as_object))
                .ok_or_else(batch_malformed)?;
            let item = req
                .items
                .iter()
                .find(|i| i.id == id)
                .ok_or_else(|| err("Claude CLI batch response has an unknown id"))?;
            if by_id.contains_key(id) {
                return Err(err("Claude CLI batch response has a duplicate id"));
            }
            if texts.len() != item.destinations.len() {
                return Err(err("Claude CLI batch response has mismatched languages"));
            }
            let mut out = Vec::with_capacity(item.destinations.len());
            for lang in &item.destinations {
                let text = texts
                    .get(lang.code())
                    .ok_or_else(|| err("Claude CLI batch response has mismatched languages"))?
                    .as_str()
                    .ok_or_else(batch_malformed)?;
                if text.trim().is_empty() && !item.text.trim().is_empty() {
                    return Err(err("Claude CLI batch response has an empty translation"));
                }
                out.push((*lang, text.to_string()));
            }
            by_id.insert(id, out);
        }
        let items = req
            .items
            .iter()
            .map(|i| {
                by_id
                    .remove(i.id.as_str())
                    .map(|t| (i.id.clone(), t))
                    .ok_or_else(|| err("Claude CLI batch response is missing ids"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BatchTranslationResult { items })
    }

    fn correct_batch(
        &self,
        req: &BatchCorrectionRequest,
    ) -> Result<BatchCorrectionResult, LlmError> {
        let parsed = self.ask_json(&batch_correction_prompt(req, &self.model))?;
        let entries = batch_entries(&parsed, "corrections")?;
        let mut by_id: HashMap<&str, String> = HashMap::new();
        for entry in entries {
            let (id, corrected) = entry
                .get("id")
                .and_then(Value::as_str)
                .zip(entry.get("corrected").and_then(Value::as_str))
                .ok_or_else(batch_malformed)?;
            let item = req
                .items
                .iter()
                .find(|i| i.id == id)
                .ok_or_else(|| err("Claude CLI batch response has an unknown id"))?;
            if by_id.contains_key(id) {
                return Err(err("Claude CLI batch response has a duplicate id"));
            }
            if corrected.trim().is_empty() && !item.text.trim().is_empty() {
                return Err(err("Claude CLI batch response has an empty correction"));
            }
            by_id.insert(id, corrected.to_string());
        }
        let items = req
            .items
            .iter()
            .map(|i| {
                by_id
                    .remove(i.id.as_str())
                    .map(|t| (i.id.clone(), t))
                    .ok_or_else(|| err("Claude CLI batch response is missing ids"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BatchCorrectionResult { items })
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
    fn spawn_failure_message_contains_the_shared_marker() {
        let p = provider(Err(CliError::Spawn("denied".into())));
        let m = msg(p.translate(&treq(&[Lang::En])));
        assert!(m.contains(crate::client::CLI_START_FAILURE_MARKER), "{m}");
        assert!(
            !format!("{}", CliError::Timeout).contains(crate::client::CLI_START_FAILURE_MARKER)
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

    fn bitem(id: &str, text: &str, dests: &[Lang]) -> ct_core::BatchItem {
        ct_core::BatchItem {
            id: id.into(),
            text: text.into(),
            source: Lang::Ko,
            destinations: dests.to_vec(),
        }
    }

    fn breq(items: Vec<ct_core::BatchItem>) -> ct_core::BatchTranslationRequest {
        ct_core::BatchTranslationRequest {
            items,
            tone: Tone::Neutral,
        }
    }

    fn one(dests: &[Lang]) -> ct_core::BatchTranslationRequest {
        breq(vec![bitem("c0", "가", dests)])
    }

    #[test]
    fn batch_prompt_embeds_items_as_json_and_keeps_placeholder_rule() {
        let t = fake(result(
            r#"{"translations":[{"id":"c0","texts":{"en":"x"}}]}"#,
        ));
        let p = ClaudeCliProvider::new(t.clone(), String::new());
        p.translate_batch(&breq(vec![bitem("c0", "a\tb\n\"q\" ⟦0⟧", &[Lang::En])]))
            .unwrap();
        let prompts = t.prompts.lock().unwrap();
        assert!(prompts[0].contains(r#""id": "c0""#));
        assert!(prompts[0].contains("⟦"));
        assert!(prompts[0].contains("\\t") && prompts[0].contains("\\n"));
    }

    #[test]
    fn batch_prompt_uses_soft_formatting_only_for_haiku() {
        let body = r#"{"translations":[{"id":"c0","texts":{"en":"x"}}]}"#;
        let strict = fake(result(body));
        let soft = fake(result(body));
        ClaudeCliProvider::new(strict.clone(), "sonnet".into())
            .translate_batch(&one(&[Lang::En]))
            .unwrap();
        ClaudeCliProvider::new(soft.clone(), "haiku".into())
            .translate_batch(&one(&[Lang::En]))
            .unwrap();
        assert!(strict.prompts.lock().unwrap()[0].contains("never reformat"));
        assert!(!soft.prompts.lock().unwrap()[0].contains("never reformat"));
    }

    #[test]
    fn batch_prompt_includes_tone_instruction() {
        let t = fake(result(
            r#"{"translations":[{"id":"c0","texts":{"en":"x"}}]}"#,
        ));
        let mut r = one(&[Lang::En]);
        r.tone = Tone::Formal;
        ClaudeCliProvider::new(t.clone(), String::new())
            .translate_batch(&r)
            .unwrap();
        assert!(t.prompts.lock().unwrap()[0].contains("formal"));
    }

    #[test]
    fn batch_success_returns_translations_in_request_order() {
        let p = provider(result(
            r#"{"translations":[{"id":"c1","texts":{"ja":"B","en":"b"}},{"id":"c0","texts":{"en":"A"}}]}"#,
        ));
        let r = p
            .translate_batch(&breq(vec![
                bitem("c0", "가", &[Lang::En]),
                bitem("c1", "나", &[Lang::En, Lang::Ja]),
            ]))
            .unwrap();
        assert_eq!(
            r.items,
            vec![
                ("c0".to_string(), vec![(Lang::En, "A".to_string())]),
                (
                    "c1".to_string(),
                    vec![(Lang::En, "b".to_string()), (Lang::Ja, "B".to_string())]
                ),
            ]
        );
    }

    #[test]
    fn batch_translation_rejects_wrong_language_keys() {
        for body in [
            r#"{"translations":[{"id":"c0","texts":{"en":"x"}}]}"#,
            r#"{"translations":[{"id":"c0","texts":{"en":"x","ko":"y"}}]}"#,
            r#"{"translations":[{"id":"c0","texts":{"en":"x","fr":"y"}}]}"#,
            r#"{"translations":[{"id":"c0","texts":["x","y"]}]}"#,
            r#"{"translations":[{"id":"c0","texts":{"en":"x","ja":5}}]}"#,
            r#"{"translations":[{"id":"c0","texts":{"en":"x","ja":" "}}]}"#,
        ] {
            let p = provider(result(body));
            assert!(
                p.translate_batch(&one(&[Lang::En, Lang::Ja])).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn batch_translation_violations_use_exact_messages() {
        let cases = [
            (
                r#"{"translations":[]}"#,
                "Claude CLI batch response is missing ids",
            ),
            (
                r#"{"translations":[{"id":"c0","texts":{"en":"x"}},{"id":"c0","texts":{"en":"y"}}]}"#,
                "Claude CLI batch response has a duplicate id",
            ),
            (
                r#"{"translations":[{"id":"c0","texts":{"en":"x"}},{"id":"zz","texts":{"en":"y"}}]}"#,
                "Claude CLI batch response has an unknown id",
            ),
            (
                r#"{"translations":[{"id":"c0","texts":{"en":""}}]}"#,
                "Claude CLI batch response has an empty translation",
            ),
            (
                r#"{"translations":[{"id":"c0","texts":{"en":5}}]}"#,
                "Claude CLI batch response has a malformed entry",
            ),
            (
                r#"{"translations":[{"id":0,"texts":{"en":"x"}}]}"#,
                "Claude CLI batch response has a malformed entry",
            ),
            (
                r#"{"translations":["c0"]}"#,
                "Claude CLI batch response has a malformed entry",
            ),
            (
                r#"{"translations":"nope"}"#,
                "Claude CLI batch response missing 'translations' list",
            ),
            (
                r#"{"translation":"x"}"#,
                "Claude CLI batch response missing 'translations' list",
            ),
            ("not json", "Claude CLI returned a malformed JSON response"),
        ];
        for (body, expected) in cases {
            let p = provider(result(body));
            assert_eq!(
                msg(p.translate_batch(&one(&[Lang::En]))),
                expected,
                "{body}"
            );
        }
    }

    #[test]
    fn batch_translation_allows_blank_result_for_blank_source() {
        let p = provider(result(
            r#"{"translations":[{"id":"c0","texts":{"en":""}}]}"#,
        ));
        let r = p
            .translate_batch(&breq(vec![bitem("c0", "  ", &[Lang::En])]))
            .unwrap();
        assert_eq!(r.items[0].1, vec![(Lang::En, String::new())]);
    }

    fn cbreq(ids: &[&str]) -> ct_core::BatchCorrectionRequest {
        ct_core::BatchCorrectionRequest {
            items: ids
                .iter()
                .map(|id| ct_core::BatchCorrectionItem {
                    id: (*id).into(),
                    text: "가".into(),
                })
                .collect(),
            mode: CorrectionMode::Spelling,
        }
    }

    #[test]
    fn batch_correction_success_and_violations() {
        let p = provider(result(
            r#"{"corrections":[{"id":"c1","corrected":"B"},{"id":"c0","corrected":"A"}]}"#,
        ));
        let r = p.correct_batch(&cbreq(&["c0", "c1"])).unwrap();
        assert_eq!(
            r.items,
            vec![("c0".into(), "A".into()), ("c1".into(), "B".into())]
        );

        let cases = [
            (
                r#"{"corrections":[]}"#,
                "Claude CLI batch response is missing ids",
            ),
            (
                r#"{"corrections":[{"id":"c0","corrected":""}]}"#,
                "Claude CLI batch response has an empty correction",
            ),
            (
                r#"{"corrections":[{"id":"c0","corrected":1}]}"#,
                "Claude CLI batch response has a malformed entry",
            ),
            (
                r#"{"corrections":[{"id":"q","corrected":"x"}]}"#,
                "Claude CLI batch response has an unknown id",
            ),
            (
                r#"{"translations":[]}"#,
                "Claude CLI batch response missing 'corrections' list",
            ),
        ];
        for (body, expected) in cases {
            let p = provider(result(body));
            assert_eq!(msg(p.correct_batch(&cbreq(&["c0"]))), expected, "{body}");
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
