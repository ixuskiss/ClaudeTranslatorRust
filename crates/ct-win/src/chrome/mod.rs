//! Chrome 전용 판정 로직 (URL 판정, 포커스 역할 분류, 앱별 전략).

pub mod adapter;
pub mod role;
pub mod sheets_backend;
pub mod sheets_edit;
pub mod strategy;
pub mod uia_backend;

pub use adapter::{
    CaptureMiss, CapturedText, ChromeAdapter, ReplaceError, RetryingInteraction, TextInteraction,
    UiaBackend,
};
pub use role::{
    Role, classify_focused_role, is_docs_url, is_gmail_url, is_google_chat_url, is_slides_url,
    is_supported_url, sheets_role,
};
pub use sheets_backend::WindowsSheetsBackend;
pub use sheets_edit::SheetsEditSafe;
pub use strategy::{
    EditingContext, FocusedElement, READ_ONLY_STRATEGIES, REPLACE_STRATEGIES, Strategy,
    is_sheets_editor_role,
};
pub use uia_backend::{WindowsUiaBackend, cursor_position};
