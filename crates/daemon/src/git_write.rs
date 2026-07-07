//! The daemon's source-control write service: applies a stage / unstage /
//! discard / commit request and answers with exactly one
//! [`DaemonMessage::GitOpResult`], confined to the watched worktree root.
//!
//! This is the daemon side of the source-control write channel
//! (`docs/spec-source-control-write.md`, `docs/protocol.md`): the write ops are
//! per-connection request/response, the same shape as the buffer/diff channels
//! (`serve_connection` writes the reply straight back to the requesting socket).
//! The reply carries only success or a human-readable error — the resulting
//! state change (staged/unstaged status, line totals, ahead counter) is never
//! echoed here; it arrives through the existing push-only git recompute the
//! `.git/index` watcher already triggers, keeping one source of truth for git
//! state.
//!
//! [`rift_explorer`]'s write ops perform no path-traversal validation themselves
//! (they trust their caller to hand a path already confined to `root`), so the
//! file-level ops confine the path here exactly as a buffer **write** is
//! ([`buffer::resolve`]). [`GitWriteOp::Commit`] carries no path.

use std::path::PathBuf;

use rift_protocol::{ClientMessage, DaemonMessage, GitWriteOp};
use tokio::sync::watch;
use tracing::warn;

use crate::buffer;
use crate::State;

/// Which file-level op to run on the blocking thread.
enum FileOp {
    Stage,
    Unstage,
    Discard,
}

/// Apply the write op in `msg` against the watched worktree root and produce its
/// [`DaemonMessage::GitOpResult`]. Called only with the four file-level write
/// ops (`StageFile` / `UnstageFile` / `DiscardFile` / `Commit`); any other
/// variant is answered as a failed op rather than a panic so a stray message can
/// never take the connection down.
pub(crate) async fn reply(state: &watch::Receiver<State>, msg: ClientMessage) -> DaemonMessage {
    // The borrow is released before any `await`: the canonical root is cloned
    // out up front, then the git I/O runs unborrowed (like `request_reply`).
    let root = state
        .borrow()
        .worktree
        .as_ref()
        .map(|snapshot| snapshot.root().to_path_buf());
    let Some(root) = root else {
        // No worktree scanned yet: there is no root to confine to. Answer with a
        // clean failure so the client's op resolves instead of hanging.
        warn!("git write op before the worktree is ready");
        return failed(op_echo(&msg), "worktree not ready".to_string());
    };

    match msg {
        ClientMessage::StageFile { path } => {
            let op = GitWriteOp::StageFile { path: path.clone() };
            finish(op, run_file(root, path, FileOp::Stage).await)
        }
        ClientMessage::UnstageFile { path } => {
            let op = GitWriteOp::UnstageFile { path: path.clone() };
            finish(op, run_file(root, path, FileOp::Unstage).await)
        }
        ClientMessage::DiscardFile { path } => {
            let op = GitWriteOp::DiscardFile { path: path.clone() };
            finish(op, run_file(root, path, FileOp::Discard).await)
        }
        ClientMessage::Commit { message } => {
            finish(GitWriteOp::Commit, run_commit(root, message).await)
        }
        other => failed(op_echo(&other), "unsupported write op".to_string()),
    }
}

/// Confine `path` to `root`, then run the file-level op on a blocking thread
/// (the `gix` index mutation is CPU/disk-bound). Returns the error string on
/// refusal or failure.
async fn run_file(root: PathBuf, path: String, op: FileOp) -> Result<(), String> {
    // Confinement only — the explorer op re-joins `root` with the relative path
    // itself, so the resolved absolute path is not otherwise used.
    if let Err(err) = buffer::resolve(&root, &path) {
        return Err(format!("invalid path: {err}"));
    }
    let relative = PathBuf::from(&path);
    tokio::task::spawn_blocking(move || match op {
        FileOp::Stage => rift_explorer::stage_file(&root, &relative),
        FileOp::Unstage => rift_explorer::unstage_file(&root, &relative),
        FileOp::Discard => rift_explorer::discard_file(&root, &relative),
    })
    .await
    .map_err(|e| format!("git op task failed: {e}"))?
    .map_err(|e| e.to_string())
}

/// Run the commit on a blocking thread. Commit carries no path, so there is
/// nothing to confine.
async fn run_commit(root: PathBuf, message: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || rift_explorer::commit(&root, &message))
        .await
        .map_err(|e| format!("commit task failed: {e}"))?
        .map_err(|e| e.to_string())
}

/// Build the reply for a completed op, logging the reason on failure.
fn finish(op: GitWriteOp, result: Result<(), String>) -> DaemonMessage {
    match result {
        Ok(()) => DaemonMessage::GitOpResult {
            op,
            ok: true,
            error: None,
        },
        Err(error) => {
            warn!(?op, %error, "git write op failed");
            failed(op, error)
        }
    }
}

/// A failed reply with `error` and no state change.
fn failed(op: GitWriteOp, error: String) -> DaemonMessage {
    DaemonMessage::GitOpResult {
        op,
        ok: false,
        error: Some(error),
    }
}

/// The [`GitWriteOp`] echo for a request — used only for the failure paths that
/// answer before dispatching (worktree-not-ready, unsupported variant).
fn op_echo(msg: &ClientMessage) -> GitWriteOp {
    match msg {
        ClientMessage::StageFile { path } => GitWriteOp::StageFile { path: path.clone() },
        ClientMessage::UnstageFile { path } => GitWriteOp::UnstageFile { path: path.clone() },
        ClientMessage::DiscardFile { path } => GitWriteOp::DiscardFile { path: path.clone() },
        ClientMessage::StageHunk { path, hunk_id } => GitWriteOp::StageHunk {
            path: path.clone(),
            hunk_id: *hunk_id,
        },
        _ => GitWriteOp::Commit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_explorer::Snapshot;
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rift-daemon-gitwrite-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp root");
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
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("run git");
        assert!(status.status.success(), "git {args:?} failed");
    }

    fn init_repo(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        git(&tmp.path, &["config", "user.name", "t"]);
        git(&tmp.path, &["config", "user.email", "t@t"]);
        std::fs::write(tmp.path.join("tracked.txt"), b"one\n").expect("write");
        git(&tmp.path, &["add", "tracked.txt"]);
        git(&tmp.path, &["commit", "-q", "-m", "init"]);
        tmp
    }

    /// A `State` receiver whose worktree root is `root`, so `reply` can resolve
    /// and confine paths against it.
    fn state_for(root: &Path) -> watch::Receiver<State> {
        let state = State {
            worktree: Some(Snapshot::scan(root).expect("scan")),
            ..State::default()
        };
        let (_tx, rx) = watch::channel(state);
        rx
    }

    fn assert_ok(msg: DaemonMessage, expected: GitWriteOp) {
        match msg {
            DaemonMessage::GitOpResult { op, ok, error } => {
                assert_eq!(op, expected, "echoed op");
                assert!(ok, "op should succeed, error: {error:?}");
                assert!(error.is_none());
            }
            other => panic!("expected GitOpResult, got {other:?}"),
        }
    }

    fn assert_failed(msg: DaemonMessage) -> String {
        match msg {
            DaemonMessage::GitOpResult { ok, error, .. } => {
                assert!(!ok, "op should have failed");
                error.expect("a failure carries an error")
            }
            other => panic!("expected GitOpResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stage_file_replies_ok_and_stages() {
        let repo = init_repo("stage-ok");
        std::fs::write(repo.path.join("tracked.txt"), b"two\n").expect("edit");
        let state = state_for(&repo.path);

        let reply = reply(
            &state,
            ClientMessage::StageFile {
                path: "tracked.txt".into(),
            },
        )
        .await;
        assert_ok(
            reply,
            GitWriteOp::StageFile {
                path: "tracked.txt".into(),
            },
        );
        assert_eq!(
            rift_explorer::GitStatus::compute(&repo.path)
                .expect("status")
                .get(Path::new("tracked.txt"))
                .map(|s| s.index),
            Some(rift_explorer::GitStatusCode::Modified)
        );
    }

    #[tokio::test]
    async fn test_commit_replies_ok() {
        let repo = init_repo("commit-ok");
        std::fs::write(repo.path.join("tracked.txt"), b"two\n").expect("edit");
        git(&repo.path, &["add", "tracked.txt"]);
        let state = state_for(&repo.path);

        let reply = reply(
            &state,
            ClientMessage::Commit {
                message: "change".into(),
            },
        )
        .await;
        assert_ok(reply, GitWriteOp::Commit);
    }

    #[tokio::test]
    async fn test_commit_nothing_staged_replies_error() {
        let repo = init_repo("commit-nothing");
        let state = state_for(&repo.path);

        let reply = reply(
            &state,
            ClientMessage::Commit {
                message: "noop".into(),
            },
        )
        .await;
        let error = assert_failed(reply);
        assert!(error.contains("nothing staged"), "got {error}");
    }

    #[tokio::test]
    async fn test_stage_rejects_path_escape() {
        let repo = init_repo("stage-escape");
        let state = state_for(&repo.path);

        let reply = reply(
            &state,
            ClientMessage::StageFile {
                path: "../escape.txt".into(),
            },
        )
        .await;
        let error = assert_failed(reply);
        assert!(error.contains("invalid path"), "got {error}");
    }

    #[tokio::test]
    async fn test_reply_before_worktree_ready_fails_cleanly() {
        let (_tx, rx) = watch::channel(State::default());
        let reply = reply(
            &rx,
            ClientMessage::StageFile {
                path: "tracked.txt".into(),
            },
        )
        .await;
        let error = assert_failed(reply);
        assert!(error.contains("worktree not ready"), "got {error}");
    }
}
