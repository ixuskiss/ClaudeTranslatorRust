//! 환경 변수(.env 포함)에서 읽는 앱 설정.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProviderKind {
    ClaudeCli,
    Mock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCliConfig {
    pub cli_path: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub timeout: Duration,
    /// N턴마다 /clear 를 보낸다. 0이면 주기적 clear 없음.
    pub clear_every_n_turns: u32,
}

impl Default for ClaudeCliConfig {
    fn default() -> Self {
        Self {
            cli_path: "claude".to_string(),
            model: None,
            effort: None,
            timeout: Duration::from_secs(60),
            clear_every_n_turns: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetsConfig {
    pub batch_max_cells: usize,
    pub batch_max_chars: usize,
    pub max_cell_chars: usize,
    pub max_range_cells: usize,
    pub job_timeout: Duration,
    pub range_replace_enabled: bool,
    pub edit_full_cell_replace_enabled: bool,
}

impl Default for SheetsConfig {
    fn default() -> Self {
        Self {
            batch_max_cells: 30,
            batch_max_chars: 5000,
            max_cell_chars: 5000,
            max_range_cells: 200,
            job_timeout: Duration::from_secs(240),
            range_replace_enabled: true,
            edit_full_cell_replace_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub llm_provider: LlmProviderKind,
    pub claude_cli: ClaudeCliConfig,
    pub sheets: SheetsConfig,
}

fn raw<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

/// 파싱 실패 시 키 이름만 로그에 남기고 `None`.
fn parse_or_warn<T: FromStr>(env: &HashMap<String, String>, key: &str) -> Option<T> {
    let value = raw(env, key)?;
    match value.trim().parse::<T>() {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::warn!(key, "invalid value; using default");
            None
        }
    }
}

fn positive_usize(env: &HashMap<String, String>, key: &str, default: usize) -> usize {
    match parse_or_warn::<usize>(env, key) {
        Some(0) => {
            tracing::warn!(key, "invalid value (expected positive); using default");
            default
        }
        Some(v) => v,
        None => default,
    }
}

fn positive_secs(env: &HashMap<String, String>, key: &str, default: Duration) -> Duration {
    match parse_or_warn::<f64>(env, key) {
        Some(v) if v > 0.0 => Duration::try_from_secs_f64(v).unwrap_or_else(|_| {
            tracing::warn!(key, "invalid value (out of range); using default");
            default
        }),
        Some(_) => {
            tracing::warn!(key, "invalid value (expected positive); using default");
            default
        }
        None => default,
    }
}

fn flag_unless_zero(env: &HashMap<String, String>, key: &str) -> bool {
    env.get(key).map(String::as_str) != Some("0")
}

fn load_sheets_config(env: &HashMap<String, String>) -> SheetsConfig {
    let d = SheetsConfig::default();
    SheetsConfig {
        batch_max_cells: positive_usize(env, "SHEETS_BATCH_MAX_CELLS", d.batch_max_cells),
        batch_max_chars: positive_usize(env, "SHEETS_BATCH_MAX_CHARS", d.batch_max_chars),
        max_cell_chars: positive_usize(env, "SHEETS_MAX_CELL_CHARS", d.max_cell_chars),
        max_range_cells: positive_usize(env, "SHEETS_MAX_RANGE_CELLS", d.max_range_cells),
        job_timeout: positive_secs(env, "SHEETS_JOB_TIMEOUT_SECONDS", d.job_timeout),
        range_replace_enabled: flag_unless_zero(env, "SHEETS_RANGE_REPLACE_ENABLED"),
        edit_full_cell_replace_enabled: flag_unless_zero(
            env,
            "SHEETS_EDIT_FULL_CELL_REPLACE_ENABLED",
        ),
    }
}

/// 명시적인 맵에서만 설정을 읽는 순수 함수.
pub fn load_app_config(env: &HashMap<String, String>) -> AppConfig {
    let d = ClaudeCliConfig::default();
    let llm_provider = match raw(env, "LLM_PROVIDER") {
        None | Some("claude_cli") => LlmProviderKind::ClaudeCli,
        Some("mock") => LlmProviderKind::Mock,
        Some(_) => {
            tracing::warn!(key = "LLM_PROVIDER", "unknown provider; using claude_cli");
            LlmProviderKind::ClaudeCli
        }
    };
    let clear_every_n_turns = parse_or_warn::<u32>(env, "CLAUDE_CLI_CLEAR_EVERY_N_TURNS")
        .unwrap_or(d.clear_every_n_turns);
    let claude_cli = ClaudeCliConfig {
        cli_path: raw(env, "CLAUDE_CLI_PATH").map_or(d.cli_path, str::to_string),
        model: raw(env, "CLAUDE_CLI_MODEL").map(str::to_string),
        effort: raw(env, "CLAUDE_CLI_EFFORT").map(str::to_string),
        timeout: positive_secs(env, "CLAUDE_CLI_TIMEOUT_SECONDS", d.timeout),
        clear_every_n_turns,
    };
    AppConfig {
        llm_provider,
        claude_cli,
        sheets: load_sheets_config(env),
    }
}

/// `exe_dir/.env`, 다음 `data_dir/.env` 순으로 읽어(먼저 읽은 값과 실제 환경 변수가
/// 이긴다) 프로세스 환경에서 설정을 만든다. 파일이 없으면 무시한다.
pub fn load_app_config_from_environment(exe_dir: &Path, data_dir: &Path) -> AppConfig {
    for dir in [exe_dir, data_dir] {
        let path = dir.join(".env");
        if path.is_file() && dotenvy::from_path(&path).is_err() {
            tracing::warn!(file = ".env", "failed to read .env file");
        }
    }
    let env: HashMap<String, String> = std::env::vars().collect();
    load_app_config(&env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn defaults_when_env_empty() {
        let c = load_app_config(&HashMap::new());
        assert_eq!(c.llm_provider, LlmProviderKind::ClaudeCli);
        assert_eq!(
            (
                c.claude_cli.cli_path.as_str(),
                c.claude_cli.timeout,
                c.claude_cli.clear_every_n_turns
            ),
            ("claude", Duration::from_secs(60), 20)
        );
        assert_eq!(
            (
                c.sheets.batch_max_cells,
                c.sheets.batch_max_chars,
                c.sheets.max_cell_chars,
                c.sheets.max_range_cells
            ),
            (30, 5000, 5000, 200)
        );
        assert_eq!(c.sheets.job_timeout, Duration::from_secs(240));
        assert!(c.sheets.range_replace_enabled && c.sheets.edit_full_cell_replace_enabled);
        assert_eq!(c.claude_cli.model, None);
        assert_eq!(c.claude_cli.effort, None);
    }

    #[test]
    fn empty_map_equals_default_structs() {
        let c = load_app_config(&HashMap::new());
        assert_eq!(
            c.claude_cli,
            ClaudeCliConfig {
                ..Default::default()
            }
        );
        assert_eq!(c.sheets, SheetsConfig::default());
    }

    #[test]
    fn mock_provider_and_empty_model_is_none() {
        let c = load_app_config(&env(&[("LLM_PROVIDER", "mock"), ("CLAUDE_CLI_MODEL", "")]));
        assert_eq!(c.llm_provider, LlmProviderKind::Mock);
        assert_eq!(c.claude_cli.model, None);
    }

    #[test]
    fn claude_cli_overrides() {
        let c = load_app_config(&env(&[
            ("CLAUDE_CLI_PATH", "C:\\tools\\claude.exe"),
            ("CLAUDE_CLI_MODEL", "claude-sonnet-5"),
            ("CLAUDE_CLI_EFFORT", "high"),
            ("CLAUDE_CLI_TIMEOUT_SECONDS", "15"),
            ("CLAUDE_CLI_CLEAR_EVERY_N_TURNS", "5"),
        ]));
        assert_eq!(c.claude_cli.cli_path, "C:\\tools\\claude.exe");
        assert_eq!(c.claude_cli.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(c.claude_cli.effort.as_deref(), Some("high"));
        assert_eq!(c.claude_cli.timeout, Duration::from_secs(15));
        assert_eq!(c.claude_cli.clear_every_n_turns, 5);
    }

    #[test]
    fn invalid_numbers_fall_back_to_defaults() {
        for bad in ["abc", "0", "-5", "", "nan", "inf", "1e999"] {
            let c = load_app_config(&env(&[
                ("CLAUDE_CLI_TIMEOUT_SECONDS", bad),
                ("SHEETS_BATCH_MAX_CELLS", bad),
                ("SHEETS_JOB_TIMEOUT_SECONDS", bad),
            ]));
            assert_eq!(c.claude_cli.timeout, Duration::from_secs(60), "{bad}");
            assert_eq!(c.sheets.batch_max_cells, 30, "{bad}");
            assert_eq!(c.sheets.job_timeout, Duration::from_secs(240), "{bad}");
        }
        let c = load_app_config(&env(&[("SHEETS_BATCH_MAX_CELLS", "-3")]));
        assert_eq!(c.sheets.batch_max_cells, 30);
    }

    #[test]
    fn clear_zero_is_allowed() {
        let c = load_app_config(&env(&[("CLAUDE_CLI_CLEAR_EVERY_N_TURNS", "0")]));
        assert_eq!(c.claude_cli.clear_every_n_turns, 0);
        let c = load_app_config(&env(&[("CLAUDE_CLI_CLEAR_EVERY_N_TURNS", "-1")]));
        assert_eq!(c.claude_cli.clear_every_n_turns, 20);
    }

    #[test]
    fn sheets_env_overrides() {
        let c = load_app_config(&env(&[
            ("SHEETS_BATCH_MAX_CELLS", "7"),
            ("SHEETS_BATCH_MAX_CHARS", "1234"),
            ("SHEETS_MAX_CELL_CHARS", "321"),
            ("SHEETS_MAX_RANGE_CELLS", "99"),
            ("SHEETS_JOB_TIMEOUT_SECONDS", "30"),
        ]));
        assert_eq!(
            (
                c.sheets.batch_max_cells,
                c.sheets.batch_max_chars,
                c.sheets.max_cell_chars,
                c.sheets.max_range_cells
            ),
            (7, 1234, 321, 99)
        );
        assert_eq!(c.sheets.job_timeout, Duration::from_secs(30));
    }

    #[test]
    fn sheets_flags_disable_only_with_zero() {
        let f = |k: &str, v: &str| load_app_config(&env(&[(k, v)])).sheets;
        assert!(!f("SHEETS_RANGE_REPLACE_ENABLED", "0").range_replace_enabled);
        assert!(f("SHEETS_RANGE_REPLACE_ENABLED", "1").range_replace_enabled);
        assert!(f("SHEETS_RANGE_REPLACE_ENABLED", "true").range_replace_enabled);
        assert!(f("SHEETS_RANGE_REPLACE_ENABLED", "").range_replace_enabled);
        assert!(!f("SHEETS_EDIT_FULL_CELL_REPLACE_ENABLED", "0").edit_full_cell_replace_enabled);
        assert!(f("SHEETS_EDIT_FULL_CELL_REPLACE_ENABLED", "yes").edit_full_cell_replace_enabled);
    }

    #[test]
    fn unknown_provider_falls_back_to_claude_cli() {
        let c = load_app_config(&env(&[("LLM_PROVIDER", "bogus")]));
        assert_eq!(c.llm_provider, LlmProviderKind::ClaudeCli);
    }

    #[test]
    fn dotenv_exe_dir_wins_over_data_dir_and_missing_files_ignored() {
        let exe = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(exe.path().join(".env"), "SHEETS_MAX_RANGE_CELLS=11\n").unwrap();
        std::fs::write(
            data.path().join(".env"),
            "SHEETS_MAX_RANGE_CELLS=22\nSHEETS_MAX_CELL_CHARS=33\n",
        )
        .unwrap();
        let c = load_app_config_from_environment(exe.path(), data.path());
        assert_eq!(c.sheets.max_range_cells, 11);
        assert_eq!(c.sheets.max_cell_chars, 33);

        let empty = tempfile::tempdir().unwrap();
        let c = load_app_config_from_environment(empty.path(), &empty.path().join("nope"));
        assert_eq!(c.claude_cli.cli_path, "claude");
    }
}
