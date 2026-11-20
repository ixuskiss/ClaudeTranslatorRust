//! 사전 YAML 파일 읽기·쓰기. 내용이 잘못돼도 실패하지 않고 문제 목록에 파일 이름과 개수만 남긴다.

use super::model::{Action, DictionaryEntry};
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::Path;

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn unreadable_file_message(file_name: &str) -> String {
    format!("{file_name}: 파일 형식 오류로 읽지 못했습니다.")
}

fn skipped_entries_message(file_name: &str, count: usize) -> String {
    format!("{file_name}: 항목 {count}개를 읽지 못해 건너뛰었습니다.")
}

fn scalar_key(key: &Value) -> Option<String> {
    match key {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn clean_translations(raw: Option<&Value>) -> BTreeMap<String, String> {
    let mut cleaned = BTreeMap::new();
    let Some(Value::Mapping(map)) = raw else {
        return cleaned;
    };
    for (language, value) in map {
        let text = match value {
            Value::Number(n) => n.to_string(),
            // serde_yaml 0.9 reads unquoted On/Yes/No as strings (PyYAML: bool), so a hand-written
            // `en: On` is kept here, unlike v5. Quoted "No"/"Yes" must survive a save/load round trip.
            Value::String(s) => s.clone(),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        if let Some(key) = scalar_key(language) {
            cleaned.insert(key.to_lowercase(), text);
        }
    }
    cleaned
}

fn parse_entry(item: &Value) -> Option<DictionaryEntry> {
    let map = item.as_mapping()?;
    let term = match map.get("term")? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if term.trim().is_empty() {
        return None;
    }
    let action = Action::parse(map.get("action")?.as_str()?)?;
    DictionaryEntry::new(term, action, clean_translations(map.get("translations"))).ok()
}

/// 사전 파일을 읽는다. 없는 파일·빈 파일은 빈 목록이다.
pub fn load_dictionary_file(path: &Path, problems: &mut Vec<String>) -> Vec<DictionaryEntry> {
    if !path.exists() {
        return Vec::new();
    }
    let name = file_name(path);
    let raw = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_yaml::from_str::<Value>(&text).ok());
    let items = match raw {
        Some(Value::Null) => return Vec::new(),
        Some(Value::Mapping(map)) => match map.get("entries") {
            None | Some(Value::Null) => return Vec::new(),
            Some(Value::Sequence(items)) => items.clone(),
            Some(_) => {
                tracing::warn!("dictionary file could not be read");
                problems.push(unreadable_file_message(&name));
                return Vec::new();
            }
        },
        _ => {
            tracing::warn!("dictionary file could not be read");
            problems.push(unreadable_file_message(&name));
            return Vec::new();
        }
    };

    let mut entries = Vec::new();
    let mut skipped = 0usize;
    for item in &items {
        match parse_entry(item) {
            Some(e) => entries.push(e),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(skipped, "skipped unusable dictionary entries");
        problems.push(skipped_entries_message(&name, skipped));
    }
    entries
}

/// `entries:` 목록으로 저장한다. 번역이 비어 있으면 `translations` 키를 생략한다.
pub fn save_dictionary_file(path: &Path, entries: &[DictionaryEntry]) -> std::io::Result<()> {
    let items: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut m = Mapping::new();
            m.insert("term".into(), Value::String(e.term.clone()));
            m.insert("action".into(), Value::String(e.action.as_str().into()));
            if !e.translations.is_empty() {
                let mut t = Mapping::new();
                for (k, v) in &e.translations {
                    t.insert(Value::String(k.clone()), Value::String(v.clone()));
                }
                m.insert("translations".into(), Value::Mapping(t));
            }
            Value::Mapping(m)
        })
        .collect();
    let mut root = Mapping::new();
    root.insert("entries".into(), Value::Sequence(items));
    let text = serde_yaml::to_string(&Value::Mapping(root))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    // tmp + rename: a crash mid-save can never truncate user.yaml.
    crate::atomic_write::write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_yaml(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn load(path: &Path) -> (Vec<DictionaryEntry>, Vec<String>) {
        let mut problems = Vec::new();
        let e = load_dictionary_file(path, &mut problems);
        (e, problems)
    }

    fn terms(entries: &[DictionaryEntry]) -> Vec<&str> {
        entries.iter().map(|x| x.term.as_str()).collect()
    }

    fn tr(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn loads_keep_and_translate_entries() {
        let d = TempDir::new().unwrap();
        let p = write_yaml(
            &d,
            "company.yaml",
            "entries:\n  - term: DA\n    action: KEEP\n  - term: logical model\n    action: TRANSLATE\n    translations:\n      ko: 논리 모델\n      en: logical model\n      vi: mô hình logic\n      ja: 論理モデル\n",
        );
        let (entries, problems) = load(&p);
        assert!(problems.is_empty());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, Action::Keep);
        assert_eq!(entries[1].action, Action::Translate);
        assert_eq!(entries[1].translations["ja"], "論理モデル");
    }

    #[test]
    fn missing_file_returns_empty_list() {
        let d = TempDir::new().unwrap();
        let (e, p) = load(&d.path().join("team.yaml"));
        assert!(e.is_empty() && p.is_empty());
    }

    #[test]
    fn empty_file_returns_empty_list() {
        let d = TempDir::new().unwrap();
        let p = write_yaml(&d, "user.yaml", "");
        let (e, problems) = load(&p);
        assert!(e.is_empty() && problems.is_empty());
    }

    #[test]
    fn unknown_action_entry_is_skipped_and_reported() {
        let d = TempDir::new().unwrap();
        let p = write_yaml(
            &d,
            "company.yaml",
            "entries:\n  - term: DA\n    action: DELETE\n  - term: CLT\n    action: KEEP\n",
        );
        let (e, problems) = load(&p);
        assert_eq!(terms(&e), ["CLT"]);
        assert_eq!(
            problems,
            ["company.yaml: 항목 1개를 읽지 못해 건너뛰었습니다."]
        );
    }

    #[test]
    fn action_is_case_insensitive() {
        let d = TempDir::new().unwrap();
        let p = write_yaml(
            &d,
            "user.yaml",
            "entries:\n  - term: DA\n    action: keep\n",
        );
        assert_eq!(load(&p).0[0].action, Action::Keep);
    }

    #[test]
    fn malformed_entries_are_skipped() {
        let d = TempDir::new().unwrap();
        let p = write_yaml(
            &d,
            "team.yaml",
            "entries:\n  - just a string\n  - action: KEEP\n  - term: \"\"\n    action: KEEP\n  - term: 123\n    action: KEEP\n  - term: DA\n",
        );
        let (e, problems) = load(&p);
        assert_eq!(terms(&e), ["123"]);
        assert_eq!(
            problems,
            ["team.yaml: 항목 4개를 읽지 못해 건너뛰었습니다."]
        );
    }

    #[test]
    fn unreadable_file_returns_empty_and_reports() {
        for content in [
            "entries:\n  - term: DA\n   action: KEEP\n",
            "entries: [unclosed\n",
            "- term: DA\n  action: KEEP\n",
            "entries: DA\n",
        ] {
            let d = TempDir::new().unwrap();
            let p = write_yaml(&d, "company.yaml", content);
            let (e, problems) = load(&p);
            assert!(e.is_empty(), "{content}");
            assert_eq!(
                problems,
                ["company.yaml: 파일 형식 오류로 읽지 못했습니다."],
                "{content}"
            );
        }
    }

    #[test]
    fn non_string_and_empty_translation_values_are_ignored_or_coerced() {
        let d = TempDir::new().unwrap();
        let p = write_yaml(
            &d,
            "user.yaml",
            "entries:\n  - term: year\n    action: TRANSLATE\n    translations:\n      ko: 연도\n      ja:\n      vi: \"\"\n      fr: \"   \"\n      en: true\n      us: false\n      de: 2024\n",
        );
        let (e, _) = load(&p);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].translations, tr(&[("ko", "연도"), ("de", "2024")]));
    }

    #[test]
    fn translate_entry_without_usable_translations_is_skipped() {
        let d = TempDir::new().unwrap();
        let p = write_yaml(
            &d,
            "user.yaml",
            "entries:\n  - term: bad\n    action: TRANSLATE\n    translations:\n      en: true\n      ja:\n  - term: DA\n    action: KEEP\n",
        );
        let (e, _) = load(&p);
        assert_eq!(terms(&e), ["DA"]);
    }

    #[test]
    fn round_trips_keep_and_translate_entries() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let entries = vec![
            DictionaryEntry::new("Jenny".into(), Action::Keep, BTreeMap::new()).unwrap(),
            DictionaryEntry::new(
                "logical model".into(),
                Action::Translate,
                tr(&[
                    ("ko", "논리 모델"),
                    ("en", "logical model"),
                    ("vi", "mô hình logic"),
                    ("ja", "論理モデル"),
                ]),
            )
            .unwrap(),
            DictionaryEntry::new("123".into(), Action::Translate, tr(&[("en", "2024")])).unwrap(),
        ];
        save_dictionary_file(&p, &entries).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("entries:"));
        let (loaded, problems) = load(&p);
        assert!(problems.is_empty());
        assert_eq!(loaded, entries);
    }

    #[test]
    fn round_trips_yes_no_translation_values() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let entries = vec![
            DictionaryEntry::new("ok".into(), Action::Translate, tr(&[("en", "Yes")])).unwrap(),
            DictionaryEntry::new("no".into(), Action::Translate, tr(&[("en", "No")])).unwrap(),
        ];
        save_dictionary_file(&p, &entries).unwrap();
        let (loaded, problems) = load(&p);
        assert!(problems.is_empty());
        assert_eq!(loaded, entries);
    }

    #[test]
    fn keep_entry_omits_translations_key() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        save_dictionary_file(
            &p,
            &[DictionaryEntry::new("DA".into(), Action::Keep, BTreeMap::new()).unwrap()],
        )
        .unwrap();
        assert!(
            !std::fs::read_to_string(&p)
                .unwrap()
                .contains("translations")
        );
    }

    #[test]
    fn saving_empty_list_produces_a_loadable_empty_file() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        save_dictionary_file(&p, &[]).unwrap();
        let (e, problems) = load(&p);
        assert!(e.is_empty() && problems.is_empty());
    }

    #[test]
    fn overwrites_existing_file_contents() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let mk = |t: &str| DictionaryEntry::new(t.into(), Action::Keep, BTreeMap::new()).unwrap();
        save_dictionary_file(&p, &[mk("OLD")]).unwrap();
        save_dictionary_file(&p, &[mk("NEW")]).unwrap();
        assert_eq!(load(&p).0, vec![mk("NEW")]);
        assert!(!d.path().join("user.yaml.tmp").exists());
    }

    #[test]
    fn failed_save_keeps_existing_file_intact() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let mk = |t: &str| DictionaryEntry::new(t.into(), Action::Keep, BTreeMap::new()).unwrap();
        save_dictionary_file(&p, &[mk("OLD")]).unwrap();
        let before = std::fs::read_to_string(&p).unwrap();
        // A directory where the temp file must go makes the write fail.
        std::fs::create_dir(d.path().join("user.yaml.tmp")).unwrap();
        assert!(save_dictionary_file(&p, &[mk("NEW")]).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }
}
