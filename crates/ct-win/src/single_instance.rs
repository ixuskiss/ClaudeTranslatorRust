//! 명명된 뮤텍스를 이용한 단일 실행 보장.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

pub const SINGLE_INSTANCE_NAME: &str = r"Local\ClaudeTranslator.SingleInstance";
pub const ALREADY_RUNNING_MESSAGE: &str = "ClaudeTranslator가 이미 실행 중입니다.";

/// 뮤텍스 핸들을 들고 있는 동안 단일 실행이 유지된다. Drop 시 핸들을 닫는다.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// 뮤텍스를 만든다. 이미 다른 인스턴스가 있으면 `Ok(None)`.
    pub fn acquire(name: &str) -> Result<Option<SingleInstance>, std::io::Error> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide는 NUL 종료된 UTF-16 버퍼이며 호출 동안 유효하다.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
        // SAFETY: CreateMutexW 직후 같은 스레드에서 읽는다.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: 방금 얻은 유효한 핸들이다.
            let _ = unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(SingleInstance(handle)))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: self.0은 acquire에서 얻은 유효한 핸들이며 한 번만 닫는다.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_returns_none() {
        let name = format!(r"Local\ClaudeTranslator.Test.{}", std::process::id());
        let first = SingleInstance::acquire(&name).unwrap();
        assert!(first.is_some());
        assert!(SingleInstance::acquire(&name).unwrap().is_none());
        drop(first);
        // 해제 후에는 다시 획득할 수 있다.
        assert!(SingleInstance::acquire(&name).unwrap().is_some());
    }
}
