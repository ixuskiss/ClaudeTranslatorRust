#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod compose;
pub mod config;
pub mod detector;
pub mod dictionary;
pub mod file_problems;
pub mod lang;
pub mod paths;
pub mod plan;
pub mod settings;

pub use lang::Lang;
pub use settings::{
    CorrectionMode, DisplayMode, SettingsStore, Tone, UserSettings, move_target, normalize_targets,
    toggle_target, with_base,
};

pub use compose::compose_replacement;
pub use config::{
    AppConfig, ClaudeCliConfig, LlmProviderKind, SheetsConfig, load_app_config,
    load_app_config_from_environment,
};
pub use detector::{LanguageDetector, RuleBasedDetector};
pub use dictionary::{
    Action, DictionaryEntry, DictionaryError, PersonalDictionaryStore, load_dictionary_file,
    save_dictionary_file,
};
pub use file_problems::file_problems_warning;
pub use paths::user_data_dir;
pub use plan::{TranslationPlan, resolve_plan};
