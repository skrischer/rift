#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("invalid escape sequence at byte {position}")]
    InvalidEscapeSequence { position: usize },
}
