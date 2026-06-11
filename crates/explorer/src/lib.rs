mod error;
mod snapshot;

pub use error::ExplorerError;
pub use snapshot::{Entry, EntryKind, Snapshot};

pub type Result<T> = std::result::Result<T, ExplorerError>;
