//! Read-only git status over a worktree root, computed with `gix`.
//!
//! [`GitStatus::compute`] runs `gix`'s status (the index<->worktree diff plus the
//! HEAD-tree<->index diff) and folds it into a per-path porcelain `XY` pair — an
//! `index` (staged) and a `worktree` (unstaged) component — keyed by path
//! relative to the root, plus the repo-level branch and ahead/behind. Pure-Rust
//! and `gpui`-free; `git2`/`libgit2` is deliberately absent (the static-musl
//! daemon constraint). The daemon maps these explorer-local types onto the
//! `rift-protocol` wire types, the same way it maps [`crate::Snapshot`] entries.
//!
//! Status honors git's own ignore rules: ignored paths never carry a status,
//! consistent with the worktree scan excluding them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{ExplorerError, Result};

/// One side's porcelain status code for a path — mirrors
/// `rift_protocol::GitStatusCode`. [`GitEntryStatus`] carries one for the index
/// (staged) side and one for the worktree (unstaged) side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitStatusCode {
    /// No change on this side.
    #[default]
    Unmodified,
    Modified,
    /// The file's type changed (e.g. regular file <-> symlink).
    TypeChange,
    Added,
    Deleted,
    Renamed,
    Copied,
    /// Updated but unmerged — a merge conflict.
    Unmerged,
    /// Present in the worktree but not tracked by git (worktree side only).
    Untracked,
}

/// The git status of one path: its index (staged) and worktree (unstaged)
/// components, mirroring git's porcelain `XY`. A clean path carries no
/// [`GitEntryStatus`] at all — it is absent from [`GitStatus::entries`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GitEntryStatus {
    pub index: GitStatusCode,
    pub worktree: GitStatusCode,
}

/// Ahead/behind commit counts of the current branch versus its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AheadBehind {
    pub ahead: u32,
    pub behind: u32,
}

/// Repo-level git state: the current branch (None when HEAD is detached) and
/// ahead/behind vs the upstream (None when the branch has no upstream or its
/// tip cannot be resolved, e.g. an unborn branch).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoState {
    pub branch: Option<String>,
    pub ahead_behind: Option<AheadBehind>,
}

/// A point-in-time git status for a worktree: per-path porcelain status plus
/// repo-level state. Clean paths are absent from `entries`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitStatus {
    entries: BTreeMap<PathBuf, GitEntryStatus>,
    repo: RepoState,
}

impl GitStatus {
    /// Compute the git status of the repository at `root` (its top-level
    /// worktree). Honors git's ignore rules — ignored paths carry no status.
    /// Returns [`ExplorerError::GitError`] only when the repository cannot be
    /// opened or the status machinery fails; a repo with no changes yields an
    /// empty entry map, not an error.
    pub fn compute(root: &Path) -> Result<Self> {
        let repo = gix::open(root)
            .map_err(|e| ExplorerError::GitError(format!("open {}: {e}", root.display())))?;

        let mut entries: BTreeMap<PathBuf, GitEntryStatus> = BTreeMap::new();

        let iter = repo
            .status(gix::progress::Discard)
            .map_err(|e| ExplorerError::GitError(format!("status init: {e}")))?
            // Per-file untracked entries, not directories collapsed to their
            // parent — the snapshot keys are per file.
            .untracked_files(gix::status::UntrackedFiles::Files)
            .into_iter(None)
            .map_err(|e| ExplorerError::GitError(format!("status iter: {e}")))?;

        for item in iter {
            let item = item.map_err(|e| ExplorerError::GitError(format!("status item: {e}")))?;
            match item {
                // HEAD-tree vs index: the staged (index) side.
                gix::status::Item::TreeIndex(change) => {
                    let (path, code) = tree_index_status(&change);
                    entries.entry(path).or_default().index = code;
                }
                // Index vs worktree: the unstaged (worktree) side, plus untracked.
                gix::status::Item::IndexWorktree(change) => {
                    if let Some((path, outcome)) = index_worktree_status(&change) {
                        let entry = entries.entry(path).or_default();
                        match outcome {
                            WorktreeOutcome::Worktree(code) => entry.worktree = code,
                            // A conflict has no clean staged/unstaged split — git
                            // porcelain shows it as `UU`/`AA`/etc.; model both
                            // sides as unmerged.
                            WorktreeOutcome::Conflict => {
                                entry.index = GitStatusCode::Unmerged;
                                entry.worktree = GitStatusCode::Unmerged;
                            }
                        }
                    }
                }
            }
        }

        let repo = repo_state(&repo);
        Ok(Self { entries, repo })
    }

    /// Per-path statuses, keyed by path relative to the worktree root. Clean
    /// paths are absent.
    pub fn entries(&self) -> &BTreeMap<PathBuf, GitEntryStatus> {
        &self.entries
    }

    /// The status of a single path, or `None` if the path is clean / unknown.
    pub fn get(&self, relative: &Path) -> Option<GitEntryStatus> {
        self.entries.get(relative).copied()
    }

    /// Repo-level branch + ahead/behind state.
    pub fn repo(&self) -> &RepoState {
        &self.repo
    }
}

/// The outcome of mapping one index-vs-worktree item.
enum WorktreeOutcome {
    /// A normal unstaged change on the worktree side.
    Worktree(GitStatusCode),
    /// A merge conflict — both index and worktree are unmerged.
    Conflict,
}

/// Convert a git repo-relative `BStr` path into a `PathBuf`.
fn bstr_to_path(bytes: &gix::bstr::BStr) -> PathBuf {
    gix::path::from_bstr(bytes).into_owned()
}

/// Map a HEAD-tree<->index change (the staged side) to a path + status code.
fn tree_index_status(change: &gix::diff::index::Change) -> (PathBuf, GitStatusCode) {
    use gix::diff::index::Change;
    let code = match change {
        Change::Addition { .. } => GitStatusCode::Added,
        Change::Deletion { .. } => GitStatusCode::Deleted,
        Change::Modification { .. } => GitStatusCode::Modified,
        Change::Rewrite { copy, .. } => {
            if *copy {
                GitStatusCode::Copied
            } else {
                GitStatusCode::Renamed
            }
        }
    };
    (bstr_to_path(change.location()), code)
}

/// Map an index-vs-worktree item (the unstaged side, plus untracked files) to a
/// path + outcome, or `None` for items that carry no status (clean,
/// needs-update, pruned/ignored directory-walk entries).
fn index_worktree_status(
    item: &gix::status::index_worktree::Item,
) -> Option<(PathBuf, WorktreeOutcome)> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    match item {
        Item::Modification {
            rela_path, status, ..
        } => {
            let code = match status {
                EntryStatus::Conflict { .. } => {
                    return Some((bstr_to_path(rela_path.as_ref()), WorktreeOutcome::Conflict));
                }
                EntryStatus::Change(Change::Removed) => GitStatusCode::Deleted,
                EntryStatus::Change(Change::Type { .. }) => GitStatusCode::TypeChange,
                EntryStatus::Change(Change::Modification { .. }) => GitStatusCode::Modified,
                // A modified submodule is reported as a worktree modification.
                EntryStatus::Change(Change::SubmoduleModification(_)) => GitStatusCode::Modified,
                // `intent-to-add` is an index entry for a not-yet-staged file:
                // git shows it on the worktree side as added.
                EntryStatus::IntentToAdd => GitStatusCode::Added,
                // No actual change — only a stat refresh would help next time.
                EntryStatus::NeedsUpdate(_) => return None,
            };
            Some((
                bstr_to_path(rela_path.as_ref()),
                WorktreeOutcome::Worktree(code),
            ))
        }
        // A directory-walk entry: untracked files (ignored are not emitted, but
        // guard anyway so only genuine untracked content carries a status).
        Item::DirectoryContents { entry, .. } => match entry.status {
            gix::dir::entry::Status::Untracked => Some((
                bstr_to_path(entry.rela_path.as_ref()),
                WorktreeOutcome::Worktree(GitStatusCode::Untracked),
            )),
            _ => None,
        },
        // Rename/copy detected between a deleted index entry and an untracked
        // worktree file; the destination path carries the rename.
        Item::Rewrite { dirwalk_entry, .. } => Some((
            bstr_to_path(dirwalk_entry.rela_path.as_ref()),
            WorktreeOutcome::Worktree(GitStatusCode::Renamed),
        )),
    }
}

/// Read the repo-level state: branch short name (None when detached) and
/// ahead/behind vs the upstream (None when there is no upstream or the tip
/// cannot be resolved). Best-effort — a failure to resolve ahead/behind yields
/// `None` rather than failing the whole status.
fn repo_state(repo: &gix::Repository) -> RepoState {
    let branch = repo
        .head_name()
        .ok()
        .flatten()
        .map(|name| name.shorten().to_string());

    RepoState {
        branch,
        ahead_behind: ahead_behind(repo),
    }
}

/// Count commits the local branch is ahead of / behind its upstream, via a
/// hidden-tip revision walk (`ahead` = reachable from local but not upstream,
/// and vice versa). `None` when HEAD is detached, has no upstream, or a tip
/// cannot be resolved.
fn ahead_behind(repo: &gix::Repository) -> Option<AheadBehind> {
    let head_ref = repo.head_ref().ok().flatten()?;
    let upstream_name = head_ref
        .remote_tracking_ref_name(gix::remote::Direction::Fetch)?
        .ok()?;
    let upstream_id = repo
        .find_reference(upstream_name.as_ref())
        .ok()?
        .into_fully_peeled_id()
        .ok()?
        .detach();
    let local_id = repo.head_id().ok()?.detach();

    let count = |tip: gix::ObjectId, hidden: gix::ObjectId| -> Option<u32> {
        let walk = repo.rev_walk([tip]).with_hidden([hidden]).all().ok()?;
        // Count reachable commits; a corrupt-object error mid-walk is dropped
        // rather than aborting the count (best-effort ahead/behind).
        Some(walk.filter(|info| info.is_ok()).count() as u32)
    };

    Some(AheadBehind {
        ahead: count(local_id, upstream_id)?,
        behind: count(upstream_id, local_id)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A self-cleaning temp dir, mirroring the snapshot/watcher test helpers so
    /// these stay self-contained without a `tempfile` dev-dependency.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("rift-git-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Run a git command in `dir`, asserting success. The fixtures use the real
    /// `git` binary as ground truth (the spec measures against
    /// `git status --porcelain`); git is present in CI and dev.
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

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// An initialized repo with one committed file on `main`, so HEAD exists.
    fn init_repo(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        write(&tmp.path.join("tracked.txt"), "v1\n");
        git(&tmp.path, &["add", "tracked.txt"]);
        git(&tmp.path, &["commit", "-q", "-m", "init"]);
        tmp
    }

    fn status_of<'a>(s: &'a GitStatus, path: &str) -> Option<&'a GitEntryStatus> {
        s.entries().get(Path::new(path))
    }

    #[test]
    fn test_clean_repo_has_no_entries_and_reports_branch() {
        let repo = init_repo("clean");
        let status = GitStatus::compute(&repo.path).expect("compute");
        assert!(status.entries().is_empty(), "clean repo carries no status");
        assert_eq!(status.repo().branch.as_deref(), Some("main"));
    }

    #[test]
    fn test_unstaged_modification_marks_worktree_modified() {
        let repo = init_repo("modify");
        write(&repo.path.join("tracked.txt"), "v2\n");

        let status = GitStatus::compute(&repo.path).expect("compute");
        assert_eq!(
            status_of(&status, "tracked.txt"),
            Some(&GitEntryStatus {
                index: GitStatusCode::Unmodified,
                worktree: GitStatusCode::Modified,
            })
        );
    }

    #[test]
    fn test_staging_moves_change_from_worktree_to_index() {
        let repo = init_repo("stage");
        write(&repo.path.join("tracked.txt"), "v2\n");
        git(&repo.path, &["add", "tracked.txt"]);

        let status = GitStatus::compute(&repo.path).expect("compute");
        assert_eq!(
            status_of(&status, "tracked.txt"),
            Some(&GitEntryStatus {
                index: GitStatusCode::Modified,
                worktree: GitStatusCode::Unmodified,
            })
        );
    }

    #[test]
    fn test_staged_then_modified_again_marks_both_sides() {
        let repo = init_repo("both");
        write(&repo.path.join("tracked.txt"), "v2\n");
        git(&repo.path, &["add", "tracked.txt"]);
        write(&repo.path.join("tracked.txt"), "v3\n");

        let status = GitStatus::compute(&repo.path).expect("compute");
        assert_eq!(
            status_of(&status, "tracked.txt"),
            Some(&GitEntryStatus {
                index: GitStatusCode::Modified,
                worktree: GitStatusCode::Modified,
            })
        );
    }

    #[test]
    fn test_new_file_staged_is_added_in_index() {
        let repo = init_repo("added");
        write(&repo.path.join("new.txt"), "new\n");
        git(&repo.path, &["add", "new.txt"]);

        let status = GitStatus::compute(&repo.path).expect("compute");
        assert_eq!(
            status_of(&status, "new.txt"),
            Some(&GitEntryStatus {
                index: GitStatusCode::Added,
                worktree: GitStatusCode::Unmodified,
            })
        );
    }

    #[test]
    fn test_untracked_file_marks_worktree_untracked() {
        let repo = init_repo("untracked");
        write(&repo.path.join("loose.txt"), "loose\n");

        let status = GitStatus::compute(&repo.path).expect("compute");
        assert_eq!(
            status_of(&status, "loose.txt"),
            Some(&GitEntryStatus {
                index: GitStatusCode::Unmodified,
                worktree: GitStatusCode::Untracked,
            })
        );
    }

    #[test]
    fn test_deleted_tracked_file_marks_worktree_deleted() {
        let repo = init_repo("deleted");
        std::fs::remove_file(repo.path.join("tracked.txt")).expect("remove");

        let status = GitStatus::compute(&repo.path).expect("compute");
        assert_eq!(
            status_of(&status, "tracked.txt"),
            Some(&GitEntryStatus {
                index: GitStatusCode::Unmodified,
                worktree: GitStatusCode::Deleted,
            })
        );
    }

    #[test]
    fn test_gitignored_path_carries_no_status() {
        let repo = init_repo("ignore");
        write(&repo.path.join(".gitignore"), "target/\n");
        git(&repo.path, &["add", ".gitignore"]);
        git(&repo.path, &["commit", "-q", "-m", "ignore"]);
        write(&repo.path.join("target/artifact"), "binary\n");

        let status = GitStatus::compute(&repo.path).expect("compute");
        assert!(
            status_of(&status, "target/artifact").is_none(),
            "ignored path must carry no status"
        );
        assert!(
            status.entries().keys().all(|p| !p.starts_with("target")),
            "no ignored entry leaks into the status"
        );
    }

    #[test]
    fn test_porcelain_agreement_across_mixed_states() {
        // Build a tree exercising staged-add, unstaged-modify, and untracked at
        // once, and assert each path's XY pair matches what `git status
        // --porcelain` reports.
        let repo = init_repo("porcelain");
        write(&repo.path.join("staged_new.txt"), "s\n");
        git(&repo.path, &["add", "staged_new.txt"]);
        write(&repo.path.join("tracked.txt"), "changed\n");
        write(&repo.path.join("untracked.txt"), "u\n");

        let status = GitStatus::compute(&repo.path).expect("compute");

        assert_eq!(
            status_of(&status, "staged_new.txt").map(|s| s.index),
            Some(GitStatusCode::Added)
        );
        assert_eq!(
            status_of(&status, "tracked.txt").map(|s| s.worktree),
            Some(GitStatusCode::Modified)
        );
        assert_eq!(
            status_of(&status, "untracked.txt").map(|s| s.worktree),
            Some(GitStatusCode::Untracked)
        );

        // Ground truth: every non-clean path git reports must be present in our
        // status, and vice versa (same key set).
        let porcelain = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo.path)
            .output()
            .expect("git status");
        let listed: std::collections::BTreeSet<String> = String::from_utf8_lossy(&porcelain.stdout)
            .lines()
            .map(|line| line[3..].to_owned())
            .collect();
        let ours: std::collections::BTreeSet<String> = status
            .entries()
            .keys()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(ours, listed, "status key set must match git porcelain");
    }
}
