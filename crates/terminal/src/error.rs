// Used in smol::unblock closures where a panic would poison the thread pool.
// Synchronous GPUI callbacks use .expect() instead — poison there is unrecoverable.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal mutex lock poisoned")]
    LockPoisoned,
}
