//! 다중 포맷 클립보드와 복원 가드 (v5 참조 §2.3, Sheets 셀 편집 복사 전용).
//!
//! 클립보드 내용은 절대 로그에 남기지 않는다.

use crate::clipboard::ClipboardError;
use std::time::{Duration, Instant};

/// 클립보드 변경 후 값이 이 시간 동안 유지돼야 안정으로 본다.
pub const WAIT_STABLE: Duration = Duration::from_millis(50);
/// 시퀀스 번호 폴링 간격.
pub const WAIT_POLL: Duration = Duration::from_millis(20);

/// 클립보드 포맷 전체의 바이트 복사본. (포맷 번호, 데이터) 순서를 유지한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    formats: Vec<(u32, Vec<u8>)>,
}

impl ClipboardSnapshot {
    pub fn new(formats: Vec<(u32, Vec<u8>)>) -> Self {
        Self { formats }
    }

    pub fn formats(&self) -> &[(u32, Vec<u8>)] {
        &self.formats
    }
}

pub trait FormatClipboard: Send + Sync {
    fn sequence_number(&self) -> u32;
    /// HGLOBAL 포맷만 담는다. 열기 실패 시 None.
    fn snapshot(&self) -> Option<ClipboardSnapshot>;
    fn restore(&self, snapshot: &ClipboardSnapshot) -> Result<(), ClipboardError>;
    fn read_text(&self) -> Option<String>;
    /// 이름 포맷("HTML Format", 사용자 정의 포맷) 원본 바이트.
    fn read_registered(&self, format_name: &str) -> Option<Vec<u8>>;
    /// 비운 뒤 이름 포맷을 모두 쓴다. CF_UNICODETEXT는 쓰지 않는다.
    fn write_registered(&self, formats: &[(&str, &[u8])]) -> Result<(), ClipboardError>;
}

/// v5 `ClipboardGuard`. begin 때 스냅샷과 시퀀스 번호를 잡고,
/// 우리가 쓴 뒤 다른 쪽이 건드리지 않았을 때만 finish에서 복원한다.
pub struct ClipboardGuard<'a> {
    cb: &'a dyn FormatClipboard,
    snapshot: Option<ClipboardSnapshot>,
    begin_seq: u32,
    own_seq: u32,
}

impl<'a> ClipboardGuard<'a> {
    pub fn begin(cb: &'a dyn FormatClipboard) -> Self {
        let snapshot = cb.snapshot();
        let seq = cb.sequence_number();
        Self {
            cb,
            snapshot,
            begin_seq: seq,
            own_seq: seq,
        }
    }

    /// 우리가 클립보드에 쓴(또는 복사한) 직후 호출한다.
    pub fn mark_own_write(&mut self) {
        self.own_seq = self.cb.sequence_number();
    }

    /// 실패해도 패닉·오류 전파 없이 끝난다.
    pub fn finish(self) {
        let Some(snapshot) = self.snapshot else {
            return;
        };
        if self.cb.sequence_number() != self.own_seq {
            tracing::debug!("clipboard guard: 사용자 복사 감지, 복원 생략");
            return;
        }
        if self.own_seq == self.begin_seq {
            return;
        }
        if let Err(e) = self.cb.restore(&snapshot) {
            tracing::warn!(error = %e, "clipboard guard: 복원 실패");
        }
    }
}

/// 시퀀스 번호가 `before`와 달라진 뒤 [`WAIT_STABLE`] 동안 유지되면 true,
/// `timeout` 안에 안 되면 false.
pub fn wait_for_clipboard_change(cb: &dyn FormatClipboard, before: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    wait_with(
        cb,
        before,
        timeout,
        WAIT_STABLE,
        WAIT_POLL,
        || start.elapsed(),
        std::thread::sleep,
    )
}

fn wait_with(
    cb: &dyn FormatClipboard,
    before: u32,
    timeout: Duration,
    stable: Duration,
    poll: Duration,
    now: impl Fn() -> Duration,
    sleep: impl Fn(Duration),
) -> bool {
    let mut seen = cb.sequence_number();
    let mut changed_at = now();
    loop {
        let seq = cb.sequence_number();
        let t = now();
        if seq != seen {
            seen = seq;
            changed_at = t;
        }
        if seen != before && t.saturating_sub(changed_at) >= stable {
            return true;
        }
        if t >= timeout {
            return false;
        }
        sleep(poll);
    }
}

pub use win32::Win32FormatClipboard;

mod win32 {
    use super::{ClipboardSnapshot, FormatClipboard};
    use crate::clipboard::ClipboardError;
    use std::thread::sleep;
    use std::time::Duration;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows::core::PCWSTR;

    const CF_UNICODETEXT: u32 = 13;
    /// 비트맵·메타파일·팔레트·확장 메타파일·owner-display·DSP* (HGLOBAL이 아님).
    const HANDLE_FORMATS: [u32; 9] = [2, 3, 9, 14, 0x80, 0x81, 0x82, 0x83, 0x8E];
    const OPEN_ATTEMPTS: usize = 5;
    const OPEN_DELAY: Duration = Duration::from_millis(20);

    #[derive(Debug, Default, Clone, Copy)]
    pub struct Win32FormatClipboard;

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

    fn register(name: &str) -> Option<u32> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide는 NUL로 끝나며 호출 동안 살아 있다.
        let id = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
        (id != 0).then_some(id)
    }

    /// 클립보드가 열린 상태에서 포맷의 HGLOBAL 바이트를 복사한다.
    ///
    /// # Safety
    /// 호출자가 클립보드를 연 상태여야 한다.
    unsafe fn read_bytes(format: u32) -> Option<Vec<u8>> {
        // SAFETY: 클립보드가 열려 있는 동안만 핸들과 잠금 포인터를 사용한다.
        unsafe {
            let handle = GetClipboardData(format).ok()?;
            let hglobal = HGLOBAL(handle.0);
            let size = GlobalSize(hglobal);
            let ptr = GlobalLock(hglobal) as *const u8;
            if ptr.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
            let _ = GlobalUnlock(hglobal);
            Some(bytes)
        }
    }

    /// 클립보드가 열린 상태에서 바이트를 해당 포맷으로 설정한다.
    ///
    /// # Safety
    /// 호출자가 클립보드를 연 상태여야 한다.
    unsafe fn write_bytes(format: u32, data: &[u8]) -> Result<(), ClipboardError> {
        // SAFETY: 할당한 핸들은 SetClipboardData 성공 시에만 시스템이 소유하며,
        // 실패하면 여기서 해제한다.
        unsafe {
            let hglobal = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1))
                .map_err(|_| ClipboardError::WriteFailed("alloc"))?;
            let ptr = GlobalLock(hglobal) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(Some(hglobal));
                return Err(ClipboardError::WriteFailed("lock"));
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            let _ = GlobalUnlock(hglobal);
            if SetClipboardData(format, Some(HANDLE(hglobal.0))).is_err() {
                let _ = GlobalFree(Some(hglobal));
                return Err(ClipboardError::WriteFailed("set"));
            }
        }
        Ok(())
    }

    impl FormatClipboard for Win32FormatClipboard {
        fn sequence_number(&self) -> u32 {
            // SAFETY: 인자 없는 조회 함수.
            unsafe { GetClipboardSequenceNumber() }
        }

        fn snapshot(&self) -> Option<ClipboardSnapshot> {
            let _open = Open::acquire().ok()?;
            let mut formats = Vec::new();
            let mut fmt = 0u32;
            loop {
                // SAFETY: 클립보드가 열려 있다.
                fmt = unsafe { EnumClipboardFormats(fmt) };
                if fmt == 0 {
                    break;
                }
                if HANDLE_FORMATS.contains(&fmt) {
                    continue;
                }
                // SAFETY: 클립보드가 열려 있다. 실패한 포맷은 건너뛴다.
                if let Some(bytes) = unsafe { read_bytes(fmt) } {
                    formats.push((fmt, bytes));
                }
            }
            Some(ClipboardSnapshot::new(formats))
        }

        fn restore(&self, snapshot: &ClipboardSnapshot) -> Result<(), ClipboardError> {
            let _open = Open::acquire()?;
            // SAFETY: 클립보드가 열려 있다.
            unsafe {
                EmptyClipboard().map_err(|_| ClipboardError::WriteFailed("empty"))?;
                for (fmt, data) in snapshot.formats() {
                    write_bytes(*fmt, data)?;
                }
            }
            Ok(())
        }

        fn read_text(&self) -> Option<String> {
            let _open = Open::acquire().ok()?;
            // SAFETY: 클립보드가 열려 있다.
            unsafe {
                IsClipboardFormatAvailable(CF_UNICODETEXT).ok()?;
                let bytes = read_bytes(CF_UNICODETEXT)?;
                let wide: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .take_while(|&u| u != 0)
                    .collect();
                Some(String::from_utf16_lossy(&wide))
            }
        }

        fn read_registered(&self, format_name: &str) -> Option<Vec<u8>> {
            let fmt = register(format_name)?;
            let _open = Open::acquire().ok()?;
            // SAFETY: 클립보드가 열려 있다.
            unsafe {
                IsClipboardFormatAvailable(fmt).ok()?;
                read_bytes(fmt)
            }
        }

        fn write_registered(&self, formats: &[(&str, &[u8])]) -> Result<(), ClipboardError> {
            let mut ids = Vec::with_capacity(formats.len());
            for (name, data) in formats {
                let id = register(name).ok_or(ClipboardError::WriteFailed("register"))?;
                ids.push((id, *data));
            }
            let _open = Open::acquire()?;
            // SAFETY: 클립보드가 열려 있다.
            unsafe {
                EmptyClipboard().map_err(|_| ClipboardError::WriteFailed("empty"))?;
                for (id, data) in ids {
                    write_bytes(id, data)?;
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "format_clipboard_tests.rs"]
mod tests;
