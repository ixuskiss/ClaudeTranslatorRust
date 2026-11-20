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
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LMENU, VK_MENU, VK_RMENU};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CallNextHookEx, GWL_EXSTYLE, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowThreadProcessId, HHOOK,
    HWND_TOPMOST, KBDLLHOOKSTRUCT, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
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
    last_release: Option<Instant>,
}
static ALT_STATE: Mutex<AltState> = Mutex::new(AltState {
    down: false,
    last_release: None,
});

const ALT_ALT_WINDOW: Duration = Duration::from_millis(200);

fn is_alt(vk: u32) -> bool {
    vk == u32::from(VK_MENU.0) || vk == u32::from(VK_LMENU.0) || vk == u32::from(VK_RMENU.0)
}

unsafe extern "system" fn ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if let Ok(mut st) = ALT_STATE.lock() {
            if is_alt(info.vkCode) {
                if is_down {
                    // Auto-repeat while held: already down, ignore.
                    st.down = true;
                } else if is_up && st.down {
                    st.down = false;
                    let now = Instant::now();
                    let fire = st
                        .last_release
                        .is_some_and(|t| now.duration_since(t) <= ALT_ALT_WINDOW);
                    if fire {
                        st.last_release = None;
                        println!("ALT_ALT");
                        if let Some(tx) = ALT_TX.get() {
                            let _ = tx.send(());
                        }
                    } else {
                        st.last_release = Some(now);
                    }
                }
            } else if is_down {
                // Any other key breaks the Alt-Alt sequence.
                st.last_release = None;
            }
        }
    }
    // SAFETY: always forward to the next hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
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

fn hwnd_of(win: &slint::Window) -> Option<HWND> {
    let wh = win.window_handle();
    let handle = wh.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

fn show_badge(badge: &BadgeWin) {
    let mut pt = POINT::default();
    // SAFETY: valid out pointer.
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        badge
            .window()
            .set_position(slint::PhysicalPosition::new(pt.x + 16, pt.y + 16));
    }
    if badge.show().is_err() {
        eprintln!("S2: show failed");
        return;
    }
    let Some(hwnd) = hwnd_of(badge.window()) else {
        eprintln!("S2: no HWND");
        return;
    };
    // SAFETY: hwnd belongs to our live window.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let add = (WS_EX_NOACTIVATE.0 | WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0) as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | add);
        let r = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
        println!("S2: badge shown, SetWindowPos ok={}", r.is_ok());
    }
}

fn bring_to_front(win: &FrontWin) {
    if win.show().is_err() {
        eprintln!("S3: show failed");
        return;
    }
    let Some(hwnd) = hwnd_of(win.window()) else {
        eprintln!("S3: no HWND");
        return;
    };
    // SAFETY: Win32 foreground calls on our own window.
    unsafe {
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

    let badge: Rc<RefCell<Option<BadgeWin>>> = Rc::new(RefCell::new(None));
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
                        if let Ok(w) = BadgeWin::new() {
                            show_badge(&w);
                            *badge.borrow_mut() = Some(w);
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
