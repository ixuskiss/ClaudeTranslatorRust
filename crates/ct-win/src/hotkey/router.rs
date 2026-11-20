use super::detector::DoublePressDetector;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Alt,
    Ctrl,
}

impl Modifier {
    fn other(self) -> Self {
        match self {
            Modifier::Alt => Modifier::Ctrl,
            Modifier::Ctrl => Modifier::Alt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDirection {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    AltAlt,
    CtrlCtrl,
    CtrlHeldAltAlt,
    AltHeldCtrlCtrl,
}

/// 한 키의 감지기 한 쌍. plain은 다른 수정키 없이, chord는 다른 수정키를 누른 채 누른 경우.
struct Family {
    plain: DoublePressDetector,
    chord: DoublePressDetector,
}

impl Family {
    fn new(interval: Arc<AtomicU64>) -> Self {
        Self {
            plain: DoublePressDetector::new(interval.clone()),
            chord: DoublePressDetector::new(interval),
        }
    }

    fn reset_pending(&mut self) {
        self.plain.reset_pending();
        self.chord.reset_pending();
    }

    fn detector(&mut self, chord: bool) -> &mut DoublePressDetector {
        if chord {
            &mut self.chord
        } else {
            &mut self.plain
        }
    }
}

/// Alt/Ctrl 키 이벤트를 제스처로 바꾼다. 가드와 작업 실행은 모른다.
pub struct ChordRouter {
    alt: Family,
    ctrl: Family,
    /// 키가 눌려 있는 동안의 기록: Some(down을 받은 감지기가 chord인지).
    active_alt: Option<bool>,
    active_ctrl: Option<bool>,
    is_physically_down: Box<dyn Fn(Modifier) -> Option<bool> + Send>,
}

impl ChordRouter {
    pub fn new(
        alt_interval_ms: Arc<AtomicU64>,
        ctrl_interval_ms: Arc<AtomicU64>,
        is_physically_down: Box<dyn Fn(Modifier) -> Option<bool> + Send>,
    ) -> Self {
        Self {
            alt: Family::new(alt_interval_ms),
            ctrl: Family::new(ctrl_interval_ms),
            active_alt: None,
            active_ctrl: None,
            is_physically_down,
        }
    }

    fn family(&mut self, key: Modifier) -> &mut Family {
        match key {
            Modifier::Alt => &mut self.alt,
            Modifier::Ctrl => &mut self.ctrl,
        }
    }

    fn active(&mut self, key: Modifier) -> &mut Option<bool> {
        match key {
            Modifier::Alt => &mut self.active_alt,
            Modifier::Ctrl => &mut self.active_ctrl,
        }
    }

    /// 훅 스레드에서 동기 호출. 발동한 제스처를 돌려준다(작업 실행은 호출자 몫).
    pub fn on_key(&mut self, key: Modifier, dir: KeyDirection, at: Instant) -> Option<Gesture> {
        let other = key.other();
        match dir {
            KeyDirection::Down => {
                if self.active(key).is_some() {
                    return None; // 눌린 키의 자동 반복
                }
                self.drop_if_stale(other);
                self.family(other).reset_pending();
                let chord = self.active(other).is_some();
                *self.active(key) = Some(chord);
                let fired = self.family(key).detector(chord).on_key_down(at);
                fired.then_some(match (key, chord) {
                    (Modifier::Alt, false) => Gesture::AltAlt,
                    (Modifier::Alt, true) => Gesture::CtrlHeldAltAlt,
                    (Modifier::Ctrl, false) => Gesture::CtrlCtrl,
                    (Modifier::Ctrl, true) => Gesture::AltHeldCtrlCtrl,
                })
            }
            KeyDirection::Up => {
                if let Some(chord) = self.active(key).take() {
                    self.family(other).reset_pending();
                    self.family(key).detector(chord).on_key_up();
                }
                None
            }
        }
    }

    /// other의 key-up을 놓쳤으면(물리적으로 떼어져 있으면) 기록을 지우고 감지기에 가짜 up을 준다.
    /// 프로브가 None(오류)이면 기록을 유지한다.
    fn drop_if_stale(&mut self, other: Modifier) {
        let Some(chord) = *self.active(other) else {
            return;
        };
        if (self.is_physically_down)(other) == Some(false) {
            *self.active(other) = None;
            self.family(other).detector(chord).on_key_up();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyDirection::{Down, Up};
    use Modifier::{Alt, Ctrl};
    use std::sync::Mutex;
    use std::time::Duration;

    type Probe = Box<dyn Fn(Modifier) -> Option<bool> + Send>;

    struct H {
        base: Instant,
        r: ChordRouter,
        fired: Vec<Gesture>,
    }

    /// 두 수정키 모두 물리적으로 떼어져 있다고 답하는 프로브.
    fn all_released() -> Probe {
        Box::new(|_| Some(false))
    }

    impl H {
        fn new() -> Self {
            // 기록 상태가 그대로 쓰이도록 항상 "눌려 있음"으로 답한다.
            Self::with_probe(Box::new(|_| Some(true)))
        }
        fn with_probe(p: Probe) -> Self {
            let ms = Arc::new(AtomicU64::new(400));
            Self {
                base: Instant::now(),
                r: ChordRouter::new(ms.clone(), ms, p),
                fired: vec![],
            }
        }
        fn ev(&mut self, k: Modifier, d: KeyDirection, t_ms: u64) {
            let at = self.base + Duration::from_millis(t_ms);
            if let Some(g) = self.r.on_key(k, d, at) {
                self.fired.push(g);
            }
        }
        fn tap(&mut self, k: Modifier, down: u64, up: u64) {
            self.ev(k, Down, down);
            self.ev(k, Up, up);
        }
    }

    #[test]
    fn alt_alt_fires_plain() {
        let mut h = H::new();
        h.tap(Alt, 0, 50);
        h.tap(Alt, 200, 250);
        assert_eq!(h.fired, [Gesture::AltAlt]);
    }

    #[test]
    fn ctrl_ctrl_fires_plain() {
        let mut h = H::new();
        h.tap(Ctrl, 0, 50);
        h.tap(Ctrl, 200, 250);
        assert_eq!(h.fired, [Gesture::CtrlCtrl]);
    }

    #[test]
    fn ctrl_held_through_alt_alt_fires_correction_replace_gesture() {
        let mut h = H::new();
        h.ev(Ctrl, Down, 0);
        h.tap(Alt, 100, 150);
        h.tap(Alt, 200, 250);
        h.ev(Ctrl, Up, 300);
        assert_eq!(h.fired, [Gesture::CtrlHeldAltAlt]);
    }

    #[test]
    fn alt_held_through_ctrl_ctrl_fires_correction_view_gesture() {
        let mut h = H::new();
        h.ev(Alt, Down, 0);
        h.tap(Ctrl, 100, 150);
        h.tap(Ctrl, 200, 250);
        h.ev(Alt, Up, 300);
        assert_eq!(h.fired, [Gesture::AltHeldCtrlCtrl]);
    }

    #[test]
    fn ctrl_tap_between_alt_taps_cancels() {
        let mut h = H::new();
        h.tap(Alt, 0, 20);
        h.tap(Ctrl, 50, 80);
        h.tap(Alt, 200, 250);
        assert!(h.fired.is_empty());
    }

    #[test]
    fn ctrl_only_on_second_tap_fires_nothing() {
        let mut h = H::new();
        h.tap(Alt, 0, 30);
        h.ev(Ctrl, Down, 50);
        h.tap(Alt, 80, 100);
        h.ev(Ctrl, Up, 120);
        h.tap(Alt, 200, 250);
        assert!(h.fired.is_empty());
    }

    #[test]
    fn modifier_released_between_taps_fires_nothing() {
        let mut h = H::new();
        h.ev(Ctrl, Down, 0);
        h.tap(Alt, 50, 100);
        h.ev(Ctrl, Up, 150);
        h.tap(Alt, 200, 250);
        assert!(h.fired.is_empty());
    }

    #[test]
    fn modifier_released_and_repressed_between_taps_fires_nothing() {
        let mut h = H::new();
        h.ev(Ctrl, Down, 0);
        h.tap(Alt, 50, 100);
        h.ev(Ctrl, Up, 120);
        h.ev(Ctrl, Down, 140);
        h.tap(Alt, 200, 250);
        h.ev(Ctrl, Up, 300);
        assert!(h.fired.is_empty());
    }

    #[test]
    fn auto_repeat_of_held_modifier_is_ignored() {
        let mut h = H::new();
        h.ev(Ctrl, Down, 0);
        h.ev(Ctrl, Down, 30);
        h.ev(Ctrl, Down, 60);
        h.tap(Alt, 100, 150);
        h.tap(Alt, 200, 250);
        h.ev(Ctrl, Up, 300);
        assert_eq!(h.fired, [Gesture::CtrlHeldAltAlt]);
    }

    #[test]
    fn plain_then_chord_then_plain_within_interval_fires_nothing() {
        let mut h = H::new();
        h.tap(Alt, 0, 30);
        h.ev(Ctrl, Down, 50);
        h.tap(Alt, 80, 100);
        h.ev(Ctrl, Up, 120);
        h.tap(Alt, 200, 250);
        assert!(h.fired.is_empty());
    }

    #[test]
    fn both_held_then_one_released_then_valid_gesture_works() {
        let mut h = H::new();
        h.ev(Ctrl, Down, 0);
        h.ev(Alt, Down, 50);
        h.ev(Alt, Up, 100);
        h.ev(Ctrl, Up, 150);
        h.tap(Alt, 1000, 1050);
        h.tap(Alt, 1200, 1250);
        assert_eq!(h.fired, [Gesture::AltAlt]);
    }

    #[test]
    fn up_for_unknown_key_is_ignored() {
        let mut h = H::new();
        h.ev(Alt, Up, 0);
        h.ev(Ctrl, Up, 10);
        h.tap(Alt, 100, 150);
        h.tap(Alt, 200, 250);
        assert_eq!(h.fired, [Gesture::AltAlt]);
    }

    #[test]
    fn unknown_key_equivalent_third_tap_needs_new_pair() {
        let mut h = H::new();
        h.tap(Alt, 0, 30);
        h.tap(Alt, 100, 130);
        h.tap(Alt, 200, 230);
        assert_eq!(h.fired, [Gesture::AltAlt]);
    }

    #[test]
    fn stale_other_modifier_is_released_on_next_down() {
        let mut h = H::with_probe(all_released());
        h.ev(Ctrl, Down, 0); // key-up 유실
        h.tap(Alt, 1000, 1050);
        h.tap(Alt, 1200, 1250);
        assert_eq!(h.fired, [Gesture::AltAlt]);
    }

    #[test]
    fn physically_held_other_modifier_still_routes_to_chord() {
        let state = Arc::new(Mutex::new(true));
        let s = state.clone();
        let mut h = H::with_probe(Box::new(move |_| s.lock().ok().map(|g| *g)));
        h.ev(Ctrl, Down, 0);
        h.tap(Alt, 100, 150);
        h.tap(Alt, 300, 350);
        assert_eq!(h.fired, [Gesture::CtrlHeldAltAlt]);
    }

    #[test]
    fn stale_alt_routes_ctrl_ctrl_to_plain() {
        let mut h = H::with_probe(all_released());
        h.ev(Alt, Down, 0);
        h.tap(Ctrl, 1000, 1050);
        h.tap(Ctrl, 1200, 1250);
        assert_eq!(h.fired, [Gesture::CtrlCtrl]);
    }

    #[test]
    fn stale_ctrl_dropped_then_next_ctrl_ctrl_works() {
        // 가짜 up이 감지기에 전달되어 다음 실제 Ctrl이 자동 반복으로 오인되지 않는다.
        let mut h = H::with_probe(all_released());
        h.ev(Ctrl, Down, 0);
        h.tap(Alt, 1000, 1050);
        h.tap(Ctrl, 2000, 2050);
        h.tap(Ctrl, 2200, 2250);
        assert_eq!(h.fired, [Gesture::CtrlCtrl]);
    }

    #[test]
    fn probe_error_keeps_recorded_state() {
        let mut h = H::with_probe(Box::new(|_| None));
        h.ev(Ctrl, Down, 0);
        h.tap(Alt, 100, 150);
        h.tap(Alt, 300, 350);
        assert_eq!(h.fired, [Gesture::CtrlHeldAltAlt]);
    }
}
