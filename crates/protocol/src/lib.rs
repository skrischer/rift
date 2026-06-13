use serde::{Deserialize, Serialize};
use std::time::SystemTime;

mod frame;

pub use frame::{encode_frame, FrameDecoder, FrameError};

/// Wire protocol version negotiated during the client/daemon handshake.
///
/// Independent of the crate's semver: bump it when the message wire format
/// changes in a way that requires both sides to agree.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Input { pane_id: u32, data: String },
    ResizePane { pane_id: u32, cols: u16, rows: u16 },
    TmuxCommand { cmd: String },
    Hello { version: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    PaneOutput {
        pane_id: u32,
        cells: Vec<u8>,
    },
    StateUpdate {
        sessions: Vec<String>,
    },
    /// Initial worktree contents, sent on connect. A large tree is split across
    /// several `WorktreeSnapshot` messages: the client appends `entries` from
    /// each in order and holds the complete tree once it receives the message
    /// with `final_chunk` set. `root` is the absolute daemon-side project root;
    /// entry paths are relative to it.
    WorktreeSnapshot {
        root: String,
        entries: Vec<WorktreeEntry>,
        final_chunk: bool,
    },
    /// Incremental worktree change since the last snapshot or update. The client
    /// upserts `added` and `changed` by path and drops `removed` paths. A move is
    /// modeled as the old path in `removed` plus the new path in `added` (rename
    /// events are not trusted; moves are reconciled through the snapshot diff).
    UpdateWorktree {
        added: Vec<WorktreeEntry>,
        changed: Vec<WorktreeEntry>,
        removed: Vec<String>,
    },
    /// Incremental git-status change decorating the worktree entries. The
    /// client upserts the status of every `changed` path and drops the
    /// decoration for every `cleared` path (the file returned to clean / was
    /// removed from git's view). Keyed by path relative to the worktree root —
    /// the same key space as [`WorktreeEntry::path`]; ignored paths never
    /// appear. The daemon diffs its previous git state against the new one to
    /// produce these deltas, mirroring the `UpdateWorktree` pattern. A status
    /// arriving for a path the client has not yet added is reconciled
    /// client-side (the worktree snapshot is the source of truth; see #135).
    UpdateGitStatus {
        changed: Vec<GitStatusEntry>,
        cleared: Vec<String>,
    },
    /// Repo-level git state for the watched worktree, recomputed on `.git/`
    /// changes (commit, branch switch, staging). `branch` is `None` when HEAD
    /// is detached; `ahead_behind` is `None` when the current branch has no
    /// upstream. Produced and streamed by Phase 3.3, but not wired into the
    /// statusbar by it (the #18 statusbar swap is a later step).
    RepoState {
        branch: Option<String>,
        ahead_behind: Option<AheadBehind>,
    },
    Welcome {
        version: u32,
    },
}

/// A single worktree entry, keyed by its path relative to the worktree root.
///
/// `mtime` is the file's last-modification time. It is what lets the daemon's
/// snapshot diff observe a content modification — which leaves `path`, `kind`,
/// and `ignored` unchanged — and surface it as a `changed` entry the client can
/// upsert. A `changed` entry always carries the full record, not just the path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub path: String,
    pub kind: EntryKind,
    pub ignored: bool,
    pub mtime: SystemTime,
}

/// Whether a [`WorktreeEntry`] is a regular file or a directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
}

/// One side's porcelain status code for a path.
///
/// Git models each path as an **index** (staged) component and a **worktree**
/// (unstaged) component — the `XY` pair of `git status --porcelain`.
/// [`GitEntryStatus`] carries both. Most codes can appear on either side;
/// [`GitStatusCode::Untracked`] is only ever a worktree-side code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStatusCode {
    /// No change on this side.
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
    /// Present in the worktree but not tracked by git.
    Untracked,
}

/// The git status of one path: its index (staged) and worktree (unstaged)
/// components, mirroring git's porcelain `XY`.
///
/// Examples: an untracked file is `{ index: Unmodified, worktree: Untracked }`;
/// a file staged and then left alone is `{ index: Modified, worktree:
/// Unmodified }`; a tracked file edited but not staged is `{ index:
/// Unmodified, worktree: Modified }`. A clean (unmodified on both sides) path
/// carries no status at all — it is never sent, and a path returning to clean
/// is reported via `cleared` in [`DaemonMessage::UpdateGitStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitEntryStatus {
    pub index: GitStatusCode,
    pub worktree: GitStatusCode,
}

/// A path paired with its git status, keyed by path relative to the worktree
/// root — the same key space as [`WorktreeEntry::path`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusEntry {
    pub path: String,
    pub status: GitEntryStatus,
}

/// Ahead/behind commit counts of the current branch versus its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AheadBehind {
    pub ahead: u32,
    pub behind: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_hello_roundtrip_current_version_preserves_version() {
        let msg = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&msg).expect("serialize Hello");
        assert_eq!(json, r#"{"type":"hello","version":1}"#);

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize Hello");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::Hello { version } => assert_eq!(version, PROTOCOL_VERSION),
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn test_welcome_roundtrip_current_version_preserves_version() {
        let msg = DaemonMessage::Welcome {
            version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&msg).expect("serialize Welcome");
        assert_eq!(json, r#"{"type":"welcome","version":1}"#);

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize Welcome");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::Welcome { version } => assert_eq!(version, PROTOCOL_VERSION),
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    #[test]
    fn test_hello_mismatched_version_parses_differing_version() {
        let json = r#"{"type":"hello","version":999}"#;
        let parsed: ClientMessage = serde_json::from_str(json).expect("deserialize Hello");
        match parsed {
            ClientMessage::Hello { version } => {
                assert_ne!(version, PROTOCOL_VERSION);
                assert_eq!(version, 999);
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn test_worktree_snapshot_roundtrip_preserves_entries_and_chunk_flag() {
        let msg = DaemonMessage::WorktreeSnapshot {
            root: "/home/dev/project".to_owned(),
            entries: vec![
                WorktreeEntry {
                    path: "src".to_owned(),
                    kind: EntryKind::Dir,
                    ignored: false,
                    mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                },
                WorktreeEntry {
                    path: "target/debug/build".to_owned(),
                    kind: EntryKind::File,
                    ignored: true,
                    mtime: SystemTime::UNIX_EPOCH + Duration::new(1_700_000_001, 500),
                },
            ],
            final_chunk: false,
        };

        let json = serde_json::to_string(&msg).expect("serialize WorktreeSnapshot");
        assert!(json.contains(r#""type":"worktree_snapshot""#));
        assert!(json.contains(r#""kind":"dir""#));
        assert!(json.contains(r#""kind":"file""#));
        assert!(json.contains(r#""ignored":true"#));
        assert!(json.contains(r#""final_chunk":false"#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize WorktreeSnapshot");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_update_worktree_roundtrip_preserves_added_changed_removed() {
        let msg = DaemonMessage::UpdateWorktree {
            added: vec![WorktreeEntry {
                path: "src/new.rs".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            }],
            changed: vec![WorktreeEntry {
                path: "src/main.rs".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            }],
            removed: vec!["src/old.rs".to_owned()],
        };

        let json = serde_json::to_string(&msg).expect("serialize UpdateWorktree");
        assert!(json.contains(r#""type":"update_worktree""#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize UpdateWorktree");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::UpdateWorktree {
                added,
                changed,
                removed,
            } => {
                assert_eq!(added.len(), 1);
                assert_eq!(changed.len(), 1);
                assert_eq!(removed, vec!["src/old.rs".to_owned()]);
            }
            other => panic!("expected UpdateWorktree, got {other:?}"),
        }
    }

    #[test]
    fn test_worktree_snapshot_final_chunk_true_with_empty_entries_roundtrips() {
        let msg = DaemonMessage::WorktreeSnapshot {
            root: "/home/dev/project".to_owned(),
            entries: vec![],
            final_chunk: true,
        };
        let json = serde_json::to_string(&msg).expect("serialize WorktreeSnapshot");
        assert!(json.contains(r#""final_chunk":true"#));
        assert!(json.contains(r#""entries":[]"#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize WorktreeSnapshot");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_worktree_entry_mtime_serializes_as_epoch_secs_and_nanos() {
        let msg = DaemonMessage::WorktreeSnapshot {
            root: "/p".to_owned(),
            entries: vec![WorktreeEntry {
                path: "a".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH + Duration::new(5, 7),
            }],
            final_chunk: true,
        };
        let json = serde_json::to_string(&msg).expect("serialize WorktreeSnapshot");
        // Pin the wire shape of `mtime`: the protocol may migrate to MessagePack,
        // so an accidental change to the timestamp representation must fail a test.
        assert!(json.contains(r#""mtime":{"secs_since_epoch":5,"nanos_since_epoch":7}"#));
    }

    #[test]
    fn test_update_git_status_roundtrip_preserves_changed_and_cleared() {
        let msg = DaemonMessage::UpdateGitStatus {
            changed: vec![
                GitStatusEntry {
                    path: "src/main.rs".to_owned(),
                    status: GitEntryStatus {
                        index: GitStatusCode::Unmodified,
                        worktree: GitStatusCode::Modified,
                    },
                },
                GitStatusEntry {
                    path: "new.rs".to_owned(),
                    status: GitEntryStatus {
                        index: GitStatusCode::Added,
                        worktree: GitStatusCode::Unmodified,
                    },
                },
            ],
            cleared: vec!["was_dirty.rs".to_owned()],
        };

        let json = serde_json::to_string(&msg).expect("serialize UpdateGitStatus");
        assert!(json.contains(r#""type":"update_git_status""#));
        assert!(json.contains(r#""index":"added""#));
        assert!(json.contains(r#""worktree":"modified""#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize UpdateGitStatus");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::UpdateGitStatus { changed, cleared } => {
                assert_eq!(changed.len(), 2);
                assert_eq!(cleared, vec!["was_dirty.rs".to_owned()]);
            }
            other => panic!("expected UpdateGitStatus, got {other:?}"),
        }
    }

    #[test]
    fn test_git_entry_status_untracked_and_conflict_pairs_roundtrip() {
        // The two edge pairs: an untracked file (worktree-only `Untracked`) and
        // a merge conflict (`Unmerged` on both sides).
        let untracked = GitEntryStatus {
            index: GitStatusCode::Unmodified,
            worktree: GitStatusCode::Untracked,
        };
        let conflict = GitEntryStatus {
            index: GitStatusCode::Unmerged,
            worktree: GitStatusCode::Unmerged,
        };
        for status in [untracked, conflict] {
            let json = serde_json::to_string(&status).expect("serialize GitEntryStatus");
            let parsed: GitEntryStatus =
                serde_json::from_str(&json).expect("deserialize GitEntryStatus");
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_repo_state_roundtrip_branch_and_detached_head() {
        let on_branch = DaemonMessage::RepoState {
            branch: Some("main".to_owned()),
            ahead_behind: Some(AheadBehind {
                ahead: 2,
                behind: 1,
            }),
        };
        let json = serde_json::to_string(&on_branch).expect("serialize RepoState");
        assert!(json.contains(r#""type":"repo_state""#));
        assert!(json.contains(r#""branch":"main""#));
        assert!(json.contains(r#""ahead":2"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize RepoState"),
            on_branch
        );

        // Detached HEAD with no upstream: both fields are absent (`None`).
        let detached = DaemonMessage::RepoState {
            branch: None,
            ahead_behind: None,
        };
        let json = serde_json::to_string(&detached).expect("serialize detached RepoState");
        assert!(json.contains(r#""branch":null"#));
        assert!(json.contains(r#""ahead_behind":null"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize detached RepoState"),
            detached
        );
    }

    #[test]
    fn test_git_status_code_unknown_variant_is_rejected() {
        // serde rejects an unknown enum variant rather than silently defaulting,
        // so a future daemon emitting a code this client does not know fails
        // loudly instead of being misread as a valid status.
        let err = serde_json::from_str::<GitStatusCode>(r#""partially_staged""#);
        assert!(err.is_err(), "unknown status code must not deserialize");
    }
}
