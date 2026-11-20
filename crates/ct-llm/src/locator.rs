//! Best-effort location of the Claude Code CLI executable.
//!
//! `claude` is frequently not on PATH for a GUI-launched process, so besides PATH we try
//! npm's default global bin directory on Windows, then fall back to the bare name so the
//! eventual error is a plain "not found" from the OS.

use std::path::{Path, PathBuf};

/// An explicit `configured` value other than empty / bare `claude` always wins.
pub fn resolve_claude_cli_path(
    configured: &str,
    which: &dyn Fn(&str) -> Option<PathBuf>,
    home: &Path,
) -> String {
    if !configured.is_empty() && configured != "claude" {
        return configured.to_string();
    }
    if let Some(found) = which("claude") {
        return found.to_string_lossy().into_owned();
    }
    let npm_default = home
        .join("AppData")
        .join("Roaming")
        .join("npm")
        .join("claude.cmd");
    if npm_default.exists() {
        return npm_default.to_string_lossy().into_owned();
    }
    "claude".to_string()
}

/// Looks `name` up on PATH (trying PATHEXT extensions on Windows).
pub(crate) fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };
    for dir in std::env::split_paths(&path) {
        let plain = dir.join(name);
        if cfg!(not(windows)) && plain.is_file() {
            return Some(plain);
        }
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn none(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn explicit_non_default_path_wins() {
        let r = resolve_claude_cli_path(r"C:\custom\claude.exe", &none, Path::new("."));
        assert_eq!(r, r"C:\custom\claude.exe");
    }

    #[test]
    fn bare_default_falls_through_to_path_resolution() {
        let which = |_: &str| Some(PathBuf::from(r"C:\on\path\claude.cmd"));
        assert_eq!(
            resolve_claude_cli_path("claude", &which, Path::new(".")),
            r"C:\on\path\claude.cmd"
        );
    }

    #[test]
    fn empty_string_falls_through_to_path_resolution() {
        let which = |_: &str| Some(PathBuf::from(r"C:\on\path\claude.cmd"));
        assert_eq!(
            resolve_claude_cli_path("", &which, Path::new(".")),
            r"C:\on\path\claude.cmd"
        );
    }

    #[test]
    fn falls_back_to_npm_default_when_not_on_path() {
        let home = tempfile::tempdir().unwrap();
        let npm = home.path().join("AppData").join("Roaming").join("npm");
        std::fs::create_dir_all(&npm).unwrap();
        std::fs::write(npm.join("claude.cmd"), "").unwrap();
        assert_eq!(
            resolve_claude_cli_path("", &none, home.path()),
            npm.join("claude.cmd").to_string_lossy()
        );
    }

    #[test]
    fn falls_back_to_bare_claude_when_nothing_found() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(resolve_claude_cli_path("", &none, home.path()), "claude");
    }
}
