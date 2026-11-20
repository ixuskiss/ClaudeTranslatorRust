#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod client;
pub mod prompts;
pub mod provider;

pub use client::CliError;
pub use provider::{ClaudeCliProvider, CliTransport};
