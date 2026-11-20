//! 개인 사전: 모델, YAML 로드·저장, 저장소.

pub mod loader;
pub mod model;
pub mod protector;
pub mod store;

pub use loader::{load_dictionary_file, save_dictionary_file};
pub use model::{Action, DictionaryEntry, DictionaryError};
pub use protector::{
    MaskMatch, MaskResult, PlaceholderIntegrityError, mask, restore, restore_fragment,
    validate_integrity, validate_integrity_across,
};
pub use store::PersonalDictionaryStore;
