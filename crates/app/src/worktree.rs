//! Client-side worktree model: the in-memory mirror of the daemon's tree.
//!
//! Snapshot-as-source-of-truth — the model only applies daemon messages
//! (chunked `WorktreeSnapshot`, incremental `UpdateWorktree`); it never
//! mutates optimistically (`docs/spec-daemon-filetree.md`). Pure data, no
//! GPUI — headless-verifiable; the rendered explorer panel is a later
//! sub-spec that consumes this model.

use std::collections::BTreeMap;

use rift_protocol::WorktreeEntry;

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
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        self.root = Some(root);
        self.synced = true;
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
        for entry in added.into_iter().chain(changed) {
            self.entries.insert(entry.path.clone(), entry);
        }
        for path in &removed {
            self.entries.remove(path);
        }
        true
    }

    /// The daemon-side project root, once a complete snapshot has arrived.
    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
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
