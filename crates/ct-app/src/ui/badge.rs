//! "Translating..." badge (v5 §4.7), built the spike S2 way: created once off-screen to get
//! its HWND, hidden, styled no-activate/topmost/toolwindow while hidden, and from then on
//! shown only with `SW_SHOWNOACTIVATE` so it never steals focus from Chrome.

use std::cell::RefCell;

use slint::ComponentHandle;
use windows::Win32::Foundation::HWND;

use super::BadgeWin;
use super::win32::{
    HWND_RETRIES, Rect, hide_window, hwnd_of, move_no_activate, set_no_activate_topmost,
    show_no_activate, wait_for_hwnd, work_area_at,
};
use crate::flows::Badge;

/// Offset from the cursor (physical px), v5 §4.7.
const CURSOR_OFFSET: i32 = 16;

thread_local! {
    /// The single badge, owned by the UI thread once its HWND is known.
    static BADGE: RefCell<Option<(BadgeWin, HWND)>> = const { RefCell::new(None) };
}

/// Top-left of a `size` badge at cursor +16/+16, clamped into `work` (all physical px).
pub fn badge_position(cursor: (i32, i32), size: (i32, i32), work: Option<Rect>) -> (i32, i32) {
    let (mut x, mut y) = (cursor.0 + CURSOR_OFFSET, cursor.1 + CURSOR_OFFSET);
    if let Some((left, top, right, bottom)) = work {
        x = x.min(right - size.0).max(left);
        y = y.min(bottom - size.1).max(top);
    }
    (x, y)
}

/// `Badge` handle; callable from any thread (work is marshalled to the UI thread).
pub struct BadgeWindow {
    _private: (),
}

impl BadgeWindow {
    /// Schedules the one-time hidden creation on the event loop. Call once at startup.
    pub fn create() -> Self {
        if let Err(e) = slint::invoke_from_event_loop(create_badge) {
            tracing::error!(error = %e, "badge: could not schedule creation");
        }
        Self { _private: () }
    }
}

fn create_badge() {
    let badge = match BadgeWin::new() {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "badge: create failed");
            return;
        }
    };
    badge
        .window()
        .set_position(slint::PhysicalPosition::new(-32000, -32000));
    if let Err(e) = badge.show() {
        tracing::error!(error = %e, "badge: initial off-screen show failed");
        return;
    }
    let probe = badge.as_weak();
    wait_for_hwnd(
        move || {
            let b = probe.upgrade().ok_or_else(|| "badge dropped".to_string())?;
            hwnd_of(b.window())
        },
        HWND_RETRIES,
        move |res| match res {
            Ok(hwnd) => {
                hide_window(hwnd);
                set_no_activate_topmost(hwnd);
                tracing::info!("badge: created and styled while hidden");
                BADGE.with(|slot| *slot.borrow_mut() = Some((badge, hwnd)));
            }
            Err(e) => tracing::error!(error = %e, "badge: no HWND after retries"),
        },
    );
}

fn show_on_ui(text: &str) {
    BADGE.with(|slot| {
        let slot = slot.borrow();
        let Some((badge, hwnd)) = slot.as_ref() else {
            tracing::warn!("badge: show before the badge was ready");
            return;
        };
        badge.set_text(text.into());
        let scale = badge.window().scale_factor();
        let size = (
            (badge.get_content_width() * scale).ceil() as i32,
            (badge.get_content_height() * scale).ceil() as i32,
        );
        if let Some(cursor) = ct_win::chrome::cursor_position() {
            let (x, y) = badge_position(cursor, size, work_area_at(cursor.0, cursor.1));
            move_no_activate(*hwnd, x, y);
        }
        badge.window().request_redraw();
        show_no_activate(*hwnd);
    });
}

fn hide_on_ui() {
    BADGE.with(|slot| {
        if let Some((_, hwnd)) = slot.borrow().as_ref() {
            hide_window(*hwnd);
        }
    });
}

impl Badge for BadgeWindow {
    fn show(&self, text: &str) {
        let text = text.to_string();
        if let Err(e) = slint::invoke_from_event_loop(move || show_on_ui(&text)) {
            tracing::warn!(error = %e, "badge: show not delivered");
        }
    }

    fn hide(&self) {
        if let Err(e) = slint::invoke_from_event_loop(hide_on_ui) {
            tracing::warn!(error = %e, "badge: hide not delivered");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_sits_at_cursor_plus_16() {
        assert_eq!(
            badge_position((100, 200), (120, 30), Some((0, 0, 1920, 1040))),
            (116, 216)
        );
    }

    #[test]
    fn badge_is_clamped_into_the_work_area() {
        let work = Some((1920, 0, 3840, 1040));
        assert_eq!(badge_position((3830, 1035), (120, 30), work), (3720, 1010));
        assert_eq!(badge_position((1800, -50), (120, 30), work), (1920, 0));
    }
}
