//! 사용자 데이터 폴더 위치.

use std::path::{Path, PathBuf};

const APP_DIR_NAME: &str = "ClaudeTranslator";

/// 사용자 데이터 폴더. `%APPDATA%\ClaudeTranslator`, APPDATA가 없거나 비어 있으면
/// `<home>/.claudetranslator` (개발용 대체 경로).
pub fn user_data_dir(appdata: Option<&str>, home: &Path) -> PathBuf {
    match appdata {
        Some(a) if !a.is_empty() => Path::new(a).join(APP_DIR_NAME),
        _ => home.join(".claudetranslator"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn data_dir_uses_appdata() {
        let d = user_data_dir(Some("C:\\Users\\a\\AppData\\Roaming"), Path::new("/home/x"));
        assert_eq!(
            d,
            PathBuf::from("C:\\Users\\a\\AppData\\Roaming").join("ClaudeTranslator")
        );
    }

    #[test]
    fn falls_back_to_home_when_appdata_missing_or_empty() {
        let home = Path::new("/home/x");
        assert_eq!(user_data_dir(None, home), home.join(".claudetranslator"));
        assert_eq!(
            user_data_dir(Some(""), home),
            home.join(".claudetranslator")
        );
    }
}
