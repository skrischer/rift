//! The daemon's diff service: answers a `RequestDiff` by computing `path`'s
//! structured line diff (current on-disk content vs its blob at HEAD),
//! confined to the watched worktree root.
//!
//! This is the daemon side of the source-control diff channel
//! (`docs/spec-source-control.md`, `docs/protocol.md`): the client pulls a
//! diff for the file currently under review ([`compute`]), the same
//! request/response shape as the buffer channel's `OpenFile`. There is no
//! out-of-root carve-out here — every diff target is a worktree entry — so
//! the path is confined exactly as a buffer **write** is confined
//! ([`buffer::resolve`]).
//!
//! [`rift_explorer::compute_diff`] performs no path-traversal validation
//! itself (it trusts its caller to hand it a path already confined to
//! `root`), so this module's confinement check is load-bearing, not
//! defense-in-depth.

use std::fmt;
use std::path::{Path, PathBuf};

use rift_explorer::FileDiff;

use crate::buffer::{self, BufferError};

/// Why a diff request was refused or could not be computed.
#[derive(Debug)]
pub enum DiffError {
    /// The requested path escaped the worktree root — see [`buffer::resolve`].
    InvalidPath(BufferError),
    /// `rift_explorer::compute_diff` failed to read the repository or its
    /// objects.
    Explorer(rift_explorer::ExplorerError),
    /// The blocking compute task panicked. Not expected in practice (the
    /// compute's own internal invariants are asserted, not this-can-happen
    /// error paths), but the join is still fallible, so it is a variant
    /// rather than an `.unwrap()`.
    Join(tokio::task::JoinError),
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffError::InvalidPath(err) => write!(f, "invalid diff path: {err}"),
            DiffError::Explorer(err) => write!(f, "diff compute failed: {err}"),
            DiffError::Join(err) => write!(f, "diff compute task failed: {err}"),
        }
    }
}

impl std::error::Error for DiffError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DiffError::InvalidPath(err) => Some(err),
            DiffError::Explorer(err) => Some(err),
            DiffError::Join(err) => Some(err),
        }
    }
}

/// Compute `path`'s diff against HEAD, confined to `root`.
///
/// `path` is confined exactly as a buffer write is ([`buffer::resolve`]):
/// `..` and symlink escapes are refused before any repository I/O runs. The
/// compute itself is CPU-bound (`gix`'s blob diff over both sides,
/// `imara-diff`), so it runs on a blocking thread via `spawn_blocking`,
/// keeping the dispatch loop's async I/O path unblocked.
pub async fn compute(root: &Path, path: &str) -> Result<FileDiff, DiffError> {
    // Confinement only — `compute_diff` re-joins `root` with the relative
    // path itself, so the resolved absolute path is not otherwise used.
    buffer::resolve(root, path).map_err(DiffError::InvalidPath)?;

    let root = root.to_path_buf();
    let relative = PathBuf::from(path);
    tokio::task::spawn_blocking(move || rift_explorer::compute_diff(&root, &relative))
        .await
        .map_err(DiffError::Join)?
        .map_err(DiffError::Explorer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A self-cleaning temp dir, mirroring `buffer.rs`'s test helper.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("rift-daemon-diff-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            // Canonicalize so the confinement check (which compares against the
            // canonical root) matches — the system temp dir is a symlink on some
            // setups.
            let path = path.canonicalize().expect("canonicalize temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("run git");
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    fn init_repo(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        write(&tmp.path.join("tracked.txt"), b"one\ntwo\nthree\n");
        git(&tmp.path, &["add", "tracked.txt"]);
        git(&tmp.path, &["commit", "-q", "-m", "init"]);
        tmp
    }

    #[tokio::test]
    async fn test_compute_modified_file_returns_hunks() {
        let repo = init_repo("modified");
        write(&repo.path.join("tracked.txt"), b"one\nTWO\nthree\n");

        let diff = compute(&repo.path, "tracked.txt")
            .await
            .expect("compute succeeds");
        match diff {
            FileDiff::Hunks(hunks) => assert_eq!(hunks.len(), 1),
            other => panic!("expected Hunks, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_compute_untracked_file_diffs_against_empty_head() {
        let repo = init_repo("untracked");
        write(&repo.path.join("new.txt"), b"fresh\n");

        let diff = compute(&repo.path, "new.txt")
            .await
            .expect("compute succeeds");
        match diff {
            FileDiff::Hunks(hunks) => {
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].old_len, 0, "untracked file has no old-side lines");
            }
            other => panic!("expected Hunks, got {other:?}"),
        }
    }

    /// A path with no changes vs HEAD returns an empty hunk list, not an
    /// error or a sentinel — the acceptance-level "no changes" case.
    #[tokio::test]
    async fn test_compute_unchanged_file_returns_empty_hunks() {
        let repo = init_repo("unchanged");

        let diff = compute(&repo.path, "tracked.txt")
            .await
            .expect("compute succeeds");
        match diff {
            FileDiff::Hunks(hunks) => assert!(hunks.is_empty()),
            other => panic!("expected empty Hunks, got {other:?}"),
        }
    }

    /// A path with no worktree entry at all (never committed, never present on
    /// disk) diffs empty-vs-empty rather than erroring — the acceptance-level
    /// "not in the repo" case.
    #[tokio::test]
    async fn test_compute_missing_path_returns_empty_hunks() {
        let repo = init_repo("missing");

        let diff = compute(&repo.path, "nope.txt")
            .await
            .expect("compute succeeds");
        match diff {
            FileDiff::Hunks(hunks) => assert!(hunks.is_empty()),
            other => panic!("expected empty Hunks, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_compute_rejects_parent_dir_escape() {
        let tmp = TempDir::new("escape-parent");
        let outside = tmp.path.parent().expect("temp has a parent");
        let secret = outside.join("rift-diff-secret.txt");
        std::fs::write(&secret, b"top secret").expect("write secret");

        let err = compute(&tmp.path, "../rift-diff-secret.txt")
            .await
            .expect_err("parent escape is refused");
        assert!(matches!(err, DiffError::InvalidPath(_)), "got {err:?}");

        let _ = std::fs::remove_file(&secret);
    }

    #[tokio::test]
    async fn test_compute_rejects_absolute_path() {
        let repo = init_repo("escape-abs");

        let err = compute(&repo.path, "/etc/passwd")
            .await
            .expect_err("absolute path is refused");
        assert!(matches!(err, DiffError::InvalidPath(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_compute_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new("escape-symlink");
        let outside = tmp.path.parent().expect("temp has a parent");
        let secret = outside.join("rift-diff-symlink-secret.txt");
        std::fs::write(&secret, b"top secret").expect("write secret");
        symlink(&secret, tmp.path.join("link.txt")).expect("create symlink");

        let err = compute(&tmp.path, "link.txt")
            .await
            .expect_err("symlink escape is refused");
        assert!(matches!(err, DiffError::InvalidPath(_)), "got {err:?}");

        let _ = std::fs::remove_file(&secret);
    }
}
