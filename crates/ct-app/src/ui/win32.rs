//! Win32 helpers for Slint windows (patterns validated by spike S2/S3, `examples/spike.rs`).

use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, HWND_NOTOPMOST,
    HWND_TOPMOST, MB_ICONWARNING, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};
use windows::core::HSTRING;

/// Default retry budget for [`wait_for_hwnd`]: 40 x 50 ms (~2 s).
pub const HWND_RETRIES: u32 = 40;
const HWND_RETRY_DELAY: Duration = Duration::from_millis(50);

/// A monitor rectangle in physical pixels (`left, top, right, bottom`).
pub type Rect = (i32, i32, i32, i32);

pub fn hwnd_of(win: &slint::Window) -> Result<HWND, String> {
    let wh = win.window_handle();
    let handle = wh.window_handle().map_err(|e| format!("{e:?}"))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
        other => Err(format!("not a Win32 handle: {other:?}")),
    }
}

/// Slint (winit) realises the native window on the event loop after `show()` returns, so
/// the HWND is retried every 50 ms. Must be called on the UI thread.
pub fn wait_for_hwnd(
    get: impl Fn() -> Result<HWND, String> + 'static,
    tries: u32,
    done: impl FnOnce(Result<HWND, String>) + 'static,
) {
    match get() {
        Ok(h) => done(Ok(h)),
        Err(e) if tries == 0 => done(Err(e)),
        Err(_) => slint::Timer::single_shot(HWND_RETRY_DELAY, move || {
            wait_for_hwnd(get, tries - 1, done)
        }),
    }
}

/// Adds `WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW` (apply while hidden).
pub fn set_no_activate_topmost(hwnd: HWND) {
    // SAFETY: plain style read/write on a window handle; an invalid handle only fails.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let add = (WS_EX_NOACTIVATE.0 | WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0) as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | add);
    }
}

pub fn hide_window(hwnd: HWND) {
    // SAFETY: ShowWindow on a window handle; an invalid handle only fails.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// Moves the window to physical `(x, y)` above all others without activating it.
pub fn move_no_activate(hwnd: HWND, x: i32, y: i32) {
    // SAFETY: SetWindowPos on a window handle; an invalid handle only fails.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE,
        );
    }
}

/// Shows the window without activating it and keeps it topmost (spike S2).
pub fn show_no_activate(hwnd: HWND) {
    // SAFETY: ShowWindow/SetWindowPos on a window handle; an invalid handle only fails.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if let Err(e) = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        ) {
            tracing::warn!(error = %e, "show_no_activate: SetWindowPos failed");
        }
    }
}

/// `HWND_TOPMOST` -> `SetForegroundWindow` -> `HWND_NOTOPMOST` (v5 §4.6, spike S3): pops the
/// window to the front without leaving it always-on-top.
pub fn bring_to_front(hwnd: HWND) {
    // SAFETY: z-order/foreground calls on our own window handle.
    unsafe {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW;
        if let Err(e) = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) {
            tracing::warn!(error = %e, "bring_to_front: topmost failed");
        }
        if !SetForegroundWindow(hwnd).as_bool() || GetForegroundWindow() != hwnd {
            // Windows refuses foreground changes from a background process (e.g. right
            // after a hotkey while Chrome is active). Attach to the foreground thread's
            // input queue for the call (v5 spike S3); otherwise Esc keeps going to Chrome.
            let fg = GetForegroundWindow();
            let fg_tid = GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            let attached =
                fg_tid != 0 && fg_tid != me && AttachThreadInput(me, fg_tid, true).as_bool();
            let ok = SetForegroundWindow(hwnd).as_bool();
            if attached {
                let _ = AttachThreadInput(me, fg_tid, false);
            }
            tracing::debug!(
                attached,
                ok,
                foreground = GetForegroundWindow() == hwnd,
                "bring_to_front: AttachThreadInput fallback"
            );
        }
        if let Err(e) = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags) {
            tracing::warn!(error = %e, "bring_to_front: notopmost failed");
        }
    }
}

/// Warning box that blocks the calling thread until the user closes it. Used before the
/// process exits (second instance), where a background thread would die with the process.
pub fn message_box_blocking(title: &str, text: &str) {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    // SAFETY: both strings outlive the call; no owner window.
    unsafe {
        MessageBoxW(
            None,
            &text,
            &title,
            MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

/// Non-modal warning box: `MessageBoxW` runs on its own thread so the caller never blocks.
pub fn message_box(title: &str, text: &str) {
    let title = title.to_string();
    let text = text.to_string();
    let spawned = std::thread::Builder::new()
        .name("message-box".into())
        .spawn(move || message_box_blocking(&title, &text));
    if let Err(e) = spawned {
        tracing::error!(error = %e, "message_box: thread spawn failed");
    }
}

fn monitor_rect(info: &MONITORINFO) -> Rect {
    let RECT {
        left,
        top,
        right,
        bottom,
    } = info.rcWork;
    (left, top, right, bottom)
}

/// Work area (physical px) of the monitor containing (or nearest to) the point.
pub fn work_area_at(x: i32, y: i32) -> Option<Rect> {
    // SAFETY: MonitorFromPoint never fails with DEFAULTTONEAREST; valid out struct.
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info)
            .as_bool()
            .then(|| monitor_rect(&info))
    }
}

/// Work area (physical px) of the monitor the window is on (or nearest to).
pub fn work_area_of(hwnd: HWND) -> Option<Rect> {
    // SAFETY: MonitorFromWindow never fails with DEFAULTTONEAREST; valid out struct.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info)
            .as_bool()
            .then(|| monitor_rect(&info))
    }
}
