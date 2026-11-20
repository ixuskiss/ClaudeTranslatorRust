//! Turns `AppConfig` (+ Settings overrides) into a concrete `LlmProvider`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ct_core::{AppConfig, ClaudeCliConfig, LlmProvider, LlmProviderKind};

use crate::catalog::ModelCatalog;
use crate::client::{ClaudeCliClient, CliError};
use crate::locator::{CliLaunch, home_dir, resolve_claude_cli, which_on_path};
use crate::mock::MockProvider;
use crate::models::ModelInfo;
use crate::provider::ClaudeCliProvider;
use crate::spawner::CliSpawner;

/// Settings-dialog values; an empty string means "do not override".
pub struct ProviderOverrides {
    pub cli_path: String,
    pub model: String,
    pub effort: String,
}

pub struct BuiltProvider {
    pub provider: Arc<dyn LlmProvider>,
    pub client: Option<Arc<ClaudeCliClient>>,
}

fn non_empty(over: &str, base: &Option<String>) -> Option<String> {
    if over.is_empty() {
        base.clone()
    } else {
        Some(over.to_string())
    }
}

/// Pure part of the factory: applies overrides (non-empty wins), uses the already
/// resolved CLI launch spec, and drops effort when the chosen model is known to the catalog
/// and supports no effort levels (e.g. haiku).
pub fn resolve_cli_config(
    cfg: &ClaudeCliConfig,
    overrides: &ProviderOverrides,
    models: &[ModelInfo],
    launch: &CliLaunch,
) -> ClaudeCliConfig {
    let model = non_empty(&overrides.model, &cfg.model);
    let mut effort = non_empty(&overrides.effort, &cfg.effort);
    if let Some(m) = model.as_deref().filter(|m| !m.is_empty())
        && models
            .iter()
            .any(|info| info.value == m && info.effort_levels.is_empty())
    {
        effort = None;
    }
    ClaudeCliConfig {
        cli_path: launch.program.to_string_lossy().into_owned(),
        cli_prefix_args: launch.prefix_args.clone(),
        model,
        effort,
        ..cfg.clone()
    }
}

pub fn build_llm_provider(
    cfg: &AppConfig,
    cli_cwd: &Path,
    overrides: &ProviderOverrides,
    models: &[ModelInfo],
    spawner: Arc<dyn CliSpawner>,
) -> BuiltProvider {
    if cfg.llm_provider == LlmProviderKind::Mock {
        return BuiltProvider {
            provider: Arc::new(MockProvider),
            client: None,
        };
    }
    let configured = if overrides.cli_path.is_empty() {
        cfg.claude_cli.cli_path.as_str()
    } else {
        overrides.cli_path.as_str()
    };
    let launch = resolve_claude_cli(configured, &which_on_path, &home_dir());
    // Path only: never log prompt/text content.
    tracing::info!(
        program = %launch.program.display(),
        prefix_args = launch.prefix_args.len(),
        "Claude CLI resolved"
    );
    let config = resolve_cli_config(&cfg.claude_cli, overrides, models, &launch);
    let model = config.model.clone().unwrap_or_default();
    let client = Arc::new(ClaudeCliClient::new(
        config,
        Some(cli_cwd.to_path_buf()),
        spawner,
    ));
    BuiltProvider {
        provider: Arc::new(ClaudeCliProvider::new(client.clone(), model)),
        client: Some(client),
    }
}

/// Background-startup step: boots the CLI via `list_models` (which starts the process
/// itself; a separate warm_up would let a real turn slip in before `initialize`) and
/// refreshes the catalog. No-op for the mock provider.
pub fn refresh_catalog_and_warm_up(
    built: &BuiltProvider,
    catalog: &ModelCatalog,
) -> RefreshOutcome {
    let Some(client) = &built.client else {
        return RefreshOutcome::Skipped;
    };
    let mut spawn_failed = false;
    let refreshed = catalog.refresh(|| {
        let result = client.list_models(Duration::from_secs(10));
        spawn_failed = matches!(result, Err(CliError::Spawn(_)));
        result
    });
    if refreshed {
        RefreshOutcome::Refreshed
    } else if spawn_failed {
        RefreshOutcome::SpawnFailed
    } else {
        RefreshOutcome::Failed
    }
}

/// Result of [`refresh_catalog_and_warm_up`]; catalog semantics are unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Mock provider: nothing to do.
    Skipped,
    Refreshed,
    /// Failed for a reason other than the process not starting (or returned no models).
    Failed,
    /// The CLI process could not be started (`CliError::Spawn`).
    SpawnFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::fallback_models;
    use crate::client::build_command;
    use crate::spawner::SpawnedCli;
    use ct_core::LlmProviderKind;
    use std::io;
    use std::path::{Path, PathBuf};

    struct NoSpawn;
    impl CliSpawner for NoSpawn {
        fn spawn(&self, _: &[String], _: Option<&Path>) -> io::Result<SpawnedCli> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no cli"))
        }
    }

    fn app(kind: LlmProviderKind) -> AppConfig {
        AppConfig {
            llm_provider: kind,
            claude_cli: ClaudeCliConfig::default(),
            sheets: ct_core::SheetsConfig::default(),
        }
    }
    fn cli(f: impl FnOnce(&mut ClaudeCliConfig)) -> ClaudeCliConfig {
        let mut c = ClaudeCliConfig::default();
        f(&mut c);
        c
    }
    fn ov(path: &str, model: &str, effort: &str) -> ProviderOverrides {
        ProviderOverrides {
            cli_path: path.into(),
            model: model.into(),
            effort: effort.into(),
        }
    }
    fn resolve(c: &ClaudeCliConfig, o: &ProviderOverrides) -> ClaudeCliConfig {
        resolve_cli_config(
            c,
            o,
            &fallback_models(),
            &CliLaunch {
                program: PathBuf::from("claude"),
                prefix_args: Vec::new(),
            },
        )
    }
    fn arg_after(cmd: &[String], flag: &str) -> Option<String> {
        cmd.iter()
            .position(|a| a == flag)
            .and_then(|i| cmd.get(i + 1))
            .cloned()
    }

    #[test]
    fn factory_mock_kind_has_no_client() {
        let cfg = app(LlmProviderKind::Mock);
        let b = build_llm_provider(
            &cfg,
            Path::new("."),
            &ov("", "", ""),
            &[],
            Arc::new(NoSpawn),
        );
        assert!(b.client.is_none());
    }

    #[test]
    fn claude_cli_kind_has_client() {
        let cfg = app(LlmProviderKind::ClaudeCli);
        let b = build_llm_provider(
            &cfg,
            Path::new("."),
            &ov(r"C:\x\claude.exe", "", ""),
            &[],
            Arc::new(NoSpawn),
        );
        assert!(b.client.is_some());
    }

    #[test]
    fn refresh_reports_spawn_failure_and_keeps_catalog() {
        let cfg = app(LlmProviderKind::ClaudeCli);
        let built = build_llm_provider(
            &cfg,
            Path::new("."),
            &ov(r"C:\x\claude.exe", "", ""),
            &[],
            Arc::new(NoSpawn),
        );
        let catalog = ModelCatalog::new(None);
        let before = catalog.get();
        assert_eq!(
            refresh_catalog_and_warm_up(&built, &catalog),
            RefreshOutcome::SpawnFailed
        );
        assert_eq!(catalog.get(), before);
        assert_eq!(catalog.source(), crate::catalog::CatalogSource::Fallback);
    }

    #[test]
    fn refresh_is_skipped_for_mock_provider() {
        let built = build_llm_provider(
            &app(LlmProviderKind::Mock),
            Path::new("."),
            &ov("", "", ""),
            &[],
            Arc::new(NoSpawn),
        );
        assert_eq!(
            refresh_catalog_and_warm_up(&built, &ModelCatalog::new(None)),
            RefreshOutcome::Skipped
        );
    }

    #[test]
    fn cli_path_override_takes_precedence() {
        let c = resolve_cli_config(
            &ClaudeCliConfig::default(),
            &ov(r"C:\override\claude.exe", "", ""),
            &[],
            &CliLaunch {
                program: PathBuf::from(r"C:\override\claude.exe"),
                prefix_args: vec!["cli.js".to_string()],
            },
        );
        assert_eq!(c.cli_path, r"C:\override\claude.exe");
        assert_eq!(c.cli_prefix_args, ["cli.js"]);
    }

    #[test]
    fn factory_override_precedence() {
        let env = cli(|c| c.model = Some("haiku".into()));
        assert_eq!(
            resolve(&env, &ov("", "opus", "")).model.as_deref(),
            Some("opus")
        );
        assert_eq!(
            resolve(&env, &ov("", "", "")).model.as_deref(),
            Some("haiku")
        );
    }

    #[test]
    fn effort_override_takes_precedence_and_empty_falls_back() {
        let env = cli(|c| c.effort = Some("low".into()));
        assert_eq!(
            resolve(&env, &ov("", "", "high")).effort.as_deref(),
            Some("high")
        );
        assert_eq!(
            resolve(&env, &ov("", "", "")).effort.as_deref(),
            Some("low")
        );
    }

    #[test]
    fn factory_drops_effort_for_model_without_effort_levels() {
        let env = cli(|c| c.effort = Some("low".into()));
        let c = resolve(&env, &ov("", "haiku", ""));
        assert_eq!(c.effort, None);
        assert_eq!(arg_after(&build_command(&c), "--effort"), None);
        let c = resolve(&env, &ov("", "opus", ""));
        assert_eq!(
            arg_after(&build_command(&c), "--effort").as_deref(),
            Some("low")
        );
    }

    #[test]
    fn effort_kept_when_model_unknown_to_catalog() {
        let env = cli(|c| c.effort = Some("low".into()));
        let c = resolve(&env, &ov("", "claude-x", ""));
        assert_eq!(c.effort.as_deref(), Some("low"));
    }
}
