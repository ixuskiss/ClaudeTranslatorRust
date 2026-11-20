//! Claude CLI client errors (the client itself is added in a later task).

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CliError {
    #[error("Claude CLI timed out")]
    Timeout,
    #[error("Claude CLI process ended unexpectedly")]
    ProcessEnded,
    #[error("failed to start Claude CLI: {0}")]
    Spawn(String),
    #[error("Claude CLI I/O error: {0}")]
    Io(String),
    #[error("Claude CLI rejected the request: {0}")]
    Rejected(String),
}
