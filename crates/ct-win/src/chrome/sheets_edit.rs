//! Sheets 셀 편집 교체 안전 래퍼 (v5 `SheetsEditSafeInteraction`, UI-ref §2.7, S-ref §5.3).
//!
//! `google_sheets` 컨텍스트에만 적용한다. 다른 앱은 그대로 통과한다.

use super::adapter::{CaptureMiss, CapturedText, ReplaceError, TextInteraction};

const SHEETS_APP: &str = "google_sheets";

pub struct SheetsEditSafe<T: TextInteraction> {
    inner: T,
    allow_full_cell_replace: bool,
}

impl<T: TextInteraction> SheetsEditSafe<T> {
    pub fn new(inner: T, allow_full_cell_replace: bool) -> Self {
        Self {
            inner,
            allow_full_cell_replace,
        }
    }
}

/// 앞 공백을 무시하고 `=` 또는 `+`로 시작하면 Sheets가 수식으로 해석한다.
fn starts_with_formula_prefix(text: &str) -> bool {
    text.trim_start().starts_with(['=', '+'])
}

impl<T: TextInteraction> TextInteraction for SheetsEditSafe<T> {
    fn capture(&self) -> Option<CapturedText> {
        self.inner.capture()
    }

    /// 그대로 위임해 `RetryingInteraction`의 NoContext 즉시 종료가 유지되게 한다.
    fn try_capture(&self) -> Result<CapturedText, CaptureMiss> {
        self.inner.try_capture()
    }

    fn replace(&self, captured: &CapturedText, new_text: &str) -> Result<(), ReplaceError> {
        if captured.context.app != SHEETS_APP {
            return self.inner.replace(captured, new_text);
        }
        if starts_with_formula_prefix(new_text) {
            tracing::info!("sheets edit replace refused: formula prefix");
            return Err(ReplaceError::FormulaPrefix);
        }
        if !captured.context.has_selection && !self.allow_full_cell_replace {
            tracing::info!("sheets edit replace refused: full-cell replace disabled");
            return Err(ReplaceError::FullCellNotEnabled);
        }
        // 다시 캡처해 컨텍스트 전체(앱, 필드, 선택 여부, 텍스트)가 같을 때만 교체한다.
        match self.inner.try_capture() {
            Ok(now) if now.context == captured.context => {}
            _ => {
                tracing::info!("sheets edit replace refused: target changed");
                return Err(ReplaceError::ContextChanged);
            }
        }
        self.inner.replace(captured, new_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::strategy::EditingContext;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// `captures`를 capture 호출마다 하나씩 돌려준다(마지막 값이 반복된다).
    struct Recording {
        captures: Vec<Result<CapturedText, CaptureMiss>>,
        capture_calls: AtomicUsize,
        replace_calls: Mutex<Vec<(CapturedText, String)>>,
    }

    impl Recording {
        fn new(captures: Vec<Option<CapturedText>>) -> Self {
            Self::with_results(
                captures
                    .into_iter()
                    .map(|c| c.ok_or(CaptureMiss::NoContext))
                    .collect(),
            )
        }
        fn with_results(captures: Vec<Result<CapturedText, CaptureMiss>>) -> Self {
            Self {
                captures,
                capture_calls: AtomicUsize::new(0),
                replace_calls: Mutex::new(Vec::new()),
            }
        }
        fn replaces(&self) -> Vec<(CapturedText, String)> {
            self.replace_calls.lock().unwrap().clone()
        }
    }

    impl TextInteraction for Recording {
        fn capture(&self) -> Option<CapturedText> {
            self.try_capture().ok()
        }
        fn try_capture(&self) -> Result<CapturedText, CaptureMiss> {
            let i = self.capture_calls.fetch_add(1, Ordering::SeqCst);
            self.captures[i.min(self.captures.len() - 1)].clone()
        }
        fn replace(&self, captured: &CapturedText, new_text: &str) -> Result<(), ReplaceError> {
            self.replace_calls
                .lock()
                .unwrap()
                .push((captured.clone(), new_text.to_string()));
            Ok(())
        }
    }

    fn captured(app: &'static str, has_selection: bool, text: &str) -> CapturedText {
        CapturedText {
            context: EditingContext {
                app,
                field_id: "sheets_cell_edit",
                has_selection,
                text: text.to_string(),
            },
        }
    }

    fn sel(text: &str) -> CapturedText {
        captured("google_sheets", true, text)
    }

    #[test]
    fn edit_replace_rejects_translation_starting_with_formula_prefix() {
        for bad in ["=foo", "  =foo", "+1+1", "\t=SUM(A1)"] {
            let cap = sel("x");
            let safe = SheetsEditSafe::new(Recording::new(vec![Some(cap.clone())]), false);
            assert_eq!(
                safe.replace(&cap, bad),
                Err(ReplaceError::FormulaPrefix),
                "{bad:?}"
            );
            assert!(safe.inner.replaces().is_empty());
        }
    }

    #[test]
    fn edit_replace_delegates_normal_text_in_sheets() {
        let cap = sel("x");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(cap.clone())]), false);
        assert_eq!(safe.replace(&cap, "Hello = world"), Ok(()));
        assert_eq!(
            safe.inner.replaces(),
            vec![(cap, "Hello = world".to_string())]
        );
    }

    #[test]
    fn other_apps_are_never_filtered_or_rechecked() {
        let cap = captured("google_chat", true, "x");
        let safe = SheetsEditSafe::new(Recording::new(vec![None]), false);
        assert_eq!(safe.replace(&cap, "=not a formula in chat"), Ok(()));
        assert_eq!(
            safe.inner.replaces(),
            vec![(cap, "=not a formula in chat".to_string())]
        );
        assert_eq!(safe.inner.capture_calls.load(Ordering::SeqCst), 0);
        // 선택 없는 다른 앱 교체도 전체 교체 설정과 무관하다.
        let cap = captured("gmail", false, "body");
        let safe = SheetsEditSafe::new(Recording::new(vec![None]), false);
        assert_eq!(safe.replace(&cap, "new"), Ok(()));
    }

    #[test]
    fn capture_is_delegated_unchanged() {
        let cap = sel("x");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(cap.clone())]), false);
        assert_eq!(safe.capture(), Some(cap));
        assert_eq!(safe.inner.capture_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sheets_edit_safe_forwards_try_capture_miss_kind() {
        for miss in [
            CaptureMiss::NoFocus,
            CaptureMiss::Unsupported,
            CaptureMiss::NoContext,
        ] {
            let safe = SheetsEditSafe::new(Recording::with_results(vec![Err(miss)]), true);
            assert_eq!(safe.try_capture(), Err(miss));
            assert_eq!(safe.inner.capture_calls.load(Ordering::SeqCst), 1);
        }
        let cap = sel("x");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(cap.clone())]), true);
        assert_eq!(safe.try_capture(), Ok(cap));
    }

    #[test]
    fn full_cell_replacement_blocked_by_default() {
        let cap = captured("google_sheets", false, "x");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(cap.clone())]), false);
        assert_eq!(
            safe.replace(&cap, "Hello"),
            Err(ReplaceError::FullCellNotEnabled)
        );
        assert!(safe.inner.replaces().is_empty());
        assert_eq!(safe.inner.capture_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn full_cell_replacement_allowed_when_explicitly_enabled() {
        let cap = captured("google_sheets", false, "x");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(cap.clone())]), true);
        assert_eq!(safe.replace(&cap, "Hello"), Ok(()));
        assert_eq!(safe.inner.replaces(), vec![(cap, "Hello".to_string())]);
    }

    #[test]
    fn selection_replacement_never_needs_the_flag() {
        let cap = sel("x");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(cap.clone())]), false);
        assert_eq!(safe.replace(&cap, "Hello"), Ok(()));
    }

    #[test]
    fn replace_aborts_when_the_edited_text_changed_since_capture() {
        let original = sel("안녕");
        let moved_on = sel("다른 셀의 글자");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(moved_on)]), false);
        assert_eq!(
            safe.replace(&original, "Hello"),
            Err(ReplaceError::ContextChanged)
        );
        assert!(safe.inner.replaces().is_empty());
    }

    #[test]
    fn replace_aborts_when_the_edit_context_is_gone() {
        let original = sel("x");
        let safe = SheetsEditSafe::new(Recording::new(vec![None]), false);
        assert_eq!(
            safe.replace(&original, "Hello"),
            Err(ReplaceError::ContextChanged)
        );
        assert!(safe.inner.replaces().is_empty());
    }

    #[test]
    fn replace_aborts_when_selection_state_changed_since_capture() {
        let original = captured("google_sheets", true, "안녕");
        let now = captured("google_sheets", false, "안녕");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(now)]), true);
        assert_eq!(
            safe.replace(&original, "Hello"),
            Err(ReplaceError::ContextChanged)
        );
        assert!(safe.inner.replaces().is_empty());
    }

    #[test]
    fn replace_aborts_when_recaptured_app_differs() {
        let original = sel("x");
        let now = captured("google_chat", true, "x");
        let safe = SheetsEditSafe::new(Recording::new(vec![Some(now)]), true);
        assert_eq!(
            safe.replace(&original, "Hello"),
            Err(ReplaceError::ContextChanged)
        );
    }
}
