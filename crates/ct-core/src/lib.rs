#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod lang;
pub mod settings;

pub use lang::Lang;
pub use settings::{
    CorrectionMode, DisplayMode, SettingsStore, Tone, UserSettings, move_target, normalize_targets,
    toggle_target, with_base,
};
