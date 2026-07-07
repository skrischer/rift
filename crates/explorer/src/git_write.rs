//! Source-control write operations over a worktree root, applied with `gix`.
//!
//! These are the write half of `docs/spec-source-control-write.md`: the four
//! file-level ops the daemon exposes over the protocol. Each mutates the user's
//! index or worktree and then relies on the daemon's existing push recompute
//! (triggered by the `.git/index` watcher) to converge every view — the ops
//! themselves return only success or a human-readable error, never state.
//!
//! - [`stage_file`] writes the current worktree blob into the index (`git add`
//!   semantics: an add for an untracked path, autocrlf/clean filters applied via
//!   gix's pipeline; a worktree-absent path stages its deletion).
//! - [`unstage_file`] restores the index entry from HEAD (removing it for a path
//!   that has no HEAD entry — a newly staged add).
//! - [`discard_file`] restores the worktree file from the index (checkout-file
//!   semantics: unstaged edits reverted, staged content kept; an untracked path,
//!   absent from the index, is removed). **Destructive** — the client gates it
//!   behind a confirm dialog and never batches it.
//! - [`commit`] builds a tree from the index (gix `tree-editor`), commits it with
//!   `parents = [HEAD]` and the config identity, rejecting an empty message or a
//!   nothing-staged index (index tree == HEAD tree).
//!
//! Every index mutation writes the index atomically through gix's lock-file
//! commit (`.git/index.lock` -> rename), so a failure never leaves a half-written
//! index. A lock already held by a live agent gets one bounded retry before the
//! op errors cleanly. Pure-Rust and `gpui`-free; `git2`/`libgit2` is deliberately
//! absent (the static-musl daemon constraint), and no `git` binary is spawned.

use std::path::Path;
use std::time::Duration;

use gix::bstr::{BStr, BString};
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::objs::tree::EntryKind;

use crate::{ExplorerError, Result};

/// How long to wait before the single retry when `.git/index.lock` is already
/// held (a live agent writing concurrently) before the op errors out.
const INDEX_LOCK_RETRY: Duration = Duration::from_millis(50);

/// Stage the whole file at `relative` (a path relative to `root`, the same key
/// space as the worktree entries): write its current worktree content into the
/// index. An untracked path is added; a tracked one is updated (autocrlf/clean
/// filters applied via gix's pipeline, exec bit and symlink target preserved);
/// a path absent from the worktree stages its deletion (removed from the index).
pub fn stage_file(root: &Path, relative: &Path) -> Result<()> {
    let repo = open(root)?;
    let rela = rela_path(relative);
    let mut index = repo.open_index().map_err(git_err("open index"))?;
    let (mut pipeline, _persisted) = repo
        .filter_pipeline(None)
        .map_err(git_err("filter pipeline"))?;

    let object = pipeline
        .worktree_file_to_object(rela.as_ref(), &index)
        .map_err(|e| path_err("stage", relative, e))?;
    match object {
        Some((id, kind, _md)) => set_index_entry(&mut index, rela.as_ref(), id, kind_to_mode(kind)),
        // Absent from the worktree (or an untrackable type): staging removes the
        // index entry, i.e. stages the deletion.
        None => remove_index_entry(&mut index, rela.as_ref()),
    }
    // The tree-cache extension is stale after any entry mutation; drop it so a
    // later `write-tree` (ours or git's) never trusts a wrong cached subtree.
    index.remove_tree();
    write_index(&mut index)
}

/// Unstage the whole file at `relative`: restore its index entry from HEAD. A
/// path that has no HEAD entry (a newly staged add) is removed from the index,
/// leaving it untracked. The worktree file is untouched.
pub fn unstage_file(root: &Path, relative: &Path) -> Result<()> {
    let repo = open(root)?;
    let rela = rela_path(relative);
    let mut index = repo.open_index().map_err(git_err("open index"))?;

    let head_tree = repo
        .head_tree_id_or_empty()
        .map_err(git_err("head tree"))?
        .object()
        .map_err(git_err("read head tree"))?
        .try_into_tree()
        .map_err(git_err("head is not a tree"))?;
    match head_tree
        .lookup_entry_by_path(relative)
        .map_err(|e| path_err("head lookup", relative, e))?
    {
        Some(entry) => {
            let mode = kind_to_mode(entry.mode().kind());
            set_index_entry(&mut index, rela.as_ref(), entry.object_id(), mode);
        }
        // No HEAD entry: the file was newly added, so unstaging removes it from
        // the index entirely (it becomes untracked again).
        None => remove_index_entry(&mut index, rela.as_ref()),
    }
    index.remove_tree();
    write_index(&mut index)
}

/// Discard the worktree edits to `relative`: restore its worktree content from
/// the index (checkout-file semantics). A tracked path is rewritten from its
/// index blob (unstaged edits reverted, staged content preserved); an untracked
/// path — absent from the index — is removed from the worktree. **Destructive.**
/// The index is never modified.
pub fn discard_file(root: &Path, relative: &Path) -> Result<()> {
    let repo = open(root)?;
    let rela = rela_path(relative);
    let index = repo.open_index().map_err(git_err("open index"))?;
    let abs = root.join(relative);

    match index.entry_by_path(rela.as_ref()) {
        Some(entry) => {
            let content = read_blob(&repo, entry.id, relative)?;
            let kind = entry
                .mode
                .to_tree_entry_mode()
                .map(|mode| mode.kind())
                .ok_or_else(|| {
                    ExplorerError::GitError(format!(
                        "unsupported index mode for {}",
                        relative.display()
                    ))
                })?;
            restore_worktree_file(&abs, &content, kind, relative)
        }
        // Absent from the index: an untracked file — discarding removes it.
        None => remove_worktree_path(&abs, relative),
    }
}

/// Commit the currently staged index: build a tree from it (gix `tree-editor`),
/// commit with `parents = [HEAD]` and the repo's config identity. Rejects an
/// empty/whitespace-only message, an index with unmerged (conflicted) entries,
/// or a nothing-staged state (the index tree equals the HEAD tree) — never a
/// partial commit.
pub fn commit(root: &Path, message: &str) -> Result<()> {
    if message.trim().is_empty() {
        return Err(ExplorerError::GitError("commit message is empty".into()));
    }

    let repo = open(root)?;
    let index = repo.open_index().map_err(git_err("open index"))?;
    if index
        .entries()
        .iter()
        .any(|entry| entry.stage() != Stage::Unconflicted)
    {
        return Err(ExplorerError::GitError(
            "cannot commit with unmerged (conflicted) index entries — resolve conflicts first"
                .into(),
        ));
    }

    let tree_id = build_index_tree(&repo, &index)?;
    let head_tree_id = repo
        .head_tree_id_or_empty()
        .map_err(git_err("head tree"))?
        .detach();
    if tree_id == head_tree_id {
        return Err(ExplorerError::GitError("nothing staged to commit".into()));
    }

    // `parents = [HEAD]` for a normal repo; an unborn HEAD (no commits yet) has
    // none, so this becomes the initial commit.
    let parents: Vec<gix::ObjectId> = match repo.head_id() {
        Ok(id) => vec![id.detach()],
        Err(_) => Vec::new(),
    };
    repo.commit("HEAD", message, tree_id, parents)
        .map_err(git_err("commit"))?;
    Ok(())
}

/// Build a tree object from every entry in `index` via the tree editor, seeded
/// from the empty tree, and return its id. The index is the single source of the
/// staged state, so upserting each entry reproduces exactly what a commit of the
/// index would contain.
fn build_index_tree(repo: &gix::Repository, index: &gix::index::State) -> Result<gix::ObjectId> {
    let empty = gix::ObjectId::empty_tree(repo.object_hash());
    let mut editor = repo.edit_tree(empty).map_err(git_err("open tree editor"))?;
    for entry in index.entries() {
        let path = entry.path(index);
        let kind = entry
            .mode
            .to_tree_entry_mode()
            .map(|mode| mode.kind())
            .ok_or_else(|| ExplorerError::GitError(format!("unsupported index mode for {path}")))?;
        editor
            .upsert(path, kind, entry.id)
            .map_err(|e| ExplorerError::GitError(format!("tree upsert {path}: {e}")))?;
    }
    Ok(editor.write().map_err(git_err("write tree"))?.detach())
}

/// Replace any entries at `rela` (across all stages) with a single stage-0 entry
/// pointing at `id`/`mode`, then re-sort so path lookups stay valid.
///
/// The stat is left zeroed: gix's status then compares the worktree content
/// against `id` directly for this one path (a mismatched stat forces a content
/// check), which reports the correct staged/unstaged split without depending on
/// filesystem timestamps. Only this entry is touched, so the rest of the index
/// keeps its stat and the next recompute reads just this file.
fn set_index_entry(index: &mut gix::index::File, rela: &BStr, id: gix::ObjectId, mode: Mode) {
    remove_index_entry(index, rela);
    index.dangerously_push_entry(Stat::default(), id, Flags::empty(), mode, rela);
    index.sort_entries();
}

/// Remove every index entry at `rela`, across all stages.
fn remove_index_entry(index: &mut gix::index::File, rela: &BStr) {
    index.remove_entries(|_, path, _| path == rela);
}

/// Write the index to `.git/index` through gix's lock-file commit (atomic
/// rename). If the lock is already held (a live agent), wait briefly and retry
/// once before surfacing a clean error.
fn write_index(index: &mut gix::index::File) -> Result<()> {
    match index.write(gix::index::write::Options::default()) {
        Ok(()) => Ok(()),
        Err(gix::index::file::write::Error::AcquireLock(_)) => {
            std::thread::sleep(INDEX_LOCK_RETRY);
            index
                .write(gix::index::write::Options::default())
                .map_err(|e| ExplorerError::GitError(format!("write index after lock retry: {e}")))
        }
        Err(e) => Err(ExplorerError::GitError(format!("write index: {e}"))),
    }
}

/// Read a blob's raw content by id — the index-staged content restored to the
/// worktree by [`discard_file`].
fn read_blob(repo: &gix::Repository, id: gix::ObjectId, relative: &Path) -> Result<Vec<u8>> {
    let mut blob = repo
        .find_object(id)
        .map_err(|e| path_err("read index blob", relative, e))?
        .try_into_blob()
        .map_err(|e| path_err("index object is not a blob", relative, e))?;
    Ok(blob.take_data())
}

/// Restore `abs` from `content`, replacing any existing file/symlink so a type
/// change restores cleanly. A `Link` writes a symlink to the target bytes; an
/// executable blob restores the exec bit; a regular blob is a plain write.
fn restore_worktree_file(
    abs: &Path,
    content: &[u8],
    kind: EntryKind,
    relative: &Path,
) -> Result<()> {
    remove_if_exists(abs, relative)?;
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| path_err("create parent dirs", relative, e))?;
    }
    match kind {
        EntryKind::Link => write_symlink(abs, content, relative),
        EntryKind::BlobExecutable => {
            std::fs::write(abs, content).map_err(|e| path_err("restore file", relative, e))?;
            set_executable(abs, relative)
        }
        _ => std::fs::write(abs, content).map_err(|e| path_err("restore file", relative, e)),
    }
}

/// Remove `abs` if it exists (file or symlink); an already-absent path is a
/// no-op. Used before restoring so a regular-file <-> symlink type change is
/// clean.
fn remove_if_exists(abs: &Path, relative: &Path) -> Result<()> {
    match std::fs::symlink_metadata(abs) {
        Ok(_) => {
            std::fs::remove_file(abs).map_err(|e| path_err("remove worktree file", relative, e))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(path_err("stat worktree file", relative, e)),
    }
}

/// Remove `abs` (discarding an untracked file); an already-absent path is a
/// no-op.
fn remove_worktree_path(abs: &Path, relative: &Path) -> Result<()> {
    match std::fs::remove_file(abs) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(path_err("remove untracked file", relative, e)),
    }
}

#[cfg(unix)]
fn write_symlink(abs: &Path, target: &[u8], relative: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let target = std::ffi::OsStr::from_bytes(target);
    std::os::unix::fs::symlink(target, abs).map_err(|e| path_err("restore symlink", relative, e))
}

#[cfg(not(unix))]
fn write_symlink(abs: &Path, target: &[u8], relative: &Path) -> Result<()> {
    // A non-unix host cannot create a POSIX symlink from raw target bytes; write
    // the target as a regular file so no partial state is left. The daemon runs
    // on Linux, so this branch only matters for host-side test builds.
    std::fs::write(abs, target).map_err(|e| path_err("restore symlink target", relative, e))
}

#[cfg(unix)]
fn set_executable(abs: &Path, relative: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(abs)
        .map_err(|e| path_err("stat restored file", relative, e))?
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(abs, perms).map_err(|e| path_err("set exec bit", relative, e))
}

#[cfg(not(unix))]
fn set_executable(_abs: &Path, _relative: &Path) -> Result<()> {
    Ok(())
}

/// Map a tree entry kind to the matching index mode.
fn kind_to_mode(kind: EntryKind) -> Mode {
    match kind {
        EntryKind::Blob => Mode::FILE,
        EntryKind::BlobExecutable => Mode::FILE_EXECUTABLE,
        EntryKind::Link => Mode::SYMLINK,
        EntryKind::Commit => Mode::COMMIT,
        EntryKind::Tree => Mode::DIR,
    }
}

/// The repo-relative, slash-separated path gix's index/tree APIs expect.
fn rela_path(relative: &Path) -> BString {
    gix::path::to_unix_separators_on_windows(gix::path::into_bstr(relative)).into_owned()
}

fn open(root: &Path) -> Result<gix::Repository> {
    gix::open(root).map_err(|e| ExplorerError::GitError(format!("open {}: {e}", root.display())))
}

/// Map any gix error into an [`ExplorerError::GitError`] prefixed with `context`.
fn git_err<E: std::fmt::Display>(context: &'static str) -> impl Fn(E) -> ExplorerError {
    move |e| ExplorerError::GitError(format!("{context}: {e}"))
}

/// Like [`git_err`], but names the offending path.
fn path_err(context: &'static str, relative: &Path, e: impl std::fmt::Display) -> ExplorerError {
    ExplorerError::GitError(format!("{context} {}: {e}", relative.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GitStatus, GitStatusCode};
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A self-cleaning temp dir, mirroring the `git.rs`/`diff.rs` test helpers so
    /// this file stays self-contained without a `tempfile` dev-dependency.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("rift-git-write-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            // Canonicalize so gix and our joins agree on the root (the system
            // temp dir is a symlink on some setups).
            let path = path.canonicalize().expect("canonicalize temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Run a git command in `dir`, asserting success — ground truth for the
    /// fixtures, mirroring `git.rs`. git is present in CI and dev.
    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        let output = Command::new("git")
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
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// An initialized repo with one committed file on `main` and a config
    /// identity, so HEAD exists and `commit` can resolve an author/committer.
    fn init_repo(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        git(&tmp.path, &["config", "user.name", "t"]);
        git(&tmp.path, &["config", "user.email", "t@t"]);
        write(&tmp.path.join("tracked.txt"), b"one\ntwo\nthree\n");
        git(&tmp.path, &["add", "tracked.txt"]);
        git(&tmp.path, &["commit", "-q", "-m", "init"]);
        tmp
    }

    fn entry(root: &Path, rel: &str) -> Option<crate::GitEntryStatus> {
        GitStatus::compute(root)
            .expect("compute status")
            .get(Path::new(rel))
    }

    /// `git status --porcelain` for one path's `XY`, or `None` when git reports
    /// it clean — ground truth for the staged/unstaged split.
    fn porcelain_xy(root: &Path, rel: &str) -> Option<String> {
        let out = git(root, &["status", "--porcelain", "--", rel]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|line| line[..2].to_string())
    }

    #[test]
    fn test_stage_tracked_modification_moves_change_to_index() {
        let repo = init_repo("stage-mod");
        write(&repo.path.join("tracked.txt"), b"one\nTWO\nthree\n");

        stage_file(&repo.path, Path::new("tracked.txt")).expect("stage");

        assert_eq!(
            porcelain_xy(&repo.path, "tracked.txt").as_deref(),
            Some("M ")
        );
        assert_eq!(
            entry(&repo.path, "tracked.txt"),
            Some(crate::GitEntryStatus {
                index: GitStatusCode::Modified,
                worktree: GitStatusCode::Unmodified,
            })
        );
    }

    #[test]
    fn test_stage_untracked_file_adds_it_to_index() {
        let repo = init_repo("stage-untracked");
        write(&repo.path.join("new.txt"), b"fresh\n");

        stage_file(&repo.path, Path::new("new.txt")).expect("stage");

        assert_eq!(porcelain_xy(&repo.path, "new.txt").as_deref(), Some("A "));
        assert_eq!(
            entry(&repo.path, "new.txt").map(|s| s.index),
            Some(GitStatusCode::Added)
        );
    }

    #[test]
    fn test_stage_deleted_file_stages_the_removal() {
        let repo = init_repo("stage-deleted");
        std::fs::remove_file(repo.path.join("tracked.txt")).expect("remove");

        stage_file(&repo.path, Path::new("tracked.txt")).expect("stage");

        assert_eq!(
            porcelain_xy(&repo.path, "tracked.txt").as_deref(),
            Some("D ")
        );
        assert_eq!(
            entry(&repo.path, "tracked.txt").map(|s| s.index),
            Some(GitStatusCode::Deleted)
        );
    }

    #[test]
    fn test_stage_preserves_other_entries() {
        // Staging one path must not disturb another already-staged path — the
        // surgical single-entry edit, not a full index rebuild.
        let repo = init_repo("stage-preserve");
        write(&repo.path.join("a.txt"), b"a\n");
        write(&repo.path.join("b.txt"), b"b\n");
        git(&repo.path, &["add", "a.txt"]);

        stage_file(&repo.path, Path::new("b.txt")).expect("stage b");

        assert_eq!(
            entry(&repo.path, "a.txt").map(|s| s.index),
            Some(GitStatusCode::Added),
            "the previously staged path stays staged"
        );
        assert_eq!(
            entry(&repo.path, "b.txt").map(|s| s.index),
            Some(GitStatusCode::Added)
        );
    }

    #[test]
    fn test_unstage_modification_restores_worktree_side() {
        let repo = init_repo("unstage-mod");
        write(&repo.path.join("tracked.txt"), b"one\nTWO\nthree\n");
        git(&repo.path, &["add", "tracked.txt"]);

        unstage_file(&repo.path, Path::new("tracked.txt")).expect("unstage");

        assert_eq!(
            porcelain_xy(&repo.path, "tracked.txt").as_deref(),
            Some(" M")
        );
        assert_eq!(
            entry(&repo.path, "tracked.txt"),
            Some(crate::GitEntryStatus {
                index: GitStatusCode::Unmodified,
                worktree: GitStatusCode::Modified,
            })
        );
    }

    #[test]
    fn test_unstage_newly_added_removes_it_from_index() {
        let repo = init_repo("unstage-add");
        write(&repo.path.join("new.txt"), b"fresh\n");
        git(&repo.path, &["add", "new.txt"]);

        unstage_file(&repo.path, Path::new("new.txt")).expect("unstage");

        // No HEAD entry: unstaging leaves it untracked (`??`), not staged.
        assert_eq!(porcelain_xy(&repo.path, "new.txt").as_deref(), Some("??"));
        assert_eq!(
            entry(&repo.path, "new.txt").map(|s| s.worktree),
            Some(GitStatusCode::Untracked)
        );
    }

    #[test]
    fn test_unstage_deleted_file_restores_index_deletion_to_worktree_side() {
        let repo = init_repo("unstage-deleted");
        std::fs::remove_file(repo.path.join("tracked.txt")).expect("remove");
        git(&repo.path, &["add", "tracked.txt"]); // stage the deletion (`D `)

        unstage_file(&repo.path, Path::new("tracked.txt")).expect("unstage");

        // HEAD still has the blob, so the index is restored to it; the worktree
        // is still missing the file -> unstaged deletion (` D`).
        assert_eq!(
            porcelain_xy(&repo.path, "tracked.txt").as_deref(),
            Some(" D")
        );
    }

    #[test]
    fn test_discard_reverts_unstaged_modification() {
        let repo = init_repo("discard-mod");
        write(&repo.path.join("tracked.txt"), b"one\nEDITED\nthree\n");

        discard_file(&repo.path, Path::new("tracked.txt")).expect("discard");

        assert_eq!(
            std::fs::read(repo.path.join("tracked.txt")).expect("read"),
            b"one\ntwo\nthree\n",
            "the worktree is restored from the index (== HEAD here)"
        );
        assert!(
            entry(&repo.path, "tracked.txt").is_none(),
            "the path is clean after discard"
        );
    }

    #[test]
    fn test_discard_keeps_staged_content() {
        // Stage a change, edit further in the worktree, then discard: the
        // worktree must fall back to the STAGED content, not to HEAD.
        let repo = init_repo("discard-staged");
        write(&repo.path.join("tracked.txt"), b"staged\n");
        git(&repo.path, &["add", "tracked.txt"]);
        write(&repo.path.join("tracked.txt"), b"worktree-edit\n");

        discard_file(&repo.path, Path::new("tracked.txt")).expect("discard");

        assert_eq!(
            std::fs::read(repo.path.join("tracked.txt")).expect("read"),
            b"staged\n",
            "discard restores the staged (index) content, not HEAD"
        );
        assert_eq!(
            entry(&repo.path, "tracked.txt"),
            Some(crate::GitEntryStatus {
                index: GitStatusCode::Modified,
                worktree: GitStatusCode::Unmodified,
            }),
            "the staged change survives; the worktree side is clean"
        );
    }

    #[test]
    fn test_discard_removes_untracked_file() {
        let repo = init_repo("discard-untracked");
        write(&repo.path.join("loose.txt"), b"loose\n");

        discard_file(&repo.path, Path::new("loose.txt")).expect("discard");

        assert!(
            !repo.path.join("loose.txt").exists(),
            "an untracked file is removed by discard"
        );
    }

    #[test]
    fn test_commit_advances_head_and_clears_staged() {
        let repo = init_repo("commit-advance");
        let before = git(&repo.path, &["rev-parse", "HEAD"]);
        let before = String::from_utf8_lossy(&before.stdout).trim().to_owned();
        write(&repo.path.join("tracked.txt"), b"committed\n");
        git(&repo.path, &["add", "tracked.txt"]);

        commit(&repo.path, "change tracked").expect("commit");

        let after = git(&repo.path, &["rev-parse", "HEAD"]);
        let after = String::from_utf8_lossy(&after.stdout).trim().to_owned();
        assert_ne!(before, after, "HEAD advanced");

        let subject = git(&repo.path, &["log", "-1", "--pretty=%s"]);
        assert_eq!(
            String::from_utf8_lossy(&subject.stdout).trim(),
            "change tracked"
        );
        assert!(
            porcelain_xy(&repo.path, "tracked.txt").is_none(),
            "the committed path is clean afterwards"
        );
    }

    #[test]
    fn test_commit_initial_on_unborn_head() {
        // A repo with staged files but no commits yet: the commit has no parent.
        let tmp = TempDir::new("commit-initial");
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        git(&tmp.path, &["config", "user.name", "t"]);
        git(&tmp.path, &["config", "user.email", "t@t"]);
        write(&tmp.path.join("first.txt"), b"first\n");
        git(&tmp.path, &["add", "first.txt"]);

        commit(&tmp.path, "initial").expect("commit");

        let count = git(&tmp.path, &["rev-list", "--count", "HEAD"]);
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
    }

    #[test]
    fn test_commit_empty_message_rejected() {
        let repo = init_repo("commit-empty-msg");
        write(&repo.path.join("tracked.txt"), b"x\n");
        git(&repo.path, &["add", "tracked.txt"]);

        let err = commit(&repo.path, "   \n\t").expect_err("empty message rejected");
        assert!(err.to_string().contains("message is empty"), "got {err}");
        // No commit was made: the change stays staged.
        assert_eq!(
            porcelain_xy(&repo.path, "tracked.txt").as_deref(),
            Some("M ")
        );
    }

    #[test]
    fn test_commit_nothing_staged_rejected() {
        // A worktree edit that was never staged: the index tree still equals the
        // HEAD tree, so there is nothing to commit.
        let repo = init_repo("commit-nothing");
        write(&repo.path.join("tracked.txt"), b"unstaged\n");

        let err = commit(&repo.path, "should fail").expect_err("nothing staged rejected");
        assert!(err.to_string().contains("nothing staged"), "got {err}");
    }

    #[cfg(unix)]
    #[test]
    fn test_stage_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let repo = init_repo("stage-exec");
        let script = repo.path.join("run.sh");
        write(&script, b"#!/bin/sh\necho hi\n");
        let mut perms = std::fs::metadata(&script).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod");

        stage_file(&repo.path, Path::new("run.sh")).expect("stage");

        // git records the exec bit as mode 100755 in the index.
        let ls = git(&repo.path, &["ls-files", "--stage", "--", "run.sh"]);
        let out = String::from_utf8_lossy(&ls.stdout);
        assert!(
            out.starts_with("100755 "),
            "staged mode must carry the exec bit, got {out:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_stage_symlink_records_link_mode() {
        let repo = init_repo("stage-symlink");
        std::os::unix::fs::symlink("tracked.txt", repo.path.join("link.txt")).expect("symlink");

        stage_file(&repo.path, Path::new("link.txt")).expect("stage");

        let ls = git(&repo.path, &["ls-files", "--stage", "--", "link.txt"]);
        let out = String::from_utf8_lossy(&ls.stdout);
        assert!(
            out.starts_with("120000 "),
            "staged mode must be a symlink, got {out:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_discard_restores_symlink_from_index() {
        // A committed symlink, retargeted in the worktree, is restored to its
        // index target — and stays a symlink, not a regular file.
        let repo = init_repo("discard-symlink");
        std::os::unix::fs::symlink("tracked.txt", repo.path.join("link.txt")).expect("symlink");
        git(&repo.path, &["add", "link.txt"]);
        git(&repo.path, &["commit", "-q", "-m", "add link"]);
        std::fs::remove_file(repo.path.join("link.txt")).expect("remove link");
        std::os::unix::fs::symlink("other.txt", repo.path.join("link.txt")).expect("retarget");

        discard_file(&repo.path, Path::new("link.txt")).expect("discard");

        let meta = std::fs::symlink_metadata(repo.path.join("link.txt")).expect("lstat");
        assert!(
            meta.file_type().is_symlink(),
            "restored path is still a symlink"
        );
        assert_eq!(
            std::fs::read_link(repo.path.join("link.txt")).expect("readlink"),
            Path::new("tracked.txt"),
            "restored to the index target"
        );
    }

    #[test]
    fn test_stage_then_commit_end_to_end() {
        // The panel's core flow: stage an untracked file, commit, and confirm
        // git sees a clean tree with the new file committed.
        let repo = init_repo("stage-commit-e2e");
        write(&repo.path.join("feature.txt"), b"new feature\n");

        stage_file(&repo.path, Path::new("feature.txt")).expect("stage");
        commit(&repo.path, "add feature").expect("commit");

        let tracked = git(&repo.path, &["ls-files", "--", "feature.txt"]);
        assert_eq!(
            String::from_utf8_lossy(&tracked.stdout).trim(),
            "feature.txt"
        );
        let porcelain = git(&repo.path, &["status", "--porcelain"]);
        assert!(
            porcelain.stdout.is_empty(),
            "worktree is clean after stage + commit"
        );
    }
}
