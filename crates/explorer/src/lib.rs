mod error;
mod snapshot;
mod watcher;

pub use error::ExplorerError;
pub use snapshot::{Change, Entry, EntryKind, Snapshot};
pub use watcher::Watcher;

pub type Result<T> = std::result::Result<T, ExplorerError>;
