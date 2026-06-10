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
}
