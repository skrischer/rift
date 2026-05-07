#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal mutex lock poisoned")]
    LockPoisoned,
    #[error("PTY channel closed")]
    PtyChannelClosed,
}
