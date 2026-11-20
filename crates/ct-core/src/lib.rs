#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod file_problems;
pub mod lang;
pub mod paths;
pub mod settings;

pub use lang::Lang;
pub use settings::{
    CorrectionMode, DisplayMode, SettingsStore, Tone, UserSettings, move_target, normalize_targets,
    toggle_target, with_base,
};

pub use config::{
    AppConfig, ClaudeCliConfig, LlmProviderKind, SheetsConfig, load_app_config,
    load_app_config_from_environment,
};
pub use file_problems::file_problems_warning;
pub use paths::user_data_dir;
