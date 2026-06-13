use serde::{Deserialize, Serialize};
use std::time::SystemTime;

mod frame;

pub use frame::{encode_frame, FrameDecoder, FrameError};

/// Wire protocol version negotiated during the client/daemon handshake.
///
/// Independent of the crate's semver: bump it when the message wire format
/// changes in a way that requires both sides to agree.
pub const PROTOCOL_VERSION: u32 = 1;

/// Messages the client sends to the daemon.
///
/// `Attach` opens this client's own tmux control-mode attach for a named
/// session; `Input`, `ResizePane`, and `TmuxCommand` then drive that attach, and
/// the daemon streams the reverse path back as [`DaemonMessage`] layout and
/// pane-output events. Pane input is opaque bytes — the protocol forwards it to
/// tmux and never interprets it (agent-agnostic).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Open a terminal attach for `session`, carrying the `RIFT_SESSION` knob
    /// end-to-end: the daemon runs attach-or-create (`new-session -A -s
    /// <session>`) per attach, so the dogfooding isolation session
    /// (`RIFT_SESSION=rift-dev`) survives the protocol seam. The daemon answers
    /// with a [`DaemonMessage::LayoutSnapshot`] baseline, then the live stream.
    Attach {
        session: String,
    },
    Input {
        pane_id: u32,
        data: String,
    },
    ResizePane {
        pane_id: u32,
        cols: u16,
        rows: u16,
    },
    TmuxCommand {
        cmd: String,
    },
    Hello {
        version: u32,
    },
}

/// Messages the daemon sends to the client.
///
/// ## Terminal snapshot ↔ live-stream consistency contract
///
/// On [`ClientMessage::Attach`] the daemon opens this client's own tmux
/// control-mode attach and sends exactly one [`LayoutSnapshot`] — the complete
/// window/pane layout as of the attach instant — and from that instant streams
/// the live notifications: [`LayoutUpdate`] for every structural change and
/// [`PaneOutput`] for pane bytes. The seam between the snapshot and the live
/// stream is **gap-free and duplicate-free**:
///
/// - **No gap**: the daemon subscribes to tmux's notification stream before it
///   reads the snapshot, so every change at or after the snapshot instant
///   appears in the live stream; none is lost in the handover.
/// - **No duplicate**: the snapshot is the baseline state, not a replay — no
///   layout change already reflected in it is re-sent as a live event.
///   [`LayoutUpdate`] carries the full latest layout (replace semantics), so even
///   a coalesced or reordered change converges without double-applying.
///
/// On reconnect the daemon reattaches and sends a fresh [`LayoutSnapshot`]; the
/// client resets its layout to it and resumes from the new baseline — tmux
/// remains the session persistence, so no terminal state is lost. Pane scrollback
/// that predates the attach is fetched separately via `capture-pane` (command
/// emission) and is outside this contract — it governs only the seam between the
/// attach snapshot and the live `%output` stream.
///
/// [`LayoutSnapshot`]: DaemonMessage::LayoutSnapshot
/// [`LayoutUpdate`]: DaemonMessage::LayoutUpdate
/// [`PaneOutput`]: DaemonMessage::PaneOutput
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Raw terminal bytes for one pane, in stream order. Per the VTE-location
    /// spike verdict the daemon forwards bytes, not cells: the client feeds them
    /// straight into its `alacritty_terminal::Term`, so the payload is an opaque
    /// ANSI byte run the protocol never interprets (agent-agnostic). `pane_id` is
    /// tmux's `%<n>` pane id as an integer.
    PaneOutput {
        pane_id: u32,
        bytes: Vec<u8>,
    },
    /// The complete window/pane layout for `session`, sent once per attach as the
    /// baseline of the consistency contract (see the type-level docs). The client
    /// replaces its entire layout model with this — on first attach and again on
    /// every reconnect.
    LayoutSnapshot {
        session: String,
        windows: Vec<WindowLayout>,
    },
    /// The full latest window/pane layout for `session` after a structural change
    /// (window add/close, pane split/resize, active-window switch). Carries the
    /// whole layout, not a delta, so applying it is an idempotent replace.
    LayoutUpdate {
        session: String,
        windows: Vec<WindowLayout>,
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

/// One tmux window inside a [`DaemonMessage::LayoutSnapshot`] /
/// [`DaemonMessage::LayoutUpdate`]: its identity, title, active flag, and the
/// panes it holds. `window_id` is tmux's `@<n>` window id as an integer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowLayout {
    pub window_id: u32,
    pub name: String,
    /// Whether this is the session's active (currently selected) window.
    pub active: bool,
    pub panes: Vec<PaneLayout>,
}

/// One tmux pane's identity, active flag, and geometry within its window.
///
/// Geometry is in terminal cells, matching tmux's layout coordinates: `left` and
/// `top` are the pane's offset from the window's top-left corner, `width` and
/// `height` its size. `pane_id` is tmux's `%<n>` pane id as an integer — the same
/// id space as the `pane_id` in [`DaemonMessage::PaneOutput`] and
/// [`ClientMessage::Input`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub pane_id: u32,
    /// Whether this is the window's active pane.
    pub active: bool,
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
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
    fn test_attach_roundtrip_carries_session_name() {
        // The attach request is the seam that carries `RIFT_SESSION` end-to-end,
        // so the session name must survive serialization untouched.
        let msg = ClientMessage::Attach {
            session: "rift-dev".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize Attach");
        assert_eq!(json, r#"{"type":"attach","session":"rift-dev"}"#);

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize Attach");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::Attach { session } => assert_eq!(session, "rift-dev"),
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn test_input_roundtrip_preserves_pane_and_data() {
        let msg = ClientMessage::Input {
            pane_id: 3,
            data: "ls\n".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize Input");
        assert!(json.contains(r#""type":"input""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).expect("deserialize Input"),
            msg
        );
    }

    #[test]
    fn test_resize_pane_roundtrip_preserves_dimensions() {
        let msg = ClientMessage::ResizePane {
            pane_id: 7,
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&msg).expect("serialize ResizePane");
        assert!(json.contains(r#""type":"resize_pane""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).expect("deserialize ResizePane"),
            msg
        );
    }

    #[test]
    fn test_tmux_command_roundtrip_preserves_cmd() {
        let msg = ClientMessage::TmuxCommand {
            cmd: "split-window -h".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize TmuxCommand");
        assert!(json.contains(r#""type":"tmux_command""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).expect("deserialize TmuxCommand"),
            msg
        );
    }

    #[test]
    fn test_client_message_unknown_type_is_rejected() {
        // An unknown tag fails loudly rather than being silently misread, so a
        // future client message a daemon does not know is not mistaken for a
        // known one.
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"frobnicate"}"#);
        assert!(
            err.is_err(),
            "unknown client message type must not deserialize"
        );
    }

    #[test]
    fn test_attach_missing_session_field_is_rejected() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"attach"}"#);
        assert!(
            err.is_err(),
            "attach without a session must not deserialize"
        );
    }

    #[test]
    fn test_pane_output_roundtrip_carries_bytes_field() {
        // The spike verdict pins pane output as raw bytes, not cells: the wire
        // field is `bytes` and round-trips the exact byte run (control bytes
        // included).
        let msg = DaemonMessage::PaneOutput {
            pane_id: 2,
            bytes: vec![0x1b, b'[', b'1', b'm', b'h', b'i'],
        };
        let json = serde_json::to_string(&msg).expect("serialize PaneOutput");
        assert!(json.contains(r#""type":"pane_output""#));
        assert!(json.contains(r#""bytes":[27,91,49,109,104,105]"#));
        assert!(
            !json.contains("cells"),
            "pane output must not carry a cells field"
        );

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize PaneOutput");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::PaneOutput { pane_id, bytes } => {
                assert_eq!(pane_id, 2);
                assert_eq!(bytes, vec![0x1b, b'[', b'1', b'm', b'h', b'i']);
            }
            other => panic!("expected PaneOutput, got {other:?}"),
        }
    }

    fn sample_layout() -> Vec<WindowLayout> {
        vec![
            WindowLayout {
                window_id: 1,
                name: "editor".to_owned(),
                active: true,
                panes: vec![
                    PaneLayout {
                        pane_id: 0,
                        active: true,
                        left: 0,
                        top: 0,
                        width: 80,
                        height: 24,
                    },
                    PaneLayout {
                        pane_id: 1,
                        active: false,
                        left: 81,
                        top: 0,
                        width: 79,
                        height: 24,
                    },
                ],
            },
            WindowLayout {
                window_id: 2,
                name: "logs".to_owned(),
                active: false,
                panes: vec![PaneLayout {
                    pane_id: 2,
                    active: true,
                    left: 0,
                    top: 0,
                    width: 160,
                    height: 24,
                }],
            },
        ]
    }

    #[test]
    fn test_window_and_pane_layout_roundtrip_preserves_all_fields() {
        for window in sample_layout() {
            let json = serde_json::to_string(&window).expect("serialize WindowLayout");
            let parsed: WindowLayout =
                serde_json::from_str(&json).expect("deserialize WindowLayout");
            assert_eq!(parsed, window);
        }
    }

    #[test]
    fn test_layout_snapshot_roundtrip_preserves_windows_and_panes() {
        let msg = DaemonMessage::LayoutSnapshot {
            session: "rift".to_owned(),
            windows: sample_layout(),
        };
        let json = serde_json::to_string(&msg).expect("serialize LayoutSnapshot");
        assert!(json.contains(r#""type":"layout_snapshot""#));
        assert!(json.contains(r#""session":"rift""#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize LayoutSnapshot");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::LayoutSnapshot { session, windows } => {
                assert_eq!(session, "rift");
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[0].panes.len(), 2);
                assert!(windows[0].active);
                assert!(windows[0].panes[0].active);
            }
            other => panic!("expected LayoutSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn test_layout_update_roundtrip_preserves_layout() {
        let msg = DaemonMessage::LayoutUpdate {
            session: "rift-dev".to_owned(),
            windows: sample_layout(),
        };
        let json = serde_json::to_string(&msg).expect("serialize LayoutUpdate");
        assert!(json.contains(r#""type":"layout_update""#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize LayoutUpdate"),
            msg
        );
    }

    #[test]
    fn test_layout_snapshot_empty_windows_roundtrips() {
        // A fresh session may attach before any window exists; an empty layout is
        // a valid baseline, not an error.
        let msg = DaemonMessage::LayoutSnapshot {
            session: "rift".to_owned(),
            windows: vec![],
        };
        let json = serde_json::to_string(&msg).expect("serialize empty LayoutSnapshot");
        assert!(json.contains(r#""windows":[]"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize empty LayoutSnapshot"),
            msg
        );
    }

    #[test]
    fn test_daemon_message_unknown_type_is_rejected() {
        let err = serde_json::from_str::<DaemonMessage>(r#"{"type":"sparkle"}"#);
        assert!(
            err.is_err(),
            "unknown daemon message type must not deserialize"
        );
    }

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
