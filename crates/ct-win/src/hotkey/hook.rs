//! 저수준 키보드 훅(WH_KEYBOARD_LL)과 F15 주입 (v5 참조 §1.7).
//!
//! 훅은 전용 스레드에서 설치하고 그 스레드가 메시지 루프를 돈다. 콜백은 자유
//! `extern "system"` 함수이므로 훅별 상태(콜백 Box, `AltTapState`)는 훅 스레드의
//! `thread_local`에 둔다. LL 훅 콜백은 항상 설치한 스레드에서 호출되므로 락이 필요 없다.
//! 실제 키는 절대 막지 않고 항상 `CallNextHookEx`를 반환한다.

use super::f15::{AltTapState, F15_RELEASE_DELAY, RawKey};
use super::router::{KeyDirection, Modifier};
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    GetLastError, HINSTANCE, LPARAM, LRESULT, SetLastError, WIN32_ERROR, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_F15, VK_LCONTROL, VK_LMENU, VK_MENU, VK_RCONTROL, VK_RMENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

type OnKey = Box<dyn FnMut(Modifier, KeyDirection, Instant) + Send>;

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("훅 스레드를 만들지 못했습니다: {0}")]
    Spawn(String),
    #[error("키보드 훅 설치에 실패했습니다: {0}")]
    Install(String),
    #[error("키보드 훅 설치 결과를 5초 안에 받지 못했습니다")]
    Timeout,
}

struct HookState {
    on_key: OnKey,
    alt_tap: AltTapState,
}

thread_local! {
    /// 훅 스레드에서만 설정/접근된다.
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// vk 코드를 분류한다. 좌/우는 하나의 논리 키로 취급한다.
fn classify_vk(vk: u32) -> RawKey {
    let is = |k: VIRTUAL_KEY| vk == u32::from(k.0);
    if is(VK_LMENU) || is(VK_RMENU) || is(VK_MENU) {
        RawKey::Alt
    } else if is(VK_LCONTROL) || is(VK_RCONTROL) || is(VK_CONTROL) {
        RawKey::Ctrl
    } else {
        RawKey::Other
    }
}

/// 키 하나를 주입한다. `INPUT`은 실제 Win32 크기라 `cbSize`가 올바르다.
fn send_key(vk: VIRTUAL_KEY, up: bool) {
    let flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: 완전히 초기화된 INPUT 하나와 올바른 cbSize.
    let sent = unsafe {
        SetLastError(WIN32_ERROR(0));
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32)
    };
    if sent != 1 {
        // SAFETY: 스레드 로컬 오류 코드를 읽을 뿐이다.
        let err = unsafe { GetLastError() };
        tracing::warn!(
            vk = vk.0,
            flags = flags.0,
            error = err.0,
            "SendInput failed"
        );
    }
}

/// 콜백 본문. F15 up을 예약해야 하면 true를 반환한다(F15 down은 이미 보냈다).
fn handle_event(vk: u32, is_down: bool) -> bool {
    let key = classify_vk(vk);
    let dir = if is_down {
        KeyDirection::Down
    } else {
        KeyDirection::Up
    };
    let now = Instant::now();
    STATE.with(|cell| {
        // 재진입이면 건너뛰고 그대로 넘긴다.
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(st) = guard.as_mut() else {
            return false;
        };
        let request_f15 = st.alt_tap.on_event(key, dir);
        if request_f15 {
            // 실제 Alt-up을 체인에 넘기기 전에 F15 down을 보낸다.
            send_key(VK_F15, false);
        }
        // Other는 AltTapState에만 가고 on_key에는 가지 않는다.
        let modifier = match key {
            RawKey::Alt => Some(Modifier::Alt),
            RawKey::Ctrl => Some(Modifier::Ctrl),
            RawKey::Other => None,
        };
        if let Some(m) = modifier {
            (st.on_key)(m, dir, now);
        }
        request_f15
    })
}

unsafe extern "system" fn ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mut release_f15 = false;
    if code >= 0 && lparam.0 != 0 {
        // SAFETY: WH_KEYBOARD_LL에서 code >= 0이면 lparam은 KBDLLHOOKSTRUCT를 가리킨다.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = (info.flags & LLKHF_INJECTED).0 != 0;
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if !injected && (is_down || is_up) {
            let vk = info.vkCode;
            release_f15 = catch_unwind(AssertUnwindSafe(|| handle_event(vk, is_down)))
                .unwrap_or_else(|_| {
                    tracing::error!("keyboard hook callback panicked");
                    false
                });
        }
    }
    // SAFETY: 항상 다음 훅으로 넘긴다. 실제 키는 막지 않는다.
    let result = unsafe { CallNextHookEx(None, code, wparam, lparam) };
    if release_f15 {
        // 훅 스레드를 막지 않도록 지연된 up은 별도 스레드에서 보낸다.
        let spawned = std::thread::Builder::new()
            .name("f15-release".into())
            .spawn(|| {
                std::thread::sleep(F15_RELEASE_DELAY);
                send_key(VK_F15, true);
            });
        if spawned.is_err() {
            // F15가 눌린 채로 남지 않도록 즉시 해제한다.
            send_key(VK_F15, true);
        }
    }
    result
}

/// 훅 스레드 본체. 설치 결과(스레드 id 또는 오류 메시지)를 `ready`로 보낸다.
fn run_hook_thread(
    on_key: OnKey,
    handle: Arc<AtomicIsize>,
    ready: mpsc::Sender<Result<u32, String>>,
) {
    // SAFETY: 표준 LL 훅 설치. 이 스레드가 아래에서 메시지를 펌프한다.
    unsafe {
        let hmod = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("GetModuleHandleW: {e}")));
                return;
            }
        };
        STATE.with(|c| {
            *c.borrow_mut() = Some(HookState {
                on_key,
                alt_tap: AltTapState::new(),
            });
        });
        let hook =
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), Some(HINSTANCE(hmod.0)), 0) {
                Ok(h) => h,
                Err(e) => {
                    STATE.with(|c| *c.borrow_mut() = None);
                    let _ = ready.send(Err(format!("SetWindowsHookExW: {e}")));
                    return;
                }
            };
        handle.store(hook.0 as isize, Ordering::SeqCst);
        let _ = ready.send(Ok(GetCurrentThreadId()));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        unhook(&handle);
        STATE.with(|c| *c.borrow_mut() = None);
    }
}

/// 핸들이 남아 있으면 한 번만 해제한다(스레드와 stop이 경쟁해도 안전).
fn unhook(handle: &AtomicIsize) {
    let raw = handle.swap(0, Ordering::SeqCst);
    if raw != 0 {
        // SAFETY: SetWindowsHookExW가 돌려준 핸들이며 swap으로 한 번만 해제된다.
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut _));
        }
    }
}

pub struct KeyboardHook {
    thread_id: u32,
    handle: Arc<AtomicIsize>,
    done: mpsc::Receiver<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl KeyboardHook {
    /// 훅 스레드를 띄우고 설치 결과를 기다린다(최대 5초). 실패하면 Err.
    pub fn start(on_key: OnKey) -> Result<KeyboardHook, HookError> {
        let handle = Arc::new(AtomicIsize::new(0));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let thread_handle = handle.clone();
        let join = std::thread::Builder::new()
            .name("keyboard-hook".into())
            .spawn(move || {
                run_hook_thread(on_key, thread_handle, ready_tx);
                let _ = done_tx.send(());
            })
            .map_err(|e| HookError::Spawn(e.to_string()))?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(thread_id)) => Ok(KeyboardHook {
                thread_id,
                handle,
                done: done_rx,
                join: Some(join),
            }),
            Ok(Err(msg)) => {
                let _ = join.join();
                Err(HookError::Install(msg))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(HookError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = join.join();
                Err(HookError::Install("hook thread exited".into()))
            }
        }
    }

    /// `WM_QUIT`을 보내고 최대 5초 join한 뒤, 남은 핸들이 있으면 Unhook한다.
    pub fn stop(self) {
        drop(self);
    }

    fn shutdown(&mut self) {
        let Some(join) = self.join.take() else {
            return;
        };
        // SAFETY: 훅 스레드 id로 WM_QUIT를 보낸다.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if self.done.recv_timeout(STOP_TIMEOUT).is_ok() {
            let _ = join.join();
        } else {
            tracing::warn!("keyboard hook thread did not stop within 5 s");
        }
        unhook(&self.handle);
    }
}

impl Drop for KeyboardHook {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_modifier_vks() {
        assert_eq!(classify_vk(0xA4), RawKey::Alt);
        assert_eq!(classify_vk(0xA5), RawKey::Alt);
        assert_eq!(classify_vk(0x12), RawKey::Alt);
        assert_eq!(classify_vk(0xA2), RawKey::Ctrl);
        assert_eq!(classify_vk(0xA3), RawKey::Ctrl);
        assert_eq!(classify_vk(0x11), RawKey::Ctrl);
        assert_eq!(classify_vk(0x41), RawKey::Other);
        assert_eq!(classify_vk(0xA0), RawKey::Other); // LSHIFT
    }
}
