//! Client-side worktree model: the in-memory mirror of the daemon's tree.
//!
//! Snapshot-as-source-of-truth — the model only applies daemon messages
//! (chunked `WorktreeSnapshot`, incremental `UpdateWorktree`); it never
//! mutates optimistically (`docs/spec-daemon-filetree.md`). Pure data, no
//! GPUI — headless-verifiable; the rendered explorer panel is a later
//! sub-spec that consumes this model.

use std::collections::BTreeMap;

use rift_protocol::{AheadBehind, GitEntryStatus, GitStatusEntry, WorktreeEntry};

/// In-flight accumulation of a chunked snapshot (`final_chunk` not yet seen).
#[derive(Debug)]
struct PendingSnapshot {
    root: String,
    entries: Vec<WorktreeEntry>,
}

/// The client's mirror of the daemon-side worktree.
///
/// A complete snapshot replaces the whole tree; a `WorktreeSnapshot` chunk
/// arriving after a completed one starts a new accumulation, never a
/// continuation (the daemon re-broadcasts the full snapshot on every client
/// handshake — see the spec decision log, #110). Updates arriving before the
/// first complete snapshot are dropped: the daemon guarantees a full snapshot
/// follows on the handshake, and that snapshot is authoritative.
#[derive(Debug, Default)]
pub struct WorktreeModel {
    root: Option<String>,
    entries: BTreeMap<String, WorktreeEntry>,
    pending: Option<PendingSnapshot>,
    /// Whether a complete snapshot has ever been applied.
    synced: bool,
    /// Per-path git status, keyed the same way as `entries`. Independent of the
    /// tree: a status may exist for a path the tree has not added yet (a renderer
    /// joins the two by path), so a racing git update can never corrupt the tree.
    /// Reset whenever a worktree snapshot completes — the daemon re-sends the full
    /// git status right after every snapshot (`spec-daemon-git-status.md`, #134).
    git: BTreeMap<String, GitEntryStatus>,
    /// Repo-level branch name (`None` = detached HEAD or no repo).
    branch: Option<String>,
    /// Ahead/behind vs the upstream (`None` = no upstream / no repo).
    ahead_behind: Option<AheadBehind>,
}

impl WorktreeModel {
    /// Fold one `WorktreeSnapshot` chunk into the model. Chunks accumulate
    /// until the one carrying `final_chunk`, which atomically replaces the
    /// tree. Returns `true` when this chunk completed a snapshot.
    pub fn apply_snapshot_chunk(
        &mut self,
        root: String,
        entries: Vec<WorktreeEntry>,
        final_chunk: bool,
    ) -> bool {
        if let Some(pending) = &self.pending {
            if pending.root != root {
                // A different root mid-accumulation means the daemon started a
                // new snapshot stream; the newest stream wins (snapshot as
                // source of truth), so the stale partial accumulation is
                // discarded rather than mixed in.
                tracing::warn!(
                    stale = %pending.root,
                    new = %root,
                    "worktree snapshot root changed mid-accumulation; restarting"
                );
                self.pending = None;
            }
        }
        let pending = self.pending.get_or_insert_with(|| PendingSnapshot {
            root,
            entries: Vec::new(),
        });
        pending.entries.extend(entries);
        if !final_chunk {
            return false;
        }

        let PendingSnapshot { root, entries } = self
            .pending
            .take()
            .expect("pending snapshot was just populated above");
        self.entries = entries
            .into_iter()
            // Deliberate duplication, not a borrow-checker escape: the map key
            // and the wire entry both own the path — keeping the full entry as
            // the value preserves the wire record for consumers (upsert
            // blindly, per the spec), so the key is a copy of it.
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        self.root = Some(root);
        self.synced = true;
        // The daemon re-sends the full git status immediately behind every
        // snapshot, so drop the prior decoration here — it is about to be
        // re-applied from that replay, and anything not in it has gone clean
        // (or this is a non-repo root, where nothing follows and git stays empty).
        self.git.clear();
        self.branch = None;
        self.ahead_behind = None;
        true
    }

    /// Apply one incremental update: upsert `added` and `changed` by path,
    /// drop `removed` paths. Returns `false` (and changes nothing) while no
    /// complete snapshot has arrived yet — the snapshot the daemon sends on
    /// the handshake supersedes any update raced ahead of it.
    pub fn apply_update(
        &mut self,
        added: Vec<WorktreeEntry>,
        changed: Vec<WorktreeEntry>,
        removed: Vec<String>,
    ) -> bool {
        if !self.synced {
            return false;
        }
        // `added` upserts unconditionally. A `changed` entry should target a
        // path the tree already holds; one that does not is a divergence signal
        // — the client is missing an entry the daemon's baseline has (the #227
        // snapshot-loss bug). Warn, then upsert anyway: the snapshot stays the
        // source of truth and the next full snapshot reconciles. The deliberate
        // key/value path duplication matches `apply_snapshot_chunk` — the value
        // keeps the full wire entry.
        for entry in added {
            self.entries.insert(entry.path.clone(), entry);
        }
        for entry in changed {
            if !self.entries.contains_key(&entry.path) {
                tracing::warn!(
                    path = %entry.path,
                    "worktree changed update targets an unknown path; tree may have diverged"
                );
            }
            self.entries.insert(entry.path.clone(), entry);
        }
        for path in &removed {
            self.entries.remove(path);
        }
        true
    }

    /// Apply an incremental git-status update: upsert each `changed` entry's
    /// status by path, drop the decoration for each `cleared` path (the file
    /// returned to clean).
    ///
    /// Applied unconditionally — the git map is independent of the tree, so a
    /// status for a path not yet in `entries` is simply buffered and decorates
    /// the entry once it arrives; it can never corrupt the tree. This is the
    /// reconciliation rule for an unknown path: buffer, never drop, since the
    /// daemon only sends status for paths it tracks and the next snapshot resets.
    pub fn apply_git_update(&mut self, changed: Vec<GitStatusEntry>, cleared: Vec<String>) {
        for entry in changed {
            self.git.insert(entry.path, entry.status);
        }
        for path in &cleared {
            self.git.remove(path);
        }
    }

    /// Apply the repo-level git state: current branch and ahead/behind. Replaces
    /// the held values wholesale (the daemon sends the full repo state, not a
    /// delta).
    pub fn apply_repo_state(&mut self, branch: Option<String>, ahead_behind: Option<AheadBehind>) {
        self.branch = branch;
        self.ahead_behind = ahead_behind;
    }

    /// The daemon-side project root, once a complete snapshot has arrived.
    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    /// The git status of one path, or `None` if the path is clean / undecorated.
    pub fn git_status(&self, path: &str) -> Option<GitEntryStatus> {
        self.git.get(path).copied()
    }

    /// All per-path git statuses, keyed by path relative to [`WorktreeModel::root`].
    pub fn git_statuses(&self) -> &BTreeMap<String, GitEntryStatus> {
        &self.git
    }

    /// The current branch name, or `None` for a detached HEAD / non-repo root.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// Ahead/behind vs the upstream, or `None` when there is no upstream / repo.
    pub fn ahead_behind(&self) -> Option<AheadBehind> {
        self.ahead_behind
    }

    /// All entries, keyed by their path relative to [`WorktreeModel::root`].
    pub fn entries(&self) -> &BTreeMap<String, WorktreeEntry> {
        &self.entries
    }

    /// Look up a single entry by its relative path.
    pub fn get(&self, path: &str) -> Option<&WorktreeEntry> {
        self.entries.get(path)
    }

    /// Number of entries in the mirrored tree.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the mirrored tree holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_protocol::EntryKind;
    use std::time::{Duration, SystemTime};

    fn entry(path: &str, kind: EntryKind, secs: u64) -> WorktreeEntry {
        WorktreeEntry {
            path: path.to_owned(),
            kind,
            ignored: false,
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    fn file(path: &str, secs: u64) -> WorktreeEntry {
        entry(path, EntryKind::File, secs)
    }

    fn dir(path: &str) -> WorktreeEntry {
        entry(path, EntryKind::Dir, 0)
    }

    #[test]
    fn test_apply_snapshot_single_final_chunk_builds_tree() {
        let mut model = WorktreeModel::default();
        let complete = model.apply_snapshot_chunk(
            "/proj".into(),
            vec![dir("src"), file("src/main.rs", 1), file("README.md", 2)],
            true,
        );

        assert!(complete);
        assert_eq!(model.root(), Some("/proj"));
        assert_eq!(model.len(), 3);
        assert_eq!(
            model.get("src").map(|e| e.kind.clone()),
            Some(EntryKind::Dir)
        );
        assert!(model.get("src/main.rs").is_some());
    }

    #[test]
    fn test_apply_snapshot_chunks_accumulate_until_final() {
        let mut model = WorktreeModel::default();
        assert!(!model.apply_snapshot_chunk("/proj".into(), vec![file("a.txt", 1)], false));
        // Mid-accumulation the tree is still empty — the replace is atomic.
        assert!(model.is_empty());
        assert!(model.root().is_none());

        assert!(model.apply_snapshot_chunk("/proj".into(), vec![file("b.txt", 2)], true));
        assert_eq!(model.len(), 2);
        assert!(model.get("a.txt").is_some());
        assert!(model.get("b.txt").is_some());
    }

    #[test]
    fn test_apply_snapshot_after_final_replaces_tree() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![file("old.txt", 1)], true);

        // A new snapshot (e.g. the re-broadcast on a later handshake) starts a
        // fresh accumulation and replaces the tree wholesale — entries absent
        // from it must disappear.
        model.apply_snapshot_chunk("/proj".into(), vec![file("new.txt", 2)], true);
        assert_eq!(model.len(), 1);
        assert!(model.get("old.txt").is_none());
        assert!(model.get("new.txt").is_some());
    }

    #[test]
    fn test_apply_snapshot_chunk_with_different_root_restarts_accumulation() {
        let mut model = WorktreeModel::default();
        assert!(!model.apply_snapshot_chunk("/stale".into(), vec![file("a.txt", 1)], false));

        // A chunk for a different root discards the stale partial accumulation
        // and starts fresh — the completed tree contains only the new stream.
        assert!(!model.apply_snapshot_chunk("/proj".into(), vec![file("b.txt", 2)], false));
        assert!(model.apply_snapshot_chunk("/proj".into(), vec![file("c.txt", 3)], true));

        assert_eq!(model.root(), Some("/proj"));
        assert_eq!(model.len(), 2);
        assert!(model.get("a.txt").is_none());
        assert!(model.get("b.txt").is_some());
        assert!(model.get("c.txt").is_some());
    }

    #[test]
    fn test_apply_update_upserts_added_changed_and_drops_removed() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![
                file("keep.txt", 1),
                file("stale.txt", 1),
                file("gone.txt", 1),
            ],
            true,
        );

        let applied = model.apply_update(
            vec![file("fresh.txt", 5)],
            vec![file("stale.txt", 9)],
            vec!["gone.txt".into()],
        );

        assert!(applied);
        assert_eq!(model.len(), 3);
        assert!(model.get("fresh.txt").is_some());
        assert!(model.get("gone.txt").is_none());
        let stale = model.get("stale.txt").expect("changed entry present");
        assert_eq!(stale.mtime, SystemTime::UNIX_EPOCH + Duration::from_secs(9));
    }

    #[test]
    fn test_apply_update_changed_on_missing_path_still_upserts() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![file("known.txt", 1)], true);

        // A `changed` for a path absent from the tree is a divergence signal:
        // `apply_update` logs a warning (verified live at the QA gate) and still
        // upserts it blindly, per the snapshot-as-source-of-truth contract.
        let applied = model.apply_update(vec![], vec![file("ghost.txt", 5)], vec![]);

        assert!(applied);
        assert!(model.get("ghost.txt").is_some());
        assert_eq!(model.len(), 2);
    }

    #[test]
    fn test_apply_update_before_first_snapshot_is_dropped() {
        let mut model = WorktreeModel::default();
        let applied = model.apply_update(vec![file("early.txt", 1)], vec![], vec![]);

        assert!(!applied);
        assert!(model.is_empty());

        // The authoritative snapshot that follows on the handshake wins.
        model.apply_snapshot_chunk("/proj".into(), vec![file("real.txt", 2)], true);
        assert_eq!(model.len(), 1);
        assert!(model.get("early.txt").is_none());
    }

    // --- git status decoration (#135) ---

    use rift_protocol::{GitEntryStatus, GitStatusCode, GitStatusEntry};

    fn git_entry(path: &str, index: GitStatusCode, worktree: GitStatusCode) -> GitStatusEntry {
        GitStatusEntry {
            path: path.to_owned(),
            status: GitEntryStatus { index, worktree },
        }
    }

    #[test]
    fn test_apply_git_update_upserts_and_clears() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![file("a.rs", 1), file("b.rs", 1)], true);

        model.apply_git_update(
            vec![
                git_entry("a.rs", GitStatusCode::Unmodified, GitStatusCode::Modified),
                git_entry("b.rs", GitStatusCode::Added, GitStatusCode::Unmodified),
            ],
            vec![],
        );
        assert_eq!(
            model.git_status("a.rs"),
            Some(GitEntryStatus {
                index: GitStatusCode::Unmodified,
                worktree: GitStatusCode::Modified
            })
        );
        assert_eq!(
            model.git_status("b.rs").map(|s| s.index),
            Some(GitStatusCode::Added)
        );

        // `cleared` drops the decoration (the file returned to clean).
        model.apply_git_update(vec![], vec!["a.rs".into()]);
        assert_eq!(model.git_status("a.rs"), None);
        assert!(model.git_status("b.rs").is_some());
    }

    #[test]
    fn test_apply_git_update_unknown_path_is_buffered_not_dropped() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![file("known.rs", 1)], true);

        // A status for a path the tree does not (yet) hold is buffered: it is
        // recorded and decorates the entry once it arrives, never corrupting the
        // tree (the maps are independent).
        model.apply_git_update(
            vec![git_entry(
                "pending.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Untracked,
            )],
            vec![],
        );
        assert_eq!(
            model.git_status("pending.rs").map(|s| s.worktree),
            Some(GitStatusCode::Untracked)
        );
        // The tree is untouched by the git update.
        assert!(model.get("pending.rs").is_none());
        assert_eq!(model.len(), 1);
    }

    #[test]
    fn test_git_update_before_snapshot_is_reset_by_snapshot() {
        // Symmetry with the tree's `apply_update_before_first_snapshot`: a git
        // update may arrive before the first snapshot; it is buffered, then the
        // snapshot reset wipes it so no pre-snapshot decoration leaks. The
        // authoritative full git replay follows the snapshot.
        let mut model = WorktreeModel::default();
        model.apply_git_update(
            vec![git_entry(
                "early.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Modified,
            )],
            vec![],
        );
        model.apply_repo_state(Some("stale".into()), None);

        model.apply_snapshot_chunk("/proj".into(), vec![file("early.rs", 1)], true);
        assert_eq!(model.git_status("early.rs"), None);
        assert_eq!(model.branch(), None);
    }

    #[test]
    fn test_apply_repo_state_stores_branch_and_ahead_behind() {
        let mut model = WorktreeModel::default();
        model.apply_repo_state(
            Some("main".into()),
            Some(AheadBehind {
                ahead: 2,
                behind: 1,
            }),
        );
        assert_eq!(model.branch(), Some("main"));
        assert_eq!(
            model.ahead_behind(),
            Some(AheadBehind {
                ahead: 2,
                behind: 1
            })
        );

        // Detached / no upstream replaces wholesale.
        model.apply_repo_state(None, None);
        assert_eq!(model.branch(), None);
        assert_eq!(model.ahead_behind(), None);
    }

    #[test]
    fn test_snapshot_resets_stale_git_decoration() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![file("a.rs", 1)], true);
        model.apply_git_update(
            vec![git_entry(
                "a.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Modified,
            )],
            vec![],
        );
        model.apply_repo_state(Some("main".into()), None);

        // A new snapshot (e.g. on reattach) drops the prior git decoration and
        // repo state; the daemon re-sends the full git status right behind it.
        model.apply_snapshot_chunk("/proj".into(), vec![file("a.rs", 2)], true);
        assert_eq!(model.git_status("a.rs"), None);
        assert_eq!(model.branch(), None);
        assert!(model.git_statuses().is_empty());
    }

    #[test]
    fn test_git_status_sequence_reproduces_daemon_view() {
        // Mirror the daemon's lifecycle: worktree snapshot, then the full git
        // replay (UpdateGitStatus with everything + RepoState), then incremental
        // updates as the user stages and edits. The final decoration must match
        // git's own view (staged vs unstaged) plus the branch.
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![
                file("staged.rs", 1),
                file("dirty.rs", 1),
                file("loose.rs", 1),
            ],
            true,
        );

        // Full replay behind the snapshot.
        model.apply_git_update(
            vec![
                git_entry("staged.rs", GitStatusCode::Added, GitStatusCode::Unmodified),
                git_entry(
                    "dirty.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Modified,
                ),
                git_entry(
                    "loose.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Untracked,
                ),
            ],
            vec![],
        );
        model.apply_repo_state(
            Some("main".into()),
            Some(AheadBehind {
                ahead: 1,
                behind: 0,
            }),
        );

        // Incremental: dirty.rs gets staged (moves to the index side); loose.rs
        // gets committed away (cleared).
        model.apply_git_update(
            vec![git_entry(
                "dirty.rs",
                GitStatusCode::Modified,
                GitStatusCode::Unmodified,
            )],
            vec!["loose.rs".into()],
        );

        assert_eq!(
            model.git_status("staged.rs"),
            Some(GitEntryStatus {
                index: GitStatusCode::Added,
                worktree: GitStatusCode::Unmodified
            })
        );
        assert_eq!(
            model.git_status("dirty.rs"),
            Some(GitEntryStatus {
                index: GitStatusCode::Modified,
                worktree: GitStatusCode::Unmodified
            })
        );
        assert_eq!(model.git_status("loose.rs"), None);
        assert_eq!(model.branch(), Some("main"));
        assert_eq!(
            model.ahead_behind(),
            Some(AheadBehind {
                ahead: 1,
                behind: 0
            })
        );
    }

    #[test]
    fn test_delta_sequence_reproduces_daemon_tree() {
        // Mirror the daemon's lifecycle: an initial snapshot followed by the
        // update sequence its watcher would emit (add, modify, move as
        // remove+add, delete). The final client tree must equal the tree the
        // daemon would hold after the same changes.
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![dir("src"), file("src/main.rs", 10), file("notes.txt", 10)],
            true,
        );

        // add src/lib.rs
        model.apply_update(vec![file("src/lib.rs", 11)], vec![], vec![]);
        // modify src/main.rs (mtime bump)
        model.apply_update(vec![], vec![file("src/main.rs", 12)], vec![]);
        // move notes.txt -> docs/notes.txt (remove + add, plus the new dir)
        model.apply_update(
            vec![dir("docs"), file("docs/notes.txt", 13)],
            vec![],
            vec!["notes.txt".into()],
        );

        let expected: BTreeMap<String, WorktreeEntry> = [
            dir("src"),
            file("src/main.rs", 12),
            file("src/lib.rs", 11),
            dir("docs"),
            file("docs/notes.txt", 13),
        ]
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

        assert_eq!(model.entries(), &expected);
        assert_eq!(model.root(), Some("/proj"));
    }
}
