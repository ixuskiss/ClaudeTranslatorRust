//! Supported languages.

/// Supported language. Declaration order is the canonical order (`ko, en, vi, ja`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lang {
    Ko,
    En,
    Vi,
    Ja,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::Ko, Lang::En, Lang::Vi, Lang::Ja];

    pub fn code(self) -> &'static str {
        match self {
            Lang::Ko => "ko",
            Lang::En => "en",
            Lang::Vi => "vi",
            Lang::Ja => "ja",
        }
    }

    /// Trims surrounding whitespace and ignores case.
    pub fn from_code(code: &str) -> Option<Lang> {
        let code = code.trim().to_ascii_lowercase();
        Lang::ALL.into_iter().find(|l| l.code() == code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trip() {
        for l in Lang::ALL {
            assert_eq!(Lang::from_code(l.code()), Some(l));
        }
    }

    #[test]
    fn from_code_trims_and_ignores_case() {
        assert_eq!(Lang::from_code("  JA "), Some(Lang::Ja));
        assert_eq!(Lang::from_code("Ko"), Some(Lang::Ko));
        assert_eq!(Lang::from_code("xx"), None);
        assert_eq!(Lang::from_code(""), None);
    }

    #[test]
    fn ord_follows_all_order() {
        let mut v = vec![Lang::Ja, Lang::Vi, Lang::En, Lang::Ko];
        v.sort();
        assert_eq!(v, Lang::ALL.to_vec());
    }
}
