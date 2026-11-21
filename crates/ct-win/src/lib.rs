#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod chrome;
pub mod clipboard;
pub mod format_clipboard;
pub mod hotkey;
pub mod keys;
pub mod registry;
pub mod single_instance;
