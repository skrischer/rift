//! The app root view: the cockpit that composes the three live surfaces —
//! the file-tree explorer (#186), the code editor (#187), and the terminal
//! [`SessionView`] — into one layout, and wires the file-tree/editor pair onto
//! the daemon transport (`docs/spec-editor.md`, the render debut + write-back).
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
//! worktree-family messages and the buffer-channel replies into this view, which
//! folds them onto the GPUI foreground — mirroring how the terminal snapshot /
//! pane-output streams already bridge the two runtimes (`docs/patterns.md`):
//!
//! - **Worktree structure** (`worktree_rx`): snapshot / update / git / repo /
//!   diagnostics messages fold into the [`FileTree`]'s [`WorktreeModel`] so the
//!   tree appears and updates live. (The tree only renders structure today;
//!   git/diagnostics decoration on the tree is a later explorer-panel sub-spec,
//!   but the model carries them already.) After each fold, the open path's new
//!   snapshot `mtime` is handed to the editor as the **concurrent-write signal**
//!   (#188): a clean buffer auto-reloads, a dirty one surfaces a conflict. After a
//!   `Diagnostics` fold the open file's set is re-pushed to the editor as **inline
//!   markers** (#189) — consuming the existing client diagnostics model (#178), so
//!   a fixed error clears its marker.
//! - **Buffer reply** (`buffer_rx`): a `FileContent { path, content, mtime }`
//!   loads into the [`EditorView`] (and its inline diagnostics are re-applied,
//!   since opening rebuilt the input); a `SaveResult` / `SaveConflict` resolves a
//!   save (commit the new base `mtime`, or surface the conflict without losing
//!   the buffer).
//!
//! The reverse path runs through three request channels. `open_file_tx` issues an
//! `OpenFile` read: when the tree emits [`FileTreeEvent::OpenFile`] (or the editor
//! auto-reloads), this view tells the editor to begin opening (arming its timeout)
//! and sends the path to the tokio side. `save_file_tx` carries a `SaveFile` the
//! editor builds from the open buffer on a [`crate::editor::Save`]. `buffer_change_tx`
//! carries the **live-buffer feed** (#189): the editor sends `BufferChanged` on a
//! debounced edit and `BufferClosed` on close / switch / save, so the daemon feeds
//! the LSP the live buffer and an unsaved error surfaces without a save first. The
//! daemon's read/write replies return on `buffer_rx`; diagnostics return on
//! `worktree_rx` as `Diagnostics`.

use flume::{Receiver, Sender};
use gpui::{
    div, px, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, Styled as _, Window,
};
use gpui_component::ActiveTheme as _;
use rift_protocol::{ClientMessage, DaemonMessage};
use rift_terminal::SessionView;
use tracing::debug;

use crate::editor::EditorView;
use crate::file_tree::{FileTree, FileTreeEvent};

/// Fixed width of the file-tree explorer column. The editor and terminal share
/// the remaining width.
const EXPLORER_WIDTH: f32 = 240.0;

/// The flume endpoints the workspace consumes to bridge the daemon stream (run
/// by the tokio side) onto its GPUI surfaces, plus the request endpoints it emits
/// `OpenFile` / `SaveFile` on. Handed in at construction so `main.rs` owns the
/// matching senders/receivers it threads into `consume_daemon_messages`.
pub struct WorkspaceChannels {
    /// Worktree-family daemon messages (snapshot / update / git / repo /
    /// diagnostics) to fold into the file tree's model.
    pub worktree_rx: Receiver<DaemonMessage>,
    /// Buffer-channel replies to route to the editor: `FileContent` (load),
    /// `SaveResult` (save landed), `SaveConflict` (save refused).
    pub buffer_rx: Receiver<DaemonMessage>,
    /// Nav replies to route to the editor: `DefinitionResponse` (#196),
    /// `HoverResponse` (#197), `ReferencesResponse` (#198).
    pub nav_rx: Receiver<DaemonMessage>,
    /// Read requests: the root-relative path of a file to open. The tokio side
    /// turns each into a `ClientMessage::OpenFile`.
    pub open_file_tx: Sender<String>,
    /// Write requests: a `ClientMessage::SaveFile` the editor built from the open
    /// buffer. The tokio side forwards it onto the protocol verbatim.
    pub save_file_tx: Sender<ClientMessage>,
    /// Live-buffer feed (#189): a `ClientMessage::BufferChanged` (debounced edit)
    /// or `BufferClosed` (close / switch / save) the editor emits so the daemon
    /// feeds the LSP the live buffer. The tokio side forwards it verbatim.
    pub buffer_change_tx: Sender<ClientMessage>,
    /// Navigation requests: `DefinitionRequest` (#196).
    pub nav_tx: Sender<ClientMessage>,
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
        let WorkspaceChannels {
            worktree_rx,
            buffer_rx,
            nav_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
        } = channels;

        let file_tree = cx.new(|_| FileTree::new());
        let editor = {
            let open_file_tx = open_file_tx.clone();
            cx.new(|cx| {
                EditorView::new(
                    open_file_tx,
                    save_file_tx,
                    buffer_change_tx,
                    nav_tx,
                    window,
                    cx,
                )
            })
        };

        // Open requests originate from the tree's `OpenFile` event: arm the
        // editor's open (and its timeout) and send the path to the tokio side.
        // Selecting a file touches nothing but this — no tmux pane/window state.
        cx.subscribe_in(
            &file_tree,
            window,
            |this, _tree, event: &FileTreeEvent, window, cx| {
                let FileTreeEvent::OpenFile { path } = event;
                this.editor.update(cx, |editor, cx| {
                    editor.begin_open(path.clone(), false, window, cx);
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
        // the exit signal (mirroring the terminal snapshot bridge). `spawn_in`
        // (not `spawn`) so the editor's auto-reload can re-arm a load with the
        // window in scope.
        {
            cx.spawn_in(window, async move |this, cx| loop {
                let Ok(msg) = worktree_rx.recv_async().await else {
                    break;
                };
                let result = this.update_in(cx, |view, window, cx| {
                    let is_diagnostics = matches!(msg, DaemonMessage::Diagnostics { .. });
                    view.file_tree.update(cx, |tree, cx| {
                        apply_worktree_message(tree, msg);
                        cx.notify();
                    });
                    // Concurrent-write signal (#188): after folding the structure
                    // update, hand the editor the open path's new snapshot `mtime`.
                    // The editor compares it against the buffer's base `mtime` and
                    // auto-reloads (clean) or surfaces a conflict (dirty). Tapping
                    // the model the tree already mirrors keeps the comparison
                    // base-vs-snapshot — never an independent stat.
                    view.notify_editor_of_open_path_mtime(window, cx);
                    // Inline diagnostics (#189): after a diagnostics fold, re-push
                    // the open file's full set to the editor so its inline markers
                    // converge with the model (fixing an error clears the marker).
                    // Only on a diagnostics message — the structure folds do not
                    // touch the diagnostics map.
                    if is_diagnostics {
                        view.push_open_file_diagnostics(cx);
                    }
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        // Buffer replies -> editor: `FileContent` loads, `SaveResult` commits the
        // new base `mtime`, `SaveConflict` surfaces the conflict. Each routed
        // method ignores a reply for a path no longer open. Routed through this
        // view's weak handle so a load can be followed by re-pushing the freshly
        // opened file's inline diagnostics (#189) — the `begin_open` recreated the
        // input with an empty `DiagnosticSet`, so the open file's existing set must
        // be re-applied once its content has loaded.
        {
            cx.spawn_in(window, async move |this, cx| loop {
                let Ok(msg) = buffer_rx.recv_async().await else {
                    break;
                };
                let result = this.update_in(cx, |view, window, cx| {
                    let loaded = matches!(msg, DaemonMessage::FileContent { .. });
                    view.editor.update(cx, |editor, cx| match msg {
                        DaemonMessage::FileContent {
                            path,
                            content,
                            mtime,
                        } => editor.load(path, content, mtime, window, cx),
                        DaemonMessage::SaveResult { path, mtime } => {
                            editor.apply_save_result(path, mtime, cx)
                        }
                        DaemonMessage::SaveConflict { path, .. } => {
                            editor.apply_save_conflict(path, cx)
                        }
                        _ => {}
                    });
                    // After a load the editor rebuilt its input with an empty
                    // `DiagnosticSet`, so re-apply the open file's inline markers
                    // (#189) from the model the tree mirrors.
                    if loaded {
                        view.push_open_file_diagnostics(cx);
                    }
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        // Nav reply stream -> editor: `DefinitionResponse` routes to
        // `apply_definition_response`; `HoverResponse` routes to
        // `apply_hover_response` (#197); `ReferencesResponse` routes to
        // `apply_references_response` (#198). Routed through this view's weak
        // handle so a closed window ends the loop gracefully.
        {
            cx.spawn_in(window, async move |this, cx| loop {
                let Ok(msg) = nav_rx.recv_async().await else {
                    break;
                };
                let result = this.update_in(cx, |view, window, cx| {
                    view.editor.update(cx, |editor, cx| match msg {
                        DaemonMessage::DefinitionResponse { id, targets } => {
                            editor.apply_definition_response(id, targets, window, cx);
                        }
                        DaemonMessage::HoverResponse { id, content } => {
                            editor.apply_hover_response(id, content, cx);
                        }
                        DaemonMessage::ReferencesResponse { id, locations } => {
                            editor.apply_references_response(id, locations, cx);
                        }
                        _ => {}
                    });
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

    /// Feed the editor the open path's current snapshot `mtime` as the
    /// concurrent-write signal (#188). Looks the open path up in the file tree's
    /// mirrored model — the same `mtime` the buffer's base is compared against —
    /// and, when present, hands it to [`EditorView::note_external_change`]. A no-op
    /// when no file is open or the open path is not (yet) in the tree.
    fn notify_editor_of_open_path_mtime(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open_path) = self
            .editor
            .read(cx)
            .open_path()
            .map(std::borrow::ToOwned::to_owned)
        else {
            return;
        };
        let Some(mtime) = self
            .file_tree
            .read(cx)
            .model()
            .get(&open_path)
            .map(|entry| entry.mtime)
        else {
            return;
        };
        self.editor.update(cx, |editor, cx| {
            editor.note_external_change(mtime, window, cx);
        });
    }

    /// Push the open file's full diagnostic set (aggregated across servers) from
    /// the file tree's model onto the editor's inline markers (#189). Looks the
    /// open path up in the same model the tree mirrors — the existing client
    /// diagnostics layer (#178), consumed, not redesigned — and hands a flattened
    /// `Vec<Diagnostic>` to [`EditorView::set_diagnostics`]. An open path with no
    /// diagnostics applies an empty set, which clears every inline marker (so a
    /// fixed error clears). A no-op when no file is open.
    fn push_open_file_diagnostics(&self, cx: &mut Context<Self>) {
        let Some(open_path) = self
            .editor
            .read(cx)
            .open_path()
            .map(std::borrow::ToOwned::to_owned)
        else {
            return;
        };
        // Flatten the per-server sets into one list — the editor renders all
        // servers' diagnostics for the file together (a linter + a type-checker
        // aggregate), mirroring the model's per-`(file, server)` keying.
        let items: Vec<rift_protocol::Diagnostic> = self
            .file_tree
            .read(cx)
            .model()
            .diagnostics(&open_path)
            .map(|by_server| by_server.values().flatten().cloned().collect())
            .unwrap_or_default();
        self.editor.update(cx, |editor, cx| {
            editor.set_diagnostics(&items, cx);
        });
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
