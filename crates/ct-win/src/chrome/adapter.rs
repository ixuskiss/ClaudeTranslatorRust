//! Chrome 어댑터와 캡처 재시도 (v5 참조 §2.4, §2.5).
//!
//! 교체 직전에 포커스를 다시 조사해 캡처 때와 같은 앱·필드인지 확인하고,
//! 달라졌으면 아무것도 건드리지 않고 중단한다. 텍스트는 로그에 남기지 않는다.

use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use super::strategy::{EditingContext, FocusedElement, Strategy};
use crate::clipboard::{Clipboard, run_with_temporary_content};

const RETRY_ATTEMPTS: usize = 5;
const RETRY_DELAY: Duration = Duration::from_millis(80);

pub trait UiaBackend: Send + Sync {
    fn focused_element(&self) -> Option<FocusedElement>;
    fn foreground_is_docs_or_slides(&self) -> bool;
    fn paste_over_selection(&self) -> Result<(), String>;
    fn paste_replacing_full_text(&self) -> Result<(), String>;
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CapturedText {
    pub context: EditingContext,
}

pub trait TextInteraction: Send + Sync {
    fn capture(&self) -> Option<CapturedText>;
    fn replace(&self, captured: &CapturedText, new_text: &str) -> Result<(), ReplaceError>;
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum ReplaceError {
    #[error("Nothing is focused anymore; aborting to protect original text")]
    NothingFocused,
    #[error("Focus moved to an unsupported app; aborting replace")]
    Unsupported,
    #[error("Editing context changed since capture; aborting replace to protect original text")]
    ContextChanged,
    #[error("{0}")]
    Paste(String),
}

pub struct ChromeAdapter {
    backend: Arc<dyn UiaBackend>,
    clipboard: Arc<dyn Clipboard>,
    strategies: &'static [Strategy],
}

impl ChromeAdapter {
    pub fn new(
        backend: Arc<dyn UiaBackend>,
        clipboard: Arc<dyn Clipboard>,
        strategies: &'static [Strategy],
    ) -> Self {
        Self {
            backend,
            clipboard,
            strategies,
        }
    }

    /// 목록 순서상 처음 맞는 전략만 쓴다. 그 전략이 None이면 다음 전략으로 넘어가지 않는다.
    fn strategy_for(&self, focused: &FocusedElement) -> Option<Strategy> {
        self.strategies.iter().copied().find(|s| s.matches(focused))
    }
}

impl TextInteraction for ChromeAdapter {
    fn capture(&self) -> Option<CapturedText> {
        let focused = self.backend.focused_element()?;
        let context = self.strategy_for(&focused)?.identify_context(&focused)?;
        Some(CapturedText { context })
    }

    fn replace(&self, captured: &CapturedText, new_text: &str) -> Result<(), ReplaceError> {
        // 아무것도 건드리기 전에 전체 조사를 다시 하고 검증한다.
        let focused = self
            .backend
            .focused_element()
            .ok_or(ReplaceError::NothingFocused)?;
        let strategy = self
            .strategy_for(&focused)
            .ok_or(ReplaceError::Unsupported)?;
        let now = strategy
            .identify_context(&focused)
            .ok_or(ReplaceError::ContextChanged)?;
        // 앱과 필드만 비교한다. 번역 중 텍스트나 선택 여부가 바뀌어도 같은 필드면 교체한다.
        if now.app != captured.context.app || now.field_id != captured.context.field_id {
            return Err(ReplaceError::ContextChanged);
        }
        let has_selection = captured.context.has_selection;
        run_with_temporary_content(self.clipboard.as_ref(), new_text, || {
            if has_selection {
                self.backend.paste_over_selection()
            } else {
                self.backend.paste_replacing_full_text()
            }
        })
        .map_err(ReplaceError::Paste)
    }
}

/// 캡처만 재시도하는 최외곽 래퍼. 교체는 그대로 전달한다.
pub struct RetryingInteraction<T: TextInteraction> {
    inner: T,
    attempts: usize,
    delay: Duration,
}

impl<T: TextInteraction> RetryingInteraction<T> {
    pub fn new(inner: T) -> Self {
        Self::with_delay(inner, RETRY_ATTEMPTS, RETRY_DELAY)
    }

    pub fn with_delay(inner: T, attempts: usize, delay: Duration) -> Self {
        Self {
            inner,
            attempts,
            delay,
        }
    }
}

impl<T: TextInteraction> TextInteraction for RetryingInteraction<T> {
    fn capture(&self) -> Option<CapturedText> {
        for attempt in 0..self.attempts {
            if let Some(c) = self.inner.capture() {
                return Some(c);
            }
            if attempt + 1 < self.attempts {
                sleep(self.delay);
            }
        }
        None
    }

    fn replace(&self, captured: &CapturedText, new_text: &str) -> Result<(), ReplaceError> {
        self.inner.replace(captured, new_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::role::Role;
    use crate::chrome::strategy::REPLACE_STRATEGIES;
    use crate::clipboard::ClipboardError;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBackend {
        queue: Mutex<Vec<Option<FocusedElement>>>,
        probes: AtomicUsize,
        pastes: Mutex<Vec<(&'static str, Option<String>)>>,
        clipboard: Arc<MemClipboard>,
        fail_paste: bool,
    }

    impl FakeBackend {
        fn new(queue: Vec<Option<FocusedElement>>, clipboard: Arc<MemClipboard>) -> Arc<Self> {
            Arc::new(Self {
                queue: Mutex::new(queue),
                probes: AtomicUsize::new(0),
                pastes: Mutex::new(Vec::new()),
                clipboard,
                fail_paste: false,
            })
        }
        fn paste_count(&self) -> usize {
            self.pastes.lock().unwrap().len()
        }
        fn record(&self, kind: &'static str) -> Result<(), String> {
            self.pastes
                .lock()
                .unwrap()
                .push((kind, self.clipboard.get_text()));
            if self.fail_paste {
                Err("paste failed".into())
            } else {
                Ok(())
            }
        }
    }

    impl UiaBackend for FakeBackend {
        fn focused_element(&self) -> Option<FocusedElement> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            let mut q = self.queue.lock().unwrap();
            if q.len() > 1 {
                q.remove(0)
            } else {
                q[0].clone()
            }
        }
        fn foreground_is_docs_or_slides(&self) -> bool {
            false
        }
        fn paste_over_selection(&self) -> Result<(), String> {
            self.record("selection")
        }
        fn paste_replacing_full_text(&self) -> Result<(), String> {
            self.record("full")
        }
    }

    struct MemClipboard {
        text: Mutex<Option<String>>,
    }
    impl MemClipboard {
        fn new(t: &str) -> Arc<Self> {
            Arc::new(Self {
                text: Mutex::new(Some(t.to_string())),
            })
        }
    }
    impl Clipboard for MemClipboard {
        fn get_text(&self) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
        fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
            *self.text.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
        fn sequence_number(&self) -> u32 {
            0
        }
    }

    fn chat(selection: Option<&str>) -> FocusedElement {
        FocusedElement {
            url: "https://chat.google.com/room/x".into(),
            role: Role::Compose,
            name: "Message".into(),
            full_text: "hello world".into(),
            selection_text: selection.map(String::from),
        }
    }

    fn gmail(role: Role, full: &str) -> FocusedElement {
        FocusedElement {
            url: "https://mail.google.com/mail/u/0/#inbox".into(),
            role,
            name: String::new(),
            full_text: full.into(),
            selection_text: None,
        }
    }

    fn adapter(
        queue: Vec<Option<FocusedElement>>,
    ) -> (ChromeAdapter, Arc<FakeBackend>, Arc<MemClipboard>) {
        let cb = MemClipboard::new("old");
        let backend = FakeBackend::new(queue, cb.clone());
        let a = ChromeAdapter::new(backend.clone(), cb.clone(), &REPLACE_STRATEGIES);
        (a, backend, cb)
    }

    #[test]
    fn capture_none_when_nothing_focused_or_unsupported_or_unsafe() {
        assert!(adapter(vec![None]).0.capture().is_none());
        let mut e = chat(None);
        e.url = "https://example.com/x".into();
        assert!(adapter(vec![Some(e)]).0.capture().is_none());
        let mut e = chat(None);
        e.role = Role::MessageHistory;
        assert!(adapter(vec![Some(e)]).0.capture().is_none());
    }

    #[test]
    fn capture_returns_selection_or_full_text() {
        let c = adapter(vec![Some(chat(Some("hello")))])
            .0
            .capture()
            .unwrap();
        assert!(c.context.has_selection);
        assert_eq!(c.context.text, "hello");
        let c = adapter(vec![Some(chat(None))]).0.capture().unwrap();
        assert!(!c.context.has_selection);
        assert_eq!(c.context.text, "hello world");
    }

    #[test]
    fn capture_uses_first_matching_strategy_without_fallthrough() {
        // Gmail 전략이 먼저 맞고 To 필드라 None. 뒤의 전략으로 넘어가면 안 된다.
        static ORDER: [Strategy; 2] = [Strategy::Gmail, Strategy::GmailReadOnly];
        let cb = MemClipboard::new("old");
        let mut e = gmail(Role::To, "a@b.c");
        e.selection_text = Some("a@b.c".into());
        let backend = FakeBackend::new(vec![Some(e)], cb.clone());
        let a = ChromeAdapter::new(backend, cb, &ORDER);
        assert!(a.capture().is_none());
    }

    #[test]
    fn replace_aborts_when_focus_moved_to_other_field() {
        let (a, backend, _) = adapter(vec![
            Some(gmail(Role::Subject, "subj")),
            Some(gmail(Role::Body, "body")),
        ]);
        let captured = a.capture().unwrap();
        assert_eq!(captured.context.field_id, "subject");
        assert_eq!(
            a.replace(&captured, "new"),
            Err(ReplaceError::ContextChanged)
        );
        assert_eq!(backend.paste_count(), 0);
    }

    #[test]
    fn replace_aborts_on_nothing_focused_unsupported_and_unsafe_context() {
        let (a, backend, _) = adapter(vec![Some(chat(None)), None]);
        let c = a.capture().unwrap();
        assert_eq!(a.replace(&c, "x"), Err(ReplaceError::NothingFocused));
        assert_eq!(backend.paste_count(), 0);

        let mut other = chat(None);
        other.url = "https://example.com".into();
        let (a, backend, _) = adapter(vec![Some(chat(None)), Some(other)]);
        let c = a.capture().unwrap();
        assert_eq!(a.replace(&c, "x"), Err(ReplaceError::Unsupported));
        assert_eq!(backend.paste_count(), 0);

        let mut hist = chat(None);
        hist.role = Role::MessageHistory;
        let (a, backend, _) = adapter(vec![Some(chat(None)), Some(hist)]);
        let c = a.capture().unwrap();
        assert_eq!(a.replace(&c, "x"), Err(ReplaceError::ContextChanged));
        assert_eq!(backend.paste_count(), 0);
    }

    #[test]
    fn replace_aborts_when_focus_moved_to_different_app() {
        let (a, backend, _) = adapter(vec![Some(chat(None)), Some(gmail(Role::Body, "b"))]);
        let c = a.capture().unwrap();
        assert_eq!(a.replace(&c, "x"), Err(ReplaceError::ContextChanged));
        assert_eq!(backend.paste_count(), 0);
    }

    #[test]
    fn replace_compares_only_app_and_field() {
        let mut changed = chat(Some("different"));
        changed.full_text = "totally different".into();
        let (a, backend, _) = adapter(vec![Some(chat(None)), Some(changed)]);
        let c = a.capture().unwrap();
        assert_eq!(a.replace(&c, "new"), Ok(()));
        // 캡처 당시(선택 없음) 기준으로 전체 교체한다.
        assert_eq!(backend.pastes.lock().unwrap()[0].0, "full");
    }

    #[test]
    fn replace_with_selection_pastes_over_selection() {
        let (a, backend, _) = adapter(vec![Some(chat(Some("hello")))]);
        let c = a.capture().unwrap();
        a.replace(&c, "new").unwrap();
        let pastes = backend.pastes.lock().unwrap();
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].0, "selection");
    }

    #[test]
    fn replace_without_selection_reselects_all_then_pastes() {
        let (a, backend, _) = adapter(vec![Some(chat(None))]);
        let c = a.capture().unwrap();
        a.replace(&c, "new").unwrap();
        let pastes = backend.pastes.lock().unwrap();
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].0, "full");
    }

    #[test]
    fn replace_reprobes_focus_before_pasting() {
        let (a, backend, _) = adapter(vec![Some(chat(None))]);
        let c = a.capture().unwrap();
        a.replace(&c, "new").unwrap();
        assert_eq!(backend.probes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn replace_puts_new_text_on_clipboard_during_paste_and_restores() {
        let (a, backend, cb) = adapter(vec![Some(chat(None))]);
        let c = a.capture().unwrap();
        a.replace(&c, "new text").unwrap();
        assert_eq!(
            backend.pastes.lock().unwrap()[0].1.as_deref(),
            Some("new text")
        );
        assert_eq!(cb.get_text().as_deref(), Some("old"));
    }

    #[test]
    fn replace_paste_failure_maps_to_paste_error_and_restores_clipboard() {
        let cb = MemClipboard::new("old");
        let backend = Arc::new(FakeBackend {
            queue: Mutex::new(vec![Some(chat(None))]),
            probes: AtomicUsize::new(0),
            pastes: Mutex::new(Vec::new()),
            clipboard: cb.clone(),
            fail_paste: true,
        });
        let a = ChromeAdapter::new(backend, cb.clone(), &REPLACE_STRATEGIES);
        let c = a.capture().unwrap();
        assert_eq!(
            a.replace(&c, "new"),
            Err(ReplaceError::Paste("paste failed".into()))
        );
        assert_eq!(cb.get_text().as_deref(), Some("old"));
    }

    struct FakeInteraction {
        results: Vec<Option<CapturedText>>,
        capture_calls: AtomicUsize,
        replace_calls: AtomicUsize,
    }
    impl FakeInteraction {
        fn new(results: Vec<Option<CapturedText>>) -> Self {
            Self {
                results,
                capture_calls: AtomicUsize::new(0),
                replace_calls: AtomicUsize::new(0),
            }
        }
    }
    impl TextInteraction for FakeInteraction {
        fn capture(&self) -> Option<CapturedText> {
            let i = self.capture_calls.fetch_add(1, Ordering::SeqCst);
            self.results[i.min(self.results.len() - 1)].clone()
        }
        fn replace(&self, _: &CapturedText, _: &str) -> Result<(), ReplaceError> {
            self.replace_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn cap(text: &str) -> CapturedText {
        CapturedText {
            context: EditingContext {
                app: "google_chat",
                field_id: "compose",
                has_selection: false,
                text: text.into(),
            },
        }
    }

    const NO_DELAY: Duration = Duration::from_millis(0);

    #[test]
    fn retrying_capture_returns_first_success_without_more_calls() {
        let r = RetryingInteraction::with_delay(
            FakeInteraction::new(vec![Some(cap("first"))]),
            5,
            NO_DELAY,
        );
        assert_eq!(r.capture().unwrap().context.text, "first");
        assert_eq!(r.inner.capture_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retrying_capture_retries_until_success() {
        let r = RetryingInteraction::with_delay(
            FakeInteraction::new(vec![None, None, Some(cap("third"))]),
            5,
            NO_DELAY,
        );
        assert_eq!(r.capture().unwrap().context.text, "third");
        assert_eq!(r.inner.capture_calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn retrying_capture_tries_five_times_then_none() {
        let r = RetryingInteraction::new(FakeInteraction::new(vec![None]));
        let start = std::time::Instant::now();
        assert!(r.capture().is_none());
        assert_eq!(r.inner.capture_calls.load(Ordering::SeqCst), 5);
        // 시도 사이에만 80 ms씩 4번 쉰다 (마지막 뒤에는 쉬지 않는다).
        assert!(start.elapsed() >= Duration::from_millis(320));
    }

    #[test]
    fn retrying_replace_is_not_retried() {
        let r = RetryingInteraction::with_delay(FakeInteraction::new(vec![None]), 5, NO_DELAY);
        assert_eq!(r.replace(&cap("x"), "new"), Ok(()));
        assert_eq!(r.inner.replace_calls.load(Ordering::SeqCst), 1);
    }
}
