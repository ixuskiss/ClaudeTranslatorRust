//! Windows UI Automation 백엔드 (v5 참조 §2.6).
//!
//! UIA는 포그라운드 창, omnibox URL, 포커스된 요소의 Name/ControlType(Sheets면 AutomationId), 렌더 위젯
//! `SetFocus`, Sheets 셀 편집기의 ValuePattern에만 쓴다. 그 밖의 텍스트는 클립보드 왕복(Ctrl+C)으로 읽는다.
//! 캡처한 텍스트는 절대 로그에 남기지 않는다.

use super::adapter::UiaBackend;
use super::role::{Role, classify_focused_role, is_docs_url, is_slides_url, is_supported_url};
use super::strategy::{FocusedElement, is_sheets_editor_role};
use crate::clipboard::{Clipboard, copy_with_sentinel};
use crate::format_clipboard::{ClipboardGuard, FormatClipboard, wait_for_clipboard_change};
use crate::keys::{ALT_RELEASE_TIMEOUT, Chord, KeySender, wait_until_alt_released};
use ct_sheets::html_rich_text_state;
use ct_sheets::routing::is_google_sheets_url;
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
/// Sheets 셀 편집기 복사가 클립보드를 바꿀 때까지 기다리는 최대 시간(S-ref §5.1). 폴링은 20 ms.
pub const SHEETS_COPY_TIMEOUT: Duration = Duration::from_millis(300);
const HTML_FORMAT: &str = "HTML Format";
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
/// 표준 종류(50000~50040)는 모두 이름을 준다. Sheets 분기는 빈 이름을 표 그리드로 보므로
/// 메뉴 항목 등 다른 종류가 빈 문자열이 되면 안 된다. 모르는 값만 빈 문자열이다.
pub fn control_type_name(id: i32) -> &'static str {
    match id {
        50000 => "ButtonControl",
        50001 => "CalendarControl",
        50002 => "CheckBoxControl",
        50003 => "ComboBoxControl",
        50004 => "EditControl",
        50005 => "HyperlinkControl",
        50006 => "ImageControl",
        50007 => "ListItemControl",
        50008 => "ListControl",
        50009 => "MenuControl",
        50010 => "MenuBarControl",
        50011 => "MenuItemControl",
        50012 => "ProgressBarControl",
        50013 => "RadioButtonControl",
        50014 => "ScrollBarControl",
        50015 => "SliderControl",
        50016 => "SpinnerControl",
        50017 => "StatusBarControl",
        50018 => "TabControl",
        50019 => "TabItemControl",
        50020 => "TextControl",
        50021 => "ToolBarControl",
        50022 => "ToolTipControl",
        50023 => "TreeControl",
        50024 => "TreeItemControl",
        50025 => "CustomControl",
        50026 => "GroupControl",
        50027 => "ThumbControl",
        50028 => "DataGridControl",
        50029 => "DataItemControl",
        50030 => "DocumentControl",
        50031 => "SplitButtonControl",
        50032 => "WindowControl",
        50033 => "PaneControl",
        50034 => "HeaderControl",
        50035 => "HeaderItemControl",
        50036 => "TableControl",
        50037 => "TitleBarControl",
        50038 => "SeparatorControl",
        50039 => "SemanticZoomControl",
        50040 => "AppBarControl",
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
    format_clipboard: Arc<dyn FormatClipboard>,
}

impl WindowsUiaBackend {
    pub fn new(
        keys: Arc<dyn KeySender>,
        clipboard: Arc<dyn Clipboard>,
        format_clipboard: Arc<dyn FormatClipboard>,
    ) -> Self {
        WindowsUiaBackend {
            keys,
            clipboard,
            format_clipboard,
        }
    }
}

impl UiaBackend for WindowsUiaBackend {
    fn focused_element(&self) -> Option<FocusedElement> {
        let window = ChromeWindow::foreground()?;
        let url = window.url();
        let (focused, role) = resolve_focus(&window, self.keys.as_ref(), &url)?;

        if is_sheets_editor_role(role) {
            return Some(self.sheets_editor_element(url, role, focused));
        }

        let (full_text, selection_text) =
            capture_texts(self.keys.as_ref(), self.clipboard.as_ref(), role);
        tracing::debug!(
            ?role,
            control_type = focused.control_type,
            selection_chars = selection_text.as_ref().map(|s| s.chars().count()),
            full_text_chars = full_text.chars().count(),
            "focused element captured"
        );
        Some(FocusedElement {
            url,
            role,
            name: focused.name,
            full_text,
            selection_text,
            editor_full_text: None,
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

impl WindowsUiaBackend {
    /// S-ref §5.1. sentinel·Ctrl+A 없이 Ctrl+C 한 번과 시퀀스 번호로 선택을 판정하고,
    /// 셀 전체 텍스트는 ValuePattern으로 읽는다. 클립보드 구간 잠금은 호출하는 어댑터가 잡고 있다.
    fn sheets_editor_element(&self, url: String, role: Role, focused: Focused) -> FocusedElement {
        let (selection_text, html) =
            copy_editor_selection(self.keys.as_ref(), self.format_clipboard.as_ref());
        let editor_full_text = focused.value().map(strip_one_trailing_newline);
        let rich = html.as_deref().and_then(html_rich_text_state);
        tracing::debug!(
            ?role,
            full_chars = editor_full_text.as_ref().map(|s| s.chars().count()),
            selection_chars = selection_text.as_ref().map(|s| s.chars().count()),
            rich,
            "sheets editor capture"
        );
        FocusedElement {
            url,
            role,
            name: focused.name,
            full_text: String::new(),
            selection_text,
            editor_full_text,
        }
    }
}

/// UI-ref 2.6 포커스 해석: Alt 해제 대기(<= 0.5초), 포커스 읽기와 역할 분류, 툴바 포커스 복구.
/// 복구 뒤에도 포커스를 못 읽으면 None이다. 텍스트 경로와 Sheets 라우팅이 함께 쓴다.
pub(crate) fn resolve_focus(
    window: &ChromeWindow,
    keys: &dyn KeySender,
    url: &str,
) -> Option<(Focused, Role)> {
    wait_until_alt_released(keys, ALT_RELEASE_TIMEOUT);
    // AutomationId는 Sheets URL일 때만 읽는다(S-ref §1.3).
    let read_id = is_google_sheets_url(url);
    let mut focused = window.focused(read_id)?;
    let mut role = focused.classify(url);

    if needs_toolbar_recovery(focused.control_type, role, url) {
        let recovered = url.contains("docs.google.com") && window.focus_render_widget();
        if !recovered {
            // Docs/Slides에서는 Esc가 텍스트 선택을 지우므로 SetFocus가 실패했을 때만 온다.
            if send_chord(keys, Chord::Esc).is_err() {
                tracing::debug!("escape recovery key failed");
            }
            sleep(ESCAPE_RECOVERY_DELAY);
        }
        // 복구 뒤 포커스를 다시 못 읽으면 툴바 버튼에 Ctrl+C를 보내지 않고 중단한다(v5 §2.6 1e/1g).
        let Some(f) = window.focused(read_id) else {
            tracing::debug!(recovered, "toolbar focus recovery: focus unreadable");
            return None;
        };
        focused = f;
        role = focused.classify(url);
        tracing::debug!(recovered, ?role, "toolbar focus recovery");
    }
    Some((focused, role))
}

/// Alt가 떼지기를 기다린 뒤 키를 보낸다(Alt가 눌린 채면 Alt+Esc 등 창 전환이 된다).
fn send_chord(keys: &dyn KeySender, chord: Chord) -> Result<(), String> {
    wait_until_alt_released(keys, ALT_RELEASE_TIMEOUT);
    keys.send(chord).map_err(|e| e.to_string())
}

/// Sheets 셀 편집기 복사: (선택 텍스트, 클립보드 HTML). 다중 포맷 가드로 사용자 클립보드를
/// 되돌린다. 빈 선택이면 Chrome이 아무것도 복사하지 않으므로 0.3초 안에 시퀀스 번호가
/// 바뀌지 않으면 선택 없음이다. Ctrl+A는 보내지 않는다.
fn copy_editor_selection(
    keys: &dyn KeySender,
    cb: &dyn FormatClipboard,
) -> (Option<String>, Option<String>) {
    let mut guard = ClipboardGuard::begin(cb);
    let before = cb.sequence_number();
    let copied = match send_chord(keys, Chord::CtrlC) {
        Ok(()) => wait_for_clipboard_change(cb, before, SHEETS_COPY_TIMEOUT),
        Err(_) => {
            tracing::debug!("sheets editor copy key failed");
            false
        }
    };
    let result = if copied {
        guard.mark_own_write();
        let text = cb.read_text().filter(|t| !t.is_empty());
        let html = cb.read_registered(HTML_FORMAT).map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .trim_end_matches('\0')
                .to_string()
        });
        (text, html)
    } else {
        (None, None)
    };
    guard.finish();
    result
}

/// ValuePattern 값 끝의 `\n` 하나만 지운다(Sheets 편집기가 붙인다).
pub(crate) fn strip_one_trailing_newline(mut value: String) -> String {
    if value.ends_with('\n') {
        value.pop();
    }
    value
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
pub(crate) struct ChromeWindow {
    uia: IUIAutomation,
    element: IUIAutomationElement,
}

impl ChromeWindow {
    pub(crate) fn foreground() -> Option<ChromeWindow> {
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
    pub(crate) fn url(&self) -> String {
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

    /// 포커스된 요소와 Name, ControlType 이름. `read_automation_id`면 AutomationId도 읽는다.
    fn focused(&self, read_automation_id: bool) -> Option<Focused> {
        // SAFETY: 이 스레드의 UIA로 조회한다.
        unsafe {
            let el = self.uia.GetFocusedElement().ok()?;
            let name = el.CurrentName().map(|n| n.to_string()).unwrap_or_default();
            let ct = el.CurrentControlType().map(|c| c.0).unwrap_or(0);
            let automation_id = if read_automation_id {
                el.CurrentAutomationId()
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            Some(Focused {
                element: el,
                name,
                control_type: control_type_name(ct),
                automation_id,
            })
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
        self.find_where(max_depth, &|el| {
            // SAFETY: 이 스레드에서 얻은 요소에 대한 조회 호출.
            unsafe {
                el.CurrentControlType().is_ok_and(|c| c == control_type)
                    && el.CurrentClassName().is_ok_and(|c| c == class)
            }
        })
    }

    /// `is_match`를 만족하는 첫 요소(깊이 제한 전위 순회, 자식이 깊이 1).
    fn find_where(
        &self,
        max_depth: u32,
        is_match: &dyn Fn(&IUIAutomationElement) -> bool,
    ) -> Option<IUIAutomationElement> {
        // SAFETY: 이 스레드의 UIA로 만든다.
        let walker = unsafe { self.uia.ControlViewWalker() }.ok()?;
        find_in(&walker, &self.element, is_match, 1, max_depth)
    }

    /// `is_match`를 만족하는 첫 EditControl의 ValuePattern 값.
    /// 요소가 없거나 값을 못 읽으면 None.
    pub(crate) fn edit_value_where(
        &self,
        max_depth: u32,
        is_match: &dyn Fn(&IUIAutomationElement) -> bool,
    ) -> Option<String> {
        let el = self.find_where(max_depth, &|el| {
            // SAFETY: 이 스레드에서 얻은 요소에 대한 조회 호출.
            let is_edit =
                unsafe { el.CurrentControlType() }.is_ok_and(|c| c == UIA_EditControlTypeId);
            is_edit && is_match(el)
        })?;
        // SAFETY: 이 스레드에서 얻은 요소에 대한 조회 호출.
        unsafe {
            let pattern = el
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()?;
            pattern.CurrentValue().ok().map(|v| v.to_string())
        }
    }
}

/// 포커스된 UIA 요소 스냅샷. 만든 스레드 안에서만 쓴다.
pub(crate) struct Focused {
    element: IUIAutomationElement,
    name: String,
    control_type: &'static str,
    /// Sheets URL이 아니면 읽지 않아 빈 문자열이다.
    automation_id: String,
}

impl Focused {
    fn classify(&self, url: &str) -> Role {
        classify_focused_role(url, &self.name, self.control_type, &self.automation_id)
    }

    /// ValuePattern 값. 읽지 못하면 None.
    pub(crate) fn value(&self) -> Option<String> {
        // SAFETY: 이 스레드에서 얻은 요소에 대한 조회 호출.
        unsafe {
            let pattern = self
                .element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()?;
            pattern.CurrentValue().ok().map(|v| v.to_string())
        }
    }
}

fn find_in(
    walker: &IUIAutomationTreeWalker,
    parent: &IUIAutomationElement,
    is_match: &dyn Fn(&IUIAutomationElement) -> bool,
    depth: u32,
    max_depth: u32,
) -> Option<IUIAutomationElement> {
    // SAFETY: 모든 요소와 walker는 이 스레드에서 얻었다.
    unsafe {
        let mut child = walker.GetFirstChildElement(parent).ok();
        while let Some(el) = child {
            if is_match(&el) {
                return Some(el);
            }
            if depth < max_depth
                && let Some(found) = find_in(walker, &el, is_match, depth + 1, max_depth)
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
    use crate::format_clipboard::ClipboardSnapshot;
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
        assert_eq!(control_type_name(50003), "ComboBoxControl");
        assert_eq!(control_type_name(50011), "MenuItemControl");
        assert_eq!(control_type_name(50032), "WindowControl");
        assert_eq!(control_type_name(50019), "TabItemControl");
        assert_eq!(control_type_name(50005), "HyperlinkControl");
        assert_eq!(control_type_name(49999), "");
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

    /// 다중 포맷 클립보드 + 키 가짜. Ctrl+C는 `copy`가 있으면 텍스트·HTML을 쓰고 시퀀스를 올린다.
    struct EditorFake {
        seq: Mutex<u32>,
        text: Mutex<Option<String>>,
        html: Mutex<Option<Vec<u8>>>,
        sent: Mutex<Vec<Chord>>,
        restores: Mutex<Vec<ClipboardSnapshot>>,
        copy: Option<(&'static str, &'static str)>,
    }

    impl EditorFake {
        fn new(copy: Option<(&'static str, &'static str)>) -> EditorFake {
            EditorFake {
                seq: Mutex::new(7),
                text: Mutex::new(Some("user clipboard".into())),
                html: Mutex::new(None),
                sent: Mutex::new(Vec::new()),
                restores: Mutex::new(Vec::new()),
                copy,
            }
        }
        fn snapshot_value() -> ClipboardSnapshot {
            ClipboardSnapshot::new(vec![(13, b"u\0s\0e\0r\0\0\0".to_vec())])
        }
    }

    impl FormatClipboard for EditorFake {
        fn sequence_number(&self) -> u32 {
            *self.seq.lock().unwrap()
        }
        fn snapshot(&self) -> Option<ClipboardSnapshot> {
            Some(EditorFake::snapshot_value())
        }
        fn restore(&self, snapshot: &ClipboardSnapshot) -> Result<(), ClipboardError> {
            self.restores.lock().unwrap().push(snapshot.clone());
            *self.seq.lock().unwrap() += 1;
            Ok(())
        }
        fn read_text(&self) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
        fn read_registered(&self, format_name: &str) -> Option<Vec<u8>> {
            assert_eq!(format_name, "HTML Format");
            self.html.lock().unwrap().clone()
        }
        fn write_registered(&self, _: &[(&str, &[u8])]) -> Result<(), ClipboardError> {
            unreachable!("editor capture never writes")
        }
    }

    impl KeySender for EditorFake {
        fn send(&self, chord: Chord) -> Result<(), KeyError> {
            self.sent.lock().unwrap().push(chord);
            if chord == Chord::CtrlC
                && let Some((text, html)) = self.copy
            {
                *self.text.lock().unwrap() = Some(text.into());
                let mut bytes = html.as_bytes().to_vec();
                bytes.push(0);
                *self.html.lock().unwrap() = Some(bytes);
                *self.seq.lock().unwrap() += 1;
            }
            Ok(())
        }
        fn is_down(&self, _m: Modifier) -> bool {
            false
        }
    }

    const EDITOR_HTML: &str = "<span data-sheets-root=\"1\">picked</span>";

    #[test]
    fn editor_capture_never_sends_select_all() {
        let f = EditorFake::new(Some(("picked", EDITOR_HTML)));
        let (selection, html) = copy_editor_selection(&f, &f);
        assert_eq!(selection.as_deref(), Some("picked"));
        assert_eq!(html.as_deref(), Some(EDITOR_HTML));
        assert_eq!(*f.sent.lock().unwrap(), vec![Chord::CtrlC]);
        // 우리 복사가 바꾼 클립보드는 사용자 내용으로 되돌린다.
        assert_eq!(
            *f.restores.lock().unwrap(),
            vec![EditorFake::snapshot_value()]
        );
    }

    #[test]
    fn editor_copy_without_clipboard_change_is_no_selection() {
        let f = EditorFake::new(None);
        let start = std::time::Instant::now();
        let (selection, html) = copy_editor_selection(&f, &f);
        assert_eq!((selection, html), (None, None));
        assert!(start.elapsed() >= SHEETS_COPY_TIMEOUT);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(*f.sent.lock().unwrap(), vec![Chord::CtrlC]);
        // 우리가 쓰지 않았으므로 복원하지 않는다.
        assert!(f.restores.lock().unwrap().is_empty());
    }

    #[test]
    fn editor_copy_of_empty_text_is_no_selection() {
        let f = EditorFake::new(Some(("", "")));
        let (selection, _) = copy_editor_selection(&f, &f);
        assert_eq!(selection, None);
        assert_eq!(*f.sent.lock().unwrap(), vec![Chord::CtrlC]);
    }

    #[test]
    fn editor_value_drops_exactly_one_trailing_newline() {
        assert_eq!(strip_one_trailing_newline("abc\n".into()), "abc");
        assert_eq!(strip_one_trailing_newline("abc\n\n".into()), "abc\n");
        assert_eq!(strip_one_trailing_newline("abc".into()), "abc");
        assert_eq!(strip_one_trailing_newline("\n".into()), "");
        assert_eq!(strip_one_trailing_newline("a\nb ".into()), "a\nb ");
    }

    #[test]
    fn editor_roles_use_editor_capture() {
        assert!(is_sheets_editor_role(Role::SheetsCellEditor));
        assert!(is_sheets_editor_role(Role::SheetsFormulaBar));
        for role in [
            Role::SheetsGrid,
            Role::SheetsNameBox,
            Role::Compose,
            Role::Unknown,
        ] {
            assert!(!is_sheets_editor_role(role), "{role:?}");
        }
        assert!(!full_text_fallback_allowed(Role::SheetsCellEditor));
        assert!(!full_text_fallback_allowed(Role::SheetsGrid));
    }
}
