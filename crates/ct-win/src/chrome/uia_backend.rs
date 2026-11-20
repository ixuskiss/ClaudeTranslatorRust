//! Windows UI Automation 백엔드 (v5 참조 §2.6).
//!
//! UIA는 포그라운드 창, omnibox URL, 포커스된 요소의 Name/ControlType, 렌더 위젯
//! `SetFocus`에만 쓴다. 텍스트는 모두 클립보드 왕복(Ctrl+C)으로 읽는다.
//! 캡처한 텍스트는 절대 로그에 남기지 않는다.

use super::adapter::UiaBackend;
use super::role::{Role, classify_focused_role, is_docs_url, is_slides_url, is_supported_url};
use super::strategy::FocusedElement;
use crate::clipboard::{Clipboard, copy_with_sentinel};
use crate::keys::{ALT_RELEASE_TIMEOUT, Chord, KeySender, wait_until_alt_released};
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, POINT, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
    IUIAutomationValuePattern, UIA_CONTROLTYPE_ID, UIA_EditControlTypeId, UIA_PaneControlTypeId,
    UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetCursorPos, GetForegroundWindow};

const CHROME_CLASS_PREFIX: &str = "Chrome_WidgetWin";
const CHROME_RENDER_WIDGET_CLASS: &str = "Chrome_RenderWidgetHostHWND";
const RENDER_WIDGET_SEARCH_DEPTH: u32 = 10;
const OMNIBOX_CLASS: &str = "OmniboxViewViews";
const OMNIBOX_SEARCH_DEPTH: u32 = 20;

/// Ctrl+A 뒤 여러 문단 필드가 선택을 마칠 때까지 기다리는 시간.
pub const SELECT_ALL_SETTLE: Duration = Duration::from_millis(200);
/// Esc로 툴바 포커스를 되돌린 뒤 다시 읽기 전 대기.
pub const ESCAPE_RECOVERY_DELAY: Duration = Duration::from_millis(100);
/// Ctrl+V 뒤 Chrome이 붙여넣기를 소비할 때까지 기다리는 시간(P5).
pub const PASTE_SETTLE: Duration = Duration::from_millis(500);

/// 툴바 포커스 복구 대상인가 (v5 §2.6 e)
pub fn needs_toolbar_recovery(control_type: &str, role: Role, url: &str) -> bool {
    control_type == "ButtonControl"
        && matches!(role, Role::MessageHistory | Role::Unknown)
        && is_supported_url(url)
}

/// 선택이 없을 때 전체 텍스트를 복사할 역할인가 (Compose, Subject, Body, Textbox, SlideCanvas)
pub fn full_text_fallback_allowed(role: Role) -> bool {
    matches!(
        role,
        Role::Compose | Role::Subject | Role::Body | Role::Textbox | Role::SlideCanvas
    )
}

/// UIA ControlType 숫자를 `classify_focused_role`이 쓰는 이름으로 바꾼다.
/// 분류에 쓰지 않는 종류는 빈 문자열이다.
pub fn control_type_name(id: i32) -> &'static str {
    match id {
        50000 => "ButtonControl",
        50004 => "EditControl",
        50020 => "TextControl",
        50025 => "CustomControl",
        50026 => "GroupControl",
        50030 => "DocumentControl",
        50033 => "PaneControl",
        _ => "",
    }
}

/// 마우스 커서 위치(물리 픽셀). UIA를 쓰지 않는다.
pub fn cursor_position() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    // SAFETY: 유효한 POINT 포인터를 넘긴다.
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x, p.y))
}

pub struct WindowsUiaBackend {
    keys: Arc<dyn KeySender>,
    clipboard: Arc<dyn Clipboard>,
}

impl WindowsUiaBackend {
    pub fn new(keys: Arc<dyn KeySender>, clipboard: Arc<dyn Clipboard>) -> Self {
        WindowsUiaBackend { keys, clipboard }
    }
}

impl UiaBackend for WindowsUiaBackend {
    fn focused_element(&self) -> Option<FocusedElement> {
        let window = ChromeWindow::foreground()?;
        wait_until_alt_released(self.keys.as_ref(), ALT_RELEASE_TIMEOUT);

        let url = window.url();
        let focused = window.focused()?;
        let (mut name, mut control_type) = focused;
        let mut role = classify_focused_role(&url, &name, control_type);

        if needs_toolbar_recovery(control_type, role, &url) {
            let recovered = url.contains("docs.google.com") && window.focus_render_widget();
            if !recovered {
                // Docs/Slides에서는 Esc가 텍스트 선택을 지우므로 SetFocus가 실패했을 때만 온다.
                if send_chord(self.keys.as_ref(), Chord::Esc).is_err() {
                    tracing::debug!("escape recovery key failed");
                }
                sleep(ESCAPE_RECOVERY_DELAY);
            }
            // 복구 뒤 포커스를 다시 못 읽으면 툴바 버튼에 Ctrl+C를 보내지 않고 중단한다(v5 §2.6 1e/1g).
            let Some((n, ct)) = window.focused() else {
                tracing::debug!(recovered, "toolbar focus recovery: focus unreadable");
                return None;
            };
            name = n;
            control_type = ct;
            role = classify_focused_role(&url, &name, control_type);
            tracing::debug!(recovered, ?role, "toolbar focus recovery");
        }

        let (full_text, selection_text) =
            capture_texts(self.keys.as_ref(), self.clipboard.as_ref(), role);
        tracing::debug!(
            ?role,
            control_type,
            selection_chars = selection_text.as_ref().map(|s| s.chars().count()),
            full_text_chars = full_text.chars().count(),
            "focused element captured"
        );
        Some(FocusedElement {
            url,
            role,
            name,
            full_text,
            selection_text,
        })
    }

    fn foreground_is_docs_or_slides(&self) -> bool {
        ChromeWindow::foreground().is_some_and(|w| {
            let url = w.url();
            is_docs_url(&url) || is_slides_url(&url)
        })
    }

    fn paste_over_selection(&self) -> Result<(), String> {
        paste_over_selection_with(self.keys.as_ref())
    }

    fn paste_replacing_full_text(&self) -> Result<(), String> {
        paste_replacing_full_text_with(self.keys.as_ref())
    }
}

/// Alt가 떼지기를 기다린 뒤 키를 보낸다(Alt가 눌린 채면 Alt+Esc 등 창 전환이 된다).
fn send_chord(keys: &dyn KeySender, chord: Chord) -> Result<(), String> {
    wait_until_alt_released(keys, ALT_RELEASE_TIMEOUT);
    keys.send(chord).map_err(|e| e.to_string())
}

/// (full_text, selection_text). 선택이 있으면 full_text는 빈 문자열이다.
/// 선택이 없고 역할이 허용하면 Ctrl+A 후 sentinel 복사로 전체 텍스트를 읽는다(P2).
fn capture_texts(keys: &dyn KeySender, cb: &dyn Clipboard, role: Role) -> (String, Option<String>) {
    let selection = copy_with_sentinel(cb, || send_chord(keys, Chord::CtrlC));
    if selection.is_some() || !full_text_fallback_allowed(role) {
        return (String::new(), selection);
    }
    if send_chord(keys, Chord::CtrlA).is_err() {
        return (String::new(), None);
    }
    sleep(SELECT_ALL_SETTLE);
    let full = copy_with_sentinel(cb, || send_chord(keys, Chord::CtrlC)).unwrap_or_default();
    (full, None)
}

fn paste_over_selection_with(keys: &dyn KeySender) -> Result<(), String> {
    send_chord(keys, Chord::CtrlV)?;
    sleep(PASTE_SETTLE);
    Ok(())
}

/// 캡처 때의 선택이 번역 중에 풀렸을 수 있으므로 Ctrl+A로 다시 고른다.
fn paste_replacing_full_text_with(keys: &dyn KeySender) -> Result<(), String> {
    send_chord(keys, Chord::CtrlA)?;
    sleep(SELECT_ALL_SETTLE);
    send_chord(keys, Chord::CtrlV)?;
    sleep(PASTE_SETTLE);
    Ok(())
}

thread_local! {
    static COM_INITIALIZED: Cell<bool> = const { Cell::new(false) };
    static AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}

/// 이 스레드의 `IUIAutomation`. COM은 스레드마다 한 번만 초기화한다.
/// UIA 객체는 스레드를 넘기지 않는다.
fn automation() -> Option<IUIAutomation> {
    COM_INITIALIZED.with(|done| {
        if !done.get() {
            // SAFETY: 이 스레드에서 처음 한 번만 부른다. 해제는 스레드 종료에 맡긴다.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            // RPC_E_CHANGED_MODE: 이미 다른 아파트로 초기화된 스레드. UIA는 그대로 쓸 수 있다.
            if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                tracing::warn!(hresult = hr.0, "CoInitializeEx failed");
            }
            done.set(true);
        }
    });
    AUTOMATION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            // SAFETY: COM이 이 스레드에서 초기화된 뒤 만든다.
            match unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) } {
                Ok(uia) => *slot = Some(uia),
                Err(e) => tracing::warn!(hresult = e.code().0, "CUIAutomation create failed"),
            }
        }
        slot.clone()
    })
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: 버퍼 길이를 함께 넘긴다.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    let len = usize::try_from(len).unwrap_or(0).min(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// 포그라운드 Chrome 창의 UIA 요소. 만든 스레드 안에서만 쓴다.
struct ChromeWindow {
    uia: IUIAutomation,
    element: IUIAutomationElement,
}

impl ChromeWindow {
    fn foreground() -> Option<ChromeWindow> {
        // SAFETY: 인자 없는 조회 함수.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() || !class_name(hwnd).starts_with(CHROME_CLASS_PREFIX) {
            tracing::debug!("foreground window is not Chrome");
            return None;
        }
        let uia = automation()?;
        // SAFETY: 유효한 창 핸들로 이 스레드의 UIA를 부른다.
        let element = unsafe { uia.ElementFromHandle(hwnd) }.ok()?;
        Some(ChromeWindow { uia, element })
    }

    /// omnibox 텍스트(스킴이 빠질 수 있다). ValuePattern → Name → 빈 문자열.
    fn url(&self) -> String {
        let Some(omnibox) = self.find(OMNIBOX_CLASS, UIA_EditControlTypeId, OMNIBOX_SEARCH_DEPTH)
        else {
            return String::new();
        };
        // SAFETY: 이 스레드에서 얻은 요소에 대한 조회 호출.
        unsafe {
            match omnibox.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                Ok(pattern) => pattern
                    .CurrentValue()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                Err(_) => omnibox
                    .CurrentName()
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            }
        }
    }

    /// 포커스된 요소의 (Name, ControlType 이름). 없으면 None.
    fn focused(&self) -> Option<(String, &'static str)> {
        // SAFETY: 이 스레드의 UIA로 조회한다.
        unsafe {
            let el = self.uia.GetFocusedElement().ok()?;
            let name = el.CurrentName().map(|n| n.to_string()).unwrap_or_default();
            let ct = el.CurrentControlType().map(|c| c.0).unwrap_or(0);
            Some((name, control_type_name(ct)))
        }
    }

    /// Docs/Slides: 렌더 위젯 Pane에 UIA SetFocus (키를 보내지 않는다).
    fn focus_render_widget(&self) -> bool {
        let Some(pane) = self.find(
            CHROME_RENDER_WIDGET_CLASS,
            UIA_PaneControlTypeId,
            RENDER_WIDGET_SEARCH_DEPTH,
        ) else {
            return false;
        };
        // SAFETY: 이 스레드에서 얻은 요소.
        unsafe { pane.SetFocus() }.is_ok()
    }

    /// v5 `searchDepth`와 같은 깊이 제한 전위 순회(Control View). 자식이 깊이 1이다.
    fn find(
        &self,
        class: &str,
        control_type: UIA_CONTROLTYPE_ID,
        max_depth: u32,
    ) -> Option<IUIAutomationElement> {
        // SAFETY: 이 스레드의 UIA로 만든다.
        let walker = unsafe { self.uia.ControlViewWalker() }.ok()?;
        find_in(&walker, &self.element, class, control_type, 1, max_depth)
    }
}

fn find_in(
    walker: &IUIAutomationTreeWalker,
    parent: &IUIAutomationElement,
    class: &str,
    control_type: UIA_CONTROLTYPE_ID,
    depth: u32,
    max_depth: u32,
) -> Option<IUIAutomationElement> {
    // SAFETY: 모든 요소와 walker는 이 스레드에서 얻었다.
    unsafe {
        let mut child = walker.GetFirstChildElement(parent).ok();
        while let Some(el) = child {
            let is_match = el.CurrentControlType().is_ok_and(|c| c == control_type)
                && el.CurrentClassName().is_ok_and(|c| c == class);
            if is_match {
                return Some(el);
            }
            if depth < max_depth
                && let Some(found) = find_in(walker, &el, class, control_type, depth + 1, max_depth)
            {
                return Some(found);
            }
            child = walker.GetNextSiblingElement(&el).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ClipboardError;
    use crate::hotkey::Modifier;
    use crate::keys::KeyError;
    use std::sync::Mutex;

    const CHAT: &str = "https://chat.google.com/room/abc";
    const GMAIL: &str = "mail.google.com/mail/u/0/#inbox";
    const DOCS: &str = "docs.google.com/document/d/abc/edit";
    const SLIDES: &str = "docs.google.com/presentation/d/abc/edit";
    const OTHER: &str = "https://example.com/";

    #[test]
    fn recovery_only_for_button_on_history_or_unknown_in_supported_url() {
        for url in [CHAT, GMAIL, DOCS, SLIDES] {
            assert!(needs_toolbar_recovery("ButtonControl", Role::Unknown, url));
        }
        assert!(needs_toolbar_recovery(
            "ButtonControl",
            Role::MessageHistory,
            CHAT
        ));
        // 버튼이 아니면 복구하지 않는다 (대화 기록 클릭은 안전하게 실패해야 한다).
        assert!(!needs_toolbar_recovery(
            "EditControl",
            Role::MessageHistory,
            CHAT
        ));
        assert!(!needs_toolbar_recovery("", Role::Unknown, GMAIL));
        // 다른 역할이면 복구하지 않는다.
        assert!(!needs_toolbar_recovery(
            "ButtonControl",
            Role::Compose,
            CHAT
        ));
        assert!(!needs_toolbar_recovery(
            "ButtonControl",
            Role::Subject,
            GMAIL
        ));
        assert!(!needs_toolbar_recovery("ButtonControl", Role::Canvas, DOCS));
        // 지원하지 않는 URL이면 복구하지 않는다.
        assert!(!needs_toolbar_recovery(
            "ButtonControl",
            Role::Unknown,
            OTHER
        ));
        assert!(!needs_toolbar_recovery("ButtonControl", Role::Unknown, ""));
    }

    #[test]
    fn docs_canvas_never_uses_full_text_fallback() {
        assert!(!full_text_fallback_allowed(Role::Canvas));
        for role in [
            Role::Compose,
            Role::Subject,
            Role::Body,
            Role::Textbox,
            Role::SlideCanvas,
        ] {
            assert!(full_text_fallback_allowed(role), "{role:?}");
        }
        for role in [
            Role::MessageHistory,
            Role::Cc,
            Role::Bcc,
            Role::To,
            Role::Unknown,
        ] {
            assert!(!full_text_fallback_allowed(role), "{role:?}");
        }
    }

    #[test]
    fn control_type_ids_map_to_classifier_names() {
        assert_eq!(control_type_name(50000), "ButtonControl");
        assert_eq!(control_type_name(50004), "EditControl");
        assert_eq!(control_type_name(50030), "DocumentControl");
        assert_eq!(control_type_name(50033), "PaneControl");
        assert_eq!(control_type_name(50026), "GroupControl");
        assert_eq!(control_type_name(50025), "CustomControl");
        assert_eq!(control_type_name(50020), "TextControl");
        assert_eq!(control_type_name(50003), "");
        assert_eq!(control_type_name(0), "");
    }

    /// 클립보드 + 키 가짜. Ctrl+C는 그때의 `copy_result`를 클립보드에 쓴다(None이면 아무것도 안 함).
    struct Fake {
        text: Mutex<Option<String>>,
        seq: Mutex<u32>,
        sent: Mutex<Vec<Chord>>,
        /// Ctrl+A 전 / 후 Ctrl+C 결과.
        copy_before_select_all: Option<String>,
        copy_after_select_all: Option<String>,
    }

    impl Fake {
        fn new(clip: &str, before: Option<&str>, after: Option<&str>) -> Arc<Fake> {
            Arc::new(Fake {
                text: Mutex::new(Some(clip.to_string())),
                seq: Mutex::new(0),
                sent: Mutex::new(Vec::new()),
                copy_before_select_all: before.map(String::from),
                copy_after_select_all: after.map(String::from),
            })
        }
        fn sent(&self) -> Vec<Chord> {
            self.sent.lock().unwrap().clone()
        }
        fn clip(&self) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
    }

    impl Clipboard for Fake {
        fn get_text(&self) -> Option<String> {
            self.clip()
        }
        fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
            *self.text.lock().unwrap() = Some(text.to_string());
            *self.seq.lock().unwrap() += 1;
            Ok(())
        }
        fn sequence_number(&self) -> u32 {
            *self.seq.lock().unwrap()
        }
    }

    impl KeySender for Fake {
        fn send(&self, chord: Chord) -> Result<(), KeyError> {
            let selected_all = self.sent().contains(&Chord::CtrlA);
            self.sent.lock().unwrap().push(chord);
            if chord == Chord::CtrlC {
                let result = if selected_all {
                    &self.copy_after_select_all
                } else {
                    &self.copy_before_select_all
                };
                if let Some(t) = result {
                    let _ = self.set_text(t);
                }
            }
            Ok(())
        }
        fn is_down(&self, _m: Modifier) -> bool {
            false
        }
    }

    #[test]
    fn selection_skips_full_text_copy_and_restores_clipboard() {
        let f = Fake::new("user", Some("picked"), Some("all"));
        let (full, sel) = capture_texts(f.as_ref(), f.as_ref(), Role::Compose);
        assert_eq!(sel.as_deref(), Some("picked"));
        assert_eq!(full, "");
        assert_eq!(f.sent(), vec![Chord::CtrlC]);
        assert_eq!(f.clip().as_deref(), Some("user"));
    }

    #[test]
    fn no_selection_copies_full_text_with_sentinel_when_allowed() {
        let f = Fake::new("user", None, Some("whole field"));
        let (full, sel) = capture_texts(f.as_ref(), f.as_ref(), Role::Body);
        assert_eq!(sel, None);
        assert_eq!(full, "whole field");
        assert_eq!(f.sent(), vec![Chord::CtrlC, Chord::CtrlA, Chord::CtrlC]);
        assert_eq!(f.clip().as_deref(), Some("user"));
    }

    #[test]
    fn empty_field_full_text_is_empty_not_previous_clipboard() {
        let f = Fake::new("user", None, None);
        let (full, sel) = capture_texts(f.as_ref(), f.as_ref(), Role::SlideCanvas);
        assert_eq!((full.as_str(), sel), ("", None));
        assert_eq!(f.clip().as_deref(), Some("user"));
    }

    #[test]
    fn docs_canvas_without_selection_sends_only_ctrl_c() {
        let f = Fake::new("user", None, Some("all"));
        let (full, sel) = capture_texts(f.as_ref(), f.as_ref(), Role::Canvas);
        assert_eq!((full.as_str(), sel), ("", None));
        assert_eq!(f.sent(), vec![Chord::CtrlC]);
    }

    #[test]
    fn paste_sequences_and_settle_waits() {
        let f = Fake::new("", None, None);
        let start = std::time::Instant::now();
        paste_over_selection_with(f.as_ref()).unwrap();
        assert!(start.elapsed() >= PASTE_SETTLE);
        assert_eq!(f.sent(), vec![Chord::CtrlV]);

        let f = Fake::new("", None, None);
        let start = std::time::Instant::now();
        paste_replacing_full_text_with(f.as_ref()).unwrap();
        assert!(start.elapsed() >= SELECT_ALL_SETTLE + PASTE_SETTLE);
        assert_eq!(f.sent(), vec![Chord::CtrlA, Chord::CtrlV]);
    }
}
