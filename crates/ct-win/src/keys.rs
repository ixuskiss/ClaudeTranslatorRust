//! 합성 키 전송(Ctrl+C/A/V, Esc)과 수식키 해제 대기 (v5 참조 §1.1, §2.6).
//!
//! Enter, Ctrl+Enter 같은 전송 키는 절대 합성하지 않는다. 정착 대기는 호출자 몫이다.

use crate::hotkey::Modifier;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{GetLastError, SetLastError, WIN32_ERROR};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_RCONTROL,
    VK_RMENU,
};

pub const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
pub const ALT_RELEASE_TIMEOUT: Duration = Duration::from_millis(500);
pub const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    CtrlC,
    CtrlA,
    CtrlV,
    Esc,
}

impl Chord {
    pub const ALL: [Chord; 4] = [Chord::CtrlC, Chord::CtrlA, Chord::CtrlV, Chord::Esc];

    /// (VK, up) 순서열. Ctrl 조합은 Ctrl down, 키 down, 키 up, Ctrl up.
    fn sequence(self) -> Vec<(VIRTUAL_KEY, bool)> {
        let letter = match self {
            Chord::CtrlC => VIRTUAL_KEY(b'C' as u16),
            Chord::CtrlA => VIRTUAL_KEY(b'A' as u16),
            Chord::CtrlV => VIRTUAL_KEY(b'V' as u16),
            Chord::Esc => return vec![(VK_ESCAPE, false), (VK_ESCAPE, true)],
        };
        vec![
            (VK_LCONTROL, false),
            (letter, false),
            (letter, true),
            (VK_LCONTROL, true),
        ]
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("SendInput이 {sent}개만 주입했습니다 (오류 코드 {last_error})")]
    SendInputFailed { sent: u32, last_error: u32 },
}

pub trait KeySender: Send + Sync {
    /// 한 번의 `SendInput`으로 down/up 묶음을 보낸다.
    fn send(&self, chord: Chord) -> Result<(), KeyError>;
    /// `GetAsyncKeyState & 0x8000`.
    fn is_down(&self, m: Modifier) -> bool;
}

pub struct Win32KeySender;

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn vk_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: 값 인자만 받는 조회 함수.
    let state = unsafe { GetAsyncKeyState(i32::from(vk.0)) };
    (state as u16) & 0x8000 != 0
}

impl KeySender for Win32KeySender {
    fn send(&self, chord: Chord) -> Result<(), KeyError> {
        let inputs: Vec<INPUT> = chord
            .sequence()
            .into_iter()
            .map(|(vk, up)| key_input(vk, up))
            .collect();
        // SAFETY: 완전히 초기화된 INPUT 배열과 올바른 cbSize.
        let sent = unsafe {
            SetLastError(WIN32_ERROR(0));
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32)
        };
        if sent as usize != inputs.len() {
            // SAFETY: 스레드 로컬 오류 코드를 읽을 뿐이다.
            let last_error = unsafe { GetLastError() }.0;
            return Err(KeyError::SendInputFailed { sent, last_error });
        }
        Ok(())
    }

    fn is_down(&self, m: Modifier) -> bool {
        match m {
            Modifier::Alt => vk_down(VK_LMENU) || vk_down(VK_RMENU),
            Modifier::Ctrl => vk_down(VK_LCONTROL) || vk_down(VK_RCONTROL),
        }
    }
}

/// Ctrl과 Alt가 모두 떼질 때까지 기다린다. 제때 떼면 true, 시간 초과면 false.
pub fn wait_for_modifiers_released(keys: &dyn KeySender, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if !keys.is_down(Modifier::Ctrl) && !keys.is_down(Modifier::Alt) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Alt가 떼질 때까지 기다린다. 시간이 다 돼도 그냥 반환한다(키는 어차피 보낸다).
pub fn wait_until_alt_released(keys: &dyn KeySender, timeout: Duration) {
    let start = Instant::now();
    while keys.is_down(Modifier::Alt) && start.elapsed() < timeout {
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 각 수식키는 n번째 조회부터 떼진 상태가 된다(usize::MAX면 계속 눌림).
    struct Fake {
        ctrl_calls: AtomicUsize,
        alt_calls: AtomicUsize,
        ctrl_up_at: usize,
        alt_up_at: usize,
    }

    impl Fake {
        fn new(ctrl_up_at: usize, alt_up_at: usize) -> Self {
            Fake {
                ctrl_calls: AtomicUsize::new(0),
                alt_calls: AtomicUsize::new(0),
                ctrl_up_at,
                alt_up_at,
            }
        }
    }

    impl KeySender for Fake {
        fn send(&self, _chord: Chord) -> Result<(), KeyError> {
            Ok(())
        }
        fn is_down(&self, m: Modifier) -> bool {
            let (calls, up_at) = match m {
                Modifier::Ctrl => (&self.ctrl_calls, self.ctrl_up_at),
                Modifier::Alt => (&self.alt_calls, self.alt_up_at),
            };
            calls.fetch_add(1, Ordering::SeqCst) < up_at
        }
    }

    #[test]
    fn returns_immediately_when_released() {
        let fake = Fake::new(0, 0);
        let start = Instant::now();
        assert!(wait_for_modifiers_released(&fake, Duration::from_secs(2)));
        assert!(start.elapsed() < POLL);
        assert_eq!(fake.ctrl_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fake.alt_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn waits_until_both_released() {
        let fake = Fake::new(2, 3);
        assert!(wait_for_modifiers_released(&fake, Duration::from_secs(2)));
        // Ctrl은 2번 눌림 뒤 떼져도 Alt가 풀릴 때까지 매 폴마다 재조회된다.
        assert_eq!(fake.ctrl_calls.load(Ordering::SeqCst), 6);
        assert_eq!(fake.alt_calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn times_out_false_when_held() {
        let fake = Fake::new(usize::MAX, 0);
        let start = Instant::now();
        assert!(!wait_for_modifiers_released(
            &fake,
            Duration::from_millis(100)
        ));
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn alt_wait_returns_after_timeout_even_if_held() {
        let fake = Fake::new(0, usize::MAX);
        let start = Instant::now();
        wait_until_alt_released(&fake, Duration::from_millis(100));
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn chord_never_includes_enter() {
        use windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN;
        for chord in Chord::ALL {
            for (vk, _) in chord.sequence() {
                assert_ne!(vk, VK_RETURN, "{chord:?}");
            }
        }
    }

    #[test]
    fn esc_has_no_ctrl_and_ctrl_chords_are_wrapped() {
        let esc = Chord::Esc.sequence();
        assert_eq!(esc, vec![(VK_ESCAPE, false), (VK_ESCAPE, true)]);
        let c = Chord::CtrlC.sequence();
        assert_eq!(c.first(), Some(&(VK_LCONTROL, false)));
        assert_eq!(c.last(), Some(&(VK_LCONTROL, true)));
        assert_eq!(c.len(), 4);
    }
}
