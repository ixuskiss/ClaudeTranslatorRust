//! Crash-safe file replacement: write `<file>.tmp`, flush it to disk, then rename it over
//! the target, so a crash mid-write can never leave a truncated user file.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

fn tmp_path(path: &Path) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Creates missing parent directories, then atomically replaces `path` with `contents`.
/// On failure the original file is left untouched (a stray temp file is removed when
/// possible).
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(path);
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(contents)?;
        f.sync_all()
    });
    let outcome = written.and_then(|()| std::fs::rename(&tmp, path));
    if outcome.is_err() && tmp.is_file() {
        let _ = std::fs::remove_file(&tmp);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_file_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a").join("f.yaml");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        assert!(!dir.path().join("a").join("f.yaml.tmp").exists());
    }

    #[test]
    fn failure_keeps_original_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.yaml");
        std::fs::write(&p, "original").unwrap();
        std::fs::create_dir(dir.path().join("f.yaml.tmp")).unwrap();
        assert!(write_atomic(&p, b"new").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "original");
    }
}
