//! The worktree [`Snapshot`]: a point-in-time, ignore-pruned view of a project
//! root, entries keyed by their path relative to that root.
//!
//! The snapshot is the source of truth the client mirrors — it is never
//! optimistically mutated downstream, only rebuilt or diffed against.
//! [`Snapshot::scan`] walks the tree once, honoring VCS ignore rules, and
//! [`Snapshot::diff`] turns an old and new snapshot into the [`Change`] deltas the
//! [`crate::Watcher`] streams as the tree evolves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::WalkBuilder;

use crate::{ExplorerError, Result};

/// Whether a [`Snapshot`] entry is a regular file or a directory.
///
/// Mirrors `rift_protocol::EntryKind`. A symlink is recorded as an
/// [`EntryKind::File`] leaf — the scan does not follow it (see [`Snapshot::scan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

/// A single worktree entry, looked up by its path relative to the snapshot root.
///
/// The fields mirror `rift_protocol::WorktreeEntry` (minus the path, which is the
/// map key) so the daemon can map a snapshot entry onto the wire one-to-one.
/// `mtime` is the change detector the incremental diff relies on: a content edit
/// leaves `kind` unchanged, so without `mtime` the diff could not observe that the
/// file changed (see the spec decision log, #107).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// Whether VCS ignore rules cover this entry. Ignored paths are excluded from
    /// the scan in v1, so every entry a snapshot holds is currently `false`; the
    /// field mirrors the protocol entry and leaves room to surface greyed-out
    /// ignored entries later without reshaping the model.
    pub ignored: bool,
    pub mtime: SystemTime,
}

/// A single delta between two snapshots, produced by [`Snapshot::diff`] and
/// applied with [`Snapshot::apply`].
///
/// A move surfaces as a [`Change::Removed`] of the old path plus a
/// [`Change::Added`] of the new one — there is no dedicated rename, matching the
/// spec decision to reconcile moves through the snapshot diff rather than trusting
/// backend-specific rename events. `Added` and `Changed` carry the full entry so a
/// consumer can upsert blindly without restatting the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added { path: PathBuf, entry: Entry },
    Changed { path: PathBuf, entry: Entry },
    Removed { path: PathBuf },
}

/// A point-in-time view of the worktree, entries keyed by path relative to `root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    root: PathBuf,
    entries: BTreeMap<PathBuf, Entry>,
}

impl Snapshot {
    /// Walk `root` once and build a snapshot, honoring VCS ignore rules.
    ///
    /// `.git/` and anything a `.gitignore` matches (e.g. `target/`) are excluded;
    /// dotfiles that are not ignored (`.gitignore`, `.github/`) are kept. The walk
    /// does not follow symlinks — a symlink is recorded as a leaf [`EntryKind::File`]
    /// and never traversed, so symlink loops cannot arise. A directory that cannot be
    /// read (permission denied) or any other per-entry error is skipped and logged,
    /// never fatal to the scan; the only error this returns is an inaccessible `root`.
    ///
    /// If a *directory's* own metadata cannot be read it is skipped while the walk
    /// still descends into it, so a child entry may exist in the map without its
    /// parent; consumers building a tree must tolerate missing intermediate nodes.
    pub fn scan(root: &Path) -> Result<Self> {
        let root = root.canonicalize().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ExplorerError::PathNotFound(root.display().to_string())
            } else {
                ExplorerError::ScanError(format!("cannot access root {}: {e}", root.display()))
            }
        })?;

        let walker = WalkBuilder::new(&root)
            .hidden(false) // keep unignored dotfiles like .gitignore / .github
            .require_git(false) // honor .gitignore even outside a checked-out repo
            .git_global(false) // self-contained: ignore the host's global gitignore
            .parents(false) // do not climb above the project root for ignore files
            .filter_entry(|entry| entry.file_name() != ".git") // never descend into .git/
            .build();

        let mut entries = BTreeMap::new();
        for result in walker {
            let entry = match result {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(%err, "skipping unreadable worktree entry");
                    continue;
                }
            };

            // The walker yields `root` itself at depth 0; it has no relative path.
            if entry.depth() == 0 {
                continue;
            }

            let kind = match entry.file_type() {
                Some(file_type) if file_type.is_dir() => EntryKind::Dir,
                _ => EntryKind::File,
            };

            let mtime = match entry.metadata() {
                Ok(metadata) => match metadata.modified() {
                    Ok(mtime) => mtime,
                    Err(err) => {
                        tracing::warn!(path = %entry.path().display(), %err, "cannot read mtime, skipping");
                        continue;
                    }
                },
                Err(err) => {
                    tracing::warn!(%err, "cannot read metadata, skipping");
                    continue;
                }
            };

            let relative = entry
                .path()
                .strip_prefix(&root)
                .expect("walker yields paths under the scanned root");

            entries.insert(
                relative.to_path_buf(),
                Entry {
                    kind,
                    ignored: false,
                    mtime,
                },
            );
        }

        Ok(Self { root, entries })
    }

    /// The absolute, canonicalized root this snapshot was scanned from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All entries, keyed by path relative to [`Snapshot::root`].
    pub fn entries(&self) -> &BTreeMap<PathBuf, Entry> {
        &self.entries
    }

    /// Look up a single entry by its path relative to the root.
    pub fn get(&self, relative: &Path) -> Option<&Entry> {
        self.entries.get(relative)
    }

    /// Number of entries in the snapshot.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the snapshot holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Diff this snapshot against a newer one, yielding the deltas that turn `self`
    /// into `next`: [`Change::Added`] for entries only in `next`, [`Change::Changed`]
    /// for entries whose value differs (e.g. a bumped `mtime`), and
    /// [`Change::Removed`] for entries only in `self`. An entry that is equal on both
    /// sides yields nothing. [`Snapshot::apply`] is the exact inverse — applying the
    /// result to `self` reproduces `next`.
    pub fn diff(&self, next: &Snapshot) -> Vec<Change> {
        let mut changes = Vec::new();
        for (path, entry) in &next.entries {
            match self.entries.get(path) {
                None => changes.push(Change::Added {
                    path: path.clone(),
                    entry: entry.clone(),
                }),
                Some(previous) if previous != entry => changes.push(Change::Changed {
                    path: path.clone(),
                    entry: entry.clone(),
                }),
                Some(_) => {}
            }
        }
        for path in self.entries.keys() {
            if !next.entries.contains_key(path) {
                changes.push(Change::Removed { path: path.clone() });
            }
        }
        changes
    }

    /// Apply `changes` (as produced by [`Snapshot::diff`]) in place: `Added`/`Changed`
    /// upsert the entry, `Removed` deletes it. If `self` started equal to some `a`,
    /// then `self.apply(&a.diff(&b))` leaves `self` equal to `b`.
    pub fn apply(&mut self, changes: &[Change]) {
        for change in changes {
            match change {
                Change::Added { path, entry } | Change::Changed { path, entry } => {
                    self.entries.insert(path.clone(), entry.clone());
                }
                Change::Removed { path } => {
                    self.entries.remove(path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A self-cleaning temporary directory rooted under the system temp dir.
    /// Avoids a dev-dependency on `tempfile` for the fixture trees these tests need.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("rift-explorer-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    #[test]
    fn test_scan_fixture_tree_yields_entries_matching_on_disk_structure() {
        let tmp = TempDir::new("structure");
        let root = &tmp.path;
        write_file(&root.join("README.md"), "# readme");
        write_file(&root.join("src/main.rs"), "fn main() {}");
        write_file(&root.join("src/lib.rs"), "pub fn lib() {}");
        write_file(&root.join("docs/guide.md"), "guide");

        let snapshot = Snapshot::scan(root).expect("scan succeeds");

        assert_eq!(snapshot.len(), 6);
        assert_eq!(
            snapshot.get(Path::new("README.md")).map(|e| e.kind),
            Some(EntryKind::File)
        );
        assert_eq!(
            snapshot.get(Path::new("src")).map(|e| e.kind),
            Some(EntryKind::Dir)
        );
        assert_eq!(
            snapshot.get(Path::new("src/main.rs")).map(|e| e.kind),
            Some(EntryKind::File)
        );
        assert_eq!(
            snapshot.get(Path::new("src/lib.rs")).map(|e| e.kind),
            Some(EntryKind::File)
        );
        assert_eq!(
            snapshot.get(Path::new("docs")).map(|e| e.kind),
            Some(EntryKind::Dir)
        );
        assert_eq!(
            snapshot.get(Path::new("docs/guide.md")).map(|e| e.kind),
            Some(EntryKind::File)
        );
    }

    #[test]
    fn test_scan_observes_changed_mtime_on_rescan() {
        let tmp = TempDir::new("mtime");
        let root = &tmp.path;
        let file = root.join("watched.txt");
        write_file(&file, "v1");

        let before = Snapshot::scan(root).expect("first scan");
        let before_mtime = before
            .get(Path::new("watched.txt"))
            .expect("entry present")
            .mtime;

        // Bump the mtime deterministically (no sleep) to a known-later instant; this
        // is the only signal that lets the incremental diff (#109) observe a content
        // edit, so the rescan must surface it.
        let bumped = before_mtime + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open file")
            .set_modified(bumped)
            .expect("set mtime");

        let after = Snapshot::scan(root).expect("rescan");
        let after_mtime = after
            .get(Path::new("watched.txt"))
            .expect("entry present")
            .mtime;

        assert_ne!(before_mtime, after_mtime);
        assert!(after_mtime > before_mtime);
    }

    #[test]
    fn test_scan_excludes_git_target_and_gitignored_paths_but_keeps_unignored_dotfiles() {
        let tmp = TempDir::new("ignore");
        let root = &tmp.path;
        write_file(&root.join(".gitignore"), "target/\nbuild/\n");
        write_file(&root.join("src/main.rs"), "fn main() {}");
        write_file(&root.join("target/debug/app"), "binary");
        write_file(&root.join("build/out.o"), "obj");
        write_file(&root.join(".git/HEAD"), "ref: refs/heads/main");
        write_file(&root.join(".github/workflows/ci.yml"), "name: ci");

        let snapshot = Snapshot::scan(root).expect("scan succeeds");

        // Tracked content and unignored dotfiles are kept.
        assert!(snapshot.get(Path::new("src/main.rs")).is_some());
        assert!(snapshot.get(Path::new(".gitignore")).is_some());
        assert!(snapshot
            .get(Path::new(".github/workflows/ci.yml"))
            .is_some());

        // Ignored paths are excluded entirely.
        assert!(snapshot.get(Path::new("target")).is_none());
        assert!(snapshot.get(Path::new("target/debug/app")).is_none());
        assert!(snapshot.get(Path::new("build")).is_none());
        assert!(snapshot.get(Path::new(".git")).is_none());
        assert!(snapshot.get(Path::new(".git/HEAD")).is_none());
        assert!(snapshot.entries().keys().all(|p| {
            !p.starts_with("target") && !p.starts_with("build") && !p.starts_with(".git")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn test_scan_symlink_loop_is_not_fatal() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new("symlink");
        let root = &tmp.path;
        write_file(&root.join("a/file.txt"), "x");
        // Self-referential loop: a/loop -> a. With follow_links(false) it is never
        // traversed, so the scan cannot loop.
        symlink(root.join("a"), root.join("a/loop")).expect("create symlink");

        let snapshot = Snapshot::scan(root).expect("scan does not fail on a symlink loop");

        assert_eq!(
            snapshot.get(Path::new("a")).map(|e| e.kind),
            Some(EntryKind::Dir)
        );
        assert!(snapshot.get(Path::new("a/file.txt")).is_some());
        // The symlink is recorded as a leaf, not followed.
        assert_eq!(
            snapshot.get(Path::new("a/loop")).map(|e| e.kind),
            Some(EntryKind::File)
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_scan_permission_denied_dir_is_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new("perm");
        let root = &tmp.path;
        write_file(&root.join("keep.txt"), "keep");
        write_file(&root.join("secret/hidden.txt"), "secret");
        let secret = root.join("secret");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");

        let snapshot = Snapshot::scan(root).expect("scan is never fatal on permission denied");

        // Restore perms so the temp dir can be removed on drop.
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o755))
            .expect("restore perms");

        assert!(snapshot.get(Path::new("keep.txt")).is_some());
    }

    #[test]
    fn test_scan_nonexistent_root_returns_path_not_found() {
        let tmp = TempDir::new("missing");
        let missing = tmp.path.join("does-not-exist");
        match Snapshot::scan(&missing) {
            Err(ExplorerError::PathNotFound(_)) => {}
            other => panic!("expected PathNotFound, got {other:?}"),
        }
    }

    #[test]
    fn test_diff_added_file_yields_added_change() {
        let tmp = TempDir::new("diff-add");
        let root = &tmp.path;
        write_file(&root.join("a.txt"), "a");
        let before = Snapshot::scan(root).expect("scan before");
        write_file(&root.join("b.txt"), "b");
        let after = Snapshot::scan(root).expect("scan after");

        let changes = before.diff(&after);
        assert_eq!(
            changes,
            vec![Change::Added {
                path: PathBuf::from("b.txt"),
                entry: after.get(Path::new("b.txt")).expect("entry").clone(),
            }]
        );
    }

    #[test]
    fn test_diff_removed_file_yields_removed_change() {
        let tmp = TempDir::new("diff-rm");
        let root = &tmp.path;
        write_file(&root.join("a.txt"), "a");
        write_file(&root.join("b.txt"), "b");
        let before = Snapshot::scan(root).expect("before");
        std::fs::remove_file(root.join("b.txt")).expect("remove");
        let after = Snapshot::scan(root).expect("after");

        let changes = before.diff(&after);
        assert_eq!(
            changes,
            vec![Change::Removed {
                path: PathBuf::from("b.txt")
            }]
        );
    }

    #[test]
    fn test_diff_modified_file_yields_changed_with_new_mtime() {
        let tmp = TempDir::new("diff-mod");
        let root = &tmp.path;
        let file = root.join("a.txt");
        write_file(&file, "a");
        let before = Snapshot::scan(root).expect("before");
        let before_mtime = before.get(Path::new("a.txt")).expect("entry").mtime;

        let bumped = before_mtime + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open file")
            .set_modified(bumped)
            .expect("set mtime");
        let after = Snapshot::scan(root).expect("after");

        match before.diff(&after).as_slice() {
            [Change::Changed { path, entry }] => {
                assert_eq!(path, Path::new("a.txt"));
                assert!(entry.mtime > before_mtime);
            }
            other => panic!("expected a single Changed, got {other:?}"),
        }
    }

    #[test]
    fn test_diff_move_is_remove_plus_add() {
        let tmp = TempDir::new("diff-move");
        let root = &tmp.path;
        write_file(&root.join("from/x.txt"), "x");
        let before = Snapshot::scan(root).expect("before");
        std::fs::create_dir_all(root.join("to")).expect("mkdir to");
        std::fs::rename(root.join("from/x.txt"), root.join("to/x.txt")).expect("rename");
        let after = Snapshot::scan(root).expect("after");

        let changes = before.diff(&after);
        assert!(changes
            .iter()
            .any(|c| matches!(c, Change::Removed { path } if path == Path::new("from/x.txt"))));
        assert!(changes.iter().any(|c| {
            matches!(c, Change::Added { path, entry }
                if path == Path::new("to/x.txt") && entry.kind == EntryKind::File)
        }));
    }

    #[test]
    fn test_diff_identical_snapshots_yield_no_changes() {
        let tmp = TempDir::new("diff-same");
        let root = &tmp.path;
        write_file(&root.join("a.txt"), "a");
        let a = Snapshot::scan(root).expect("a");
        let b = Snapshot::scan(root).expect("b");
        assert!(a.diff(&b).is_empty());
    }

    #[test]
    fn test_apply_diff_reproduces_target_snapshot() {
        let tmp = TempDir::new("apply");
        let root = &tmp.path;
        write_file(&root.join("keep.txt"), "keep");
        write_file(&root.join("remove.txt"), "gone");
        write_file(&root.join("nested/old.txt"), "old");
        let mut base = Snapshot::scan(root).expect("base");

        // Remove one file, add another, and bump a third's mtime — a mix of all three
        // change kinds across the tree.
        std::fs::remove_file(root.join("remove.txt")).expect("remove");
        write_file(&root.join("nested/new.txt"), "new");
        let keep_mtime = base.get(Path::new("keep.txt")).expect("keep").mtime;
        std::fs::File::options()
            .write(true)
            .open(root.join("keep.txt"))
            .expect("open keep")
            .set_modified(keep_mtime + std::time::Duration::from_secs(60))
            .expect("set mtime");
        let target = Snapshot::scan(root).expect("target");

        let changes = base.diff(&target);
        base.apply(&changes);
        assert_eq!(base, target);
    }
}
