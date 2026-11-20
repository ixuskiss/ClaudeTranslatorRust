use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 같은 키를 두 번 눌렀는지 감지한다. 간격은 down에서 다음 down까지 잰다.
pub struct DoublePressDetector {
    interval_ms: Arc<AtomicU64>,
    is_down: bool,
    last_press: Option<Instant>,
}

impl DoublePressDetector {
    pub fn new(interval_ms: Arc<AtomicU64>) -> Self {
        Self {
            interval_ms,
            is_down: false,
            last_press: None,
        }
    }

    /// true = 두 번째 down (쌍 소비).
    pub fn on_key_down(&mut self, at: Instant) -> bool {
        if self.is_down {
            return false; // OS 자동 반복
        }
        self.is_down = true;
        let interval = Duration::from_millis(self.interval_ms.load(Ordering::Relaxed));
        if let Some(last) = self.last_press
            && at.saturating_duration_since(last) <= interval
        {
            self.last_press = None;
            return true;
        }
        self.last_press = Some(at);
        false
    }

    pub fn on_key_up(&mut self) {
        self.is_down = false;
    }

    /// last_press만 지운다. is_down은 그대로 둔다.
    pub fn reset_pending(&mut self) {
        self.last_press = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn det(ms: u64) -> (DoublePressDetector, Arc<AtomicU64>) {
        let a = Arc::new(AtomicU64::new(ms));
        (DoublePressDetector::new(a.clone()), a)
    }
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn second_down_within_interval_fires_once() {
        let (mut d, _) = det(200);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        d.on_key_up();
        assert!(d.on_key_down(at(t, 150)));
    }

    #[test]
    fn interval_is_inclusive() {
        let (mut d, _) = det(200);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        d.on_key_up();
        assert!(d.on_key_down(at(t, 200)));
    }

    #[test]
    fn beyond_interval_rearms() {
        let (mut d, _) = det(200);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        d.on_key_up();
        assert!(!d.on_key_down(at(t, 250)));
        d.on_key_up();
        assert!(d.on_key_down(at(t, 400)));
    }

    #[test]
    fn auto_repeat_is_ignored() {
        let (mut d, _) = det(200);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        assert!(!d.on_key_down(at(t, 50)));
        d.on_key_up();
        assert!(d.on_key_down(at(t, 100)));
    }

    #[test]
    fn measured_down_to_down_not_hold() {
        let (mut d, _) = det(200);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        d.on_key_up(); // 180ms 유지 후 up
        assert!(d.on_key_down(at(t, 190)));
    }

    #[test]
    fn triple_tap_needs_fourth() {
        let (mut d, _) = det(200);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        d.on_key_up();
        assert!(d.on_key_down(at(t, 100)));
        d.on_key_up();
        assert!(!d.on_key_down(at(t, 150)));
        d.on_key_up();
        assert!(d.on_key_down(at(t, 200)));
    }

    #[test]
    fn reset_pending_keeps_is_down() {
        let (mut d, _) = det(200);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        d.reset_pending();
        assert!(!d.on_key_down(at(t, 50))); // 반복: is_down 유지
        d.on_key_up();
        assert!(!d.on_key_down(at(t, 60))); // 쌍이 지워졌으므로 새 기준
    }

    #[test]
    fn interval_change_applies_immediately() {
        let (mut d, a) = det(200);
        a.store(500, Ordering::Relaxed);
        let t = Instant::now();
        assert!(!d.on_key_down(at(t, 0)));
        d.on_key_up();
        assert!(d.on_key_down(at(t, 400)));
    }
}
