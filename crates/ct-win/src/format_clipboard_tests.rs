use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

struct Fake {
    seq: AtomicU32,
    fail_restore: bool,
    restored: Mutex<Vec<ClipboardSnapshot>>,
    restore_calls: AtomicU32,
    contents: ClipboardSnapshot,
    no_snapshot: bool,
}

impl Fake {
    fn new(fail_restore: bool) -> Self {
        Self {
            seq: AtomicU32::new(10),
            fail_restore,
            restored: Mutex::new(Vec::new()),
            restore_calls: AtomicU32::new(0),
            contents: ClipboardSnapshot::new(vec![(13, b"original".to_vec())]),
            no_snapshot: false,
        }
    }
    fn copy(&self) {
        self.seq.fetch_add(1, Ordering::SeqCst);
    }
    fn calls(&self) -> u32 {
        self.restore_calls.load(Ordering::SeqCst)
    }
}

impl FormatClipboard for Fake {
    fn sequence_number(&self) -> u32 {
        self.seq.load(Ordering::SeqCst)
    }
    fn snapshot(&self) -> Option<ClipboardSnapshot> {
        (!self.no_snapshot).then(|| self.contents.clone())
    }
    fn restore(&self, snapshot: &ClipboardSnapshot) -> Result<(), ClipboardError> {
        self.restore_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_restore {
            return Err(ClipboardError::OpenFailed);
        }
        self.restored.lock().unwrap().push(snapshot.clone());
        Ok(())
    }
    fn read_text(&self) -> Option<String> {
        None
    }
    fn read_registered(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn write_registered(&self, _: &[(&str, &[u8])]) -> Result<(), ClipboardError> {
        self.copy();
        Ok(())
    }
}

#[test]
fn guard_restores_after_own_write() {
    let cb = Fake::new(false);
    let mut guard = ClipboardGuard::begin(&cb);
    cb.copy();
    guard.mark_own_write();
    guard.finish();
    assert_eq!(*cb.restored.lock().unwrap(), vec![cb.contents.clone()]);
}

#[test]
fn guard_keeps_user_copy_made_during_section() {
    let cb = Fake::new(false);
    let mut guard = ClipboardGuard::begin(&cb);
    cb.copy();
    guard.mark_own_write();
    cb.copy(); // 사용자 복사
    guard.finish();
    assert_eq!(cb.calls(), 0);
}

#[test]
fn guard_keeps_user_copy_before_own_write_marked() {
    let cb = Fake::new(false);
    let guard = ClipboardGuard::begin(&cb);
    cb.copy();
    guard.finish();
    assert_eq!(cb.calls(), 0);
}

#[test]
fn guard_skips_restore_when_never_wrote() {
    let cb = Fake::new(false);
    ClipboardGuard::begin(&cb).finish();
    assert_eq!(cb.calls(), 0);
}

#[test]
fn guard_swallows_restore_failure() {
    let cb = Fake::new(true);
    let mut guard = ClipboardGuard::begin(&cb);
    cb.copy();
    guard.mark_own_write();
    guard.finish();
    assert_eq!(cb.calls(), 1);
}

#[test]
fn guard_without_snapshot_does_not_restore() {
    let mut cb = Fake::new(false);
    cb.no_snapshot = true;
    let mut guard = ClipboardGuard::begin(&cb);
    cb.copy();
    guard.mark_own_write();
    guard.finish();
    assert_eq!(cb.calls(), 0);
}

/// 가짜 시계: 시퀀스 번호가 시간(ms)의 함수.
struct Timeline {
    ms: AtomicU64,
    seq_at: fn(u64) -> u32,
}

impl FormatClipboard for Timeline {
    fn sequence_number(&self) -> u32 {
        (self.seq_at)(self.ms.load(Ordering::SeqCst))
    }
    fn snapshot(&self) -> Option<ClipboardSnapshot> {
        None
    }
    fn restore(&self, _: &ClipboardSnapshot) -> Result<(), ClipboardError> {
        Ok(())
    }
    fn read_text(&self) -> Option<String> {
        None
    }
    fn read_registered(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn write_registered(&self, _: &[(&str, &[u8])]) -> Result<(), ClipboardError> {
        Ok(())
    }
}

fn run(tl: &Timeline, timeout_ms: u64) -> bool {
    wait_with(
        tl,
        10,
        Duration::from_millis(timeout_ms),
        Duration::from_millis(50),
        Duration::from_millis(10),
        || Duration::from_millis(tl.ms.load(Ordering::SeqCst)),
        |d| {
            tl.ms.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
        },
    )
}

#[test]
fn wait_requires_stable_sequence() {
    let tl = Timeline {
        ms: AtomicU64::new(0),
        seq_at: |t| match t {
            0..50 => 10,
            50..70 => 11,
            _ => 12,
        },
    };
    assert!(run(&tl, 1000));
    assert!(tl.ms.load(Ordering::SeqCst) >= 120);
}

#[test]
fn wait_returns_false_on_timeout() {
    let tl = Timeline {
        ms: AtomicU64::new(0),
        seq_at: |_| 10,
    };
    assert!(!run(&tl, 200));
    assert!(tl.ms.load(Ordering::SeqCst) >= 200);
}

#[test]
fn wait_returns_false_when_never_stops_changing() {
    let tl = Timeline {
        ms: AtomicU64::new(0),
        seq_at: |t| 10 + (t / 10) as u32,
    };
    assert!(!run(&tl, 300));
}

#[test]
fn wait_public_returns_true_when_changed_before_call() {
    let cb = Fake::new(false);
    cb.copy();
    assert!(wait_for_clipboard_change(&cb, 10, Duration::from_secs(1)));
}
