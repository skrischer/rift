#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("failed to parse control mode event: {0}")]
    ParseError(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
}
