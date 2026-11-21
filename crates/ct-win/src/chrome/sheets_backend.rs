//! Windows Sheets 범위 백엔드 (S-ref §1.4, §2.1~§2.3, §4.3).
//!
//! 라우팅 상태(URL + 포커스 역할), 이름 상자 읽기, 범위 복사·파싱, 단 한 번의 범위 쓰기를 맡는다.
//! 판단은 순수 함수(`capture_decision`, `paste_target_matches`, 다듬기 함수)와
//! [`SheetsProbe`] 위의 순서 함수(`capture_with`, `write_with`)에 두고, UIA 연결은 얇게 둔다.
//! 셀 텍스트·클립보드 내용은 절대 로그에 남기지 않는다.

use super::role::{Role, sheets_role};
use super::uia_backend::{ChromeWindow, resolve_focus, strip_one_trailing_newline};
use crate::clipboard::lock_clipboard_section;
use crate::format_clipboard::{ClipboardGuard, FormatClipboard, wait_for_clipboard_change};
use crate::keys::{ALT_RELEASE_TIMEOUT, Chord, KeySender, wait_until_alt_released};
use ct_sheets::{
    CUSTOM_FORMAT, CaptureError, ParsedAddress, SheetLocation, SheetSelection, SheetsInputState,
    SheetsInteraction, SheetsRouting, UnsupportedReason, changed_text_cells, compact_from_custom,
    extract_cf_html_fragment, input_state_from_role, is_google_sheets_url, native_payload,
    parse_range_address, parse_sheets_html, parse_sheets_url,
};
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::UI::Accessibility::IUIAutomationElement;

const HTML_FORMAT: &str = "HTML Format";
/// Ctrl+C 뒤 범위 복사가 클립보드를 바꿀 때까지 기다리는 최대 시간(S-ref §2.3).
pub const RANGE_COPY_TIMEOUT: Duration = Duration::from_millis(1000);
/// Ctrl+V, Ctrl+C 뒤 붙여넣기 확인을 기다리는 최대 시간(S-ref §4.3).
/// Sheets는 붙여넣기 적용에 몇 초가 걸릴 수 있어 이 창 안에 확인되지 않는 일이 흔하다.
/// 짧게 두고, 실제 결과 확인은 서비스의 지연 재확인(`VERIFY_DELAYS`)이 맡는다.
pub const PASTE_CONFIRM_TIMEOUT: Duration = Duration::from_millis(1000);
/// 이름 상자 검색 깊이(S-ref §2.2).
pub const NAME_BOX_SEARCH_DEPTH: u32 = 24;
const NAME_BOX_AUTOMATION_ID: &str = "t-name-box";
const NAME_BOX_NAMES: [&str; 2] = ["이름 상자", "Name Box"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timeouts {
    pub copy: Duration,
    pub paste_confirm: Duration,
}

const TIMEOUTS: Timeouts = Timeouts {
    copy: RANGE_COPY_TIMEOUT,
    paste_confirm: PASTE_CONFIRM_TIMEOUT,
};

fn capture_failed() -> CaptureError {
    CaptureError { reason: None }
}

fn unsupported(reason: UnsupportedReason) -> CaptureError {
    CaptureError {
        reason: Some(reason),
    }
}

/// 복사 전 거절(S-ref §2.1 1~4, §10.1). Sheets 문서의 셀 선택이 아니면 이유 없는 실패,
/// 전체 행·열과 여러 범위, `max_range_cells`를 넘는 주소는 복사하지 않고 거절한다.
/// 주소를 모르거나 읽을 수 없으면 복사는 한다(이후 지원 판정이 `UnidentifiedTarget`으로 끝낸다).
pub(crate) fn capture_decision(
    url: Option<&str>,
    state: Option<SheetsInputState>,
    address: Option<&str>,
    max_range_cells: usize,
) -> Result<(), CaptureError> {
    let (document_id, _) = parse_sheets_url(url.unwrap_or(""));
    if document_id.is_none() || state != Some(SheetsInputState::CellSelection) {
        return Err(capture_failed());
    }
    match address.map(parse_range_address) {
        Some(ParsedAddress::Unsupported(reason))
            if reason != UnsupportedReason::UnidentifiedTarget =>
        {
            Err(unsupported(reason))
        }
        Some(ParsedAddress::Size(rows, cols)) if rows.saturating_mul(cols) > max_range_cells => {
            Err(unsupported(UnsupportedReason::RangeTooLarge))
        }
        _ => Ok(()),
    }
}

/// 붙여넣기 직전 재확인(S-ref §4.3 1): 같은 문서·gid, 셀 선택 상태, 캡처 주소가 있고 지금 주소와 같다.
pub(crate) fn paste_target_matches(
    captured: &SheetSelection,
    url: Option<&str>,
    state: Option<SheetsInputState>,
    address: Option<&str>,
) -> bool {
    let (document_id, gid) = parse_sheets_url(url.unwrap_or(""));
    let location = &captured.location;
    document_id.as_deref() == Some(location.document_id.as_str())
        && gid == location.gid
        && state == Some(SheetsInputState::CellSelection)
        && location.range_address.is_some()
        && address == location.range_address.as_deref()
}

/// 포커스 역할과 편집기 값에서 입력 상태를 정한다(S-ref §1.4 4~5).
/// 빈 셀 편집기를 범위 선택으로 보는 것은 `empty_editor_is_selection`일 때만이다.
pub(crate) fn focus_input_state(
    role: Role,
    editor_value: Option<&str>,
    empty_editor_is_selection: bool,
) -> SheetsInputState {
    let value = if empty_editor_is_selection && role == Role::SheetsCellEditor {
        editor_value
    } else {
        None
    };
    input_state_from_role(sheets_role(role), value)
}

/// 이름 상자 후보 값 중 처음으로 비어 있지 않은 값(앞뒤 공백 제거). 뒤 후보는 찾지 않는다.
pub(crate) fn first_address<I>(lookups: I) -> Option<String>
where
    I: IntoIterator<Item = Option<String>>,
{
    lookups
        .into_iter()
        .flatten()
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
}

/// GlobalSize로 늘어난 사용자 정의 포맷 바이트를 선언 길이(첫 LE u32 + 4)로 자른다.
/// 선언 길이가 버퍼보다 길거나 헤더가 없으면 그대로 둔다(디코더가 거절한다).
pub(crate) fn trim_custom_data(raw: &[u8]) -> &[u8] {
    let Some(header) = raw.first_chunk::<4>() else {
        return raw;
    };
    let declared = usize::try_from(u32::from_le_bytes(*header))
        .ok()
        .and_then(|n| n.checked_add(4));
    match declared {
        Some(n) if n <= raw.len() => &raw[..n],
        _ => raw,
    }
}

/// HTML 바이트 끝의 NUL(문자열 종결자와 GlobalSize 여분)을 지운다.
pub(crate) fn trim_trailing_nuls(raw: &[u8]) -> &[u8] {
    let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    &raw[..end]
}

/// 포커스와 이름 상자 조회(UIA). 테스트에서는 가짜로 바꾼다.
pub(crate) trait SheetsProbe {
    fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>);
    fn range_address(&self) -> Option<String>;
}

/// S-ref §2.1. 클립보드 구간 잠금은 호출자가 잡는다.
pub(crate) fn capture_with(
    probe: &dyn SheetsProbe,
    keys: &dyn KeySender,
    cb: &dyn FormatClipboard,
    max_range_cells: usize,
    timeouts: Timeouts,
) -> Result<SheetSelection, CaptureError> {
    let (url, state) = probe.url_and_state();
    let url = url.unwrap_or_default();
    let (document_id, gid) = parse_sheets_url(&url);
    let focus_ok = document_id.is_some() && state == Some(SheetsInputState::CellSelection);
    // 이름 상자는 복사 전에 읽는다.
    let address = if focus_ok {
        probe.range_address()
    } else {
        None
    };
    if let Err(e) = capture_decision(Some(&url), state, address.as_deref(), max_range_cells) {
        tracing::debug!(reason = ?e.reason, "sheets capture rejected before copy");
        return Err(e);
    }
    let Some(document_id) = document_id else {
        return Err(capture_failed());
    };

    let (html, custom) = copy_selection_data(keys, cb, timeouts.copy)?;
    let fragment = extract_cf_html_fragment(&html).map_err(|e| {
        tracing::info!(error = %e, "Sheets clipboard metadata rejected");
        capture_failed()
    })?;
    let compact = compact_from_custom(custom.as_deref()).map_err(|e| {
        tracing::info!(error = %e, "Sheets clipboard metadata rejected");
        capture_failed()
    })?;
    let grid = parse_sheets_html(&fragment, compact.as_ref()).map_err(|e| {
        tracing::info!(error = %e, "Sheets clipboard metadata rejected");
        capture_failed()
    })?;
    let parsed = address.as_deref().map(parse_range_address);
    let hidden_or_filtered =
        (parsed == Some(ParsedAddress::Size(grid.rows, grid.cols))).then_some(false);
    tracing::debug!(
        rows = grid.rows,
        cols = grid.cols,
        compact = grid.compact_json.is_some(),
        address_known = address.is_some(),
        "sheets range captured"
    );
    Ok(SheetSelection {
        location: SheetLocation {
            document_id,
            gid,
            range_address: address,
        },
        grid,
        hidden_or_filtered,
    })
}

/// S-ref §2.3: 가드 안에서 Ctrl+C, 변화 대기, HTML·사용자 정의 포맷 읽기. 텍스트는 읽지 않는다.
fn copy_selection_data(
    keys: &dyn KeySender,
    cb: &dyn FormatClipboard,
    timeout: Duration,
) -> Result<(String, Option<Vec<u8>>), CaptureError> {
    let mut guard = ClipboardGuard::begin(cb);
    let before = cb.sequence_number();
    wait_until_alt_released(keys, ALT_RELEASE_TIMEOUT);
    let copied = match keys.send(Chord::CtrlC) {
        Ok(()) => wait_for_clipboard_change(cb, before, timeout),
        Err(e) => {
            tracing::debug!(error = %e, "sheets range copy key failed");
            false
        }
    };
    if !copied {
        guard.finish();
        tracing::info!("sheets copy produced no (or unsettled) clipboard data");
        return Err(capture_failed());
    }
    guard.mark_own_write();
    let html = cb
        .read_registered(HTML_FORMAT)
        .map(|b| String::from_utf8_lossy(trim_trailing_nuls(&b)).into_owned());
    let custom = cb
        .read_registered(CUSTOM_FORMAT)
        .map(|b| trim_custom_data(&b).to_vec());
    guard.finish();
    match html {
        Some(h) if !h.is_empty() => Ok((h, custom)),
        _ => {
            tracing::info!("sheets copied data has no HTML");
            Err(capture_failed())
        }
    }
}

/// S-ref §4.3. 클립보드 구간 잠금은 호출자가 잡는다. 붙여넣기 확인이 안 되면 사용자
/// 클립보드를 복원하지 않고 Ok를 돌려준다(D9: 서비스의 검증 단계가 판단한다).
pub(crate) fn write_with(
    probe: &dyn SheetsProbe,
    keys: &dyn KeySender,
    cb: &dyn FormatClipboard,
    selection: &SheetSelection,
    cf_html: &str,
    timeouts: Timeouts,
) -> Result<(), String> {
    // 1. 붙여넣기 직전 재확인.
    let (url, state) = probe.url_and_state();
    let address = probe.range_address();
    if !paste_target_matches(selection, url.as_deref(), state, address.as_deref()) {
        tracing::info!("sheets target changed before paste");
        return Err("target changed before paste".into());
    }
    // 2. 네이티브 페이로드(compact 데이터가 있을 때만). 실패하면 클립보드를 건드리지 않는다.
    let native = match selection.grid.compact_json {
        Some(_) => {
            let fragment = extract_cf_html_fragment(cf_html).map_err(|e| {
                tracing::info!(error = %e, "sheets native payload rejected");
                e.to_string()
            })?;
            let replaced = changed_text_cells(&selection.grid, &fragment).map_err(|e| {
                tracing::info!(error = %e, "sheets native payload rejected");
                e.to_string()
            })?;
            native_payload(&selection.grid, &fragment, &replaced).map_err(|e| {
                tracing::info!(error = %e, "sheets native payload rejected");
                e.to_string()
            })?
        }
        None => None,
    };
    // 3. 가드 + 다중 포맷 쓰기. CF_UNICODETEXT는 쓰지 않는다.
    let mut guard = ClipboardGuard::begin(cb);
    let mut formats: Vec<(&str, &[u8])> = vec![(HTML_FORMAT, cf_html.as_bytes())];
    if let Some(native) = native.as_deref() {
        formats.push((CUSTOM_FORMAT, native));
    }
    if let Err(e) = cb.write_registered(&formats) {
        // 비우기는 됐을 수 있으므로 사용자 클립보드를 되돌린다.
        guard.mark_own_write();
        guard.finish();
        tracing::warn!(error = %e, "sheets paste payload write failed");
        return Err("clipboard write failed".into());
    }
    guard.mark_own_write();
    // 4. Ctrl+V 직후 Ctrl+C: 순서대로 처리되므로 복사로 인한 변화가 붙여넣기 소비를 증명한다.
    wait_until_alt_released(keys, ALT_RELEASE_TIMEOUT);
    let before = cb.sequence_number();
    if let Err(e) = keys.send(Chord::CtrlV) {
        // 아무것도 Chrome에 닿지 않았으므로 사용자 클립보드를 되돌린다.
        guard.mark_own_write();
        guard.finish();
        tracing::warn!(error = %e, "sheets paste key failed; clipboard restored");
        return Err("paste key failed".into());
    }
    let confirm_sent = match keys.send(Chord::CtrlC) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "sheets confirm copy key failed; paste not confirmed");
            false
        }
    };
    // 5. 확인되면 복원, 아니면 그대로 둔다(D9). 확인용 Ctrl+C 실패도 미확인으로 본다.
    if confirm_sent && wait_for_clipboard_change(cb, before, timeouts.paste_confirm) {
        guard.mark_own_write();
        guard.finish();
        tracing::debug!("sheets paste confirmed");
    } else {
        tracing::info!(
            "Sheets paste not confirmed; leaving the clipboard untouched instead of restoring"
        );
    }
    Ok(())
}

/// 검증 복사가 남긴 복사 표시(점선 테두리)를 Esc로 지운다. 클립보드는 건드리지 않는다.
/// Alt가 눌린 채 Esc를 보내면 창 전환이 되므로 먼저 Alt 해제를 기다린다.
pub(crate) fn clear_marquee_with(keys: &dyn KeySender) {
    wait_until_alt_released(keys, ALT_RELEASE_TIMEOUT);
    if let Err(e) = keys.send(Chord::Esc) {
        tracing::debug!(error = %e, "sheets copy marquee clear key failed");
    }
}

/// 실제 UIA 조회. 호출마다 포그라운드 창을 다시 잡는다(UIA 객체는 스레드를 넘기지 않는다).
struct UiaProbe<'a> {
    keys: &'a dyn KeySender,
    empty_editor_is_selection: bool,
}

impl SheetsProbe for UiaProbe<'_> {
    fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>) {
        let Some(window) = ChromeWindow::foreground() else {
            return (None, None);
        };
        let url = window.url();
        // Sheets가 아니면 포커스 작업·키 없이 끝낸다(모든 비 Sheets 트리거가 싸야 한다).
        if !is_google_sheets_url(&url) {
            return (Some(url), None);
        }
        let Some((focused, role)) = resolve_focus(&window, self.keys, &url) else {
            return (Some(url), None);
        };
        let editor_value = (self.empty_editor_is_selection && role == Role::SheetsCellEditor)
            .then(|| focused.value().map(strip_one_trailing_newline))
            .flatten();
        let state = focus_input_state(
            role,
            editor_value.as_deref(),
            self.empty_editor_is_selection,
        );
        tracing::debug!(?role, ?state, "sheets focus state");
        (Some(url), Some(state))
    }

    /// S-ref §2.2: AutomationId `t-name-box` → 이름에 `이름 상자` → `Name Box`, 깊이 24.
    fn range_address(&self) -> Option<String> {
        let window = ChromeWindow::foreground()?;
        let by_id = |el: &IUIAutomationElement| {
            // SAFETY: 이 스레드에서 얻은 요소에 대한 조회 호출.
            unsafe { el.CurrentAutomationId() }.is_ok_and(|id| id == NAME_BOX_AUTOMATION_ID)
        };
        let by_name = |needle: &'static str| {
            move |el: &IUIAutomationElement| {
                // SAFETY: 이 스레드에서 얻은 요소에 대한 조회 호출.
                unsafe { el.CurrentName() }.is_ok_and(|n| n.to_string().contains(needle))
            }
        };
        let [ko, en] = NAME_BOX_NAMES.map(by_name);
        let matchers: [&dyn Fn(&IUIAutomationElement) -> bool; 3] = [&by_id, &ko, &en];
        // 지연 평가: 앞 후보에서 값을 찾으면 뒤 후보는 검색하지 않는다.
        let lookups = matchers
            .into_iter()
            .map(|m| window.edit_value_where(NAME_BOX_SEARCH_DEPTH, m));
        let address = first_address(lookups);
        tracing::debug!(found = address.is_some(), "sheets name box read");
        address
    }
}

/// Sheets 범위 경로의 Windows 구현. 교체용(Alt 계열)과 읽기용(Ctrl 계열) 두 인스턴스를 만든다.
pub struct WindowsSheetsBackend {
    keys: Arc<dyn KeySender>,
    clipboard: Arc<dyn FormatClipboard>,
    assume_editable: bool,
    empty_editor_is_selection: bool,
    max_range_cells: usize,
}

impl WindowsSheetsBackend {
    pub fn new(
        keys: Arc<dyn KeySender>,
        clipboard: Arc<dyn FormatClipboard>,
        assume_editable: bool,
        empty_editor_is_selection: bool,
        max_range_cells: usize,
    ) -> Self {
        WindowsSheetsBackend {
            keys,
            clipboard,
            assume_editable,
            empty_editor_is_selection,
            max_range_cells,
        }
    }

    fn probe(&self) -> UiaProbe<'_> {
        UiaProbe {
            keys: self.keys.as_ref(),
            empty_editor_is_selection: self.empty_editor_is_selection,
        }
    }
}

impl SheetsRouting for WindowsSheetsBackend {
    /// 잠금을 잡지 않는다.
    fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>) {
        self.probe().url_and_state()
    }
}

impl SheetsInteraction for WindowsSheetsBackend {
    fn capture_selection(&self) -> Result<SheetSelection, CaptureError> {
        let _section = lock_clipboard_section();
        capture_with(
            &self.probe(),
            self.keys.as_ref(),
            self.clipboard.as_ref(),
            self.max_range_cells,
            TIMEOUTS,
        )
    }

    fn can_edit(&self) -> bool {
        self.assume_editable
    }

    fn write_selection(&self, selection: &SheetSelection, cf_html: &str) -> Result<(), String> {
        let _section = lock_clipboard_section();
        write_with(
            &self.probe(),
            self.keys.as_ref(),
            self.clipboard.as_ref(),
            selection,
            cf_html,
            TIMEOUTS,
        )
    }

    /// 키도 클립보드도 쓰지 않으므로 잠금을 잡지 않는다.
    fn current_address(&self) -> Option<String> {
        self.probe().range_address()
    }

    /// 다른 작업의 복사·붙여넣기 키 사이에 끼지 않도록 이 파일의 다른 키처럼 구간 잠금 안에서 보낸다.
    fn clear_copy_marquee(&self) {
        let _section = lock_clipboard_section();
        clear_marquee_with(self.keys.as_ref());
    }
}

#[cfg(test)]
#[path = "sheets_backend_tests.rs"]
mod tests;
