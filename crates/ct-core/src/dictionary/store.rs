//! 개인 사전 저장소. 변경할 때마다 파일에 저장하고 메모리 사본을 유지한다.
//!
//! 읽지 못한 내용이 있는 파일은 먼저 `<이름>.bak`으로 복사한다. 다음 저장이
//! 읽은 항목만으로 파일을 다시 쓰기 때문에, 사용자가 손으로 쓴 줄을 잃지 않기 위해서다.

use super::loader::{load_dictionary_file, save_dictionary_file};
use super::model::{DictionaryEntry, DictionaryError};
use std::path::{Path, PathBuf};

pub struct PersonalDictionaryStore {
    path: PathBuf,
    entries: Vec<DictionaryEntry>,
}

impl PersonalDictionaryStore {
    pub fn load(path: &Path, problems: &mut Vec<String>) -> Self {
        let mut load_problems = Vec::new();
        let entries = load_dictionary_file(path, &mut load_problems);
        if !load_problems.is_empty() {
            load_problems.extend(back_up_original(path));
        }
        problems.extend(load_problems);
        PersonalDictionaryStore {
            path: path.to_path_buf(),
            entries,
        }
    }

    pub fn entries(&self) -> &[DictionaryEntry] {
        &self.entries
    }

    fn find_index(&self, term: &str) -> Option<usize> {
        let lower = term.to_lowercase();
        self.entries
            .iter()
            .position(|e| e.term.to_lowercase() == lower)
    }

    /// 새 목록을 먼저 저장하고, 성공해야 메모리 사본을 바꾼다.
    fn commit(&mut self, next: Vec<DictionaryEntry>) -> Result<(), DictionaryError> {
        save_dictionary_file(&self.path, &next)?;
        self.entries = next;
        Ok(())
    }

    pub fn add(&mut self, entry: DictionaryEntry) -> Result<(), DictionaryError> {
        if self.find_index(&entry.term).is_some() {
            return Err(DictionaryError::Duplicate(entry.term));
        }
        let mut next = self.entries.clone();
        next.push(entry);
        self.commit(next)
    }

    pub fn update(&mut self, term: &str, entry: DictionaryEntry) -> Result<(), DictionaryError> {
        let index = self
            .find_index(term)
            .ok_or_else(|| DictionaryError::NotFound(term.to_string()))?;
        let mut next = self.entries.clone();
        next[index] = entry;
        self.commit(next)
    }

    pub fn delete(&mut self, term: &str) -> Result<(), DictionaryError> {
        let index = self
            .find_index(term)
            .ok_or_else(|| DictionaryError::NotFound(term.to_string()))?;
        let mut next = self.entries.clone();
        next.remove(index);
        self.commit(next)
    }
}

fn back_up_original(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let backup_name = format!("{name}.bak");
    let backup = path.with_file_name(&backup_name);
    match std::fs::copy(path, &backup) {
        Ok(_) => Some(format!("{name}: 원본을 {backup_name}으로 백업했습니다.")),
        Err(_) => {
            tracing::warn!("could not back up dictionary file");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::model::Action;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn keep(term: &str) -> DictionaryEntry {
        DictionaryEntry::new(term.into(), Action::Keep, BTreeMap::new()).unwrap()
    }

    fn open(path: &Path) -> (PersonalDictionaryStore, Vec<String>) {
        let mut problems = Vec::new();
        let s = PersonalDictionaryStore::load(path, &mut problems);
        (s, problems)
    }

    fn reload(path: &Path) -> Vec<DictionaryEntry> {
        load_dictionary_file(path, &mut Vec::new())
    }

    #[test]
    fn loads_existing_entries() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        save_dictionary_file(&p, &[keep("Jenny")]).unwrap();
        assert_eq!(open(&p).0.entries(), [keep("Jenny")]);
    }

    #[test]
    fn starts_empty_when_file_does_not_exist() {
        let d = TempDir::new().unwrap();
        assert!(open(&d.path().join("user.yaml")).0.entries().is_empty());
    }

    #[test]
    fn add_appends_and_persists() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let (mut s, _) = open(&p);
        s.add(keep("Jenny")).unwrap();
        assert_eq!(s.entries(), [keep("Jenny")]);
        assert_eq!(reload(&p), vec![keep("Jenny")]);
    }

    #[test]
    fn store_add_duplicate_is_case_insensitive() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let (mut s, _) = open(&p);
        s.add(keep("DA")).unwrap();
        assert!(matches!(
            s.add(keep("da")),
            Err(DictionaryError::Duplicate(_))
        ));
        assert_eq!(s.entries().len(), 1);
    }

    #[test]
    fn update_replaces_entry_and_persists() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let (mut s, _) = open(&p);
        s.add(keep("Jenny")).unwrap();
        let new = DictionaryEntry::new(
            "Jenny".into(),
            Action::Translate,
            [("en".to_string(), "Jenny (marketing)".to_string())].into(),
        )
        .unwrap();
        s.update("jenny", new.clone()).unwrap();
        assert_eq!(s.entries(), std::slice::from_ref(&new));
        assert_eq!(reload(&p), vec![new]);
    }

    #[test]
    fn update_unknown_term_is_not_found() {
        let d = TempDir::new().unwrap();
        let (mut s, _) = open(&d.path().join("user.yaml"));
        assert!(matches!(
            s.update("Nobody", keep("Nobody")),
            Err(DictionaryError::NotFound(_))
        ));
    }

    #[test]
    fn delete_removes_entry_and_persists() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let (mut s, _) = open(&p);
        s.add(keep("Jenny")).unwrap();
        s.delete("Jenny").unwrap();
        assert!(s.entries().is_empty());
        assert!(reload(&p).is_empty());
    }

    #[test]
    fn delete_unknown_term_is_not_found() {
        let d = TempDir::new().unwrap();
        let (mut s, _) = open(&d.path().join("user.yaml"));
        assert!(matches!(
            s.delete("Nobody"),
            Err(DictionaryError::NotFound(_))
        ));
    }

    #[test]
    fn unreadable_file_is_backed_up_and_reported() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        let broken = "entries: [unclosed\n";
        std::fs::write(&p, broken).unwrap();
        let (s, problems) = open(&p);
        assert!(s.entries().is_empty());
        assert_eq!(
            std::fs::read_to_string(d.path().join("user.yaml.bak")).unwrap(),
            broken
        );
        assert_eq!(
            problems,
            [
                "user.yaml: 파일 형식 오류로 읽지 못했습니다.",
                "user.yaml: 원본을 user.yaml.bak으로 백업했습니다."
            ]
        );
    }

    #[test]
    fn store_backs_up_before_rewriting_broken_file() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        std::fs::write(&p, "entries:\n  - term: DA\n    action: DELETE\n").unwrap();
        let (mut s, problems) = open(&p);
        assert_eq!(problems.len(), 2);
        s.add(keep("CLT")).unwrap();
        let bak = std::fs::read_to_string(d.path().join("user.yaml.bak")).unwrap();
        assert!(bak.contains("DELETE"));
        let terms: Vec<_> = reload(&p).into_iter().map(|e| e.term).collect();
        assert_eq!(terms, ["CLT"]);
    }

    #[test]
    fn no_backup_when_file_reads_cleanly() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("user.yaml");
        save_dictionary_file(&p, &[keep("DA")]).unwrap();
        let (_, problems) = open(&p);
        assert!(problems.is_empty());
        assert!(!d.path().join("user.yaml.bak").exists());
    }
}
