//! The daemon's clone service: executes a [`ClientMessage::CloneRepo`] by
//! shelling out to the host's `git clone` and answers with exactly one
//! [`DaemonMessage::CloneResult`] (`docs/spec-clone-repo.md`, issue #841), the
//! cold-start path that precedes creating a session rooted at the checkout.
//!
//! Unlike every other request/response arm `crate::serve_connection`'s
//! dispatch loop answers inline, a clone is unbounded (seconds to minutes):
//! [`run`] is spawned by the caller as a **detached task**, never awaited in
//! the per-connection dispatch loop, so a clone in progress never stalls that
//! connection's terminal output or its other inbound messages. Cancellation
//! kills the `git` child process: [`run`] holds the `tokio::process::Child`
//! and `select!`s its exit against a short poll of `should_interrupt` (the
//! caller flips it when the connection this clone was requested on goes
//! away), `start_kill`ing and reaping the child on interrupt, so an
//! abandoned/hung clone is aborted rather than left running forever.
//!
//! The daemon carries no git HTTP-transport dependency for this: no `gix`
//! network features, no `reqwest`/`rustls`/`aws-lc-rs` — the clone runs as an
//! external `git` process, inheriting the daemon host's own git configuration
//! (credential helpers, `insteadOf` rules, SSH agent) for free, exactly as a
//! terminal `git clone` on that host would. The one accepted tradeoff is a
//! runtime dependency on `git` being present on the host; a spawn failure
//! (`git` missing) maps to [`CloneError::GitUnavailable`].
//!
//! `<parent>/<name>` resolves under the same rootless convention
//! [`crate::browse`] uses for `parent` (an absolute host path; `""` / `"~"` /
//! `"~/…"` expand to `$HOME`); the target must not already exist (no
//! clobber, [`CloneError::TargetExists`]). On any failure the checkout is
//! never persisted at that path: `git clone` removes its own target on a
//! failed clone, and this module additionally removes it on any non-success
//! exit or on interrupt-kill, so a partial tree never survives.

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rift_protocol::{ClientMessage, CloneError, DaemonMessage};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tracing::warn;

/// The `git` binary invoked for every clone. Factored out so
/// [`spawn_and_run`]'s tests can substitute a nonexistent program name to
/// exercise the `GitUnavailable` spawn-failure path deterministically,
/// without touching the process-wide `PATH` (which would race concurrent
/// tests that shell out to the real `git`, e.g. this module's own fixture
/// setup).
const GIT_PROGRAM: &str = "git";

/// How often the cancellation loop re-checks `should_interrupt` while the
/// `git` child is running: short enough that an interrupt lands promptly,
/// negligible CPU cost over a clone that can run for minutes.
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Execute a [`ClientMessage::CloneRepo`] and return its
/// [`DaemonMessage::CloneResult`]. Intended to be spawned as a detached task
/// (see the module docs) — the caller posts the returned message onto the
/// requesting connection alone, never the shared bus. Any other message
/// variant is answered as a failed clone rather than a panic (mirrors
/// [`crate::browse::reply`]'s defensive convention).
///
/// `path` on every reply — success or failure — is the resolved
/// `<parent>/<name>` target, never empty on an early reject: the app's
/// clone-reply correlation (issue #839) matches against this echoed path, so
/// an early failure must carry it too, not just a successful clone.
pub(crate) async fn run(msg: ClientMessage, should_interrupt: Arc<AtomicBool>) -> DaemonMessage {
    let ClientMessage::CloneRepo { url, parent, name } = msg else {
        return failed(String::new(), CloneError::Other);
    };

    let target = crate::browse::resolve_path(&parent).join(&name);
    let path_string = target.to_string_lossy().into_owned();

    if let Err(err) = validate_name(&name) {
        return failed(path_string, err);
    }
    if target.exists() {
        return failed(path_string, CloneError::TargetExists);
    }
    match target.parent() {
        Some(clone_parent) if clone_parent.is_dir() => {}
        _ => return failed(path_string, CloneError::Other),
    }
    if !is_valid_clone_url(&url) {
        return failed(path_string, CloneError::InvalidUrl);
    }

    match spawn_and_run(GIT_PROGRAM, &url, &target, &should_interrupt).await {
        Ok(()) => DaemonMessage::CloneResult {
            path: path_string,
            error: None,
        },
        Err(err) => {
            // No partial tree (`docs/spec-clone-repo.md`): `git clone` already
            // removes its own target on a failed clone; this defensively
            // covers the interrupt-kill case (and any straggler) too. Disk-
            // bound, so it runs off the async runtime's worker threads, same
            // discipline as `file_ops::reply`'s `DeletePath` arm. Never
            // reached for `TargetExists`/`InvalidUrl`/the name-validation
            // reject above — those return before anything could have been
            // created at `target`.
            let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(target)).await;
            failed(path_string, err)
        }
    }
}

/// Validate a clone request's target `name`: non-empty, no path separator,
/// and not `.`/`..` — `name` is a single path component appended under the
/// resolved `parent`, never a way to escape it (the browse channel's
/// `parent` validation has no equivalent, since it takes no separate `name`).
/// The app validates this too before ever sending a request (issue #839's
/// "reject before send" fix), but the daemon never trusts the wire and
/// re-checks independently.
fn validate_name(name: &str) -> Result<(), CloneError> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        warn!(%name, "clone request rejected: invalid target name");
        return Err(CloneError::Other);
    }
    Ok(())
}

/// Validate `url` as a git URL the daemon will pass to `git clone` —
/// deliberately **gix-free** (the daemon no longer carries a direct `gix`
/// dep to borrow a URL parser from, `docs/spec-clone-repo.md`): a scheme
/// allow-list (`https://` / `http://` / `ssh://` / `git://` / `file://`)
/// plus scp-shorthand detection (`user@host:path`). Its job, now that `--`
/// (passed by [`spawn_and_run`]) neutralizes option injection, is rejecting
/// local-path-looking strings a bare `git clone <string>` would otherwise
/// happily treat as a relative path on the daemon's own filesystem — never
/// what the operator meant.
fn is_valid_clone_url(url: &str) -> bool {
    const SCHEMES: [&str; 5] = ["https://", "http://", "ssh://", "git://", "file://"];
    SCHEMES.iter().any(|scheme| url.starts_with(scheme)) || is_scp_like(url)
}

/// scp-like shorthand: `[user@]host:path` (e.g. `git@github.com:org/repo.git`).
/// Rejects anything carrying `://` up front (an explicit but unrecognized
/// scheme must not be reinterpreted as scp-like) and anything whose
/// pre-colon segment is empty, contains a `/` (a relative local path with a
/// colon further in, e.g. `foo/bar:baz`, is not scp shorthand), or whose
/// post-colon segment is empty.
fn is_scp_like(url: &str) -> bool {
    if url.contains("://") {
        return false;
    }
    let Some(colon) = url.find(':') else {
        return false;
    };
    let (host_part, rest) = url.split_at(colon);
    let path_part = &rest[1..];
    !host_part.is_empty() && !host_part.contains('/') && !path_part.is_empty()
}

/// Spawn `<git_program> clone -- <url> <target>` and drive it to completion:
/// `select!`s the child's exit against a short poll of `should_interrupt`,
/// `start_kill`ing the child once the flag flips, and classifies a non-zero
/// exit's stderr onto a [`CloneError`]. `git_program` is [`GIT_PROGRAM`] in
/// production; tests substitute a nonexistent name to exercise the
/// `GitUnavailable` path without touching the process-wide `PATH`.
async fn spawn_and_run(
    git_program: &str,
    url: &str,
    target: &Path,
    should_interrupt: &AtomicBool,
) -> Result<(), CloneError> {
    let mut command = Command::new(git_program);
    command
        .arg("clone")
        .arg("--")
        .arg(url)
        .arg(target)
        // Locale-stable stderr so `classify_git_stderr`'s substring match
        // does not depend on the daemon host's configured locale.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CloneError::GitUnavailable);
        }
        Err(_) => return Err(CloneError::Other),
    };

    // Drain stderr concurrently with the wait/interrupt loop below: reading
    // it only after the child exits risks a deadlock if `git` ever writes
    // enough to fill the pipe buffer before exiting.
    let mut stderr_pipe = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(stderr) = stderr_pipe.as_mut() {
            let _ = stderr.read_to_end(&mut buf).await;
        }
        buf
    });

    let mut ticker = tokio::time::interval(INTERRUPT_POLL_INTERVAL);
    let status = loop {
        tokio::select! {
            biased;
            result = child.wait() => break result,
            _ = ticker.tick() => {
                if should_interrupt.load(Ordering::Relaxed) {
                    let _ = child.start_kill();
                }
            }
        }
    };
    let stderr_bytes = stderr_task.await.unwrap_or_default();

    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(classify_git_stderr(&String::from_utf8_lossy(&stderr_bytes))),
        Err(_) => Err(CloneError::Other),
    }
}

/// Best-effort classification of a failed `git clone`'s stderr onto the wire
/// [`CloneError`] (`docs/spec-clone-repo.md`): recognizable substrings for a
/// rejected/absent credential and a DNS/connection failure, defaulting to
/// [`CloneError::Other`] rather than guessing — mirrors the previous gix
/// implementation's classification discipline. `stderr` is lower-cased
/// before matching; the child's `LC_ALL=C` env keeps its wording
/// locale-stable.
fn classify_git_stderr(stderr: &str) -> CloneError {
    let message = stderr.to_lowercase();
    if message.contains("auth")
        || message.contains("credential")
        || message.contains("permission denied")
    {
        CloneError::AuthFailed
    } else if message.contains("could not resolve host")
        || message.contains("network")
        || message.contains("connection")
        || message.contains("failed to connect")
    {
        CloneError::Network
    } else {
        CloneError::Other
    }
}

/// A failed reply: the resolved `path`, the typed error — logged so a
/// failed clone is visible in the daemon's own log without taking the
/// connection down.
fn failed(path: String, error: CloneError) -> DaemonMessage {
    warn!(%path, ?error, "clone failed");
    DaemonMessage::CloneResult {
        path,
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command as StdCommand;
    use std::sync::atomic::{AtomicU32, Ordering as StdOrdering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, StdOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rift-daemon-clone-{tag}-{}-{n}",
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

    /// Run a git command in `dir`, asserting success. Builds the offline
    /// fixture repo the clone tests clone from (a bare repo behind a
    /// `file://` URL) — the real `git` binary as ground truth, present in CI
    /// and dev, exactly as `crates/explorer/src/git.rs`'s test fixtures do.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = StdCommand::new("git")
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

    /// A bare fixture repo with one commit, clonable over a `file://` URL —
    /// offline, deterministic, no network in CI.
    struct BareFixture {
        _worktree: TempDir,
        bare_dir: TempDir,
    }

    impl BareFixture {
        fn new(tag: &str) -> Self {
            let worktree = TempDir::new(&format!("{tag}-src"));
            git(&worktree.path, &["init", "-q"]);
            std::fs::write(worktree.path.join("file.txt"), b"hello\n").expect("write file");
            git(&worktree.path, &["add", "file.txt"]);
            git(&worktree.path, &["commit", "-q", "-m", "init"]);

            let bare_dir = TempDir::new(&format!("{tag}-bare"));
            // `TempDir::new` already created the directory; `git clone --bare`
            // requires the target to not exist (or be empty), which it is.
            git(
                std::env::temp_dir().as_path(),
                &[
                    "clone",
                    "-q",
                    "--bare",
                    worktree.path.to_str().expect("utf8 path"),
                    bare_dir.path.to_str().expect("utf8 path"),
                ],
            );
            Self {
                _worktree: worktree,
                bare_dir,
            }
        }

        fn url(&self) -> String {
            format!("file://{}", self.bare_dir.path.display())
        }
    }

    #[tokio::test]
    async fn test_clone_repo_file_url_clones_into_parent_name_with_checkout_present() {
        let fixture = BareFixture::new("basic");
        let parent = TempDir::new("basic-parent");

        let reply = run(
            ClientMessage::CloneRepo {
                url: fixture.url(),
                parent: parent.path.to_string_lossy().into_owned(),
                name: "cloned".to_owned(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        match reply {
            DaemonMessage::CloneResult { path, error } => {
                assert_eq!(error, None, "clone should succeed");
                let expected = parent.path.join("cloned");
                assert_eq!(path, expected.to_string_lossy());
                assert!(expected.join("file.txt").is_file(), "checkout present");
            }
            other => panic!("expected CloneResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_clone_repo_existing_target_replies_target_exists() {
        let fixture = BareFixture::new("exists");
        let parent = TempDir::new("exists-parent");
        std::fs::create_dir_all(parent.path.join("cloned")).expect("pre-create target");

        let reply = run(
            ClientMessage::CloneRepo {
                url: fixture.url(),
                parent: parent.path.to_string_lossy().into_owned(),
                name: "cloned".to_owned(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        match reply {
            DaemonMessage::CloneResult { path, error } => {
                assert_eq!(error, Some(CloneError::TargetExists));
                assert_eq!(path, parent.path.join("cloned").to_string_lossy());
            }
            other => panic!("expected CloneResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_clone_repo_bogus_url_leaves_no_directory_behind() {
        let parent = TempDir::new("bogus-parent");
        let target = parent.path.join("cloned");

        let reply = run(
            ClientMessage::CloneRepo {
                url: "not a url at all".to_owned(),
                parent: parent.path.to_string_lossy().into_owned(),
                name: "cloned".to_owned(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        match reply {
            DaemonMessage::CloneResult { path, error } => {
                assert_eq!(error, Some(CloneError::InvalidUrl));
                assert_eq!(path, target.to_string_lossy(), "path echoed even on error");
            }
            other => panic!("expected CloneResult, got {other:?}"),
        }
        assert!(!target.exists(), "no partial tree left behind");
    }

    #[tokio::test]
    async fn test_clone_repo_unreachable_host_leaves_no_directory_behind() {
        let parent = TempDir::new("unreachable-parent");
        let target = parent.path.join("cloned");

        let reply = run(
            ClientMessage::CloneRepo {
                url: "https://this-host-does-not-exist.rift-clone-spike.invalid/org/repo.git"
                    .to_owned(),
                parent: parent.path.to_string_lossy().into_owned(),
                name: "cloned".to_owned(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        match reply {
            DaemonMessage::CloneResult { error, .. } => {
                assert!(error.is_some(), "unreachable host must fail, not hang");
            }
            other => panic!("expected CloneResult, got {other:?}"),
        }
        assert!(!target.exists(), "no partial tree left behind");
    }

    #[tokio::test]
    async fn test_clone_repo_pre_interrupted_aborts_and_leaves_no_directory_behind() {
        let fixture = BareFixture::new("interrupt");
        let parent = TempDir::new("interrupt-parent");
        let target = parent.path.join("cloned");

        let reply = run(
            ClientMessage::CloneRepo {
                url: fixture.url(),
                parent: parent.path.to_string_lossy().into_owned(),
                name: "cloned".to_owned(),
            },
            // Pre-interrupted: the clone must abort rather than proceed.
            Arc::new(AtomicBool::new(true)),
        )
        .await;

        match reply {
            DaemonMessage::CloneResult { error, .. } => {
                assert!(error.is_some(), "interrupted clone must not report success");
            }
            other => panic!("expected CloneResult, got {other:?}"),
        }
        assert!(!target.exists(), "no partial tree left behind");
    }

    #[tokio::test]
    async fn test_clone_repo_unknown_variant_replies_other_error() {
        let reply = run(
            ClientMessage::QuerySessionList,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        match reply {
            DaemonMessage::CloneResult { path, error } => {
                assert_eq!(path, "");
                assert_eq!(error, Some(CloneError::Other));
            }
            other => panic!("expected CloneResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_spawn_and_run_missing_git_binary_replies_git_unavailable() {
        let parent = TempDir::new("missing-git-parent");
        let target = parent.path.join("cloned");

        let result = spawn_and_run(
            "rift-daemon-test-definitely-missing-git-binary",
            "https://example.com/org/repo.git",
            &target,
            &AtomicBool::new(false),
        )
        .await;

        assert_eq!(result, Err(CloneError::GitUnavailable));
        assert!(!target.exists(), "no partial tree left behind");
    }

    #[test]
    fn test_validate_name_rejects_empty_dot_dotdot_and_separators() {
        for name in ["", ".", "..", "a/b", "a\\b"] {
            assert_eq!(
                validate_name(name),
                Err(CloneError::Other),
                "name {name:?} must be rejected"
            );
        }
    }

    #[test]
    fn test_validate_name_accepts_a_plain_component() {
        assert_eq!(validate_name("repo"), Ok(()));
    }

    #[test]
    fn test_is_valid_clone_url_accepts_https_ssh_file_and_scp_urls() {
        assert!(is_valid_clone_url("https://example.com/org/repo.git"));
        assert!(is_valid_clone_url("http://example.com/org/repo.git"));
        assert!(is_valid_clone_url("ssh://git@example.com/org/repo.git"));
        assert!(is_valid_clone_url("git@example.com:org/repo.git"));
        assert!(is_valid_clone_url("file:///tmp/somewhere"));
    }

    #[test]
    fn test_is_valid_clone_url_rejects_relative_paths_and_unrecognized_schemes() {
        assert!(!is_valid_clone_url("not a url at all"));
        assert!(!is_valid_clone_url(""));
        assert!(!is_valid_clone_url("./relative/path"));
        assert!(!is_valid_clone_url("../relative/path"));
        assert!(
            !is_valid_clone_url("ftp://example.com/org/repo.git"),
            "an unrecognized scheme must not be reinterpreted as scp-like"
        );
    }

    #[test]
    fn test_classify_git_stderr_maps_auth_and_network_substrings() {
        assert_eq!(
            classify_git_stderr("fatal: Authentication failed for 'https://example.com/'"),
            CloneError::AuthFailed
        );
        assert_eq!(
            classify_git_stderr("remote: Permission denied"),
            CloneError::AuthFailed
        );
        assert_eq!(
            classify_git_stderr("fatal: unable to access: could not resolve host"),
            CloneError::Network
        );
        assert_eq!(
            classify_git_stderr("fatal: unable to access: Failed to connect"),
            CloneError::Network
        );
        assert_eq!(
            classify_git_stderr("fatal: repository not found"),
            CloneError::Other
        );
    }
}
