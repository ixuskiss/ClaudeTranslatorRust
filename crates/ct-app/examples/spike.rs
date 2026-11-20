//! Throwaway foundation spike: Slint + tray + no-activate badge + LL keyboard hook + UIA.
//! The UI markup is inlined with `slint::slint!` instead of `ui/spike.slint`.
//! Run: `cargo run -p ct-app --example spike` and follow `tests/manual/spike.md`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::ComponentHandle;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_F15, VK_LMENU, VK_MENU, VK_RMENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CallNextHookEx, GWL_EXSTYLE, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowThreadProcessId, HHOOK,
    HWND_TOPMOST, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    SetWindowsHookExW, ShowWindow, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};
use windows::core::BOOL;

slint::slint! {
    #[edition = "1.19"]
    export component MainWin inherits Window {
        title: "Spike main";
        width: 320px;
        height: 120px;
        Text { text: "Spike main window (use the tray menu)"; }
    }
    export component BadgeWin inherits Window {
        no-frame: true;
        always-on-top: true;
        width: 140px;
        height: 32px;
        background: #202020;
        Text { text: "Translating..."; color: white; }
    }
    export component FrontWin inherits Window {
        title: "Spike front window";
        width: 300px;
        height: 100px;
        Text { text: "I should be in front (S3)"; }
    }
}

static ALT_TX: OnceLock<Sender<()>> = OnceLock::new();

struct AltState {
    down: bool,
    /// Another (non-Alt) key went down while Alt was held (Alt+Tab, Alt+F4 ...).
    combo_used: bool,
    last_release: Option<Instant>,
}
static ALT_STATE: Mutex<AltState> = Mutex::new(AltState {
    down: false,
    combo_used: false,
    last_release: None,
});

const ALT_ALT_WINDOW: Duration = Duration::from_millis(200);
/// v5 `_F15_RELEASE_DELAY_SECONDS`: F15 stays held across the real Alt-up.
const F15_RELEASE_DELAY: Duration = Duration::from_millis(50);

fn is_alt(vk: u32) -> bool {
    vk == u32::from(VK_MENU.0) || vk == u32::from(VK_LMENU.0) || vk == u32::from(VK_RMENU.0)
}

/// Injects one key event. `INPUT` from the `windows` crate has the real Win32 size, so
/// `cbSize` is correct (v5's struct-size bug made every SendInput fail silently).
fn send_key(vk: VIRTUAL_KEY, up: bool) -> bool {
    let input = INPUT {
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
    };
    // SAFETY: one fully initialised INPUT with the correct cbSize.
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    sent == 1
}

/// Port of v5 `windows_keyboard_hook.py`: on a real *solo* Alt tap's key-up, press F15
/// down BEFORE passing the Alt-up down the hook chain, and release F15 50 ms later on
/// another thread. Chrome then sees "Alt ... F15 held, Alt up" (a combo) and does not
/// move keyboard focus to its toolbar. Nothing is ever blocked; injected events
/// (our own F15, LLKHF_INJECTED) are ignored for gesture detection.
unsafe extern "system" fn ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mut release_f15 = false;
    let mut f15_down_ok = false;
    if code >= 0 && lparam.0 != 0 {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = (info.flags & LLKHF_INJECTED).0 != 0;
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if !injected && let Ok(mut st) = ALT_STATE.lock() {
            let alt = is_alt(info.vkCode);
            if is_down && !alt {
                if st.down {
                    st.combo_used = true;
                }
                // Any other key breaks the Alt-Alt sequence.
                st.last_release = None;
            }
            if alt && is_down && !st.down {
                st.down = true;
                st.combo_used = false;
            } else if alt && is_up && st.down {
                st.down = false;
                if st.combo_used {
                    st.last_release = None;
                } else {
                    f15_down_ok = send_key(VK_F15, false);
                    release_f15 = true;
                    let now = Instant::now();
                    let fire = st
                        .last_release
                        .is_some_and(|t| now.duration_since(t) <= ALT_ALT_WINDOW);
                    if fire {
                        st.last_release = None;
                        if let Some(tx) = ALT_TX.get() {
                            let _ = tx.send(());
                        }
                    } else {
                        st.last_release = Some(now);
                    }
                }
            }
        }
    }
    // SAFETY: always forward to the next hook.
    let result = unsafe { CallNextHookEx(None, code, wparam, lparam) };
    if release_f15 {
        // Printing and the delayed release happen off the hook thread so the callback
        // returns immediately (Windows drops LL hooks that block ~300 ms).
        std::thread::spawn(move || {
            std::thread::sleep(F15_RELEASE_DELAY);
            let up_ok = send_key(VK_F15, true);
            println!("S4: solo Alt tap -> F15 down ok={f15_down_ok} up ok={up_ok}");
        });
    }
    result
}

fn spawn_hook_thread() {
    std::thread::spawn(|| {
        // SAFETY: standard LL hook setup; the thread pumps messages below.
        unsafe {
            let hmod = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("S4: GetModuleHandleW failed: {e}");
                    return;
                }
            };
            let hook: HHOOK = match SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(ll_proc),
                Some(HINSTANCE(hmod.0)),
                0,
            ) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("S4: SetWindowsHookExW failed: {e}");
                    return;
                }
            };
            println!("S4: hook installed ({hook:?})");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        }
    });
}

fn spawn_uia_thread(rx: Receiver<()>) {
    std::thread::spawn(move || {
        // SAFETY: COM initialised on this thread before use; objects stay on this thread.
        unsafe {
            if let Err(e) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
                eprintln!("S5: CoInitializeEx failed: {e}");
                return;
            }
            let uia: IUIAutomation =
                match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                    Ok(u) => u,
                    Err(e) => {
                        eprintln!("S5: CUIAutomation create failed: {e}");
                        return;
                    }
                };
            println!("S5: UIA ready");
            while rx.recv().is_ok() {
                println!("ALT_ALT");
                match uia.GetFocusedElement() {
                    Ok(el) => {
                        // Never print the name itself, only its length.
                        let name_len = el.CurrentName().map(|b| b.len()).unwrap_or(0);
                        let ctype = el.CurrentControlType().map(|c| c.0).unwrap_or(-1);
                        println!("S5: focused name_len={name_len} control_type={ctype}");
                    }
                    Err(e) => println!("S5: GetFocusedElement failed: {e}"),
                }
            }
        }
    });
}

fn hwnd_of(win: &slint::Window) -> Result<HWND, String> {
    let wh = win.window_handle();
    let handle = wh.window_handle().map_err(|e| format!("{e:?}"))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
        other => Err(format!("not a Win32 handle: {other:?}")),
    }
}

/// Slint (winit backend) creates the native window lazily, on the event loop, after
/// `show()` returns — asking for the HWND right after `show()` gets nothing (first spike
/// run: "no HWND"). Retry every 50 ms for up to ~2 s, then report.
const HWND_RETRIES: u32 = 40;

fn wait_for_hwnd<G, D>(get: G, tries_left: u32, done: D)
where
    G: Fn() -> Result<HWND, String> + 'static,
    D: FnOnce(Result<HWND, String>) + 'static,
{
    match get() {
        Ok(h) => done(Ok(h)),
        Err(e) if tries_left == 0 => done(Err(e)),
        Err(_) => slint::Timer::single_shot(Duration::from_millis(50), move || {
            wait_for_hwnd(get, tries_left - 1, done)
        }),
    }
}

type BadgeSlot = Rc<RefCell<Option<(BadgeWin, HWND)>>>;

/// Creates the single badge window once (called after the event loop is running). It is
/// shown once off-screen so the HWND exists, then hidden via Win32 and given the
/// no-activate/topmost/tool-window ex-styles. Every later display goes through
/// `show_badge`, never through Slint's `show()`.
fn create_badge(slot: BadgeSlot) {
    let badge = match BadgeWin::new() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("S2: badge create failed: {e}");
            return;
        }
    };
    badge
        .window()
        .set_position(slint::PhysicalPosition::new(-32000, -32000));
    if let Err(e) = badge.show() {
        eprintln!("S2: initial off-screen show failed: {e}");
        return;
    }
    let probe = badge.clone_strong();
    wait_for_hwnd(
        move || hwnd_of(probe.window()),
        HWND_RETRIES,
        move |res| match res {
            Ok(hwnd) => {
                // SAFETY: hwnd belongs to our live window.
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    let add = (WS_EX_NOACTIVATE.0 | WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0) as isize;
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | add);
                }
                println!("S2: badge created and styled while hidden");
                *slot.borrow_mut() = Some((badge, hwnd));
            }
            Err(e) => eprintln!("S2: no HWND after retries: {e}"),
        },
    );
}

/// Displays the already-styled badge near the cursor without activating it.
fn show_badge(badge: &BadgeWin, hwnd: HWND) {
    let mut pt = POINT::default();
    // SAFETY: valid out pointer.
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        badge
            .window()
            .set_position(slint::PhysicalPosition::new(pt.x + 16, pt.y + 16));
    }
    badge.window().request_redraw();
    // SAFETY: hwnd belongs to our live window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let r = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        );
        println!("S2: badge shown, SetWindowPos ok={}", r.is_ok());
    }
}

fn hide_badge(hwnd: HWND) {
    // SAFETY: hwnd belongs to our live window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

fn bring_to_front(win: &FrontWin) {
    if win.show().is_err() {
        eprintln!("S3: show failed");
        return;
    }
    let probe = win.clone_strong();
    wait_for_hwnd(
        move || hwnd_of(probe.window()),
        HWND_RETRIES,
        |res| match res {
            Ok(hwnd) => force_foreground(hwnd),
            Err(e) => eprintln!("S3: no HWND after retries: {e}"),
        },
    );
}

fn force_foreground(hwnd: HWND) {
    // SAFETY: Win32 foreground calls on our own window.
    unsafe {
        if GetForegroundWindow() == hwnd {
            println!("S3: window was already in front when the HWND appeared");
            return;
        }
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let r: BOOL = SetForegroundWindow(hwnd);
        if r.as_bool() && GetForegroundWindow() == hwnd {
            println!("S3: SetForegroundWindow succeeded");
            return;
        }
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attached = AttachThreadInput(me, fg_tid, true).as_bool();
        let r2 = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(me, fg_tid, false);
        }
        let ok = r2.as_bool() && GetForegroundWindow() == hwnd;
        println!("S3: AttachThreadInput fallback attached={attached} foreground={ok}");
    }
}

fn make_icon() -> Option<Icon> {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for _ in 0..16 * 16 {
        rgba.extend_from_slice(&[0x20, 0x80, 0xE0, 0xFF]);
    }
    Icon::from_rgba(rgba, 16, 16).ok()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx) = channel();
    let _ = ALT_TX.set(tx);
    spawn_hook_thread();
    spawn_uia_thread(rx);

    let main_win = MainWin::new()?;
    main_win.show()?;

    let menu = Menu::new();
    let badge_item = MenuItem::new("Show badge", true, None);
    let front_item = MenuItem::new("Show front window", true, None);
    let exit_item = MenuItem::new("Exit", true, None);
    menu.append_items(&[&badge_item, &front_item, &exit_item])?;
    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("ClaudeTranslator spike");
    if let Some(icon) = make_icon() {
        builder = builder.with_icon(icon);
    }
    let _tray = builder.build()?;

    let badge: BadgeSlot = Rc::new(RefCell::new(None));
    {
        // Create the badge once the event loop runs, so Slint can realise its window.
        let slot = badge.clone();
        slint::Timer::single_shot(Duration::from_millis(100), move || create_badge(slot));
    }
    let front: Rc<RefCell<Option<FrontWin>>> = Rc::new(RefCell::new(None));
    let (badge_id, front_id, exit_id) = (
        badge_item.id().clone(),
        front_item.id().clone(),
        exit_item.id().clone(),
    );

    let poll = slint::Timer::default();
    poll.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            while TrayIconEvent::receiver().try_recv().is_ok() {}
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == exit_id {
                    let _ = slint::quit_event_loop();
                } else if ev.id == badge_id {
                    let badge = badge.clone();
                    slint::Timer::single_shot(Duration::from_secs(3), move || {
                        if let Some((w, hwnd)) = badge.borrow().as_ref() {
                            show_badge(w, *hwnd);
                            // Hide again so the next click is a fresh show (S2: judge
                            // from the 2nd show onward).
                            let hwnd = *hwnd;
                            slint::Timer::single_shot(Duration::from_secs(3), move || {
                                hide_badge(hwnd);
                            });
                        } else {
                            eprintln!("S2: badge unavailable (see the S2 line printed at startup)");
                        }
                    });
                } else if ev.id == front_id {
                    let front = front.clone();
                    slint::Timer::single_shot(Duration::from_secs(3), move || {
                        if let Ok(w) = FrontWin::new() {
                            bring_to_front(&w);
                            *front.borrow_mut() = Some(w);
                        }
                    });
                }
            }
        },
    );

    slint::run_event_loop_until_quit()?;
    Ok(())
}
