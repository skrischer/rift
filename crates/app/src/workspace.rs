//! The app root view: the cockpit that composes the three live surfaces —
//! the file-tree explorer (#186), the code editor (#187), and the terminal
//! [`SessionView`] — into one layout, and wires the file-tree/editor pair onto
//! the daemon transport (`docs/spec-editor.md`, the render debut).
//!
//! `SessionView` lives in `rift-terminal` and cannot reach back into `rift-app`
//! (the dependency runs app -> terminal), so the explorer + editor are mounted
//! *here*, beside the terminal, in the app crate that owns both. This is the
//! first view that renders the client worktree model the daemon already streams
//! and the first consumer of the buffer channel.
//!
//! ## Daemon wiring (background -> GPUI)
//!
//! The single reader of the daemon stream runs on the tokio side
//! (`main.rs::consume_daemon_messages`). It forwards, over `flume` channels, the
//! worktree-family messages and the `FileContent` reply into this view, which
//! folds them onto the GPUI foreground — mirroring how the terminal snapshot /
//! pane-output streams already bridge the two runtimes (`docs/patterns.md`):
//!
//! - **Worktree structure** (`worktree_rx`): snapshot / update / git / repo /
//!   diagnostics messages fold into the [`FileTree`]'s [`WorktreeModel`] so the
//!   tree appears and updates live. (The tree only renders structure today;
//!   git/diagnostics decoration on the tree is a later explorer-panel sub-spec,
//!   but the model carries them already.)
//! - **Buffer reply** (`file_content_rx`): a `FileContent { path, content, mtime }`
//!   loads into the [`EditorView`].
//!
//! The reverse path — issuing an `OpenFile` read request — runs through
//! `open_file_tx`: when the tree emits [`FileTreeEvent::OpenFile`], this view
//! tells the editor to begin opening (arming its timeout) and sends the path to
//! the tokio side, which emits the `ClientMessage::OpenFile`. The daemon's
//! `FileContent` reply returns on `file_content_rx`, closing the loop.

use flume::{Receiver, Sender};
use gpui::{
    div, px, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, Styled as _, Window,
};
use gpui_component::ActiveTheme as _;
use rift_protocol::DaemonMessage;
use rift_terminal::SessionView;
use tracing::debug;

use crate::editor::EditorView;
use crate::file_tree::{FileTree, FileTreeEvent};

/// Fixed width of the file-tree explorer column. The editor and terminal share
/// the remaining width.
const EXPLORER_WIDTH: f32 = 240.0;

/// The flume endpoints the workspace consumes to bridge the daemon stream (run
/// by the tokio side) onto its GPUI surfaces, plus the request endpoint it emits
/// `OpenFile` paths on. Handed in at construction so `main.rs` owns the matching
/// senders/receiver it threads into `consume_daemon_messages`.
pub struct WorkspaceChannels {
    /// Worktree-family daemon messages (snapshot / update / git / repo /
    /// diagnostics) to fold into the file tree's model.
    pub worktree_rx: Receiver<DaemonMessage>,
    /// `FileContent` replies to load into the editor.
    pub file_content_rx: Receiver<DaemonMessage>,
    /// Read requests: the root-relative path of a file to open. The tokio side
    /// turns each into a `ClientMessage::OpenFile`.
    pub open_file_tx: Sender<String>,
}

/// The composed app root.
pub struct WorkspaceView {
    file_tree: Entity<FileTree>,
    editor: Entity<EditorView>,
    session_view: Entity<SessionView>,
    /// Read-request sender: a tree open turns into a path on this channel, which
    /// the tokio side emits as `ClientMessage::OpenFile`.
    open_file_tx: Sender<String>,
}

impl WorkspaceView {
    /// Build the workspace around an already-created [`SessionView`] entity (the
    /// terminal, created in `main.rs` so it keeps owning the SSH/daemon session
    /// thread). Creates the explorer and editor, mounts all three, and starts the
    /// daemon-stream bridges.
    pub fn new(
        session_view: Entity<SessionView>,
        channels: WorkspaceChannels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let file_tree = cx.new(|_| FileTree::new());
        let editor = cx.new(|cx| EditorView::new(window, cx));

        let WorkspaceChannels {
            worktree_rx,
            file_content_rx,
            open_file_tx,
        } = channels;

        // Open requests originate from the tree's `OpenFile` event: arm the
        // editor's open (and its timeout) and send the path to the tokio side.
        // Selecting a file touches nothing but this — no tmux pane/window state.
        cx.subscribe_in(
            &file_tree,
            window,
            |this, _tree, event: &FileTreeEvent, window, cx| {
                let FileTreeEvent::OpenFile { path } = event;
                this.editor.update(cx, |editor, cx| {
                    editor.begin_open(path.clone(), window, cx);
                });
                if let Err(e) = this.open_file_tx.try_send(path.clone()) {
                    debug!(error = %e, %path, "failed to enqueue open-file request");
                }
            },
        )
        .detach();

        // Worktree structure stream -> file-tree model. Each message folds into
        // the model, then a notify repaints the tree. Routed through this view's
        // weak handle so a dropped view (window closed) ends the loop gracefully —
        // the `WeakEntity::update` `Result`, not the infallible `App` update, is
        // the exit signal (mirroring the terminal snapshot bridge).
        {
            cx.spawn(async move |this, cx| loop {
                let Ok(msg) = worktree_rx.recv_async().await else {
                    break;
                };
                let result = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        view.file_tree.update(cx, |tree, cx| {
                            apply_worktree_message(tree, msg);
                            cx.notify();
                        });
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        // Buffer replies -> editor. A `FileContent` loads into the editor (its
        // `load` ignores a reply for a file no longer being opened).
        {
            let editor = editor.clone();
            cx.spawn_in(window, async move |_this, cx| loop {
                let Ok(msg) = file_content_rx.recv_async().await else {
                    break;
                };
                let DaemonMessage::FileContent {
                    path,
                    content,
                    mtime,
                } = msg
                else {
                    continue;
                };
                let result = editor.update_in(cx, |editor, window, cx| {
                    editor.load(path, content, mtime, window, cx);
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        Self {
            file_tree,
            editor,
            session_view,
            open_file_tx,
        }
    }
}

/// Fold one worktree-family daemon message into the file tree's model. Only the
/// structure-path messages are routed here; any other variant is ignored (the
/// tokio side forwards only this family on `worktree_rx`).
fn apply_worktree_message(tree: &mut FileTree, msg: DaemonMessage) {
    let model = tree.model_mut();
    match msg {
        DaemonMessage::WorktreeSnapshot {
            root,
            entries,
            final_chunk,
        } => {
            model.apply_snapshot_chunk(root, entries, final_chunk);
        }
        DaemonMessage::UpdateWorktree {
            added,
            changed,
            removed,
        } => {
            model.apply_update(added, changed, removed);
        }
        DaemonMessage::UpdateGitStatus { changed, cleared } => {
            model.apply_git_update(changed, cleared);
        }
        DaemonMessage::RepoState {
            branch,
            ahead_behind,
        } => {
            model.apply_repo_state(branch, ahead_behind);
        }
        DaemonMessage::Diagnostics {
            path,
            server,
            items,
        } => {
            model.apply_diagnostics(path, server, items);
        }
        _ => {}
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        // Delegate focus to the terminal so keystrokes reach the active pane, as
        // they did before the editor surface mounted.
        self.session_view.focus_handle(cx)
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Explorer | editor | terminal, left to right. The explorer is a fixed
        // column; the editor and terminal split the rest. The terminal keeps its
        // own internal tab/pane/status chrome.
        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(cx.theme().background)
            .child(
                div()
                    .w(px(EXPLORER_WIDTH))
                    .h_full()
                    .flex_shrink_0()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(self.file_tree.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(self.editor.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.session_view.clone()),
            )
    }
}
