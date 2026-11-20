//! User settings model, target-language helpers and the YAML-backed store.

use crate::lang::Lang;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value { $($s => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

string_enum!(DisplayMode { WholeText => "whole_text", PerSentence => "per_sentence" });
string_enum!(Tone { Neutral => "neutral", Casual => "casual", Formal => "formal", Plain => "plain" });
string_enum!(CorrectionMode { Spelling => "spelling", Polish => "polish" });

impl CorrectionMode {
    pub fn loading_message(self) -> &'static str {
        match self {
            CorrectionMode::Spelling => "한국어 교정 중...",
            CorrectionMode::Polish => "한국어 수정 중...",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserSettings {
    pub base_language: Lang,
    /// Order = output order.
    pub target_languages: Vec<Lang>,
    pub include_original_on_replace: bool,
    pub alt_alt_interval_seconds: f64,
    pub ctrl_ctrl_interval_seconds: f64,
    pub start_on_login: bool,
    /// Empty = auto-detect.
    pub claude_cli_path: String,
    pub ctrl_ctrl_display_mode: DisplayMode,
    pub translation_tone: Tone,
    pub claude_model: String,
    pub claude_effort: String,
    pub korean_correction_mode: CorrectionMode,
}

impl Default for UserSettings {
    fn default() -> Self {
        UserSettings {
            base_language: Lang::Ko,
            target_languages: vec![Lang::En],
            include_original_on_replace: false,
            alt_alt_interval_seconds: 0.2,
            ctrl_ctrl_interval_seconds: 0.2,
            start_on_login: false,
            claude_cli_path: String::new(),
            ctrl_ctrl_display_mode: DisplayMode::WholeText,
            translation_tone: Tone::Neutral,
            claude_model: "sonnet".to_string(),
            claude_effort: "low".to_string(),
            korean_correction_mode: CorrectionMode::Spelling,
        }
    }
}

/// Drops the base language and duplicates (first occurrence wins); an empty
/// result falls back to the first non-base language.
pub fn normalize_targets(base: Lang, raw: &[Lang]) -> Vec<Lang> {
    let mut out: Vec<Lang> = Vec::new();
    for &l in raw {
        if l != base && !out.contains(&l) {
            out.push(l);
        }
    }
    if out.is_empty()
        && let Some(first) = Lang::ALL.into_iter().find(|&l| l != base)
    {
        out.push(first);
    }
    out
}

/// Adds `lang` at the end, or removes it if present (never removing the last one).
pub fn toggle_target(base: Lang, targets: &[Lang], lang: Lang) -> Vec<Lang> {
    let current = normalize_targets(base, targets);
    if lang == base {
        return current;
    }
    if current.contains(&lang) {
        if current.len() == 1 {
            return current;
        }
        return current.into_iter().filter(|&l| l != lang).collect();
    }
    let mut next = current;
    next.push(lang);
    next
}

/// Moves `lang` one step earlier (`up`) or later; no-op at the edges or if absent.
pub fn move_target(targets: &[Lang], lang: Lang, up: bool) -> Vec<Lang> {
    let mut out = targets.to_vec();
    let Some(i) = out.iter().position(|&l| l == lang) else {
        return out;
    };
    let j = if up { i.checked_sub(1) } else { Some(i + 1) };
    if let Some(j) = j
        && j < out.len()
    {
        out.swap(i, j);
    }
    out
}

pub fn with_base(settings: &UserSettings, base: Lang) -> UserSettings {
    UserSettings {
        base_language: base,
        target_languages: normalize_targets(base, &settings.target_languages),
        ..settings.clone()
    }
}

pub struct SettingsStore {
    path: PathBuf,
    settings: UserSettings,
}

fn get_bool(map: &Mapping, key: &str, default: bool) -> bool {
    map.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn get_f64(map: &Mapping, key: &str, default: f64) -> f64 {
    match map.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        _ => default,
    }
}

fn get_string(map: &Mapping, key: &str, default: &str) -> String {
    match map.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => default.to_string(),
    }
}

fn get_lang(map: &Mapping, key: &str, default: Lang) -> Lang {
    match map.get(key) {
        Some(Value::String(s)) => Lang::from_code(s).unwrap_or(default),
        _ => default,
    }
}

fn get_enum<T>(map: &Mapping, key: &str, default: T, parse: fn(&str) -> Option<T>) -> T {
    match map.get(key) {
        Some(Value::String(s)) => parse(s).unwrap_or(default),
        _ => default,
    }
}

fn get_targets(map: &Mapping, base: Lang, default: &[Lang]) -> Vec<Lang> {
    let raw: Vec<Lang> = match map.get("target_languages") {
        Some(Value::String(s)) => Lang::from_code(s).into_iter().collect(),
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(|v| v.as_str().and_then(Lang::from_code))
            .collect(),
        _ => default.to_vec(),
    };
    normalize_targets(base, &raw)
}

fn from_mapping(map: &Mapping) -> UserSettings {
    let d = UserSettings::default();
    let base = get_lang(map, "base_language", d.base_language);
    UserSettings {
        base_language: base,
        target_languages: get_targets(map, base, &d.target_languages),
        include_original_on_replace: get_bool(
            map,
            "include_original_on_replace",
            d.include_original_on_replace,
        ),
        alt_alt_interval_seconds: get_f64(
            map,
            "alt_alt_interval_seconds",
            d.alt_alt_interval_seconds,
        ),
        ctrl_ctrl_interval_seconds: get_f64(
            map,
            "ctrl_ctrl_interval_seconds",
            d.ctrl_ctrl_interval_seconds,
        ),
        start_on_login: get_bool(map, "start_on_login", d.start_on_login),
        claude_cli_path: get_string(map, "claude_cli_path", &d.claude_cli_path),
        ctrl_ctrl_display_mode: get_enum(
            map,
            "ctrl_ctrl_display_mode",
            d.ctrl_ctrl_display_mode,
            DisplayMode::parse,
        ),
        translation_tone: get_enum(map, "translation_tone", d.translation_tone, Tone::parse),
        claude_model: get_string(map, "claude_model", &d.claude_model),
        claude_effort: get_string(map, "claude_effort", &d.claude_effort),
        korean_correction_mode: get_enum(
            map,
            "korean_correction_mode",
            d.korean_correction_mode,
            CorrectionMode::parse,
        ),
    }
}

fn to_mapping(s: &UserSettings) -> Mapping {
    let mut m = Mapping::new();
    let mut put = |k: &str, v: Value| {
        m.insert(Value::String(k.to_string()), v);
    };
    put(
        "base_language",
        Value::String(s.base_language.code().into()),
    );
    put(
        "target_languages",
        Value::Sequence(
            s.target_languages
                .iter()
                .map(|l| Value::String(l.code().into()))
                .collect(),
        ),
    );
    put(
        "include_original_on_replace",
        Value::Bool(s.include_original_on_replace),
    );
    put(
        "alt_alt_interval_seconds",
        Value::from(s.alt_alt_interval_seconds),
    );
    put(
        "ctrl_ctrl_interval_seconds",
        Value::from(s.ctrl_ctrl_interval_seconds),
    );
    put("start_on_login", Value::Bool(s.start_on_login));
    put("claude_cli_path", Value::String(s.claude_cli_path.clone()));
    put(
        "ctrl_ctrl_display_mode",
        Value::String(s.ctrl_ctrl_display_mode.as_str().into()),
    );
    put(
        "translation_tone",
        Value::String(s.translation_tone.as_str().into()),
    );
    put("claude_model", Value::String(s.claude_model.clone()));
    put("claude_effort", Value::String(s.claude_effort.clone()));
    put(
        "korean_correction_mode",
        Value::String(s.korean_correction_mode.as_str().into()),
    );
    m
}

impl SettingsStore {
    /// Never fails: a missing/empty file gives defaults; an unreadable one gives
    /// defaults plus a message in `problems`.
    pub fn load(path: &Path, problems: &mut Vec<String>) -> SettingsStore {
        let settings = Self::read(path, problems);
        SettingsStore {
            path: path.to_path_buf(),
            settings,
        }
    }

    fn read(path: &Path, problems: &mut Vec<String>) -> UserSettings {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return UserSettings::default(),
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "settings file could not be read");
                return Self::unreadable(path, problems);
            }
        };
        match serde_yaml::from_str::<Value>(&text) {
            Ok(Value::Null) => UserSettings::default(),
            Ok(Value::Mapping(map)) => from_mapping(&map),
            _ => {
                tracing::warn!("settings file has an invalid format");
                Self::unreadable(path, problems)
            }
        }
    }

    fn unreadable(path: &Path, problems: &mut Vec<String>) -> UserSettings {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        problems.push(format!(
            "{name}: 파일 형식 오류로 읽지 못해 기본 설정을 사용합니다."
        ));
        UserSettings::default()
    }

    pub fn get(&self) -> &UserSettings {
        &self.settings
    }

    /// Normalizes the targets against the base, persists atomically, and only then
    /// replaces the in-memory settings (a failed write changes nothing).
    pub fn update(&mut self, mut settings: UserSettings) -> std::io::Result<()> {
        settings.target_languages =
            normalize_targets(settings.base_language, &settings.target_languages);
        let yaml = serde_yaml::to_string(&Value::Mapping(to_mapping(&settings)))
            .map_err(std::io::Error::other)?;
        crate::atomic_write::write_atomic(&self.path, yaml.as_bytes())?;
        self.settings = settings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;
    use std::path::PathBuf;

    #[test]
    fn normalize_drops_base_duplicates_and_keeps_order() {
        assert_eq!(
            normalize_targets(Lang::Ko, &[Lang::Ja, Lang::Ko, Lang::En, Lang::Ja]),
            vec![Lang::Ja, Lang::En]
        );
    }
    #[test]
    fn normalize_empty_falls_back_to_first_non_base() {
        assert_eq!(normalize_targets(Lang::Ko, &[]), vec![Lang::En]);
        assert_eq!(normalize_targets(Lang::En, &[Lang::En]), vec![Lang::Ko]);
    }
    #[test]
    fn toggle_appends_new_and_removes_existing() {
        assert_eq!(
            toggle_target(Lang::Ko, &[Lang::En], Lang::Ja),
            vec![Lang::En, Lang::Ja]
        );
        assert_eq!(
            toggle_target(Lang::Ko, &[Lang::En, Lang::Ja], Lang::En),
            vec![Lang::Ja]
        );
    }
    #[test]
    fn toggle_keeps_last_remaining_target() {
        assert_eq!(
            toggle_target(Lang::Ko, &[Lang::En], Lang::En),
            vec![Lang::En]
        );
    }
    #[test]
    fn toggle_ignores_base() {
        assert_eq!(
            toggle_target(Lang::Ko, &[Lang::En], Lang::Ko),
            vec![Lang::En]
        );
    }
    #[test]
    fn move_up_and_down_and_edges() {
        let t = [Lang::En, Lang::Ja, Lang::Vi];
        assert_eq!(
            move_target(&t, Lang::Ja, true),
            vec![Lang::Ja, Lang::En, Lang::Vi]
        );
        assert_eq!(
            move_target(&t, Lang::Ja, false),
            vec![Lang::En, Lang::Vi, Lang::Ja]
        );
        assert_eq!(move_target(&t, Lang::En, true), t.to_vec());
        assert_eq!(move_target(&t, Lang::Vi, false), t.to_vec());
        assert_eq!(move_target(&t, Lang::Ko, true), t.to_vec());
    }
    #[test]
    fn with_base_removes_new_base_from_targets() {
        let s = UserSettings {
            target_languages: vec![Lang::En],
            ..Default::default()
        };
        let s2 = with_base(&s, Lang::En);
        assert_eq!(
            (s2.base_language, s2.target_languages),
            (Lang::En, vec![Lang::Ko])
        );
    }
    #[test]
    fn defaults_match_spec() {
        let s = UserSettings::default();
        assert_eq!(
            (
                s.base_language,
                s.target_languages.clone(),
                s.include_original_on_replace
            ),
            (Lang::Ko, vec![Lang::En], false)
        );
        assert_eq!(
            (s.claude_model.as_str(), s.claude_effort.as_str()),
            ("sonnet", "low")
        );
        assert_eq!(
            (s.alt_alt_interval_seconds, s.ctrl_ctrl_interval_seconds),
            (0.2, 0.2)
        );
        assert!(!s.start_on_login);
        assert_eq!(s.claude_cli_path, "");
        assert_eq!(s.ctrl_ctrl_display_mode, DisplayMode::WholeText);
        assert_eq!(s.translation_tone, Tone::Neutral);
        assert_eq!(s.korean_correction_mode, CorrectionMode::Spelling);
    }

    fn write(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.yaml");
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }
    fn load(content: &str) -> (UserSettings, Vec<String>) {
        let (_d, p) = write(content);
        let mut problems = Vec::new();
        let s = SettingsStore::load(&p, &mut problems).get().clone();
        (s, problems)
    }

    #[test]
    fn update_normalizes_targets_and_persists_them() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.yaml");
        let mut store = SettingsStore::load(&p, &mut Vec::new());
        let s = UserSettings {
            base_language: Lang::Ko,
            target_languages: vec![Lang::En, Lang::Ko, Lang::En],
            ..UserSettings::default()
        };
        store.update(s).unwrap();
        assert_eq!(store.get().target_languages, vec![Lang::En]);
        let text = std::fs::read_to_string(&p).unwrap();
        let raw: Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(
            raw["target_languages"],
            Value::Sequence(vec![Value::String("en".into())])
        );
        let again = SettingsStore::load(&p, &mut Vec::new());
        assert_eq!(again.get().target_languages, vec![Lang::En]);
    }

    #[test]
    fn update_writes_atomically_without_leftover_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.yaml");
        let mut store = SettingsStore::load(&p, &mut Vec::new());
        store.update(UserSettings::default()).unwrap();
        store
            .update(UserSettings {
                start_on_login: true,
                ..UserSettings::default()
            })
            .unwrap();
        assert!(!dir.path().join("settings.yaml.tmp").exists());
        assert!(
            SettingsStore::load(&p, &mut Vec::new())
                .get()
                .start_on_login
        );
    }

    #[test]
    fn failed_update_keeps_file_and_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.yaml");
        let mut store = SettingsStore::load(&p, &mut Vec::new());
        store.update(UserSettings::default()).unwrap();
        let before = std::fs::read_to_string(&p).unwrap();
        // A directory where the temp file must go makes the write fail.
        std::fs::create_dir(dir.path().join("settings.yaml.tmp")).unwrap();
        let changed = UserSettings {
            start_on_login: true,
            ..UserSettings::default()
        };
        assert!(store.update(changed).is_err());
        assert_eq!(store.get(), &UserSettings::default());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[test]
    fn enums_as_str_parse_round_trip() {
        for m in [DisplayMode::WholeText, DisplayMode::PerSentence] {
            assert_eq!(DisplayMode::parse(m.as_str()), Some(m));
        }
        for m in [Tone::Neutral, Tone::Casual, Tone::Formal, Tone::Plain] {
            assert_eq!(Tone::parse(m.as_str()), Some(m));
        }
        for m in [CorrectionMode::Spelling, CorrectionMode::Polish] {
            assert_eq!(CorrectionMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(DisplayMode::PerSentence.as_str(), "per_sentence");
        assert_eq!(DisplayMode::parse("x"), None);
    }

    #[test]
    fn store_round_trips_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("settings.yaml");
        let mut problems = Vec::new();
        let mut store = SettingsStore::load(&p, &mut problems);
        let s = UserSettings {
            base_language: Lang::En,
            target_languages: vec![Lang::Ja, Lang::Ko, Lang::Vi],
            include_original_on_replace: true,
            alt_alt_interval_seconds: 0.35,
            ctrl_ctrl_interval_seconds: 0.5,
            start_on_login: true,
            claude_cli_path: r"C:\x\claude.exe".to_string(),
            ctrl_ctrl_display_mode: DisplayMode::PerSentence,
            translation_tone: Tone::Formal,
            claude_model: "opus".to_string(),
            claude_effort: "high".to_string(),
            korean_correction_mode: CorrectionMode::Polish,
        };
        store.update(s.clone()).unwrap();
        assert_eq!(store.get(), &s);
        let mut problems = Vec::new();
        let again = SettingsStore::load(&p, &mut problems);
        assert_eq!(again.get(), &s);
        assert!(problems.is_empty());
    }

    #[test]
    fn load_missing_file_is_defaults_without_problems() {
        let dir = tempfile::tempdir().unwrap();
        let mut problems = Vec::new();
        let s = SettingsStore::load(&dir.path().join("settings.yaml"), &mut problems);
        assert_eq!(s.get(), &UserSettings::default());
        assert!(problems.is_empty());
    }
    #[test]
    fn load_empty_file_is_defaults_without_problems() {
        let (s, problems) = load("");
        assert_eq!(s, UserSettings::default());
        assert!(problems.is_empty());
    }
    #[test]
    fn load_unreadable_file_is_defaults_with_problem() {
        let (s, problems) = load("[unclosed");
        assert_eq!(s, UserSettings::default());
        assert_eq!(
            problems,
            vec!["settings.yaml: 파일 형식 오류로 읽지 못해 기본 설정을 사용합니다.".to_string()]
        );
        let (_, problems) = load("- a\n- b\n");
        assert_eq!(problems.len(), 1);
    }
    #[test]
    fn load_wrong_typed_field_uses_that_default_only() {
        let (s, problems) = load("alt_alt_interval_seconds: fast\nstart_on_login: true\n");
        assert_eq!(s.alt_alt_interval_seconds, 0.2);
        assert!(s.start_on_login);
        assert!(problems.is_empty());
    }
    #[test]
    fn load_int_accepted_for_float() {
        let (s, _) = load("alt_alt_interval_seconds: 1\n");
        assert_eq!(s.alt_alt_interval_seconds, 1.0);
    }
    #[test]
    fn load_unknown_codes_and_enums_fall_back() {
        let (s, _) = load(
            "target_languages: [xx, ja]\nkorean_correction_mode: wild\nctrl_ctrl_display_mode: x\n",
        );
        assert_eq!(s.target_languages, vec![Lang::Ja]);
        assert_eq!(s.korean_correction_mode, CorrectionMode::Spelling);
        assert_eq!(s.ctrl_ctrl_display_mode, DisplayMode::WholeText);
    }
    #[test]
    fn load_accepts_single_string_target() {
        let (s, _) = load("target_languages: en\n");
        assert_eq!(s.target_languages, vec![Lang::En]);
        let (s, _) = load("target_languages: ja\n");
        assert_eq!(s.target_languages, vec![Lang::Ja]);
    }
    #[test]
    fn load_normalizes_targets_against_base() {
        let (s, _) = load("base_language: en\ntarget_languages: [en]\n");
        assert_eq!(
            (s.base_language, s.target_languages),
            (Lang::En, vec![Lang::Ko])
        );
    }
    #[test]
    fn loading_message_per_mode() {
        assert_eq!(
            CorrectionMode::Spelling.loading_message(),
            "한국어 교정 중..."
        );
        assert_eq!(
            CorrectionMode::Polish.loading_message(),
            "한국어 수정 중..."
        );
    }
}
