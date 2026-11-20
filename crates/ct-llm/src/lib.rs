#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod catalog;
pub mod client;
pub mod factory;
pub mod locator;
pub mod mock;
pub mod models;
pub mod prompts;
pub mod provider;
pub mod spawner;

pub use catalog::{CatalogSource, ListenerId, ModelCatalog, fallback_models};
pub use client::{ClaudeCliClient, CliError, build_command};
pub use factory::{
    BuiltProvider, ProviderOverrides, build_llm_provider, refresh_catalog_and_warm_up,
    resolve_cli_config,
};
pub use locator::resolve_claude_cli_path;
pub use mock::MockProvider;
pub use models::{LIST_MODELS_REQUEST_ID, ModelInfo, parse_model_info};
pub use provider::{ClaudeCliProvider, CliTransport};
pub use spawner::{ChildHandle, CliSpawner, RealSpawner, SpawnedCli};
