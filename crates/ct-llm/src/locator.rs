//! Best-effort location of the Claude Code CLI executable.
//!
//! `claude` is frequently not on PATH for a GUI-launched process, so besides PATH we try
//! npm's default global bin directory on Windows, then fall back to the bare name so the
//! eventual error is a plain "not found" from the OS.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// How to start the CLI: `program` followed by `prefix_args` (inserted before the CLI's own
/// flags). A `.cmd`/`.bat` shim is avoided whenever its real target can be found, because
/// launching one makes Windows start `cmd.exe`, which locked-down PCs block for piped
/// processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliLaunch {
    pub program: PathBuf,
    pub prefix_args: Vec<String>,
}

impl CliLaunch {
    fn direct(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
        }
    }
}

fn is_shim(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Replaces an npm `.cmd` shim by the file it runs: the native `claude.exe` of newer
/// package versions, or `node cli.js` for older ones. Non-shims pass through unchanged,
/// and a shim whose target cannot be found is kept as a last resort.
fn unwrap_shim(path: &Path, which: &dyn Fn(&str) -> Option<PathBuf>) -> CliLaunch {
    if !is_shim(path) {
        return CliLaunch::direct(path);
    }
    let Some(dir) = path.parent() else {
        return CliLaunch::direct(path);
    };
    let pkg = dir
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code");
    let exe = pkg.join("bin").join("claude.exe");
    if exe.is_file() {
        return CliLaunch::direct(exe);
    }
    let cli_js = pkg.join("cli.js");
    if cli_js.is_file() {
        let beside = dir.join("node.exe");
        let node = if beside.is_file() {
            Some(beside)
        } else {
            which("node")
        };
        if let Some(node) = node {
            return CliLaunch {
                program: node,
                prefix_args: vec![cli_js.to_string_lossy().into_owned()],
            };
        }
    }
    CliLaunch::direct(path)
}

/// Resolution order: explicit `configured` path (anything other than empty / bare
/// `claude`), then PATH (`claude.exe` before `claude.cmd`), then npm's default global bin
/// directory, then the bare name so the eventual error is a plain "not found" from the OS.
/// Shims are unwrapped at every step.
pub fn resolve_claude_cli(
    configured: &str,
    which: &dyn Fn(&str) -> Option<PathBuf>,
    home: &Path,
) -> CliLaunch {
    if !configured.is_empty() && configured != "claude" {
        return unwrap_shim(Path::new(configured), which);
    }
    if let Some(found) = which("claude.exe")
        .or_else(|| which("claude.cmd"))
        .or_else(|| which("claude"))
    {
        return unwrap_shim(&found, which);
    }
    let npm_default = home
        .join("AppData")
        .join("Roaming")
        .join("npm")
        .join("claude.cmd");
    if npm_default.is_file() {
        return unwrap_shim(&npm_default, which);
    }
    CliLaunch::direct("claude")
}

/// Looks `name` up on PATH (trying PATHEXT extensions on Windows; a name that already has
/// an extension, like `claude.exe`, is matched exactly).
pub fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    which_in(&path, name)
}

fn which_in(path: &OsStr, name: &str) -> Option<PathBuf> {
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
    for dir in std::env::split_paths(path) {
        let plain = dir.join(name);
        if plain.is_file() && (cfg!(not(windows)) || Path::new(name).extension().is_some()) {
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

pub fn home_dir() -> PathBuf {
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

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }

    fn pkg(dir: &Path) -> PathBuf {
        dir.join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
    }

    /// npm global dir with a shim and the new (native exe) package layout.
    fn new_layout(dir: &Path) -> PathBuf {
        touch(&dir.join("claude.cmd"));
        let exe = pkg(dir).join("bin").join("claude.exe");
        touch(&exe);
        exe
    }

    /// npm global dir with a shim and the old (cli.js) package layout.
    fn old_layout(dir: &Path) -> PathBuf {
        touch(&dir.join("claude.cmd"));
        let js = pkg(dir).join("cli.js");
        touch(&js);
        js
    }

    fn launch(program: &Path, args: &[&str]) -> CliLaunch {
        CliLaunch {
            program: program.to_path_buf(),
            prefix_args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn shim_on_path(cmd: PathBuf) -> impl Fn(&str) -> Option<PathBuf> {
        move |n: &str| (n == "claude.cmd").then(|| cmd.clone())
    }

    #[test]
    fn explicit_exe_path_is_used_as_is() {
        let r = resolve_claude_cli(r"C:\custom\claude.exe", &none, Path::new("."));
        assert_eq!(r, launch(Path::new(r"C:\custom\claude.exe"), &[]));
    }

    #[test]
    fn explicit_cmd_path_is_unwrapped() {
        let dir = tempfile::tempdir().unwrap();
        let exe = new_layout(dir.path());
        let cmd = dir.path().join("claude.cmd");
        let r = resolve_claude_cli(&cmd.to_string_lossy(), &none, Path::new("."));
        assert_eq!(r, launch(&exe, &[]));
    }

    #[test]
    fn new_npm_layout_on_path_resolves_to_native_exe() {
        let dir = tempfile::tempdir().unwrap();
        let exe = new_layout(dir.path());
        let which = shim_on_path(dir.path().join("claude.cmd"));
        let r = resolve_claude_cli("", &which, Path::new("."));
        assert_eq!(r, launch(&exe, &[]));
    }

    #[test]
    fn old_npm_layout_uses_node_exe_beside_shim() {
        let dir = tempfile::tempdir().unwrap();
        let js = old_layout(dir.path());
        let node = dir.path().join("node.exe");
        touch(&node);
        let which = shim_on_path(dir.path().join("claude.cmd"));
        let r = resolve_claude_cli("claude", &which, Path::new("."));
        assert_eq!(r, launch(&node, &[&js.to_string_lossy()]));
    }

    #[test]
    fn old_npm_layout_without_node_exe_uses_node_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let js = old_layout(dir.path());
        let cmd = dir.path().join("claude.cmd");
        let which = move |n: &str| match n {
            "claude.cmd" => Some(cmd.clone()),
            "node" => Some(PathBuf::from(r"C:\nodejs\node.exe")),
            _ => None,
        };
        let r = resolve_claude_cli("", &which, Path::new("."));
        assert_eq!(
            r,
            launch(Path::new(r"C:\nodejs\node.exe"), &[&js.to_string_lossy()])
        );
    }

    #[test]
    fn shim_with_unknown_target_is_kept_as_last_resort() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("claude.cmd");
        touch(&cmd);
        let which = shim_on_path(cmd.clone());
        assert_eq!(
            resolve_claude_cli("", &which, Path::new(".")),
            launch(&cmd, &[])
        );
    }

    #[test]
    fn old_layout_without_any_node_keeps_shim() {
        let dir = tempfile::tempdir().unwrap();
        old_layout(dir.path());
        let cmd = dir.path().join("claude.cmd");
        let which = shim_on_path(cmd.clone());
        assert_eq!(
            resolve_claude_cli("", &which, Path::new(".")),
            launch(&cmd, &[])
        );
    }

    #[test]
    fn native_exe_on_path_wins_over_shim() {
        let dir = tempfile::tempdir().unwrap();
        new_layout(dir.path());
        let cmd = dir.path().join("claude.cmd");
        let which = move |n: &str| match n {
            "claude.exe" => Some(PathBuf::from(r"C:\native\claude.exe")),
            "claude.cmd" => Some(cmd.clone()),
            _ => None,
        };
        let r = resolve_claude_cli("", &which, Path::new("."));
        assert_eq!(r, launch(Path::new(r"C:\native\claude.exe"), &[]));
    }

    #[test]
    fn npm_default_dir_is_unwrapped_when_not_on_path() {
        let home = tempfile::tempdir().unwrap();
        let npm = home.path().join("AppData").join("Roaming").join("npm");
        let exe = new_layout(&npm);
        assert_eq!(
            resolve_claude_cli("", &none, home.path()),
            launch(&exe, &[])
        );
    }

    #[test]
    fn falls_back_to_bare_claude_when_nothing_found() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_claude_cli("", &none, home.path()),
            launch(Path::new("claude"), &[])
        );
    }

    #[test]
    fn which_in_matches_names_with_extension_exactly() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("zz-probe.cmd"));
        touch(&dir.path().join("zz-probe.exe"));
        let path = dir.path().as_os_str();
        assert_eq!(
            which_in(path, "zz-probe.exe"),
            Some(dir.path().join("zz-probe.exe"))
        );
        assert_eq!(
            which_in(path, "zz-probe.cmd"),
            Some(dir.path().join("zz-probe.cmd"))
        );
    }
}
