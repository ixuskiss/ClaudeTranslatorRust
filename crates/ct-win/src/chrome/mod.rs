//! Chrome 전용 판정 로직 (URL 판정, 포커스 역할 분류).

pub mod role;

pub use role::{
    Role, classify_focused_role, is_docs_url, is_gmail_url, is_google_chat_url, is_slides_url,
    is_supported_url,
};
