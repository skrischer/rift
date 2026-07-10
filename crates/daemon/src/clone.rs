//! The daemon's clone service: executes a [`ClientMessage::CloneRepo`] via
//! `gix` and answers with exactly one [`DaemonMessage::CloneResult`]
//! (`docs/spec-clone-repo.md`), the cold-start path that precedes creating a
//! session rooted at the checkout.
//!
//! Unlike every other request/response arm `crate::serve_connection`'s
//! dispatch loop answers inline, a clone is unbounded (seconds to minutes):
//! [`run`] is spawned by the caller as a **detached task**, never awaited in
//! the per-connection dispatch loop, so a clone in progress never stalls that
//! connection's terminal output or its other inbound messages. Cancellation
//! rides gix's own cooperative interrupt flag — `should_interrupt`, checked
//! by `fetch_then_checkout` and `main_worktree` — which the caller flips when
//! the connection this clone was requested on goes away, so an abandoned
//! clone does not keep running forever.
//!
//! `<parent>/<name>` resolves under the same rootless convention
//! [`crate::browse`] uses for `parent` (an absolute host path; `""` / `"~"` /
//! `"~/…"` expand to `$HOME`); the target must not already exist (no
//! clobber, [`CloneError::TargetExists`]). On any failure the checkout is
//! never persisted at that path: gix's `PrepareFetch`/`PrepareCheckout`
//! delete the directory they created on `Drop` unless `persist()` is called,
//! which this module never does on an error path — success is the only path
//! that keeps the checkout (`main_worktree`'s `Ok` already consumes the
//! checkout handle internally, so nothing here calls `persist()` either way).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rift_protocol::{ClientMessage, CloneError, DaemonMessage};
use tracing::warn;

/// The env var carrying the daemon host's ambient HTTPS git credential
/// (`docs/spec-clone-repo.md`, "Remote-native auth" constraint). The daemon
/// spike found gix's git-config-honoring credential path does not recognize
/// a bare env var by this name — a URL with a bare username and no password
/// makes gix attempt an interactive terminal prompt, which fails immediately
/// in this headless process (no TTY) rather than hanging, but never
/// succeeds either — so when set, this is wired directly into an `http(s)`
/// clone URL that carries no credentials of its own. Still no client-sent
/// token: this only ever reads the daemon **host's** environment.
const GIT_AUTH_TOKEN_ENV: &str = "GIT_AUTH_TOKEN";

/// Execute a [`ClientMessage::CloneRepo`] and return its
/// [`DaemonMessage::CloneResult`]. Intended to be spawned as a detached task
/// (see the module docs) — the caller posts the returned message onto the
/// requesting connection alone, never the shared bus. Any other message
/// variant is answered as a failed clone rather than a panic (mirrors
/// [`crate::browse::reply`]'s defensive convention).
pub(crate) async fn run(msg: ClientMessage, should_interrupt: Arc<AtomicBool>) -> DaemonMessage {
    let ClientMessage::CloneRepo { url, parent, name } = msg else {
        return failed(String::new(), CloneError::Other);
    };

    let target = match resolve_target(&parent, &name) {
        Ok(target) => target,
        Err(err) => return failed(String::new(), err),
    };
    let path_string = target.to_string_lossy().into_owned();

    if target.exists() {
        return failed(path_string, CloneError::TargetExists);
    }
    match target.parent() {
        Some(clone_parent) if clone_parent.is_dir() => {}
        _ => return failed(path_string, CloneError::Other),
    }

    let Some(clone_url) = resolve_url(&url) else {
        return failed(path_string, CloneError::InvalidUrl);
    };

    // Network + disk bound (gix's blocking transport and worktree checkout),
    // same discipline as `browse::reply` and `file_ops`: never block the
    // async runtime's worker threads.
    match tokio::task::spawn_blocking(move || {
        clone_blocking(&clone_url, &target, &should_interrupt)
    })
    .await
    {
        Ok(Ok(())) => DaemonMessage::CloneResult {
            path: path_string,
            error: None,
        },
        Ok(Err(err)) => failed(path_string, err),
        Err(_) => failed(path_string, CloneError::Other),
    }
}

/// Resolve `<parent>/<name>` and validate `name`: non-empty, no path
/// separator, and not `.`/`..` — `name` is a single path component appended
/// under the resolved `parent`, never a way to escape it (the browse
/// channel's `parent` validation has no equivalent, since it takes no
/// separate `name`).
fn resolve_target(parent: &str, name: &str) -> Result<PathBuf, CloneError> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        warn!(%name, "clone request rejected: invalid target name");
        return Err(CloneError::Other);
    }
    Ok(crate::browse::resolve_path(parent).join(name))
}

/// Validate `url` as a git URL gix can clone, and embed the ambient
/// [`GIT_AUTH_TOKEN_ENV`] credential into an `http(s)` URL that carries none
/// of its own. Returns `None` when `url` is not a recognizable git URL.
///
/// gix's own URL parser is lenient: a string with no recognized scheme
/// parses successfully as a *relative local path* rather than erroring (the
/// daemon spike's finding) — cloning from wherever the daemon process's
/// working directory happens to be, never what the operator meant. Only
/// `https://` / `http://` / `ssh://` / `git://`, an explicit `file://`, and
/// the scp-like `user@host:path` shorthand are accepted.
fn resolve_url(url: &str) -> Option<String> {
    let mut parsed = gix::url::parse(url.into()).ok()?;
    if parsed.scheme == gix::url::Scheme::File && !url.starts_with("file://") {
        return None;
    }

    let is_http = matches!(
        parsed.scheme,
        gix::url::Scheme::Https | gix::url::Scheme::Http
    );
    if !is_http || parsed.user().is_some() {
        return Some(url.to_owned());
    }
    let token = std::env::var(GIT_AUTH_TOKEN_ENV)
        .ok()
        .filter(|t| !t.is_empty());
    let Some(token) = token else {
        return Some(url.to_owned());
    };
    parsed.set_user(Some("x-access-token".to_owned()));
    parsed.set_password(Some(token));
    Some(parsed.to_bstring().to_string())
}

/// Run the clone (blocking; called via `spawn_blocking`):
/// `gix::prepare_clone` -> `fetch_then_checkout` -> `main_worktree`, checking
/// `should_interrupt` throughout. `target`'s parent is already confirmed to
/// exist and `target` itself to be absent by the caller.
fn clone_blocking(
    url: &str,
    target: &std::path::Path,
    should_interrupt: &AtomicBool,
) -> Result<(), CloneError> {
    let mut prepare = gix::prepare_clone(url, target).map_err(classify_prepare_error)?;
    let (mut checkout, _fetch_outcome) = prepare
        .fetch_then_checkout(gix::progress::Discard, should_interrupt)
        .map_err(classify_fetch_error)?;
    checkout
        .main_worktree(gix::progress::Discard, should_interrupt)
        .map_err(|_| CloneError::Other)?;
    Ok(())
}

/// Classify a [`gix::clone::Error`] (from `gix::prepare_clone`) onto the wire
/// [`CloneError`]. Reached rarely in practice since [`resolve_url`] already
/// validates the URL shape up front; kept for the residual cases gix's own
/// (stricter) parse/canonicalize step can still reject.
fn classify_prepare_error(err: gix::clone::Error) -> CloneError {
    match err {
        gix::clone::Error::UrlParse(_) | gix::clone::Error::CanonicalizeUrl { .. } => {
            CloneError::InvalidUrl
        }
        _ => CloneError::Other,
    }
}

/// Best-effort classification of a `fetch_then_checkout` failure onto the
/// wire [`CloneError`] (`docs/spec-clone-repo.md`). gix's own error type
/// here is a deeply nested chain through `gix-transport`/`gix-protocol`
/// internals not meant to be pattern-matched from outside the crate, and the
/// nesting has already shifted once between recent gix releases. Its
/// `Display` message is the stable, human-authored surface (each layer
/// forwards transparently to its cause), so classification keys off
/// recognizable substrings there — confirmed against the daemon spike's
/// actual failure modes: a rejected HTTPS credential ("credentials ... not
/// accepted" / "failed to obtain credentials") and a DNS/connection failure
/// ("io error ... talking to the server"). Anything unrecognized is
/// [`CloneError::Other`], never a guess.
fn classify_fetch_error(err: gix::clone::fetch::Error) -> CloneError {
    let message = err.to_string().to_lowercase();
    if message.contains("credential") {
        CloneError::AuthFailed
    } else if message.contains("io error") || message.contains("talking to the server") {
        CloneError::Network
    } else {
        CloneError::Other
    }
}

/// A failed reply: the resolved (or best-effort empty) `path`, the typed
/// error — logged so a failed clone is visible in the daemon's own log
/// without taking the connection down.
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
            DaemonMessage::CloneResult { error, .. } => {
                assert_eq!(error, Some(CloneError::TargetExists));
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
            DaemonMessage::CloneResult { error, .. } => {
                assert_eq!(error, Some(CloneError::InvalidUrl));
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

    #[test]
    fn test_resolve_target_rejects_empty_dot_dotdot_and_separators() {
        for name in ["", ".", "..", "a/b", "a\\b"] {
            assert_eq!(
                resolve_target("/tmp", name),
                Err(CloneError::Other),
                "name {name:?} must be rejected"
            );
        }
    }

    #[test]
    fn test_resolve_target_joins_resolved_parent_and_name() {
        let target = resolve_target("/tmp/projects", "repo").expect("valid target");
        assert_eq!(target, PathBuf::from("/tmp/projects/repo"));
    }

    #[test]
    fn test_resolve_url_accepts_https_ssh_and_explicit_file_urls() {
        assert!(resolve_url("https://example.com/org/repo.git").is_some());
        assert!(resolve_url("ssh://git@example.com/org/repo.git").is_some());
        assert!(resolve_url("git@example.com:org/repo.git").is_some());
        assert!(resolve_url("file:///tmp/somewhere").is_some());
    }

    #[test]
    fn test_resolve_url_rejects_strings_gix_would_treat_as_a_relative_path() {
        assert_eq!(resolve_url("not a url at all"), None);
        assert_eq!(resolve_url(""), None);
    }

    #[test]
    fn test_resolve_url_embeds_ambient_token_only_for_bare_http_urls() {
        // SAFETY (test-only, single-threaded per-test env mutation): scoped to
        // this test and restored before it returns; no other test reads this
        // var.
        std::env::set_var(GIT_AUTH_TOKEN_ENV, "sekret");

        let with_token = resolve_url("https://example.com/org/repo.git").expect("valid url");
        assert!(
            with_token.contains("x-access-token:sekret@"),
            "{with_token}"
        );

        let already_has_user =
            resolve_url("https://someuser@example.com/org/repo.git").expect("valid url");
        assert!(
            !already_has_user.contains("x-access-token"),
            "must not override an existing credential: {already_has_user}"
        );

        let ssh_untouched = resolve_url("git@example.com:org/repo.git").expect("valid url");
        assert!(!ssh_untouched.contains("x-access-token"));

        std::env::remove_var(GIT_AUTH_TOKEN_ENV);
    }
}
