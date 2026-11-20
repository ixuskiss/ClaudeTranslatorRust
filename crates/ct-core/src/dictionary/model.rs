//! 사전 항목 모델.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Keep,
    Translate,
}

impl Action {
    /// 파일에 저장하는 값.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Keep => "KEEP",
            Action::Translate => "TRANSLATE",
        }
    }

    /// 대소문자와 앞뒤 공백을 무시하고 읽는다.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_uppercase().as_str() {
            "KEEP" => Some(Action::Keep),
            "TRANSLATE" => Some(Action::Translate),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error("TRANSLATE 항목에는 번역이 하나 이상 필요합니다: {0}")]
    TranslateWithoutTranslations(String),
    #[error("이미 개인 사전에 있는 용어입니다: {0}")]
    Duplicate(String),
    #[error("개인 사전에 없는 용어입니다: {0}")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryEntry {
    pub term: String,
    pub action: Action,
    /// 키는 소문자 언어 코드.
    pub translations: BTreeMap<String, String>,
}

impl DictionaryEntry {
    pub fn new(
        term: String,
        action: Action,
        translations: BTreeMap<String, String>,
    ) -> Result<Self, DictionaryError> {
        if action == Action::Translate && translations.is_empty() {
            return Err(DictionaryError::TranslateWithoutTranslations(term));
        }
        Ok(DictionaryEntry {
            term,
            action,
            translations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_entry_has_no_translations_required() {
        let e = DictionaryEntry::new("DA".into(), Action::Keep, BTreeMap::new()).unwrap();
        assert_eq!(e.term, "DA");
        assert_eq!(e.action, Action::Keep);
        assert!(e.translations.is_empty());
    }

    #[test]
    fn translate_entry_stores_per_language_translation() {
        let t: BTreeMap<String, String> = [
            ("ko", "논리 모델"),
            ("en", "logical model"),
            ("vi", "mô hình logic"),
            ("ja", "論理モデル"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let e = DictionaryEntry::new("logical model".into(), Action::Translate, t).unwrap();
        assert_eq!(e.translations["ko"], "논리 모델");
        assert_eq!(e.translations["ja"], "論理モデル");
    }

    #[test]
    fn translate_entry_requires_at_least_one_translation() {
        let r = DictionaryEntry::new("logical model".into(), Action::Translate, BTreeMap::new());
        assert!(matches!(
            r,
            Err(DictionaryError::TranslateWithoutTranslations(_))
        ));
    }

    #[test]
    fn action_parse_ignores_case() {
        assert_eq!(Action::parse(" keep "), Some(Action::Keep));
        assert_eq!(Action::parse("Translate"), Some(Action::Translate));
        assert_eq!(Action::parse("DELETE"), None);
    }
}
