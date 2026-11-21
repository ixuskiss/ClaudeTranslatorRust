//! Chrome URL 판정과 포커스된 UIA 요소의 역할 분류 (순수 문자열 로직).

use ct_sheets::routing::{SheetsRole, is_google_sheets_url};

/// 포커스된 요소의 역할.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Compose,
    MessageHistory,
    Subject,
    Body,
    Cc,
    Bcc,
    To,
    Canvas,
    Textbox,
    SlideCanvas,
    SheetsGrid,
    SheetsCellEditor,
    SheetsFormulaBar,
    SheetsNameBox,
    Unknown,
}

/// Sheets 라우팅(`ct_sheets::routing`)이 쓰는 역할로 바꾼다.
pub fn sheets_role(role: Role) -> SheetsRole {
    match role {
        Role::SheetsGrid => SheetsRole::Grid,
        Role::SheetsCellEditor => SheetsRole::CellEditor,
        Role::SheetsFormulaBar => SheetsRole::FormulaBar,
        Role::SheetsNameBox => SheetsRole::NameBox,
        _ => SheetsRole::Other,
    }
}

/// 선택과 편집 모두 이 콤보박스에 포커스가 간다(2026-10-13 관찰).
const SHEETS_EDITOR_AUTOMATION_ID: &str = "waffle-rich-text-editor";
const SHEETS_NAME_BOX_AUTOMATION_ID: &str = "t-name-box";
/// 셀 편집기가 아닌 Sheets EditControl(검색창, 이름 바꾸기 등)의 이름 단서.
const SHEETS_NON_CELL_EDIT_HINTS: [&str; 15] = [
    "address",
    "search",
    "find",
    "omnibox",
    "rename",
    "title",
    "comment",
    "filter",
    "주소",
    "검색",
    "찾기",
    "제목",
    "댓글",
    "필터",
    "이름 바꾸기",
];
const SHEETS_GRID_CONTROL_TYPES: [&str; 5] = [
    "",
    "PaneControl",
    "DocumentControl",
    "GroupControl",
    "CustomControl",
];

/// `[A-Z]+[1-9][0-9]*` 전체 일치 (A1 형식 셀 주소).
fn is_a1_cell_address(name: &str) -> bool {
    let letters = name.bytes().take_while(u8::is_ascii_uppercase).count();
    let digits = &name.as_bytes()[letters..];
    letters > 0 && !digits.is_empty() && digits[0] != b'0' && digits.iter().all(u8::is_ascii_digit)
}

/// S-ref §1.3. Gmail/Chat/Docs/Slides 분기가 모두 빗나간 뒤에만 온다.
fn classify_sheets_role(name: &str, lname: &str, control_type: &str, automation_id: &str) -> Role {
    match control_type {
        "ComboBoxControl" => {
            if automation_id != SHEETS_EDITOR_AUTOMATION_ID {
                Role::Unknown
            } else if name.is_empty() {
                Role::SheetsGrid
            } else if is_a1_cell_address(name) {
                Role::SheetsCellEditor
            } else {
                Role::Unknown
            }
        }
        "ButtonControl" => Role::Unknown,
        "EditControl" => {
            if automation_id == SHEETS_NAME_BOX_AUTOMATION_ID
                || lname.contains("name box")
                || name.contains("이름 상자")
            {
                Role::SheetsNameBox
            } else if lname.contains("formula") || name.contains("수식") {
                Role::SheetsFormulaBar
            } else if SHEETS_NON_CELL_EDIT_HINTS
                .iter()
                .any(|h| lname.contains(h) || name.contains(h))
            {
                Role::Unknown
            } else {
                Role::SheetsCellEditor
            }
        }
        ct if SHEETS_GRID_CONTROL_TYPES.contains(&ct) => Role::SheetsGrid,
        _ => Role::Unknown,
    }
}

/// Google Chat 단독 또는 Gmail 안에 포함된 Chat(`#chat`).
pub fn is_google_chat_url(url: &str) -> bool {
    url.contains("chat.google.com") || (url.contains("mail.google.com") && url.contains("#chat"))
}

pub fn is_gmail_url(url: &str) -> bool {
    url.contains("mail.google.com")
}

pub fn is_docs_url(url: &str) -> bool {
    url.contains("docs.google.com/document")
}

pub fn is_slides_url(url: &str) -> bool {
    url.contains("docs.google.com/presentation")
}

pub fn is_supported_url(url: &str) -> bool {
    is_google_chat_url(url) || url.contains("mail.google.com") || url.contains("docs.google.com")
}

/// 위에서 아래로 평가하며 처음 맞는 규칙이 이긴다. `automation_id`는 Sheets 분기에서만 본다
/// (UIA에서도 Sheets URL일 때만 읽는다).
pub fn classify_focused_role(
    url: &str,
    name: &str,
    control_type: &str,
    automation_id: &str,
) -> Role {
    let lname = name.to_lowercase();

    if is_gmail_url(url) {
        // Gmail 필드 이름은 URL에 남은 #chat 해시보다 우선한다.
        if lname.contains("subject") || name.contains("제목") {
            return Role::Subject;
        }
        if lname.contains("message body") || lname == "body" || name.contains("본문") {
            return Role::Body;
        }
        if (lname.contains("bcc") && lname.contains("recipient")) || name.contains("숨은참조") {
            return Role::Bcc;
        }
        if (lname.contains("cc") && lname.contains("recipient"))
            || (name.contains("참조") && !name.contains("숨은참조"))
        {
            return Role::Cc;
        }
        if (lname.contains("to") && lname.contains("recipient")) || name.contains("받는사람") {
            return Role::To;
        }
    }

    if is_google_chat_url(url) {
        if control_type == "EditControl" || (lname.contains("message") && !lname.contains("histor"))
        {
            return Role::Compose;
        }
        return Role::MessageHistory;
    }

    if is_gmail_url(url) {
        return Role::Unknown;
    }

    if is_docs_url(url) {
        return if control_type == "ButtonControl" {
            Role::Unknown
        } else {
            Role::Canvas
        };
    }

    if is_slides_url(url) {
        if control_type == "ButtonControl" {
            return Role::Unknown;
        }
        if lname.contains("text box") || lname.contains("shape") {
            return Role::Textbox;
        }
        return Role::SlideCanvas;
    }

    if is_google_sheets_url(url) {
        return classify_sheets_role(name, &lname, control_type, automation_id);
    }

    Role::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: &str = "https://chat.google.com/room/abc";
    const GMAIL_CHAT: &str = "mail.google.com/mail/u/0/#chat/dm/7gvvgcAAAAE";
    const GMAIL_COMPOSE_CHAT: &str =
        "mail.google.com/mail/u/0/#chat/dm/7gvvgcAAAAE?compose=SxfkdnRhMHJcVzfGmDxQhRNwVxhV";
    const GMAIL: &str = "https://mail.google.com/mail/u/0";
    const DOCS: &str = "https://docs.google.com/document/d/abc/edit";
    const SLIDES: &str = "https://docs.google.com/presentation/d/abc/edit";

    #[test]
    fn chat_message_name_is_compose() {
        assert_eq!(
            classify_focused_role(CHAT, "Message", "", ""),
            Role::Compose
        );
        assert_eq!(
            classify_focused_role(CHAT, "Message Jane Doe", "", ""),
            Role::Compose
        );
    }

    #[test]
    fn chat_history_and_empty_name() {
        assert_eq!(
            classify_focused_role(CHAT, "Historical messages", "", ""),
            Role::MessageHistory
        );
        assert_eq!(
            classify_focused_role(CHAT, "", "", ""),
            Role::MessageHistory
        );
    }

    #[test]
    fn chat_edit_control_is_compose() {
        assert_eq!(
            classify_focused_role(CHAT, "whatever", "EditControl", ""),
            Role::Compose
        );
    }

    #[test]
    fn chat_url_detection() {
        assert!(is_google_chat_url(CHAT));
        assert!(is_google_chat_url(GMAIL_CHAT));
        assert!(!is_google_chat_url(GMAIL));
        assert!(is_google_chat_url(GMAIL_COMPOSE_CHAT));
    }

    #[test]
    fn gmail_embedded_chat_edit_control_is_compose() {
        assert_eq!(
            classify_focused_role(GMAIL_CHAT, "안철정님에게 메시지 보내기", "EditControl", ""),
            Role::Compose
        );
    }

    #[test]
    fn gmail_embedded_chat_non_edit_is_history() {
        assert_eq!(
            classify_focused_role(GMAIL_CHAT, "some list item", "TextControl", ""),
            Role::MessageHistory
        );
    }

    #[test]
    fn gmail_body_name_beats_stale_chat_hash() {
        assert_eq!(
            classify_focused_role(GMAIL_COMPOSE_CHAT, "메일 본문", "EditControl", ""),
            Role::Body
        );
    }

    #[test]
    fn chat_compose_with_compose_query_stays_chat() {
        let url = "mail.google.com/mail/u/0/#chat/space/AAAA-0AvCYI?compose=SxfkdqLRqjwsQxQsVKXxJMwqGlWnjCZR";
        let name = "소유자 및 관리자만 게시할 수 있습니다. 채팅 기록이 사용 설정되었습니다.";
        assert_eq!(
            classify_focused_role(url, name, "EditControl", ""),
            Role::Compose
        );
    }

    #[test]
    fn docs_and_slides_url_detection() {
        assert!(is_docs_url(DOCS));
        assert!(is_slides_url(SLIDES));
        assert!(!is_docs_url(SLIDES));
        assert!(!is_slides_url(DOCS));
        assert!(!is_docs_url(GMAIL) && !is_slides_url(GMAIL));
        assert!(!is_docs_url(CHAT) && !is_slides_url(CHAT));
        assert!(!is_docs_url("https://example.com"));
    }

    #[test]
    fn gmail_and_supported_url_detection() {
        assert!(is_gmail_url(GMAIL));
        assert!(!is_gmail_url(CHAT));
        assert!(is_supported_url(CHAT));
        assert!(is_supported_url(GMAIL));
        assert!(is_supported_url(DOCS));
        assert!(is_supported_url("https://docs.google.com/spreadsheets/d/x"));
        assert!(!is_supported_url("https://example.com"));
    }

    #[test]
    fn gmail_subject_and_body() {
        assert_eq!(
            classify_focused_role(GMAIL, "Subject", "", ""),
            Role::Subject
        );
        assert_eq!(classify_focused_role(GMAIL, "제목", "", ""), Role::Subject);
        assert_eq!(
            classify_focused_role(GMAIL, "Message Body", "", ""),
            Role::Body
        );
        assert_eq!(
            classify_focused_role(GMAIL, "메일 본문", "", ""),
            Role::Body
        );
        assert_eq!(classify_focused_role(GMAIL, "Body", "", ""), Role::Body);
    }

    #[test]
    fn gmail_address_fields_distinct() {
        assert_eq!(
            classify_focused_role(GMAIL, "To recipients", "", ""),
            Role::To
        );
        assert_eq!(
            classify_focused_role(GMAIL, "Cc recipients", "", ""),
            Role::Cc
        );
        assert_eq!(
            classify_focused_role(GMAIL, "Bcc recipients", "", ""),
            Role::Bcc
        );
        assert_eq!(classify_focused_role(GMAIL, "받는사람", "", ""), Role::To);
        assert_eq!(classify_focused_role(GMAIL, "참조", "", ""), Role::Cc);
        assert_eq!(classify_focused_role(GMAIL, "숨은참조", "", ""), Role::Bcc);
    }

    #[test]
    fn gmail_unrecognized_is_unknown() {
        assert_eq!(
            classify_focused_role(GMAIL, "Search mail", "", ""),
            Role::Unknown
        );
    }

    #[test]
    fn docs_is_canvas_except_button() {
        assert_eq!(
            classify_focused_role(DOCS, "anything", "", ""),
            Role::Canvas
        );
        assert_eq!(classify_focused_role(DOCS, "", "", ""), Role::Canvas);
        assert_eq!(
            classify_focused_role(DOCS, "Update available", "ButtonControl", ""),
            Role::Unknown
        );
    }

    #[test]
    fn slides_roles() {
        assert_eq!(
            classify_focused_role(SLIDES, "Update available", "ButtonControl", ""),
            Role::Unknown
        );
        assert_eq!(
            classify_focused_role(SLIDES, "Text box", "", ""),
            Role::Textbox
        );
        assert_eq!(
            classify_focused_role(SLIDES, "Shape", "", ""),
            Role::Textbox
        );
        assert_eq!(classify_focused_role(SLIDES, "", "", ""), Role::SlideCanvas);
        assert_eq!(
            classify_focused_role(SLIDES, "Slide 3 of 10", "", ""),
            Role::SlideCanvas
        );
    }

    #[test]
    fn unrelated_url_is_unknown() {
        assert_eq!(
            classify_focused_role("https://example.com", "anything", "", ""),
            Role::Unknown
        );
    }

    #[test]
    fn english_names_are_case_insensitive() {
        assert_eq!(
            classify_focused_role(CHAT, "MESSAGE", "", ""),
            Role::Compose
        );
        assert_eq!(
            classify_focused_role(GMAIL, "SUBJECT", "", ""),
            Role::Subject
        );
    }

    const SHEETS: &str = "https://docs.google.com/spreadsheets/d/abc/edit#gid=0";
    const WAFFLE: &str = "waffle-rich-text-editor";

    #[test]
    fn classifier_sheets_roles() {
        let cases = [
            ("", "ButtonControl", Role::Unknown),
            ("", "EditControl", Role::SheetsCellEditor),
            ("Formula input", "EditControl", Role::SheetsFormulaBar),
            ("수식 입력줄", "EditControl", Role::SheetsFormulaBar),
            ("Name Box", "EditControl", Role::SheetsNameBox),
            ("이름 상자", "EditControl", Role::SheetsNameBox),
            ("Spreadsheet", "PaneControl", Role::SheetsGrid),
            ("", "", Role::SheetsGrid),
        ];
        for (name, ct, role) in cases {
            assert_eq!(
                classify_focused_role(SHEETS, name, ct, ""),
                role,
                "{name:?} {ct:?}"
            );
        }
    }

    #[test]
    fn classifier_leaves_docs_and_slides_unchanged() {
        assert_eq!(classify_focused_role(DOCS, "", "", ""), Role::Canvas);
        assert_eq!(classify_focused_role(SLIDES, "", "", ""), Role::SlideCanvas);
        // Docs는 automation id를 보지 않는다.
        assert_eq!(classify_focused_role(DOCS, "", "", WAFFLE), Role::Canvas);
    }

    #[test]
    fn classifier_non_grid_non_editor_controls_are_unknown() {
        let cases = [
            ("주소 및 검색창", "EditControl"),
            ("Address and search bar", "EditControl"),
            ("Search the menus", "EditControl"),
            ("찾기", "EditControl"),
            ("Rename", "EditControl"),
            ("Comment", "EditControl"),
            ("", "MenuItemControl"),
            ("", "WindowControl"),
            ("", "TabItemControl"),
            ("Share", "HyperlinkControl"),
        ];
        for (name, ct) in cases {
            assert_eq!(
                classify_focused_role(SHEETS, name, ct, ""),
                Role::Unknown,
                "{name:?} {ct:?}"
            );
        }
    }

    #[test]
    fn classifier_grid_only_for_content_like_controls() {
        for ct in [
            "",
            "PaneControl",
            "DocumentControl",
            "GroupControl",
            "CustomControl",
        ] {
            assert_eq!(
                classify_focused_role(SHEETS, "Spreadsheet", ct, ""),
                Role::SheetsGrid,
                "{ct:?}"
            );
        }
    }

    #[test]
    fn observed_sheets_combobox_selection_and_editing() {
        let cases = [
            ("", Role::SheetsGrid),
            ("A1", Role::SheetsCellEditor),
            ("AA123", Role::SheetsCellEditor),
            ("Search", Role::Unknown),
        ];
        for (name, role) in cases {
            assert_eq!(
                classify_focused_role(SHEETS, name, "ComboBoxControl", WAFFLE),
                role,
                "{name:?}"
            );
        }
    }

    #[test]
    fn sheets_combobox_cases() {
        assert_eq!(
            classify_focused_role(SHEETS, "", "ComboBoxControl", WAFFLE),
            Role::SheetsGrid
        );
        assert_eq!(
            classify_focused_role(SHEETS, "B12", "ComboBoxControl", WAFFLE),
            Role::SheetsCellEditor
        );
        for id in ["font-size", "", "t-name-box"] {
            assert_eq!(
                classify_focused_role(SHEETS, "B12", "ComboBoxControl", id),
                Role::Unknown
            );
            assert_eq!(
                classify_focused_role(SHEETS, "", "ComboBoxControl", id),
                Role::Unknown
            );
        }
        // 0으로 시작하는 행, 소문자, 행·열 누락, 뒤따르는 글자는 셀 주소가 아니다.
        for name in ["A0", "A01", "a1", "AB", "1", "A1B", "A1 ", " A1"] {
            assert_eq!(
                classify_focused_role(SHEETS, name, "ComboBoxControl", WAFFLE),
                Role::Unknown,
                "{name:?}"
            );
        }
    }

    #[test]
    fn unrelated_combobox_is_not_a_sheet_cell() {
        assert_eq!(
            classify_focused_role(SHEETS, "A1", "ComboBoxControl", "font-size"),
            Role::Unknown
        );
    }

    #[test]
    fn unnamed_name_box_is_not_a_cell_editor() {
        assert_eq!(
            classify_focused_role(SHEETS, "", "EditControl", "t-name-box"),
            Role::SheetsNameBox
        );
    }

    #[test]
    fn sheets_role_mapping() {
        assert_eq!(sheets_role(Role::SheetsGrid), SheetsRole::Grid);
        assert_eq!(sheets_role(Role::SheetsCellEditor), SheetsRole::CellEditor);
        assert_eq!(sheets_role(Role::SheetsFormulaBar), SheetsRole::FormulaBar);
        assert_eq!(sheets_role(Role::SheetsNameBox), SheetsRole::NameBox);
        for role in [Role::Unknown, Role::Compose, Role::Canvas, Role::Body] {
            assert_eq!(sheets_role(role), SheetsRole::Other);
        }
    }
}
