//! The daemon's directory-browse service: answers a
//! [`ClientMessage::QueryDirEntries`] with exactly one
//! [`DaemonMessage::DirEntriesReply`] — the remote root picker's discovery
//! read (`docs/spec-session-root-picker.md`, `docs/protocol.md`).
//!
//! Unlike [`crate::file_ops`] and [`crate::buffer`], this module is
//! deliberately **rootless**: it takes no [`crate::State`] and does no
//! `buffer::resolve` confinement. Its purpose is to let the user pick a *new*
//! project root, so it accepts an absolute host path and reads whatever
//! directory the daemon's SSH user can already see — the daemon's `OpenFile`
//! out-of-root read carve-out and the shell available in every tmux pane
//! already expose arbitrary-path reads; directory **enumeration** is the one
//! genuinely new capability, not a privilege escalation.
//!
//! The listing runs on `tokio::task::spawn_blocking` (disk-bound work, same
//! discipline as `file_ops`); a missing / denied / non-directory target, or
//! any other I/O failure, replies with a typed [`DirBrowseError`] rather than
//! aborting the daemon — a browse request can never take a connection down.

use std::path::{Path, PathBuf};

use rift_protocol::{ClientMessage, DaemonMessage, DirBrowseError, DirEntry};
use tracing::warn;

/// Answer a [`ClientMessage::QueryDirEntries`] with its
/// [`DaemonMessage::DirEntriesReply`]. Any other variant is answered as a
/// failed browse rather than a panic, so a stray message can never take the
/// connection down (mirrors [`crate::file_ops::reply`]'s defensive-echo
/// convention).
pub(crate) async fn reply(msg: ClientMessage) -> DaemonMessage {
    let ClientMessage::QueryDirEntries { path } = msg else {
        return failed(String::new(), DirBrowseError::Io);
    };

    let resolved = resolve_path(&path);
    match tokio::task::spawn_blocking(move || list_dir(resolved)).await {
        Ok(reply) => reply,
        Err(_) => failed(path, DirBrowseError::Io),
    }
}

/// Resolve the request's `path` to an absolute directory: `""` / `"~"` /
/// `"~/…"` expand to the daemon user's `$HOME` (degrading to `/` when `HOME`
/// is unset — a stripped daemon environment, best-effort rather than a hard
/// failure); any other value is used as-is (the caller sends an absolute
/// path).
fn resolve_path(path: &str) -> PathBuf {
    if path.is_empty() || path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

/// The daemon user's home directory, or `/` when `HOME` is unset.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

/// List `resolved`'s child directories (disk-bound; runs on
/// `spawn_blocking`). Checks the target up front with `fs::metadata`
/// (mirroring `file_ops`'s deterministic up-front checks rather than trusting
/// `io::ErrorKind` alone): a missing target replies [`DirBrowseError::NotFound`],
/// an existing non-directory target replies [`DirBrowseError::NotADirectory`].
fn list_dir(resolved: PathBuf) -> DaemonMessage {
    let path_string = resolved.to_string_lossy().into_owned();

    let metadata = match std::fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(err) => return failed(path_string, io_error(err)),
    };
    if !metadata.is_dir() {
        return failed(path_string, DirBrowseError::NotADirectory);
    }

    let read_dir = match std::fs::read_dir(&resolved) {
        Ok(read_dir) => read_dir,
        Err(err) => return failed(path_string, io_error(err)),
    };

    // Individual entries that vanish mid-listing or carry a non-UTF-8 name
    // are skipped, not a hard failure — `Result::into_iter` via `flatten`
    // drops a per-entry read error the same way.
    let mut entries: Vec<DirEntry> = read_dir
        .flatten()
        .filter_map(|entry| {
            let child_path = entry.path();
            // Symlink-FOLLOWING metadata, so a symlinked project directory is
            // included (unlike `DirEntry::file_type()`, which does not follow
            // symlinks).
            if !std::fs::metadata(&child_path)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                return None;
            }
            let name = child_path.file_name()?.to_str()?.to_owned();
            let is_git_repo = child_path.join(".git").exists();
            let git_branch = is_git_repo.then(|| read_git_branch(&child_path)).flatten();
            Some(DirEntry {
                name,
                is_git_repo,
                git_branch,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let parent = resolved
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned());

    DaemonMessage::DirEntriesReply {
        path: path_string,
        parent,
        entries,
        error: None,
    }
}

/// Parse `<repo_dir>/.git/HEAD` for the repo's current branch: `ref:
/// refs/heads/<branch>` -> `Some(<branch>)`; a detached HEAD (a raw commit
/// hash) or a missing/unreadable `HEAD` -> `None`. A plain file read, not a
/// `gix` call (the spec's deliberate dependency-light choice for this cheap
/// flag).
fn read_git_branch(repo_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(repo_dir.join(".git").join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_owned)
}

/// Map a `std::io::Error` onto the wire [`DirBrowseError`] by its
/// [`std::io::ErrorKind`] — the same refinement `file_ops::io_error` applies.
fn io_error(err: std::io::Error) -> DirBrowseError {
    match err.kind() {
        std::io::ErrorKind::NotFound => DirBrowseError::NotFound,
        std::io::ErrorKind::PermissionDenied => DirBrowseError::PermissionDenied,
        _ => DirBrowseError::Io,
    }
}

/// A failed reply: empty entries, no parent, the typed error — logged so a
/// denied/missing browse is visible in the daemon's own log without taking
/// the connection down.
fn failed(path: String, error: DirBrowseError) -> DaemonMessage {
    warn!(%path, ?error, "directory browse failed");
    DaemonMessage::DirEntriesReply {
        path,
        parent: None,
        entries: Vec::new(),
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rift-daemon-browse-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp root");
            // Canonicalize so path comparisons (the reply's `path`/`parent`)
            // match what `list_dir` reports — the system temp dir is a
            // symlink on macOS and some Linux setups.
            let path = path.canonicalize().expect("canonicalize temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn names(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[tokio::test]
    async fn test_query_dir_entries_lists_child_directories_name_sorted_excluding_files() {
        let tmp = TempDir::new("list-basic");
        std::fs::create_dir_all(tmp.path.join("zeta")).expect("mkdir zeta");
        std::fs::create_dir_all(tmp.path.join("alpha")).expect("mkdir alpha");
        std::fs::write(tmp.path.join("a-file.txt"), b"nope").expect("write file");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: tmp.path.to_string_lossy().into_owned(),
        })
        .await;

        match reply {
            DaemonMessage::DirEntriesReply {
                path,
                entries,
                error,
                ..
            } => {
                assert_eq!(path, tmp.path.to_string_lossy());
                assert_eq!(error, None);
                assert_eq!(names(&entries), vec!["alpha", "zeta"]);
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_includes_symlinked_and_dotfile_directories() {
        let tmp = TempDir::new("list-symlink-dotfile");
        let target = tmp.path.join("real_target");
        std::fs::create_dir_all(&target).expect("mkdir target");
        std::fs::create_dir_all(tmp.path.join(".hidden")).expect("mkdir dotfile dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, tmp.path.join("linked")).expect("symlink dir");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: tmp.path.to_string_lossy().into_owned(),
        })
        .await;

        match reply {
            DaemonMessage::DirEntriesReply { entries, .. } => {
                let found = names(&entries);
                assert!(
                    found.contains(&".hidden"),
                    "dotfile dir included: {found:?}"
                );
                #[cfg(unix)]
                assert!(
                    found.contains(&"linked"),
                    "symlinked dir included: {found:?}"
                );
                assert!(found.contains(&"real_target"));
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_flags_git_repo_and_branch() {
        let tmp = TempDir::new("list-git-branch");
        let repo = tmp.path.join("my-repo");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        std::fs::write(
            repo.join(".git").join("HEAD"),
            b"ref: refs/heads/feature/x\n",
        )
        .expect("write HEAD");
        std::fs::create_dir_all(tmp.path.join("not-a-repo")).expect("mkdir plain dir");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: tmp.path.to_string_lossy().into_owned(),
        })
        .await;

        match reply {
            DaemonMessage::DirEntriesReply { entries, .. } => {
                let repo_entry = entries
                    .iter()
                    .find(|e| e.name == "my-repo")
                    .expect("repo entry present");
                assert!(repo_entry.is_git_repo);
                assert_eq!(repo_entry.git_branch, Some("feature/x".to_owned()));

                let plain_entry = entries
                    .iter()
                    .find(|e| e.name == "not-a-repo")
                    .expect("plain entry present");
                assert!(!plain_entry.is_git_repo);
                assert_eq!(plain_entry.git_branch, None);
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_detached_head_reports_no_branch() {
        let tmp = TempDir::new("list-detached-head");
        let repo = tmp.path.join("detached-repo");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        std::fs::write(
            repo.join(".git").join("HEAD"),
            b"4b825dc642cb6eb9a060e54bf8d69288fbee4904\n",
        )
        .expect("write detached HEAD");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: tmp.path.to_string_lossy().into_owned(),
        })
        .await;

        match reply {
            DaemonMessage::DirEntriesReply { entries, .. } => {
                let repo_entry = entries
                    .iter()
                    .find(|e| e.name == "detached-repo")
                    .expect("repo entry present");
                assert!(repo_entry.is_git_repo);
                assert_eq!(repo_entry.git_branch, None);
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_reports_parent_and_none_at_fs_root() {
        let tmp = TempDir::new("list-parent");
        std::fs::create_dir_all(tmp.path.join("child")).expect("mkdir child");

        let child_reply = reply(ClientMessage::QueryDirEntries {
            path: tmp.path.to_string_lossy().into_owned(),
        })
        .await;
        match child_reply {
            DaemonMessage::DirEntriesReply { parent, .. } => {
                let expected_parent = tmp.path.parent().expect("temp dir has a parent");
                assert_eq!(parent, Some(expected_parent.to_string_lossy().into_owned()));
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }

        let root_reply = reply(ClientMessage::QueryDirEntries {
            path: "/".to_owned(),
        })
        .await;
        match root_reply {
            DaemonMessage::DirEntriesReply { parent, error, .. } => {
                assert_eq!(error, None);
                assert_eq!(parent, None, "filesystem root has no parent");
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_empty_path_resolves_to_home() {
        let home = std::env::var("HOME").expect("HOME set in this environment");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: String::new(),
        })
        .await;
        match reply {
            DaemonMessage::DirEntriesReply { path, .. } => assert_eq!(path, home),
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_tilde_resolves_to_home() {
        let home = std::env::var("HOME").expect("HOME set in this environment");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: "~".to_owned(),
        })
        .await;
        match reply {
            DaemonMessage::DirEntriesReply { path, .. } => assert_eq!(path, home),
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_path_empty_and_tilde_match_home_dir() {
        // `resolve_path` and `home_dir` must agree for both spellings of "the
        // picker's start level" — checked directly (not through `reply`,
        // which would need to mutate the process-global `HOME` env var and
        // race other concurrent tests) since a live `HOME` is expected in
        // this test environment.
        assert_eq!(resolve_path(""), home_dir());
        assert_eq!(resolve_path("~"), home_dir());
    }

    #[test]
    fn test_resolve_path_tilde_slash_joins_home_dir() {
        assert_eq!(resolve_path("~/projects"), home_dir().join("projects"));
    }

    #[test]
    fn test_resolve_path_absolute_path_used_as_is() {
        assert_eq!(resolve_path("/var/log"), PathBuf::from("/var/log"));
    }

    #[tokio::test]
    async fn test_query_dir_entries_missing_path_replies_not_found() {
        let tmp = TempDir::new("missing");
        let missing = tmp.path.join("does-not-exist");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: missing.to_string_lossy().into_owned(),
        })
        .await;
        match reply {
            DaemonMessage::DirEntriesReply { entries, error, .. } => {
                assert!(entries.is_empty());
                assert_eq!(error, Some(DirBrowseError::NotFound));
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_non_directory_replies_not_a_directory() {
        let tmp = TempDir::new("not-a-dir");
        let file = tmp.path.join("just-a-file.txt");
        std::fs::write(&file, b"content").expect("write file");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: file.to_string_lossy().into_owned(),
        })
        .await;
        match reply {
            DaemonMessage::DirEntriesReply { entries, error, .. } => {
                assert!(entries.is_empty());
                assert_eq!(error, Some(DirBrowseError::NotADirectory));
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_query_dir_entries_unreadable_dir_replies_permission_denied() {
        let tmp = TempDir::new("perm-denied");
        let locked = tmp.path.join("locked");
        std::fs::create_dir_all(&locked).expect("mkdir locked");
        let mut perms = std::fs::metadata(&locked).expect("metadata").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
        std::fs::set_permissions(&locked, perms.clone()).expect("chmod 000");

        let reply = reply(ClientMessage::QueryDirEntries {
            path: locked.to_string_lossy().into_owned(),
        })
        .await;

        // Restore permissions so `TempDir::drop`'s `remove_dir_all` can clean up.
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        let _ = std::fs::set_permissions(&locked, perms);

        match reply {
            DaemonMessage::DirEntriesReply { entries, error, .. } => {
                if error.is_none() {
                    // Best-effort: running with elevated privileges (e.g. root
                    // in CI) ignores directory permission bits, so `0o000`
                    // does not actually deny the read. Skip rather than flake.
                    return;
                }
                assert!(entries.is_empty());
                assert_eq!(error, Some(DirBrowseError::PermissionDenied));
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_dir_entries_unknown_variant_replies_io_error() {
        // A defensive path: `reply` is only ever called with `QueryDirEntries`
        // by `serve_connection`'s dispatch, but a stray other variant must not
        // panic.
        let reply = reply(ClientMessage::QuerySessionList).await;
        match reply {
            DaemonMessage::DirEntriesReply { entries, error, .. } => {
                assert!(entries.is_empty());
                assert_eq!(error, Some(DirBrowseError::Io));
            }
            other => panic!("expected DirEntriesReply, got {other:?}"),
        }
    }
}
