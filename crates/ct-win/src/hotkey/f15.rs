//! F15 결정 로직 (v5 참조 §1.7). 단독 Alt 탭의 키 업에서만 F15를 보내
//! Chrome이 툴바로 포커스를 가져가지 않게 한다.

use super::router::KeyDirection;
use std::time::Duration;

/// v5 `_F15_RELEASE_DELAY_SECONDS`: F15는 실제 Alt-up을 가로질러 눌린 채로 유지된다.
pub const F15_RELEASE_DELAY: Duration = Duration::from_millis(50);

/// 훅이 분류한 키 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKey {
    Alt,
    Ctrl,
    Other,
}

/// 단독 Alt 탭 판정 상태 기계(순수 로직).
#[derive(Debug, Default)]
pub struct AltTapState {
    alt_is_down: bool,
    combo_used: bool,
}

impl AltTapState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 주입되지 않은 키 이벤트마다 호출. true면 "지금 F15 down을 보내고 50ms 뒤 up".
    pub fn on_event(&mut self, key: RawKey, dir: KeyDirection) -> bool {
        match (key, dir) {
            (RawKey::Alt, KeyDirection::Down) => {
                if !self.alt_is_down {
                    self.alt_is_down = true;
                    self.combo_used = false;
                }
                false
            }
            (RawKey::Alt, KeyDirection::Up) => {
                self.alt_is_down = false;
                !self.combo_used
            }
            (_, KeyDirection::Down) => {
                if self.alt_is_down {
                    self.combo_used = true;
                }
                false
            }
            (_, KeyDirection::Up) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyDirection::{Down, Up};

    #[test]
    fn solo_alt_tap_requests_f15() {
        let mut s = AltTapState::new();
        assert!(!s.on_event(RawKey::Alt, Down));
        assert!(s.on_event(RawKey::Alt, Up));
    }

    #[test]
    fn alt_with_other_key_skips_f15() {
        let mut s = AltTapState::new();
        s.on_event(RawKey::Alt, Down);
        s.on_event(RawKey::Other, Down);
        assert!(!s.on_event(RawKey::Alt, Up));
    }

    #[test]
    fn alt_with_ctrl_after_skips_f15() {
        let mut s = AltTapState::new();
        s.on_event(RawKey::Alt, Down);
        s.on_event(RawKey::Ctrl, Down);
        assert!(!s.on_event(RawKey::Alt, Up));
    }

    #[test]
    fn ctrl_before_alt_still_requests_f15() {
        let mut s = AltTapState::new();
        s.on_event(RawKey::Ctrl, Down);
        s.on_event(RawKey::Alt, Down);
        assert!(s.on_event(RawKey::Alt, Up));
    }

    #[test]
    fn alt_up_without_down_still_requests_f15() {
        let mut s = AltTapState::new();
        assert!(s.on_event(RawKey::Alt, Up));
    }

    #[test]
    fn repeat_alt_down_does_not_reset_combo() {
        let mut s = AltTapState::new();
        s.on_event(RawKey::Alt, Down);
        s.on_event(RawKey::Other, Down);
        s.on_event(RawKey::Alt, Down); // auto-repeat
        assert!(!s.on_event(RawKey::Alt, Up));
    }

    #[test]
    fn new_alt_press_after_combo_requests_f15_again() {
        let mut s = AltTapState::new();
        s.on_event(RawKey::Alt, Down);
        s.on_event(RawKey::Other, Down);
        assert!(!s.on_event(RawKey::Alt, Up));
        s.on_event(RawKey::Alt, Down);
        assert!(s.on_event(RawKey::Alt, Up));
    }
}
