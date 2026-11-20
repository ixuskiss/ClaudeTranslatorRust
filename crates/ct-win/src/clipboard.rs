//! 클립보드 보존·복원과 sentinel 복사 (v5 참조 §2.2, §2.3, §2.6 3단계).
//!
//! 클립보드 내용은 절대 로그에 남기지 않는다.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// 프로세스 전체의 클립보드·합성 키 구간 (R8). Alt 계열과 Ctrl 계열이 동시에 돌아도
/// 캡처(Ctrl+C·sentinel)와 교체(재조사·붙여넣기·복원)가 서로 섞이지 않게 한다.
static CLIPBOARD_SECTION: Mutex<()> = Mutex::new(());

/// 구간 잠금을 얻는다. 이전 보유자가 패닉했어도(poison) 그대로 이어서 쓴다.
pub(crate) fn lock_clipboard_section() -> MutexGuard<'static, ()> {
    CLIPBOARD_SECTION
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// 선택 영역이 없을 때 복사 결과와 구분하기 위한 탐지용 값.
pub const SENTINEL: &str = "\u{0}claudetranslator_probe\u{0}";

/// Ctrl+C 이후 클립보드 시퀀스 번호가 바뀔 때까지 기다리는 최대 시간.
pub const COPY_MAX_WAIT: Duration = Duration::from_millis(500);
/// 시퀀스 번호가 바뀐 뒤 추가로 두는 정착 시간.
pub const COPY_SETTLE: Duration = Duration::from_millis(50);
const COPY_POLL: Duration = Duration::from_millis(10);
const SET_ATTEMPTS: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("클립보드를 열 수 없습니다")]
    OpenFailed,
    #[error("클립보드에 쓰지 못했습니다: {0}")]
    WriteFailed(&'static str),
}

pub trait Clipboard: Send + Sync {
    /// CF_UNICODETEXT. 실패하거나 텍스트가 없으면 None.
    fn get_text(&self) -> Option<String>;
    fn set_text(&self, text: &str) -> Result<(), ClipboardError>;
    fn sequence_number(&self) -> u32;
}

/// v5 `ClipboardFallback.run_with_temporary_content`.
/// 이전 텍스트가 없었으면(비었거나 텍스트가 아님) 복원하지 않는다.
pub fn run_with_temporary_content<T>(
    cb: &dyn Clipboard,
    text: &str,
    action: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let previous = cb.get_text();
    let mut last_err = None;
    for _ in 0..SET_ATTEMPTS {
        match cb.set_text(text) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e.to_string());
    }
    let result = action();
    if let Some(prev) = previous
        && cb.set_text(&prev).is_err()
    {
        tracing::warn!("clipboard restore failed");
    }
    result
}

/// 기본 타이밍(500 ms / 50 ms)으로 `copy_with_sentinel_timed`를 부른다.
pub fn copy_with_sentinel(
    cb: &dyn Clipboard,
    copy: impl FnOnce() -> Result<(), String>,
) -> Option<String> {
    copy_with_sentinel_timed(cb, copy, COPY_MAX_WAIT, COPY_SETTLE)
}

/// sentinel을 넣고 `copy`를 실행한 뒤 시퀀스 번호가 바뀔 때까지 `max_wait`만큼 기다리고
/// `settle`을 더 둔다. 결과가 비었거나 sentinel이면 None. 끝나면 이전 텍스트를 복원한다.
pub fn copy_with_sentinel_timed(
    cb: &dyn Clipboard,
    copy: impl FnOnce() -> Result<(), String>,
    max_wait: Duration,
    settle: Duration,
) -> Option<String> {
    let previous = cb.get_text();
    let result = probe(cb, copy, max_wait, settle);
    if let Some(prev) = previous
        && cb.set_text(&prev).is_err()
    {
        tracing::warn!("clipboard restore failed");
    }
    result
}

fn probe(
    cb: &dyn Clipboard,
    copy: impl FnOnce() -> Result<(), String>,
    max_wait: Duration,
    settle: Duration,
) -> Option<String> {
    cb.set_text(SENTINEL).ok()?;
    let seq = cb.sequence_number();
    copy().ok()?;
    let deadline = Instant::now() + max_wait;
    while cb.sequence_number() == seq {
        if Instant::now() >= deadline {
            return None;
        }
        sleep(COPY_POLL.min(max_wait));
    }
    sleep(settle);
    cb.get_text().filter(|t| !t.is_empty() && t != SENTINEL)
}

pub use win32::Win32Clipboard;

mod win32 {
    use super::{Clipboard, ClipboardError};
    use std::thread::sleep;
    use std::time::Duration;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
        IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };

    const CF_UNICODETEXT: u32 = 13;
    const OPEN_ATTEMPTS: usize = 5;
    const OPEN_DELAY: Duration = Duration::from_millis(20);

    #[derive(Debug, Default, Clone, Copy)]
    pub struct Win32Clipboard;

    /// 열린 클립보드. Drop에서 항상 닫는다.
    struct Open;

    impl Open {
        fn acquire() -> Result<Open, ClipboardError> {
            for attempt in 0..OPEN_ATTEMPTS {
                // SAFETY: 성공하면 Drop이 CloseClipboard를 부른다.
                if unsafe { OpenClipboard(None) }.is_ok() {
                    return Ok(Open);
                }
                if attempt + 1 < OPEN_ATTEMPTS {
                    sleep(OPEN_DELAY);
                }
            }
            Err(ClipboardError::OpenFailed)
        }
    }

    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: acquire가 연 클립보드를 닫는다.
            let _ = unsafe { CloseClipboard() };
        }
    }

    impl Clipboard for Win32Clipboard {
        fn get_text(&self) -> Option<String> {
            let _open = Open::acquire().ok()?;
            // SAFETY: 클립보드가 열려 있는 동안만 핸들과 잠금 포인터를 사용한다.
            unsafe {
                IsClipboardFormatAvailable(CF_UNICODETEXT).ok()?;
                let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
                let hglobal = HGLOBAL(handle.0);
                let max = GlobalSize(hglobal) / 2;
                let ptr = GlobalLock(hglobal) as *const u16;
                if ptr.is_null() {
                    return None;
                }
                let mut len = 0;
                while len < max && *ptr.add(len) != 0 {
                    len += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                let _ = GlobalUnlock(hglobal);
                Some(text)
            }
        }

        fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let size = wide.len() * 2;
            let _open = Open::acquire()?;
            // SAFETY: 클립보드가 열려 있고, 할당한 핸들은 SetClipboardData 성공 시에만
            // 시스템이 소유하며 실패하면 여기서 해제한다.
            unsafe {
                EmptyClipboard().map_err(|_| ClipboardError::WriteFailed("empty"))?;
                let hglobal = GlobalAlloc(GMEM_MOVEABLE, size)
                    .map_err(|_| ClipboardError::WriteFailed("alloc"))?;
                let ptr = GlobalLock(hglobal) as *mut u16;
                if ptr.is_null() {
                    let _ = GlobalFree(Some(hglobal));
                    return Err(ClipboardError::WriteFailed("lock"));
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(hglobal);
                if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hglobal.0))).is_err() {
                    let _ = GlobalFree(Some(hglobal));
                    return Err(ClipboardError::WriteFailed("set"));
                }
            }
            Ok(())
        }

        fn sequence_number(&self) -> u32 {
            // SAFETY: 인자 없는 조회 함수.
            unsafe { GetClipboardSequenceNumber() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        text: Mutex<Option<String>>,
        seq: Mutex<u32>,
        fail_sets: Mutex<usize>,
        set_calls: Mutex<usize>,
    }

    impl Fake {
        fn new(text: Option<&str>) -> Self {
            Fake {
                text: Mutex::new(text.map(String::from)),
                seq: Mutex::new(0),
                fail_sets: Mutex::new(0),
                set_calls: Mutex::new(0),
            }
        }
        fn current(&self) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
    }

    impl Clipboard for Fake {
        fn get_text(&self) -> Option<String> {
            self.current()
        }
        fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
            *self.set_calls.lock().unwrap() += 1;
            let mut f = self.fail_sets.lock().unwrap();
            if *f > 0 {
                *f -= 1;
                return Err(ClipboardError::OpenFailed);
            }
            *self.text.lock().unwrap() = Some(text.to_string());
            *self.seq.lock().unwrap() += 1;
            Ok(())
        }
        fn sequence_number(&self) -> u32 {
            *self.seq.lock().unwrap()
        }
    }

    const FAST: Duration = Duration::from_millis(30);
    const NONE: Duration = Duration::from_millis(0);

    #[test]
    fn temporary_content_restores_previous_text_and_skips_when_none() {
        let cb = Fake::new(Some("old"));
        let r = run_with_temporary_content(&cb, "new", || Ok(cb.current()));
        assert_eq!(r, Ok(Some("new".to_string())));
        assert_eq!(cb.current().as_deref(), Some("old"));

        let cb = Fake::new(None);
        run_with_temporary_content(&cb, "new", || Ok(())).unwrap();
        assert_eq!(cb.current().as_deref(), Some("new"));
        assert_eq!(*cb.set_calls.lock().unwrap(), 1);
    }

    #[test]
    fn temporary_content_retries_set_three_times_then_errors() {
        let cb = Fake::new(Some("old"));
        *cb.fail_sets.lock().unwrap() = 10;
        let mut ran = false;
        let r = run_with_temporary_content(&cb, "new", || {
            ran = true;
            Ok(())
        });
        assert!(r.is_err());
        assert!(!ran);
        assert_eq!(*cb.set_calls.lock().unwrap(), 3);

        let cb = Fake::new(Some("old"));
        *cb.fail_sets.lock().unwrap() = 2;
        assert!(run_with_temporary_content(&cb, "new", || Ok(())).is_ok());
    }

    #[test]
    fn temporary_content_restores_even_when_action_fails() {
        let cb = Fake::new(Some("old"));
        let r: Result<(), String> = run_with_temporary_content(&cb, "new", || Err("boom".into()));
        assert_eq!(r, Err("boom".to_string()));
        assert_eq!(cb.current().as_deref(), Some("old"));
    }

    #[test]
    fn copy_with_sentinel_returns_copied_text_and_restores() {
        let cb = Fake::new(Some("old"));
        let r = copy_with_sentinel_timed(
            &cb,
            || cb.set_text("selected").map_err(|e| e.to_string()),
            FAST,
            NONE,
        );
        assert_eq!(r.as_deref(), Some("selected"));
        assert_eq!(cb.current().as_deref(), Some("old"));
    }

    #[test]
    fn copy_with_sentinel_unchanged_is_none() {
        let cb = Fake::new(Some("old"));
        let r = copy_with_sentinel_timed(&cb, || Ok(()), FAST, NONE);
        assert_eq!(r, None);
        assert_eq!(cb.current().as_deref(), Some("old"));
    }

    #[test]
    fn copy_with_sentinel_identical_to_previous_still_counts() {
        let cb = Fake::new(Some("same"));
        let r = copy_with_sentinel_timed(
            &cb,
            || cb.set_text("same").map_err(|e| e.to_string()),
            FAST,
            NONE,
        );
        assert_eq!(r.as_deref(), Some("same"));
        assert_eq!(cb.current().as_deref(), Some("same"));
    }
}
