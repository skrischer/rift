#[derive(Debug, thiserror::Error)]
pub enum ExplorerError {
    #[error("path not found: {0}")]
    PathNotFound(String),
    #[error("scan error: {0}")]
    ScanError(String),
    #[error("watch error: {0}")]
    WatchError(String),
    #[error("git error: {0}")]
    GitError(String),
}
