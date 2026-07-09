//! The daemon's file-operation write service: applies a create / rename /
//! delete request and answers with exactly one
//! [`DaemonMessage::FileOpResult`], confined to the watched worktree root.
//!
//! This is the daemon side of the explorer file-operation channel
//! (`docs/spec-explorer-file-ops.md`, `docs/protocol.md`): the four ops are
//! per-connection request/response, the same shape as the buffer/diff/
//! git-write channels (`serve_connection` writes the reply straight back to
//! the requesting socket). The reply carries only success or a typed error —
//! the resulting tree change is never echoed here; it arrives through the
//! existing push-only `UpdateWorktree` recompute the worktree watcher already
//! triggers on the same fs event the op causes, keeping one source of truth
//! for tree structure (mirrors [`crate::git_write`]'s `GitOpResult`
//! contract).
//!
//! Every path is confined with [`buffer::resolve`] — the same resolver a
//! buffer **write** uses — before any mutation; a rename confines both `from`
//! and `to`. Create and rename refuse an existing target with an explicit
//! up-front existence check (deterministic, not `ErrorKind`-dependent); no op
//! ever overwrites content. The fs mutation itself runs on
//! `tokio::task::spawn_blocking` (disk-bound work), after the `State` borrow
//! has been released.

use std::path::PathBuf;

use rift_protocol::{ClientMessage, DaemonMessage, FileOp, FileOpError};
use tokio::sync::watch;
use tracing::warn;

use crate::buffer;
use crate::State;

/// Apply the file op in `msg` against the watched worktree root and produce
/// its [`DaemonMessage::FileOpResult`]. Called only with the file-operation
/// requests (`CreateFile` / `CreateDir` / `RenamePath` / `DeletePath`); any
/// other variant is answered as a failed op rather than a panic so a stray
/// message can never take the connection down.
pub(crate) async fn reply(state: &watch::Receiver<State>, msg: ClientMessage) -> DaemonMessage {
    // The borrow is released before any `await`: the canonical root is cloned
    // out up front, then the fs I/O runs unborrowed (like `git_write::reply`).
    let root = state
        .borrow()
        .worktree
        .as_ref()
        .map(|snapshot| snapshot.root().to_path_buf());
    let Some(root) = root else {
        // No worktree scanned yet: there is no root to confine to. Answer with
        // a clean failure so the client's op resolves instead of hanging.
        warn!("file op before the worktree is ready");
        return failed(op_echo(&msg), FileOpError::Io);
    };

    match msg {
        ClientMessage::CreateFile { path } => {
            let op = FileOp::CreateFile { path: path.clone() };
            finish(op, create_file(root, path).await)
        }
        ClientMessage::CreateDir { path } => {
            let op = FileOp::CreateDir { path: path.clone() };
            finish(op, create_dir(root, path).await)
        }
        ClientMessage::RenamePath { from, to } => {
            let op = FileOp::Rename {
                from: from.clone(),
                to: to.clone(),
            };
            finish(op, rename_path(root, from, to).await)
        }
        ClientMessage::DeletePath { path } => {
            let op = FileOp::Delete { path: path.clone() };
            finish(op, delete_path(root, path).await)
        }
        other => failed(op_echo(&other), FileOpError::Io),
    }
}

/// Create an empty regular file at `path` (confined to `root`): missing
/// parent directories are created first, then the file. An up-front
/// existence check refuses an already-occupied target with
/// [`FileOpError::AlreadyExists`] before anything is touched; the actual
/// creation still uses `create_new` so a race loses to the same error rather
/// than clobbering.
async fn create_file(root: PathBuf, path: String) -> Result<(), FileOpError> {
    let resolved = buffer::resolve(&root, &path).map_err(resolve_error)?;
    tokio::task::spawn_blocking(move || {
        if resolved.exists() {
            return Err(FileOpError::AlreadyExists);
        }
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&resolved)
            .map(|_| ())
            .map_err(io_error)
    })
    .await
    .map_err(|_| FileOpError::Io)?
}

/// Create a directory (and missing intermediates) at `path` (confined to
/// `root`). An up-front existence check refuses an already-occupied target
/// (file or directory) with [`FileOpError::AlreadyExists`].
async fn create_dir(root: PathBuf, path: String) -> Result<(), FileOpError> {
    let resolved = buffer::resolve(&root, &path).map_err(resolve_error)?;
    tokio::task::spawn_blocking(move || {
        if resolved.exists() {
            return Err(FileOpError::AlreadyExists);
        }
        std::fs::create_dir_all(&resolved).map_err(io_error)
    })
    .await
    .map_err(|_| FileOpError::Io)?
}

/// Rename/move `from` to `to` (both confined to `root`): missing parent
/// directories of `to` are created first, then `fs::rename`. Up-front
/// existence checks refuse a missing source with [`FileOpError::NotFound`]
/// and an existing target with [`FileOpError::AlreadyExists`] — no clobber.
async fn rename_path(root: PathBuf, from: String, to: String) -> Result<(), FileOpError> {
    let resolved_from = buffer::resolve(&root, &from).map_err(resolve_error)?;
    let resolved_to = buffer::resolve(&root, &to).map_err(resolve_error)?;
    tokio::task::spawn_blocking(move || {
        if !resolved_from.exists() {
            return Err(FileOpError::NotFound);
        }
        if resolved_to.exists() {
            return Err(FileOpError::AlreadyExists);
        }
        if let Some(parent) = resolved_to.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(&resolved_from, &resolved_to).map_err(io_error)
    })
    .await
    .map_err(|_| FileOpError::Io)?
}

/// Delete `path` (confined to `root`): a file via `remove_file`, a directory
/// **recursively** via `remove_dir_all`.
async fn delete_path(root: PathBuf, path: String) -> Result<(), FileOpError> {
    let resolved = buffer::resolve(&root, &path).map_err(resolve_error)?;
    tokio::task::spawn_blocking(move || {
        let metadata = std::fs::metadata(&resolved).map_err(io_error)?;
        if metadata.is_dir() {
            std::fs::remove_dir_all(&resolved).map_err(io_error)
        } else {
            std::fs::remove_file(&resolved).map_err(io_error)
        }
    })
    .await
    .map_err(|_| FileOpError::Io)?
}

/// Map a [`buffer::BufferError`] from [`buffer::resolve`] onto the wire
/// [`FileOpError`]: a path escaping the root maps to [`FileOpError::InvalidPath`];
/// any other (e.g. a permission failure while canonicalizing) falls back to
/// [`io_error`]'s `std::io::ErrorKind` mapping, matching `crate::buffer_error_reason`'s
/// refinement.
fn resolve_error(err: buffer::BufferError) -> FileOpError {
    match err {
        buffer::BufferError::PathEscape(_) => FileOpError::InvalidPath,
        buffer::BufferError::Io { source, .. } => io_error(source),
        // `resolve` never produces these — they are read-path variants — but
        // handled rather than panicking, per the "no stray message can take
        // the connection down" discipline this module follows throughout.
        buffer::BufferError::NotUtf8(_) | buffer::BufferError::TooLarge(_) => FileOpError::Io,
    }
}

/// Map a `std::io::Error` onto the wire [`FileOpError`] by its
/// [`std::io::ErrorKind`], the same refinement `crate::buffer_error_reason`
/// applies to a buffer I/O failure.
fn io_error(source: std::io::Error) -> FileOpError {
    match source.kind() {
        std::io::ErrorKind::AlreadyExists => FileOpError::AlreadyExists,
        std::io::ErrorKind::NotFound => FileOpError::NotFound,
        std::io::ErrorKind::PermissionDenied => FileOpError::PermissionDenied,
        _ => FileOpError::Io,
    }
}

/// Build the reply for a completed op, logging the reason on failure.
fn finish(op: FileOp, result: Result<(), FileOpError>) -> DaemonMessage {
    match result {
        Ok(()) => DaemonMessage::FileOpResult {
            op,
            ok: true,
            error: None,
        },
        Err(error) => {
            warn!(?op, ?error, "file op failed");
            failed(op, error)
        }
    }
}

/// A failed reply with `error` and no state change.
fn failed(op: FileOp, error: FileOpError) -> DaemonMessage {
    DaemonMessage::FileOpResult {
        op,
        ok: false,
        error: Some(error),
    }
}

/// The [`FileOp`] echo for a request — used only for the failure paths that
/// answer before dispatching (worktree-not-ready, unsupported variant).
fn op_echo(msg: &ClientMessage) -> FileOp {
    match msg {
        ClientMessage::CreateFile { path } => FileOp::CreateFile { path: path.clone() },
        ClientMessage::CreateDir { path } => FileOp::CreateDir { path: path.clone() },
        ClientMessage::RenamePath { from, to } => FileOp::Rename {
            from: from.clone(),
            to: to.clone(),
        },
        ClientMessage::DeletePath { path } => FileOp::Delete { path: path.clone() },
        _ => FileOp::Delete {
            path: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rift-daemon-fileops-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp root");
            // Canonicalize so the confinement checks (which compare against
            // the canonical root) match — the system temp dir is a symlink on
            // macOS and some Linux setups.
            let path = path.canonicalize().expect("canonicalize temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// A `State` receiver whose worktree root is `root`, so `reply` can
    /// resolve and confine paths against it. No git repository is needed —
    /// file ops are plain `std::fs`.
    fn state_for(root: &Path) -> watch::Receiver<State> {
        let state = State {
            worktree: Some(rift_explorer::Snapshot::scan(root).expect("scan")),
            ..State::default()
        };
        let (_tx, rx) = watch::channel(state);
        rx
    }

    fn assert_ok(msg: DaemonMessage, expected: FileOp) {
        match msg {
            DaemonMessage::FileOpResult { op, ok, error } => {
                assert_eq!(op, expected, "echoed op");
                assert!(ok, "op should succeed, error: {error:?}");
                assert!(error.is_none());
            }
            other => panic!("expected FileOpResult, got {other:?}"),
        }
    }

    fn assert_failed(msg: DaemonMessage, expected_error: FileOpError) {
        match msg {
            DaemonMessage::FileOpResult { ok, error, .. } => {
                assert!(!ok, "op should have failed");
                assert_eq!(error, Some(expected_error));
            }
            other => panic!("expected FileOpResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_create_file_creates_missing_parents_and_replies_ok() {
        let tmp = TempDir::new("create-file-ok");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::CreateFile {
                path: "src/new.rs".into(),
            },
        )
        .await;
        assert_ok(
            reply,
            FileOp::CreateFile {
                path: "src/new.rs".into(),
            },
        );
        let created = tmp.path.join("src/new.rs");
        assert!(created.is_file(), "file was created");
        assert_eq!(std::fs::read(&created).expect("read"), b"");
    }

    #[tokio::test]
    async fn test_create_file_existing_target_replies_already_exists_and_leaves_untouched() {
        let tmp = TempDir::new("create-file-exists");
        std::fs::write(tmp.path.join("taken.txt"), b"original").expect("seed file");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::CreateFile {
                path: "taken.txt".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::AlreadyExists);
        assert_eq!(
            std::fs::read(tmp.path.join("taken.txt")).expect("read back"),
            b"original",
            "a refused create must not touch the existing target"
        );
    }

    #[tokio::test]
    async fn test_create_dir_creates_directory_and_replies_ok() {
        let tmp = TempDir::new("create-dir-ok");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::CreateDir {
                path: "a/b/c".into(),
            },
        )
        .await;
        assert_ok(
            reply,
            FileOp::CreateDir {
                path: "a/b/c".into(),
            },
        );
        assert!(tmp.path.join("a/b/c").is_dir());
    }

    #[tokio::test]
    async fn test_create_dir_existing_target_replies_already_exists() {
        let tmp = TempDir::new("create-dir-exists");
        std::fs::create_dir_all(tmp.path.join("existing")).expect("seed dir");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::CreateDir {
                path: "existing".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::AlreadyExists);
    }

    #[tokio::test]
    async fn test_rename_path_moves_file_and_replies_ok() {
        let tmp = TempDir::new("rename-ok");
        std::fs::write(tmp.path.join("old.txt"), b"content").expect("seed file");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::RenamePath {
                from: "old.txt".into(),
                to: "moved/new.txt".into(),
            },
        )
        .await;
        assert_ok(
            reply,
            FileOp::Rename {
                from: "old.txt".into(),
                to: "moved/new.txt".into(),
            },
        );
        assert!(!tmp.path.join("old.txt").exists());
        assert_eq!(
            std::fs::read(tmp.path.join("moved/new.txt")).expect("read back"),
            b"content"
        );
    }

    #[tokio::test]
    async fn test_rename_path_existing_target_replies_already_exists_and_leaves_both_untouched() {
        let tmp = TempDir::new("rename-clobber");
        std::fs::write(tmp.path.join("from.txt"), b"source").expect("seed from");
        std::fs::write(tmp.path.join("to.txt"), b"target").expect("seed to");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::RenamePath {
                from: "from.txt".into(),
                to: "to.txt".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::AlreadyExists);
        assert_eq!(
            std::fs::read(tmp.path.join("from.txt")).expect("read back"),
            b"source"
        );
        assert_eq!(
            std::fs::read(tmp.path.join("to.txt")).expect("read back"),
            b"target"
        );
    }

    #[tokio::test]
    async fn test_rename_path_missing_source_replies_not_found() {
        let tmp = TempDir::new("rename-missing");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::RenamePath {
                from: "nope.txt".into(),
                to: "also-nope.txt".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::NotFound);
    }

    #[tokio::test]
    async fn test_delete_path_removes_file() {
        let tmp = TempDir::new("delete-file");
        std::fs::write(tmp.path.join("gone.txt"), b"bye").expect("seed file");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::DeletePath {
                path: "gone.txt".into(),
            },
        )
        .await;
        assert_ok(
            reply,
            FileOp::Delete {
                path: "gone.txt".into(),
            },
        );
        assert!(!tmp.path.join("gone.txt").exists());
    }

    #[tokio::test]
    async fn test_delete_path_removes_directory_recursively() {
        let tmp = TempDir::new("delete-dir");
        std::fs::create_dir_all(tmp.path.join("tree/nested")).expect("seed dir");
        std::fs::write(tmp.path.join("tree/nested/leaf.txt"), b"leaf").expect("seed file");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::DeletePath {
                path: "tree".into(),
            },
        )
        .await;
        assert_ok(
            reply,
            FileOp::Delete {
                path: "tree".into(),
            },
        );
        assert!(!tmp.path.join("tree").exists());
    }

    #[tokio::test]
    async fn test_create_file_path_escape_replies_invalid_path_and_touches_nothing_outside_root() {
        let tmp = TempDir::new("escape-create");
        let outside = tmp.path.parent().expect("temp has a parent");
        let victim = outside.join("rift-fileops-escape-victim.txt");
        let _ = std::fs::remove_file(&victim);
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::CreateFile {
                path: "../rift-fileops-escape-victim.txt".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::InvalidPath);
        assert!(
            !victim.exists(),
            "a refused escaping create must not touch anything outside the root"
        );
    }

    #[tokio::test]
    async fn test_rename_path_to_escape_replies_invalid_path() {
        let tmp = TempDir::new("escape-rename-to");
        std::fs::write(tmp.path.join("in-root.txt"), b"stay").expect("seed file");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::RenamePath {
                from: "in-root.txt".into(),
                to: "../rift-fileops-escape-rename.txt".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::InvalidPath);
        assert_eq!(
            std::fs::read(tmp.path.join("in-root.txt")).expect("read back"),
            b"stay",
            "a refused escaping rename must leave the source untouched"
        );
    }

    #[tokio::test]
    async fn test_delete_path_escape_replies_invalid_path() {
        let tmp = TempDir::new("escape-delete");
        let state = state_for(&tmp.path);

        let reply = reply(
            &state,
            ClientMessage::DeletePath {
                path: "../rift-fileops-escape-delete.txt".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::InvalidPath);
    }

    #[tokio::test]
    async fn test_reply_before_worktree_ready_fails_cleanly() {
        let (_tx, rx) = watch::channel(State::default());
        let reply = reply(
            &rx,
            ClientMessage::CreateFile {
                path: "new.txt".into(),
            },
        )
        .await;
        assert_failed(reply, FileOpError::Io);
    }
}
