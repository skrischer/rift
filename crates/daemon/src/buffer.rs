//! The daemon's buffer service: whole-file read and atomic whole-file write,
//! confined to the watched worktree root — with a read-only carve-out for
//! out-of-root navigation targets.
//!
//! This is the daemon side of the editor buffer channel (`spec-editor.md`): the
//! client pulls a file's whole UTF-8 content on open ([`read_file`]) and pushes
//! the whole new content on save ([`write_file`]). The service is a module, not
//! a crate — whole-file `tokio::fs` I/O needs no abstraction (no premature
//! abstraction, `CLAUDE.md`).
//!
//! Three invariants the spec pins:
//!
//! - **Root confinement (writes), read-only carve-out (reads)** — a **write** is
//!   resolved against the worktree root and rejected if it escapes (`..`, an
//!   absolute path, or a symlink that points out). The root is the same
//!   canonicalized root the worktree watcher uses
//!   ([`rift_explorer::Snapshot::root`]), so a relative buffer path keys the same
//!   space as a worktree entry. A **read** of an **absolute** path is the
//!   out-of-root carve-out (`spec-lsp-navigation.md`, 2026-06-12): the editor
//!   jumps to a stdlib/dependency definition outside the root and opens it
//!   **read-only**. The daemon serves that read; it never serves a write outside
//!   the root, so the carve-out cannot widen into one.
//! - **UTF-8 only** — v1 is source text. Non-UTF-8 / binary content is detected
//!   and refused, never silently mangled, on both read and write (pluggable
//!   binary viewers are a future sub-spec).
//! - **Atomic write** — the write lands via a temp file in the target's own
//!   directory plus a `rename` over the target, so a crash mid-save never
//!   truncates the user's file. Reads have no atomicity guarantee — a read
//!   racing a non-atomic external write may return a torn file; v1 accepts this
//!   (agents typically write atomically) and the next worktree update
//!   reconciles.
//!
//! The conflict check compares the request's `base_mtime` against the file's
//! current on-disk `mtime`: a write whose base is older than disk is **rejected,
//! not clobbered**, returning the disk `mtime` so the editor can rebase. The
//! `mtime` is the same [`std::time::SystemTime`] the worktree entry carries
//! (#107), so the base read on the structure path can be compared here.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// Why a buffer read or write was refused.
///
/// A plain enum with a hand-written [`fmt::Display`]: the daemon is a binary
/// (`anyhow`, not `thiserror`, per the constitution), so this carries no extra
/// dependency. The variants are still typed so the dispatch logic can tell a
/// stale-base no-clobber apart from a hard refusal, and the `Io` variant keeps
/// the source so the `NotFound`-on-save case (the file vanished since open) can
/// be distinguished from a real I/O failure.
#[derive(Debug)]
pub enum BufferError {
    /// The requested path escaped the worktree root — a `..` segment, an
    /// absolute path on a **write**, or a symlink resolving outside the root.
    /// Refused, not served: the buffer service never **writes** outside the
    /// watched tree, and a relative read may not climb out of it either. (An
    /// **absolute read** is the deliberate out-of-root carve-out and is served
    /// read-only, not refused — see [`read_file`].)
    PathEscape(String),
    /// The file's content is not valid UTF-8. v1 is UTF-8 text only; binary is
    /// detected and refused rather than mangled.
    NotUtf8(String),
    /// An underlying filesystem error (missing file, permission denied, a failed
    /// rename). Carries the worktree-relative path for context.
    Io {
        path: String,
        source: std::io::Error,
    },
}

impl fmt::Display for BufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BufferError::PathEscape(path) => {
                write!(f, "path {path:?} escapes the worktree root")
            }
            BufferError::NotUtf8(path) => {
                write!(
                    f,
                    "file {path:?} is not valid UTF-8 (binary is not supported)"
                )
            }
            BufferError::Io { path, source } => write!(f, "io error on {path:?}: {source}"),
        }
    }
}

impl std::error::Error for BufferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BufferError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The result of a [`write_file`] request.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The write landed; the file now has this on-disk `mtime`. The editor adopts
    /// it as the buffer's new base for the next save.
    Saved(SystemTime),
    /// The file changed on disk since the editor read it — the on-disk `mtime`
    /// no longer matches the save's `base_mtime` — so the write was rejected and
    /// the file left untouched. Carries the current on-disk `mtime` so the editor
    /// can re-open from disk to rebase.
    Conflict(SystemTime),
}

/// Read the whole file at `path` as UTF-8 text, paired with its current on-disk
/// `mtime`.
///
/// `path` is normally **relative** to the canonicalized worktree `root` and is
/// confined to it: an escape (`..` or a symlink pointing out) is refused with
/// [`BufferError::PathEscape`], never read.
///
/// As the **out-of-root read carve-out** (`spec-lsp-navigation.md`,
/// 2026-06-12), an **absolute** `path` is served **read-only**: it is the
/// daemon-side path of a navigation target outside the worktree root (a stdlib
/// or dependency file the editor jumped to, carried as
/// [`rift_protocol::NavLocation`] with `out_of_root = true`). Such a path is
/// read whole and returned, but never written — [`write_file`] refuses every
/// absolute path, so the carve-out can never widen into a write outside the
/// root. Read-only confinement is enforced client-side off `out_of_root`; the
/// daemon serves the bytes on a single-user remote (the accepted threat model).
///
/// Non-UTF-8 content is refused with [`BufferError::NotUtf8`] rather than
/// mangled, on both the in-root and out-of-root read paths.
pub async fn read_file(root: &Path, path: &str) -> Result<(String, SystemTime), BufferError> {
    let resolved = resolve_read(root, path)?;

    let bytes = tokio::fs::read(&resolved)
        .await
        .map_err(|source| BufferError::Io {
            path: path.to_owned(),
            source,
        })?;
    let content = String::from_utf8(bytes).map_err(|_| BufferError::NotUtf8(path.to_owned()))?;
    let mtime = mtime_of(&resolved, path).await?;

    Ok((content, mtime))
}

/// Write `content` to the file at `rel_path` (relative to `root`) atomically,
/// guarding against a concurrent change.
///
/// If the on-disk `mtime` is newer than `base_mtime` the file changed under the
/// editor: the write is rejected and the file left untouched, returning
/// [`SaveOutcome::Conflict`] with the current on-disk `mtime`. Otherwise the
/// content is written to a temp file in the target's own directory and renamed
/// over the target (atomic on a single filesystem), returning
/// [`SaveOutcome::Saved`] with the new on-disk `mtime`.
///
/// `content` is UTF-8 (the protocol carries a `String`), so binary is impossible
/// by construction on the write path; the type is the guard. The path is
/// confined to `root` exactly as [`read_file`] confines it.
pub async fn write_file(
    root: &Path,
    rel_path: &str,
    content: &str,
    base_mtime: SystemTime,
) -> Result<SaveOutcome, BufferError> {
    let resolved = resolve(root, rel_path)?;

    // Conflict check: compare the base the editor read against the current
    // on-disk mtime. A file that vanished since the open (NotFound) is no
    // conflict — the save recreates it. Any other stat error is surfaced.
    match mtime_of(&resolved, rel_path).await {
        Ok(disk_mtime) => {
            if disk_mtime > base_mtime {
                // Newer on disk than the editor's base: reject, do not clobber.
                return Ok(SaveOutcome::Conflict(disk_mtime));
            }
        }
        Err(BufferError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(other) => return Err(other),
    }

    // The parent must exist to hold the temp file and receive the rename. The
    // resolver confined every ancestor under the root, so creating any missing
    // intermediate directory stays inside the watched tree — this lets a save
    // land into a directory the editor navigated to even if it was created
    // since the scan, without ever stepping outside the root.
    let parent = resolved
        .parent()
        .ok_or_else(|| BufferError::PathEscape(rel_path.to_owned()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|source| BufferError::Io {
            path: rel_path.to_owned(),
            source,
        })?;
    let temp = temp_path(&resolved);

    write_then_rename(&temp, &resolved, content.as_bytes(), rel_path).await?;

    let mtime = mtime_of(&resolved, rel_path).await?;
    Ok(SaveOutcome::Saved(mtime))
}

/// Write `content` to `temp` then rename it over `target`. On any failure the
/// partial temp file is best-effort removed so a crashed write leaves no litter.
async fn write_then_rename(
    temp: &Path,
    target: &Path,
    content: &[u8],
    rel_path: &str,
) -> Result<(), BufferError> {
    let io_err = |source: std::io::Error| BufferError::Io {
        path: rel_path.to_owned(),
        source,
    };

    if let Err(source) = tokio::fs::write(temp, content).await {
        let _ = tokio::fs::remove_file(temp).await;
        return Err(io_err(source));
    }
    if let Err(source) = tokio::fs::rename(temp, target).await {
        let _ = tokio::fs::remove_file(temp).await;
        return Err(io_err(source));
    }
    Ok(())
}

/// The on-disk `mtime` of `resolved`, mapped to a [`BufferError`] carrying the
/// worktree-relative path. The same `SystemTime` the worktree snapshot reads
/// from `metadata().modified()`.
async fn mtime_of(resolved: &Path, rel_path: &str) -> Result<SystemTime, BufferError> {
    let metadata = tokio::fs::metadata(resolved)
        .await
        .map_err(|source| BufferError::Io {
            path: rel_path.to_owned(),
            source,
        })?;
    metadata.modified().map_err(|source| BufferError::Io {
        path: rel_path.to_owned(),
        source,
    })
}

/// The temp-file path for an atomic write: a sibling of `target` in the same
/// directory (so the `rename` stays on one filesystem), prefixed with a dot and
/// suffixed `.rift-tmp` to stay out of the way of an editor that might list the
/// directory.
fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp_name = format!(".{name}.rift-tmp");
    match target.parent() {
        Some(parent) => parent.join(temp_name),
        None => PathBuf::from(temp_name),
    }
}

/// Resolve `rel_path` against the canonicalized worktree `root`, refusing any
/// path that escapes it.
///
/// The defense is layered:
///
/// 1. Reject up front a path with no normal components, an absolute segment, a
///    `..`, or a Windows prefix / root-dir component — a textual escape that
///    never needs to touch disk.
/// 2. Canonicalize the *existing* leading portion of the resolved path (the file
///    itself if it exists, else its nearest existing ancestor) and require the
///    result to stay under `root`. This catches a **symlink** whose target points
///    outside the root, which the textual check cannot see.
///
/// `root` is assumed already canonicalized (it is `Snapshot::root()`); the
/// confinement compares canonical prefixes, so a symlinked root is handled
/// consistently.
fn resolve(root: &Path, rel_path: &str) -> Result<PathBuf, BufferError> {
    let rel = Path::new(rel_path);
    let escape = || BufferError::PathEscape(rel_path.to_owned());

    // Textual guard: only plain forward path segments are allowed. An empty
    // path, an absolute one, a `..`, or a root/prefix component is an escape.
    let mut has_component = false;
    for component in rel.components() {
        match component {
            Component::Normal(_) => has_component = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(escape());
            }
        }
    }
    if !has_component {
        return Err(escape());
    }

    let resolved = root.join(rel);

    // Symlink guard: canonicalize the longest existing prefix and require it to
    // stay under the root. For an existing file this canonicalizes the file
    // itself (a symlink leaf resolves to its target); for a not-yet-existing
    // save target it canonicalizes the nearest existing ancestor (its parent
    // directory), catching a symlinked directory that points out.
    let existing = nearest_existing(&resolved);
    let canonical = existing.canonicalize().map_err(|source| BufferError::Io {
        path: rel_path.to_owned(),
        source,
    })?;
    if !canonical.starts_with(root) {
        return Err(escape());
    }

    Ok(resolved)
}

/// Resolve `path` for a **read**, splitting on the out-of-root carve-out.
///
/// - A **relative** `path` is confined to `root` exactly as a write is
///   ([`resolve`]): `..` and symlink escapes are refused.
/// - An **absolute** `path` is the out-of-root carve-out: it is a navigation
///   target outside the worktree (a [`rift_protocol::NavLocation`] with
///   `out_of_root = true`) and is served **read-only**. It is returned as-is for
///   reading; no confinement check applies because the target is deliberately
///   outside the root, and no write path ever accepts it ([`write_file`] always
///   routes through [`resolve`], which refuses absolute paths). An empty path is
///   still an escape.
///
/// This is the read-only counterpart to [`resolve`]: writes stay confined to the
/// root, reads gain the out-of-root carve-out.
fn resolve_read(root: &Path, path: &str) -> Result<PathBuf, BufferError> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        // Out-of-root read carve-out: served read-only, no confinement check.
        // Never reachable from the write path — `write_file` uses `resolve`,
        // which refuses any absolute path, so this can never widen a write.
        return Ok(candidate.to_path_buf());
    }
    resolve(root, path)
}

/// The longest existing ancestor of `path` (including `path` itself when it
/// exists). Used to canonicalize the on-disk portion of a path whose leaf may not
/// exist yet (a save target), so the symlink check can run against real inodes.
/// The root always exists, so this never walks above it for a confined path.
fn nearest_existing(path: &Path) -> PathBuf {
    let mut current = path;
    loop {
        if current.exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return current.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// A self-cleaning temporary directory, mirroring the explorer / daemon test
    /// helpers so this module needs no `tempfile` dev-dependency.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("rift-buffer-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            // Canonicalize so the confinement checks (which compare against the
            // canonical root) match — the system temp dir is a symlink on macOS
            // and some Linux setups.
            let path = path.canonicalize().expect("canonicalize temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn write_disk(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    #[tokio::test]
    async fn test_read_file_returns_content_and_mtime() {
        let tmp = TempDir::new("read");
        let file = tmp.path.join("src/main.rs");
        write_disk(&file, b"fn main() {}\n");
        let on_disk = std::fs::metadata(&file)
            .expect("stat")
            .modified()
            .expect("mtime");

        let (content, mtime) = read_file(&tmp.path, "src/main.rs")
            .await
            .expect("read succeeds");
        assert_eq!(content, "fn main() {}\n");
        assert_eq!(mtime, on_disk);
    }

    #[tokio::test]
    async fn test_read_file_missing_yields_io_error() {
        let tmp = TempDir::new("read-missing");
        let err = read_file(&tmp.path, "nope.rs")
            .await
            .expect_err("missing file errors");
        match err {
            BufferError::Io { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_read_file_non_utf8_is_refused() {
        let tmp = TempDir::new("read-binary");
        // An invalid UTF-8 byte sequence (lone continuation byte 0xff).
        write_disk(&tmp.path.join("blob.bin"), &[0x00, 0xff, 0xfe, b'a']);
        let err = read_file(&tmp.path, "blob.bin")
            .await
            .expect_err("binary content is refused");
        assert!(matches!(err, BufferError::NotUtf8(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn test_write_file_creates_new_file_atomically_and_returns_mtime() {
        let tmp = TempDir::new("write-new");
        // base_mtime is irrelevant for a not-yet-existing file: the save creates
        // it. UNIX_EPOCH is a valid "I have no base" sentinel here.
        let outcome = write_file(
            &tmp.path,
            "src/new.rs",
            "pub fn x() {}\n",
            SystemTime::UNIX_EPOCH,
        )
        .await
        .expect("write succeeds");
        let saved_mtime = match outcome {
            SaveOutcome::Saved(m) => m,
            other => panic!("expected Saved, got {other:?}"),
        };

        let written = std::fs::read_to_string(tmp.path.join("src/new.rs")).expect("read back");
        assert_eq!(written, "pub fn x() {}\n");
        let on_disk = std::fs::metadata(tmp.path.join("src/new.rs"))
            .expect("stat")
            .modified()
            .expect("mtime");
        assert_eq!(saved_mtime, on_disk);

        // No temp litter left behind.
        let leftover: Vec<_> = std::fs::read_dir(tmp.path.join("src"))
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("rift-tmp"))
            .collect();
        assert!(leftover.is_empty(), "atomic write left a temp file behind");
    }

    #[tokio::test]
    async fn test_write_file_overwrites_when_base_mtime_matches_disk() {
        let tmp = TempDir::new("write-over");
        let file = tmp.path.join("a.txt");
        write_disk(&file, b"v1");
        let base = std::fs::metadata(&file)
            .expect("stat")
            .modified()
            .expect("mtime");

        // base == disk: not stale, so the write lands.
        let outcome = write_file(&tmp.path, "a.txt", "v2", base)
            .await
            .expect("write succeeds");
        assert!(matches!(outcome, SaveOutcome::Saved(_)), "got {outcome:?}");
        assert_eq!(std::fs::read_to_string(&file).expect("read back"), "v2");
    }

    /// The load-bearing acceptance test: a stale `base_mtime` (an external write
    /// bumped the on-disk mtime after the editor read it) is rejected with a
    /// conflict and the file is left untouched — no clobber.
    #[tokio::test]
    async fn test_write_file_stale_base_mtime_yields_conflict_and_leaves_file_untouched() {
        let tmp = TempDir::new("write-conflict");
        let file = tmp.path.join("shared.rs");
        write_disk(&file, b"editor read this\n");
        // The base the editor captured on open.
        let base = std::fs::metadata(&file)
            .expect("stat")
            .modified()
            .expect("mtime");

        // An external writer (an agent in a pane) bumps the on-disk mtime to a
        // strictly later instant and changes the content — deterministically, no
        // sleep.
        let agent_content = b"agent wrote this instead\n";
        std::fs::write(&file, agent_content).expect("external write");
        let bumped = base + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open to bump mtime")
            .set_modified(bumped)
            .expect("set mtime");

        // The editor saves with its now-stale base: must conflict, not clobber.
        let outcome = write_file(&tmp.path, "shared.rs", "editor's edit\n", base)
            .await
            .expect("save returns an outcome");
        match outcome {
            SaveOutcome::Conflict(disk_mtime) => {
                assert_eq!(
                    disk_mtime, bumped,
                    "conflict reports the current on-disk mtime"
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        // The file still holds the agent's content — the editor's edit was
        // rejected, the newer on-disk version untouched.
        assert_eq!(
            std::fs::read(&file).expect("read back"),
            agent_content,
            "a stale-base save must not clobber the newer on-disk file"
        );
    }

    #[tokio::test]
    async fn test_read_rejects_parent_dir_escape() {
        let tmp = TempDir::new("escape-parent");
        // A secret sibling outside the root the relative path tries to climb to.
        let outside = tmp.path.parent().expect("temp has a parent");
        let secret = outside.join("rift-buffer-secret.txt");
        std::fs::write(&secret, b"top secret").expect("write secret");

        let err = read_file(&tmp.path, "../rift-buffer-secret.txt")
            .await
            .expect_err("parent escape is refused");
        assert!(matches!(err, BufferError::PathEscape(_)), "got {err:?}");

        let _ = std::fs::remove_file(&secret);
    }

    #[tokio::test]
    async fn test_write_rejects_absolute_path() {
        // The write path keeps refusing absolute paths — the out-of-root
        // carve-out is read-only. (An absolute *read* is served read-only; see
        // `test_out_of_root_read_file_absolute_path_is_served_read_only`.)
        let tmp = TempDir::new("escape-abs");
        let err = write_file(
            &tmp.path,
            "/etc/rift-buffer-should-not-write.txt",
            "should not land",
            SystemTime::UNIX_EPOCH,
        )
        .await
        .expect_err("absolute path is refused on write");
        assert!(matches!(err, BufferError::PathEscape(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn test_read_rejects_empty_path() {
        let tmp = TempDir::new("escape-empty");
        let err = read_file(&tmp.path, "")
            .await
            .expect_err("empty path is refused");
        assert!(matches!(err, BufferError::PathEscape(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_read_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new("escape-symlink");
        // A secret outside the root.
        let outside = tmp.path.parent().expect("temp has a parent");
        let secret = outside.join("rift-buffer-symlink-secret.txt");
        std::fs::write(&secret, b"top secret").expect("write secret");
        // A symlink *inside* the root pointing at it — the textual check cannot
        // see this escape; the canonicalize guard must.
        symlink(&secret, tmp.path.join("link.txt")).expect("create symlink");

        let err = read_file(&tmp.path, "link.txt")
            .await
            .expect_err("symlink escape is refused");
        assert!(matches!(err, BufferError::PathEscape(_)), "got {err:?}");

        let _ = std::fs::remove_file(&secret);
    }

    #[tokio::test]
    async fn test_write_rejects_parent_dir_escape_and_leaves_target_untouched() {
        let tmp = TempDir::new("escape-write");
        let outside = tmp.path.parent().expect("temp has a parent");
        let victim = outside.join("rift-buffer-write-victim.txt");
        std::fs::write(&victim, b"original").expect("write victim");

        let err = write_file(
            &tmp.path,
            "../rift-buffer-write-victim.txt",
            "clobbered",
            SystemTime::UNIX_EPOCH,
        )
        .await
        .expect_err("parent escape is refused on write");
        assert!(matches!(err, BufferError::PathEscape(_)), "got {err:?}");
        // The out-of-root file is untouched.
        assert_eq!(
            std::fs::read(&victim).expect("read back"),
            b"original",
            "a refused escaping write must not touch the target"
        );

        let _ = std::fs::remove_file(&victim);
    }

    #[tokio::test]
    async fn test_write_then_read_roundtrips_content() {
        let tmp = TempDir::new("roundtrip");
        let outcome = write_file(
            &tmp.path,
            "dir/file.rs",
            "let x = 1;\n",
            SystemTime::UNIX_EPOCH,
        )
        .await
        .expect("write");
        let saved = match outcome {
            SaveOutcome::Saved(m) => m,
            other => panic!("expected Saved, got {other:?}"),
        };
        let (content, mtime) = read_file(&tmp.path, "dir/file.rs").await.expect("read");
        assert_eq!(content, "let x = 1;\n");
        assert_eq!(
            mtime, saved,
            "read-back mtime matches the save's reported mtime"
        );
    }

    /// Out-of-root carve-out: a `SaveFile` with an absolute path (the wire
    /// representation for out-of-root definition targets) is refused daemon-side.
    /// This is the acceptance-level test for the spec's "out-of-root `SaveFile`
    /// is refused daemon-side" item (issue #195).
    #[tokio::test]
    async fn test_out_of_root_save_file_is_refused_with_absolute_path() {
        let tmp = TempDir::new("out-of-root-save");
        // An absolute path simulates what the editor would send when the user
        // attempts to save a buffer opened from an out-of-root definition jump
        // (e.g. a stdlib or registry file at an absolute path).
        let err = write_file(
            &tmp.path,
            "/etc/rift-absolutely-should-not-write-here.txt",
            "should not land",
            SystemTime::UNIX_EPOCH,
        )
        .await
        .expect_err("out-of-root absolute-path save must be refused");
        assert!(
            matches!(err, BufferError::PathEscape(_)),
            "expected PathEscape, got {err:?}"
        );
    }

    /// Out-of-root carve-out: an absolute path **outside** the worktree root is
    /// served read-only — the daemon returns its content so the editor can open
    /// an out-of-root navigation target (a stdlib/dependency definition) as a
    /// read-only buffer. The path lives outside `root` and is `out_of_root` on
    /// the wire; the read is the accepted single-user-remote carve-out. This is
    /// the acceptance-level test for the spec's "out-of-root reads served
    /// read-only" item (issue #195).
    #[tokio::test]
    async fn test_out_of_root_read_file_absolute_path_is_served_read_only() {
        let tmp = TempDir::new("out-of-root-read-served");
        // A file outside the worktree root (a sibling of it), simulating a
        // stdlib/dependency file an absolute NavLocation points at.
        let outside = tmp.path.parent().expect("temp has a parent");
        let target = outside.join("rift-buffer-out-of-root-dep.rs");
        std::fs::write(&target, b"pub fn dep() {}\n").expect("write out-of-root file");
        let on_disk = std::fs::metadata(&target)
            .expect("stat")
            .modified()
            .expect("mtime");

        let abs = target.to_str().expect("utf-8 path");
        let (content, mtime) = read_file(&tmp.path, abs)
            .await
            .expect("out-of-root absolute read is served read-only");
        assert_eq!(
            content, "pub fn dep() {}\n",
            "out-of-root read returns the file content"
        );
        assert_eq!(mtime, on_disk, "out-of-root read reports the on-disk mtime");

        let _ = std::fs::remove_file(&target);
    }

    /// The carve-out is **read-only**: the exact same out-of-root absolute path
    /// that `read_file` serves is **refused** by `write_file` and the target is
    /// left untouched — the read carve-out can never widen into a write outside
    /// the root.
    #[tokio::test]
    async fn test_out_of_root_absolute_path_read_served_but_write_refused() {
        let tmp = TempDir::new("out-of-root-read-write");
        let outside = tmp.path.parent().expect("temp has a parent");
        let target = outside.join("rift-buffer-out-of-root-readonly.rs");
        std::fs::write(&target, b"original out-of-root\n").expect("write out-of-root file");
        let abs = target.to_str().expect("utf-8 path");

        // Read is served.
        let (content, _mtime) = read_file(&tmp.path, abs)
            .await
            .expect("out-of-root read is served");
        assert_eq!(content, "original out-of-root\n");

        // Write of the same absolute path is refused as a path escape.
        let err = write_file(&tmp.path, abs, "clobbered", SystemTime::UNIX_EPOCH)
            .await
            .expect_err("out-of-root absolute-path write must be refused");
        assert!(
            matches!(err, BufferError::PathEscape(_)),
            "expected PathEscape, got {err:?}"
        );
        // The out-of-root file is untouched by the refused write.
        assert_eq!(
            std::fs::read(&target).expect("read back"),
            b"original out-of-root\n",
            "a refused out-of-root write must not touch the target"
        );

        let _ = std::fs::remove_file(&target);
    }
}
