#[derive(Debug, thiserror::Error)]
pub enum ExplorerError {
    #[error("path not found: {0}")]
    PathNotFound(String),
    #[error("watch error: {0}")]
    WatchError(String),
}
