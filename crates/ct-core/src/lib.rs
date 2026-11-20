#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod atomic_write;
pub mod compose;
pub mod config;
pub mod correction;
pub mod detector;
pub mod dictionary;
pub mod explain;
pub mod file_problems;
pub mod lang;
pub mod llm;
pub mod paths;
pub mod plan;
pub mod settings;
#[cfg(test)]
mod test_support;
pub mod translate;

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
pub use correction::{CorrectionOutcome, CorrectionStatus, correct_sentences, correct_text};
pub use detector::{LanguageDetector, RuleBasedDetector};
pub use dictionary::{
    Action, DictionaryEntry, DictionaryError, PersonalDictionaryStore, load_dictionary_file,
    save_dictionary_file,
};
pub use explain::{explain_text, restore_sentence_columns};
pub use file_problems::file_problems_warning;
pub use llm::{
    CorrectionRequest, CorrectionSentence, ExplanationRequest, ExplanationResult,
    ExplanationSentence, LlmError, LlmProvider, TranslationRequest, TranslationResult,
    parse_json_response, parse_language_map,
};
pub use paths::user_data_dir;
pub use plan::{TranslationPlan, resolve_plan};
pub use translate::{DICTIONARY_INTEGRITY_MESSAGE, TranslateError, Translated, translate_text};
