//! Google 앱별 캡처 전략 (Chat/Gmail/Docs/Slides).

use super::role::{Role, is_docs_url, is_google_chat_url, is_slides_url};

/// 포커스된 요소의 스냅샷.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FocusedElement {
    pub url: String,
    pub role: Role,
    pub name: String,
    pub full_text: String,
    pub selection_text: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EditingContext {
    pub app: &'static str,
    pub field_id: &'static str,
    pub has_selection: bool,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strategy {
    GoogleChat,
    Gmail,
    GoogleChatReadOnly,
    GmailReadOnly,
    GoogleDocs,
    GoogleSlides,
}

pub const REPLACE_STRATEGIES: [Strategy; 4] = [
    Strategy::GoogleChat,
    Strategy::Gmail,
    Strategy::GoogleDocs,
    Strategy::GoogleSlides,
];

pub const READ_ONLY_STRATEGIES: [Strategy; 4] = [
    Strategy::GoogleChatReadOnly,
    Strategy::GmailReadOnly,
    Strategy::GoogleDocs,
    Strategy::GoogleSlides,
];

impl Strategy {
    pub fn matches(&self, f: &FocusedElement) -> bool {
        match self {
            // Gmail 필드 역할이면 Chat 전략은 양보한다 (#chat 해시가 남은 Gmail 작성창).
            Strategy::GoogleChat | Strategy::GoogleChatReadOnly => {
                is_google_chat_url(&f.url) && !is_gmail_field_role(f.role)
            }
            Strategy::Gmail | Strategy::GmailReadOnly => f.url.contains("mail.google.com"),
            Strategy::GoogleDocs => is_docs_url(&f.url),
            Strategy::GoogleSlides => is_slides_url(&f.url),
        }
    }

    pub fn identify_context(&self, f: &FocusedElement) -> Option<EditingContext> {
        match self {
            // 현재 작성창만 유효 대상이다. 대화 기록은 절대 건드리지 않는다.
            Strategy::GoogleChat => {
                if f.role != Role::Compose {
                    return None;
                }
                let (has_selection, text) = selection_or_full(f);
                Some(EditingContext {
                    app: "google_chat",
                    field_id: "compose",
                    has_selection,
                    text,
                })
            }
            // 제목/본문만 유효. To/Cc/Bcc 등은 None.
            Strategy::Gmail => {
                let field_id = match f.role {
                    Role::Subject => "subject",
                    Role::Body => "body",
                    _ => return None,
                };
                let (has_selection, text) = selection_or_full(f);
                Some(EditingContext {
                    app: "gmail",
                    field_id,
                    has_selection,
                    text,
                })
            }
            Strategy::GoogleChatReadOnly => selection_only(f, "google_chat", "selection"),
            Strategy::GmailReadOnly => selection_only(f, "gmail", "selection"),
            Strategy::GoogleDocs => selection_only(f, "google_docs", "selection"),
            // field_id는 선택 여부와 무관하게 "textbox" (교체 시 컨텍스트 재검증용).
            Strategy::GoogleSlides => {
                if let Some(sel) = non_empty_selection(f) {
                    return Some(EditingContext {
                        app: "google_slides",
                        field_id: "textbox",
                        has_selection: true,
                        text: sel.to_string(),
                    });
                }
                if matches!(f.role, Role::Textbox | Role::SlideCanvas) && !f.full_text.is_empty() {
                    return Some(EditingContext {
                        app: "google_slides",
                        field_id: "textbox",
                        has_selection: false,
                        text: f.full_text.clone(),
                    });
                }
                None
            }
        }
    }
}

fn is_gmail_field_role(role: Role) -> bool {
    matches!(
        role,
        Role::Subject | Role::Body | Role::Cc | Role::Bcc | Role::To
    )
}

/// 비어 있지 않은 선택(공백만 있어도 선택으로 본다).
fn non_empty_selection(f: &FocusedElement) -> Option<&str> {
    f.selection_text.as_deref().filter(|s| !s.is_empty())
}

fn selection_or_full(f: &FocusedElement) -> (bool, String) {
    match non_empty_selection(f) {
        Some(sel) => (true, sel.to_string()),
        None => (false, f.full_text.clone()),
    }
}

fn selection_only(
    f: &FocusedElement,
    app: &'static str,
    field_id: &'static str,
) -> Option<EditingContext> {
    non_empty_selection(f).map(|sel| EditingContext {
        app,
        field_id,
        has_selection: true,
        text: sel.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fe(url: &str, role: Role, full: &str, sel: Option<&str>) -> FocusedElement {
        FocusedElement {
            url: url.to_string(),
            role,
            name: String::new(),
            full_text: full.to_string(),
            selection_text: sel.map(str::to_string),
        }
    }

    const CHAT: &str = "https://chat.google.com/room/abc";
    const GMAIL: &str = "https://mail.google.com/mail/u/0";
    const CHAT_HASH: &str = "mail.google.com/mail/u/0/#chat/dm/7gvvgcAAAAE";
    const DOCS: &str = "https://docs.google.com/document/d/abc/edit";
    const SLIDES: &str = "https://docs.google.com/presentation/d/abc/edit";

    fn ctx(app: &'static str, field_id: &'static str, sel: bool, text: &str) -> EditingContext {
        EditingContext {
            app,
            field_id,
            has_selection: sel,
            text: text.to_string(),
        }
    }

    #[test]
    fn strategy_lists_order() {
        assert_eq!(REPLACE_STRATEGIES[0], Strategy::GoogleChat);
        assert_eq!(REPLACE_STRATEGIES[1], Strategy::Gmail);
        assert_eq!(READ_ONLY_STRATEGIES[0], Strategy::GoogleChatReadOnly);
        assert_eq!(READ_ONLY_STRATEGIES[1], Strategy::GmailReadOnly);
    }

    #[test]
    fn chat_matches_only_chat_url() {
        let s = Strategy::GoogleChat;
        assert!(s.matches(&fe(CHAT, Role::Unknown, "", None)));
        assert!(!s.matches(&fe(GMAIL, Role::Unknown, "", None)));
    }

    #[test]
    fn chat_matches_chat_embedded_in_gmail() {
        assert!(Strategy::GoogleChat.matches(&fe(CHAT_HASH, Role::Unknown, "", None)));
    }

    #[test]
    fn chat_does_not_match_gmail_field_roles_over_chat_hash() {
        let url = "mail.google.com/mail/u/0/#chat/dm/7gvvgcAAAAE?compose=abc123";
        for role in [Role::Subject, Role::Body, Role::Cc, Role::Bcc, Role::To] {
            assert!(!Strategy::GoogleChat.matches(&fe(url, role, "", None)));
            assert!(!Strategy::GoogleChatReadOnly.matches(&fe(url, role, "", None)));
        }
    }

    #[test]
    fn gmail_matches_real_body_reached_via_chat_hash_url() {
        let url = "mail.google.com/mail/u/0/#chat/dm/7gvvgcAAAAE?compose=abc123";
        let e = fe(url, Role::Body, "원문 내용", None);
        assert!(Strategy::Gmail.matches(&e));
        assert_eq!(
            Strategy::Gmail.identify_context(&e),
            Some(ctx("gmail", "body", false, "원문 내용"))
        );
    }

    #[test]
    fn chat_captures_selection_when_present() {
        let e = fe(
            CHAT,
            Role::Compose,
            "hello there full compose text",
            Some("hello"),
        );
        assert_eq!(
            Strategy::GoogleChat.identify_context(&e),
            Some(ctx("google_chat", "compose", true, "hello"))
        );
    }

    #[test]
    fn chat_captures_full_compose_when_no_selection() {
        let e = fe(CHAT, Role::Compose, "hello there full compose text", None);
        assert_eq!(
            Strategy::GoogleChat.identify_context(&e),
            Some(ctx(
                "google_chat",
                "compose",
                false,
                "hello there full compose text"
            ))
        );
    }

    #[test]
    fn chat_refuses_non_compose_focus() {
        let e = fe(
            CHAT,
            Role::MessageHistory,
            "an old message someone else sent",
            None,
        );
        assert_eq!(Strategy::GoogleChat.identify_context(&e), None);
    }

    #[test]
    fn whitespace_only_selection_counts_as_selection() {
        let e = fe(CHAT, Role::Compose, "full", Some("  "));
        assert_eq!(
            Strategy::GoogleChat.identify_context(&e),
            Some(ctx("google_chat", "compose", true, "  "))
        );
    }

    #[test]
    fn empty_string_selection_is_no_selection() {
        let e = fe(CHAT, Role::Compose, "full", Some(""));
        assert_eq!(
            Strategy::GoogleChat.identify_context(&e),
            Some(ctx("google_chat", "compose", false, "full"))
        );
    }

    #[test]
    fn gmail_matches_only_gmail_url() {
        let s = Strategy::Gmail;
        assert!(s.matches(&fe(GMAIL, Role::Unknown, "", None)));
        assert!(!s.matches(&fe(CHAT, Role::Unknown, "", None)));
    }

    #[test]
    fn gmail_captures_subject_field() {
        let e = fe(GMAIL, Role::Subject, "Re: budget", None);
        assert_eq!(
            Strategy::Gmail.identify_context(&e),
            Some(ctx("gmail", "subject", false, "Re: budget"))
        );
    }

    #[test]
    fn gmail_captures_body_field_independently_of_subject() {
        let e = fe(GMAIL, Role::Body, "please review", None);
        let c = Strategy::Gmail.identify_context(&e);
        assert_eq!(c.map(|c| c.field_id), Some("body"));
    }

    #[test]
    fn gmail_refuses_address_fields_and_others() {
        for role in [Role::To, Role::Cc, Role::Bcc, Role::Unknown, Role::Compose] {
            let e = fe(GMAIL, role, "someone@example.com", None);
            assert_eq!(Strategy::Gmail.identify_context(&e), None);
        }
    }

    #[test]
    fn chat_read_only_captures_selection_regardless_of_role() {
        let e = fe(
            CHAT,
            Role::MessageHistory,
            "",
            Some("an old message someone else sent"),
        );
        assert_eq!(
            Strategy::GoogleChatReadOnly.identify_context(&e),
            Some(ctx(
                "google_chat",
                "selection",
                true,
                "an old message someone else sent"
            ))
        );
    }

    #[test]
    fn chat_read_only_refuses_without_selection() {
        let e = fe(CHAT, Role::MessageHistory, "x", None);
        assert_eq!(Strategy::GoogleChatReadOnly.identify_context(&e), None);
    }

    #[test]
    fn gmail_read_only_captures_selection_regardless_of_role() {
        let e = fe(
            "https://mail.google.com/mail/u/0/#inbox/abc",
            Role::Unknown,
            "",
            Some("text selected while reading a received email"),
        );
        assert_eq!(
            Strategy::GmailReadOnly.identify_context(&e),
            Some(ctx(
                "gmail",
                "selection",
                true,
                "text selected while reading a received email"
            ))
        );
    }

    #[test]
    fn gmail_read_only_refuses_without_selection() {
        let e = fe(
            "https://mail.google.com/mail/u/0/#inbox/abc",
            Role::Unknown,
            "x",
            None,
        );
        assert_eq!(Strategy::GmailReadOnly.identify_context(&e), None);
    }

    #[test]
    fn read_only_matches_follow_replace_strategies() {
        assert!(Strategy::GmailReadOnly.matches(&fe(GMAIL, Role::Unknown, "", None)));
        assert!(!Strategy::GmailReadOnly.matches(&fe(CHAT, Role::Unknown, "", None)));
        assert!(Strategy::GoogleChatReadOnly.matches(&fe(CHAT, Role::Unknown, "", None)));
    }

    #[test]
    fn docs_matches_only_document_url() {
        let s = Strategy::GoogleDocs;
        assert!(s.matches(&fe(DOCS, Role::Unknown, "", None)));
        assert!(!s.matches(&fe(SLIDES, Role::Unknown, "", None)));
    }

    #[test]
    fn docs_captures_selection_only() {
        let e = fe(
            DOCS,
            Role::Canvas,
            "entire document text",
            Some("selected phrase"),
        );
        assert_eq!(
            Strategy::GoogleDocs.identify_context(&e),
            Some(ctx("google_docs", "selection", true, "selected phrase"))
        );
    }

    #[test]
    fn docs_refuses_when_no_selection() {
        let e = fe(DOCS, Role::Canvas, "entire document text", None);
        assert_eq!(Strategy::GoogleDocs.identify_context(&e), None);
    }

    #[test]
    fn slides_matches_only_presentation_url() {
        let s = Strategy::GoogleSlides;
        assert!(s.matches(&fe(SLIDES, Role::Unknown, "", None)));
        assert!(!s.matches(&fe(DOCS, Role::Unknown, "", None)));
    }

    #[test]
    fn slides_selection_uses_textbox_field_id() {
        let e = fe(
            SLIDES,
            Role::Textbox,
            "title text and more",
            Some("title text"),
        );
        assert_eq!(
            Strategy::GoogleSlides.identify_context(&e),
            Some(ctx("google_slides", "textbox", true, "title text"))
        );
    }

    #[test]
    fn slides_refuses_when_not_editing_a_textbox() {
        let e = fe(SLIDES, Role::SlideCanvas, "", None);
        assert_eq!(Strategy::GoogleSlides.identify_context(&e), None);
    }

    #[test]
    fn slides_captures_selection_with_slide_canvas_role() {
        let e = fe(
            SLIDES,
            Role::SlideCanvas,
            "title text and more",
            Some("title text"),
        );
        let c = Strategy::GoogleSlides.identify_context(&e);
        assert_eq!(c.map(|c| c.text), Some("title text".to_string()));
    }

    #[test]
    fn slides_whole_textbox_when_no_selection_for_textbox_and_canvas() {
        for role in [Role::Textbox, Role::SlideCanvas] {
            let e = fe(SLIDES, role, "title text", None);
            assert_eq!(
                Strategy::GoogleSlides.identify_context(&e),
                Some(ctx("google_slides", "textbox", false, "title text"))
            );
        }
    }

    #[test]
    fn slides_refuses_textbox_without_selection_or_full_text() {
        let e = fe(SLIDES, Role::Textbox, "", None);
        assert_eq!(Strategy::GoogleSlides.identify_context(&e), None);
    }

    #[test]
    fn slides_other_roles_without_selection_refuse() {
        let e = fe(SLIDES, Role::Unknown, "text", None);
        assert_eq!(Strategy::GoogleSlides.identify_context(&e), None);
    }
}
