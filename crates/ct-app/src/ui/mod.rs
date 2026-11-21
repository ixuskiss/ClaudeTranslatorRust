//! Slint windows (badge, explanation window, settings) and their Win32 helpers.
//!
//! `build.rs` compiles only `ui/app.slint`, which re-exports the components of the other
//! `ui/*.slint` files, so a single `include_modules!()` brings them all in.

#[allow(clippy::all, clippy::unwrap_used, clippy::expect_used)]
mod generated {
    slint::include_modules!();
}

pub mod badge;
pub mod dictionary;
pub mod explain;
pub mod settings;
pub mod tray;
pub mod win32;

pub(crate) use generated::{
    BadgeWin, DictRow, DictionaryWin, ExplainCard, ExplainLine, ExplainWin, SettingsWin, SheetCell,
    SheetRow, TargetRow,
};

pub use badge::BadgeWindow;
pub use dictionary::DictionaryWindow;
pub use explain::ExplainWindow;
pub use settings::SettingsWindow;
