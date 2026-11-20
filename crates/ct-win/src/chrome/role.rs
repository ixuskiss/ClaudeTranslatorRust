//! Chrome URL 판정과 포커스된 UIA 요소의 역할 분류 (순수 문자열 로직).

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
    Unknown,
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

/// 위에서 아래로 평가하며 처음 맞는 규칙이 이긴다.
pub fn classify_focused_role(url: &str, name: &str, control_type: &str) -> Role {
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
        assert_eq!(classify_focused_role(CHAT, "Message", ""), Role::Compose);
        assert_eq!(
            classify_focused_role(CHAT, "Message Jane Doe", ""),
            Role::Compose
        );
    }

    #[test]
    fn chat_history_and_empty_name() {
        assert_eq!(
            classify_focused_role(CHAT, "Historical messages", ""),
            Role::MessageHistory
        );
        assert_eq!(classify_focused_role(CHAT, "", ""), Role::MessageHistory);
    }

    #[test]
    fn chat_edit_control_is_compose() {
        assert_eq!(
            classify_focused_role(CHAT, "whatever", "EditControl"),
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
            classify_focused_role(GMAIL_CHAT, "안철정님에게 메시지 보내기", "EditControl"),
            Role::Compose
        );
    }

    #[test]
    fn gmail_embedded_chat_non_edit_is_history() {
        assert_eq!(
            classify_focused_role(GMAIL_CHAT, "some list item", "TextControl"),
            Role::MessageHistory
        );
    }

    #[test]
    fn gmail_body_name_beats_stale_chat_hash() {
        assert_eq!(
            classify_focused_role(GMAIL_COMPOSE_CHAT, "메일 본문", "EditControl"),
            Role::Body
        );
    }

    #[test]
    fn chat_compose_with_compose_query_stays_chat() {
        let url = "mail.google.com/mail/u/0/#chat/space/AAAA-0AvCYI?compose=SxfkdqLRqjwsQxQsVKXxJMwqGlWnjCZR";
        let name = "소유자 및 관리자만 게시할 수 있습니다. 채팅 기록이 사용 설정되었습니다.";
        assert_eq!(
            classify_focused_role(url, name, "EditControl"),
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
        assert_eq!(classify_focused_role(GMAIL, "Subject", ""), Role::Subject);
        assert_eq!(classify_focused_role(GMAIL, "제목", ""), Role::Subject);
        assert_eq!(classify_focused_role(GMAIL, "Message Body", ""), Role::Body);
        assert_eq!(classify_focused_role(GMAIL, "메일 본문", ""), Role::Body);
        assert_eq!(classify_focused_role(GMAIL, "Body", ""), Role::Body);
    }

    #[test]
    fn gmail_address_fields_distinct() {
        assert_eq!(classify_focused_role(GMAIL, "To recipients", ""), Role::To);
        assert_eq!(classify_focused_role(GMAIL, "Cc recipients", ""), Role::Cc);
        assert_eq!(
            classify_focused_role(GMAIL, "Bcc recipients", ""),
            Role::Bcc
        );
        assert_eq!(classify_focused_role(GMAIL, "받는사람", ""), Role::To);
        assert_eq!(classify_focused_role(GMAIL, "참조", ""), Role::Cc);
        assert_eq!(classify_focused_role(GMAIL, "숨은참조", ""), Role::Bcc);
    }

    #[test]
    fn gmail_unrecognized_is_unknown() {
        assert_eq!(
            classify_focused_role(GMAIL, "Search mail", ""),
            Role::Unknown
        );
    }

    #[test]
    fn docs_is_canvas_except_button() {
        assert_eq!(classify_focused_role(DOCS, "anything", ""), Role::Canvas);
        assert_eq!(classify_focused_role(DOCS, "", ""), Role::Canvas);
        assert_eq!(
            classify_focused_role(DOCS, "Update available", "ButtonControl"),
            Role::Unknown
        );
    }

    #[test]
    fn slides_roles() {
        assert_eq!(
            classify_focused_role(SLIDES, "Update available", "ButtonControl"),
            Role::Unknown
        );
        assert_eq!(classify_focused_role(SLIDES, "Text box", ""), Role::Textbox);
        assert_eq!(classify_focused_role(SLIDES, "Shape", ""), Role::Textbox);
        assert_eq!(classify_focused_role(SLIDES, "", ""), Role::SlideCanvas);
        assert_eq!(
            classify_focused_role(SLIDES, "Slide 3 of 10", ""),
            Role::SlideCanvas
        );
    }

    #[test]
    fn unrelated_url_is_unknown() {
        assert_eq!(
            classify_focused_role("https://example.com", "anything", ""),
            Role::Unknown
        );
    }

    #[test]
    fn english_names_are_case_insensitive() {
        assert_eq!(classify_focused_role(CHAT, "MESSAGE", ""), Role::Compose);
        assert_eq!(classify_focused_role(GMAIL, "SUBJECT", ""), Role::Subject);
    }
}
