//! Test helpers shared across ct-core unit tests.

use crate::llm::{
    BatchCorrectionRequest, BatchCorrectionResult, BatchTranslationRequest, BatchTranslationResult,
    CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationResult, LlmError,
    LlmProvider, TranslationRequest, TranslationResult,
};
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

/// Scripted `LlmProvider`. Queue responses per method with `push_*` (chainable); each call
/// pops the next one (an unscripted call returns `Err(LlmError("FakeLlm: no response queued"))`).
/// Every request is recorded: read them with `*_requests()`, count with `*_calls()`.
#[derive(Default)]
pub(crate) struct FakeLlm {
    translate: Mutex<Script<TranslationRequest, TranslationResult>>,
    explain: Mutex<Script<ExplanationRequest, ExplanationResult>>,
    correct: Mutex<Script<CorrectionRequest, String>>,
    correct_sentences: Mutex<Script<CorrectionRequest, Vec<CorrectionSentence>>>,
    translate_batch: Mutex<Script<BatchTranslationRequest, BatchTranslationResult>>,
    correct_batch: Mutex<Script<BatchCorrectionRequest, BatchCorrectionResult>>,
    batch_responder: Mutex<Option<BatchResponder>>,
    correct_responder: Mutex<Option<CorrectBatchResponder>>,
}

/// Computes a batch translation reply from the request when no scripted reply is queued.
pub(crate) type BatchResponder =
    Box<dyn Fn(&BatchTranslationRequest) -> Result<BatchTranslationResult, LlmError> + Send + Sync>;

/// Computes a batch correction reply from the request when no scripted reply is queued.
pub(crate) type CorrectBatchResponder =
    Box<dyn Fn(&BatchCorrectionRequest) -> Result<BatchCorrectionResult, LlmError> + Send + Sync>;

struct Script<Req, Res> {
    responses: VecDeque<Result<Res, LlmError>>,
    requests: Vec<Req>,
}

impl<Req, Res> Default for Script<Req, Res> {
    fn default() -> Self {
        Self {
            responses: VecDeque::new(),
            requests: Vec::new(),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn call<Req: Clone, Res>(m: &Mutex<Script<Req, Res>>, req: &Req) -> Result<Res, LlmError> {
    let mut s = lock(m);
    s.requests.push(req.clone());
    s.responses
        .pop_front()
        .unwrap_or_else(|| Err(LlmError("FakeLlm: no response queued".into())))
}

macro_rules! accessors {
    ($field:ident, $req:ty, $res:ty, $push:ident, $requests:ident, $calls:ident) => {
        pub(crate) fn $push(&self, r: Result<$res, LlmError>) -> &Self {
            lock(&self.$field).responses.push_back(r);
            self
        }
        pub(crate) fn $requests(&self) -> Vec<$req> {
            lock(&self.$field).requests.clone()
        }
        pub(crate) fn $calls(&self) -> usize {
            lock(&self.$field).requests.len()
        }
    };
}

#[allow(dead_code)] // used by later tasks' tests
impl FakeLlm {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    accessors!(
        translate,
        TranslationRequest,
        TranslationResult,
        push_translate,
        translate_requests,
        translate_calls
    );
    accessors!(
        explain,
        ExplanationRequest,
        ExplanationResult,
        push_explain,
        explain_requests,
        explain_calls
    );
    accessors!(
        correct,
        CorrectionRequest,
        String,
        push_correct,
        correct_requests,
        correct_calls
    );
    accessors!(
        correct_sentences,
        CorrectionRequest,
        Vec<CorrectionSentence>,
        push_correct_sentences,
        correct_sentences_requests,
        correct_sentences_calls
    );
    accessors!(
        translate_batch,
        BatchTranslationRequest,
        BatchTranslationResult,
        push_translate_batch,
        translate_batch_requests,
        translate_batch_calls
    );
    accessors!(
        correct_batch,
        BatchCorrectionRequest,
        BatchCorrectionResult,
        push_correct_batch,
        correct_batch_requests,
        correct_batch_calls
    );

    /// Replies to every unscripted `translate_batch` call by computing from the request.
    pub(crate) fn set_batch_responder(&self, r: BatchResponder) -> &Self {
        *lock(&self.batch_responder) = Some(r);
        self
    }

    /// Replies to every unscripted `correct_batch` call by computing from the request.
    pub(crate) fn set_correct_responder(&self, r: CorrectBatchResponder) -> &Self {
        *lock(&self.correct_responder) = Some(r);
        self
    }

    /// Responder that answers each item with `"[교정] <text>"`.
    pub(crate) fn echo_correct_responder() -> CorrectBatchResponder {
        Box::new(|req| {
            Ok(BatchCorrectionResult {
                items: req
                    .items
                    .iter()
                    .map(|i| (i.id.clone(), format!("[교정] {}", i.text)))
                    .collect(),
            })
        })
    }

    /// Responder that answers each item with `"[<lang>] <text>"` per destination.
    pub(crate) fn echo_batch_responder() -> BatchResponder {
        Box::new(|req| {
            Ok(BatchTranslationResult {
                items: req
                    .items
                    .iter()
                    .map(|i| {
                        (
                            i.id.clone(),
                            i.destinations
                                .iter()
                                .map(|l| (*l, format!("[{}] {}", l.code(), i.text)))
                                .collect(),
                        )
                    })
                    .collect(),
            })
        })
    }
}

impl LlmProvider for FakeLlm {
    fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, LlmError> {
        call(&self.translate, req)
    }
    fn explain(&self, req: &ExplanationRequest) -> Result<ExplanationResult, LlmError> {
        call(&self.explain, req)
    }
    fn correct(&self, req: &CorrectionRequest) -> Result<String, LlmError> {
        call(&self.correct, req)
    }
    fn correct_sentences(
        &self,
        req: &CorrectionRequest,
    ) -> Result<Vec<CorrectionSentence>, LlmError> {
        call(&self.correct_sentences, req)
    }
    fn translate_batch(
        &self,
        req: &BatchTranslationRequest,
    ) -> Result<BatchTranslationResult, LlmError> {
        let queued = {
            let mut s = lock(&self.translate_batch);
            s.requests.push(req.clone());
            s.responses.pop_front()
        };
        if let Some(r) = queued {
            return r;
        }
        match lock(&self.batch_responder).as_ref() {
            Some(f) => f(req),
            None => Err(LlmError("FakeLlm: no response queued".into())),
        }
    }
    fn correct_batch(
        &self,
        req: &BatchCorrectionRequest,
    ) -> Result<BatchCorrectionResult, LlmError> {
        let queued = {
            let mut s = lock(&self.correct_batch);
            s.requests.push(req.clone());
            s.responses.pop_front()
        };
        if let Some(r) = queued {
            return r;
        }
        match lock(&self.correct_responder).as_ref() {
            Some(f) => f(req),
            None => Err(LlmError("FakeLlm: no response queued".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;
    use crate::settings::{CorrectionMode, Tone};

    #[test]
    fn fake_llm_pops_in_order_and_records_requests() {
        let fake = FakeLlm::new();
        fake.push_translate(Ok(TranslationResult {
            translations: vec![(Lang::En, "Hi".into())],
        }))
        .push_translate(Err(LlmError("boom".into())));
        let req = TranslationRequest {
            text: "안녕".into(),
            source: Lang::Ko,
            destinations: vec![Lang::En],
            tone: Tone::Neutral,
        };
        assert!(fake.translate(&req).is_ok());
        assert_eq!(fake.translate(&req), Err(LlmError("boom".into())));
        assert!(fake.translate(&req).is_err());
        assert_eq!(fake.translate_calls(), 3);
        assert_eq!(fake.translate_requests()[0].destinations, vec![Lang::En]);
        assert_eq!(fake.correct_calls(), 0);
        let c = CorrectionRequest {
            text: "x".into(),
            mode: CorrectionMode::Spelling,
        };
        fake.push_correct(Ok("y".into()));
        assert_eq!(fake.correct(&c).unwrap(), "y");
    }
}
