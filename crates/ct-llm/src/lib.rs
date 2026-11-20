#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod client;
pub mod models;
pub mod prompts;
pub mod provider;
pub mod spawner;

pub use client::{ClaudeCliClient, CliError, build_command};
pub use models::{LIST_MODELS_REQUEST_ID, ModelInfo, parse_model_info};
pub use provider::{ClaudeCliProvider, CliTransport};
pub use spawner::{ChildHandle, CliSpawner, RealSpawner, SpawnedCli};
