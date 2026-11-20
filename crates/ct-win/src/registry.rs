//! 자동 시작(HKCU Run) 레지스트리 관리. release 빌드에서만 동작한다.

use std::path::Path;

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows::core::PCWSTR;

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_VALUE: &str = "ClaudeTranslator";

/// release 빌드면 실행 파일 경로를 따옴표로 감싼 명령줄, 아니면 `None`.
pub fn autostart_command(is_release: bool, exe: &Path) -> Option<String> {
    is_release.then(|| format!("\"{}\"", exe.display()))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 자동 시작 등록/해제. release가 아니면 아무것도 하지 않는다. 실패는 경고 로그만 남긴다.
pub fn apply_autostart(enabled: bool, is_release: bool, exe: &Path) {
    let Some(command) = autostart_command(is_release, exe) else {
        return;
    };
    let key = wide(RUN_KEY);
    let value = wide(RUN_VALUE);
    if enabled {
        let data = wide(&command);
        let bytes = data.len() * std::mem::size_of::<u16>();
        // SAFETY: key/value/data는 NUL 종료된 UTF-16 버퍼이며 호출 동안 유효하다.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(value.as_ptr()),
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                bytes as u32,
            )
        };
        if status.is_err() {
            tracing::warn!(code = status.0, "autostart set failed");
        }
    } else {
        // SAFETY: key/value는 NUL 종료된 UTF-16 버퍼이며 호출 동안 유효하다.
        let status = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(value.as_ptr()),
            )
        };
        if status.is_err() && status != ERROR_FILE_NOT_FOUND {
            tracing::warn!(code = status.0, "autostart delete failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_quoted_only_in_release() {
        let exe = Path::new(r"C:\a b\x.exe");
        assert_eq!(
            autostart_command(true, exe),
            Some(r#""C:\a b\x.exe""#.to_string())
        );
        assert_eq!(autostart_command(false, exe), None);
    }

    #[test]
    fn apply_is_noop_when_not_release() {
        // 디버그 빌드에서는 레지스트리를 건드리지 않고 그냥 반환해야 한다.
        apply_autostart(true, false, Path::new(r"C:\a b\x.exe"));
        apply_autostart(false, false, Path::new(r"C:\a b\x.exe"));
    }
}
