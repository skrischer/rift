mod diff;
mod error;
mod git;
mod git_write;
mod snapshot;
mod watcher;

pub use diff::{compute as compute_diff, DiffHunk, DiffLine, DiffLineKind, FileDiff};
pub use error::ExplorerError;
pub use git::{AheadBehind, GitEntryStatus, GitStatus, GitStatusCode, RepoState};
pub use git_write::{commit, discard_file, stage_file, unstage_file};
pub use snapshot::{Change, Entry, EntryKind, Snapshot};
pub use watcher::Watcher;

pub type Result<T> = std::result::Result<T, ExplorerError>;
