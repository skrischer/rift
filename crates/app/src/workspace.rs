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
use gpui_component::dock::{DockArea, DockItem};
use gpui_component::ActiveTheme as _;
use rift_protocol::{ClientMessage, DaemonMessage};
use rift_terminal::SessionView;
use tracing::debug;

use crate::editor::EditorView;
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::problems_panel::ProblemsPanel;
use crate::terminal_panel::TerminalPanel;

/// Initial width of the left (explorer) dock, in pixels. Purely a starting
/// point for the user's first resize — `DockArea` owns the size afterward,
/// replacing the old fixed explorer column (`docs/spec-ide-shell.md`, #324).
const LEFT_DOCK_WIDTH: f32 = 240.0;

/// Initial width of the (collapsed) right dock.
const RIGHT_DOCK_WIDTH: f32 = 240.0;

/// Initial height of the (collapsed) bottom dock.
const BOTTOM_DOCK_HEIGHT: f32 = 200.0;

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
    /// The problems panel (`docs/spec-problems-panel.md`, #342): a read-only
    /// mirror of `file_tree`'s model, docked in the bottom zone. Kept as its
    /// own field (mirroring `file_tree`/`editor`) so tests can reach it
    /// directly rather than reaching into the dock's private panel tree.
    // Read only by tests until #343 wires jump-to-location into production; use
    // `allow` (not `expect`) since the field IS read under `cfg(test)`, which
    // would make an `expect(dead_code)` unfulfilled on the `--all-targets` build.
    #[allow(dead_code)]
    problems_panel: Entity<ProblemsPanel>,
    /// Read-request sender: a tree open turns into a path on this channel, which
    /// the tokio side emits as `ClientMessage::OpenFile`.
    open_file_tx: Sender<String>,
    /// The dock shell (`docs/spec-ide-shell.md`, issue #324): explorer in the
    /// left dock, editor|terminal split in the center, problems panel in the
    /// (collapsed by default) bottom dock, right collapsed. Holds its own
    /// strong references to the panel entities; this view keeps its own
    /// handles too (`file_tree`, `editor`, `problems_panel` above) so the
    /// daemon-stream bridges keep working unchanged after the layout refactor.
    dock_area: Entity<DockArea>,
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

        // Dock shell (`docs/spec-ide-shell.md`, issue #324): explorer (left) +
        // editor|terminal (center split) + problems panel (bottom, #342) +
        // right dock present but collapsed, for phase 12 (source control) to
        // dock into later with no layout rewrite. Built after the daemon-stream
        // bridges above so the panel entities they close over already exist.
        let dock_area = cx.new(|cx| DockArea::new("rift-dock", Some(1), window, cx));
        let weak_dock_area = dock_area.downgrade();

        let terminal_panel = cx.new(|_| TerminalPanel::new(session_view.clone()));
        let problems_panel = cx.new(|cx| ProblemsPanel::new(file_tree.clone(), cx));

        let left_item = DockItem::tab(file_tree.clone(), &weak_dock_area, window, cx);
        let center_item = DockItem::h_split(
            vec![
                DockItem::tab(editor.clone(), &weak_dock_area, window, cx),
                DockItem::tab(terminal_panel, &weak_dock_area, window, cx),
            ],
            &weak_dock_area,
            window,
            cx,
        );
        // Real, collapsed dock (not a placeholder view) — an empty `TabPanel`
        // with zero panels, matching `Dock::new`'s own empty-dock shape. The
        // bottom dock, in contrast, now carries the problems panel — real
        // content, but still collapsed by default until the user opens it.
        let right_item = DockItem::tabs(Vec::new(), &weak_dock_area, window, cx);
        let bottom_item = DockItem::tab(problems_panel.clone(), &weak_dock_area, window, cx);

        dock_area.update(cx, |dock, cx| {
            dock.set_center(center_item, window, cx);
            dock.set_left_dock(left_item, Some(px(LEFT_DOCK_WIDTH)), true, window, cx);
            dock.set_right_dock(right_item, Some(px(RIGHT_DOCK_WIDTH)), false, window, cx);
            dock.set_bottom_dock(bottom_item, Some(px(BOTTOM_DOCK_HEIGHT)), false, window, cx);
        });

        Self {
            file_tree,
            editor,
            session_view,
            problems_panel,
            open_file_tx,
            dock_area,
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
        // The dock shell fills the window under the current OS chrome; the
        // `flex_col` mirrors `examples/dock.rs` at the pinned gpui-component
        // rev so future top chrome (status bar, palette) can stack above the
        // dock without another layout rewrite (both deferred, `docs/spec-ide-shell.md`).
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .child(self.dock_area.clone())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Axis, TestAppContext};
    use gpui_component::dock::{DockPlacement, Panel};
    use gpui_component::Root;

    /// A `WorkspaceChannels` wired to throwaway flume endpoints — no daemon is
    /// attached in this test, only the dock's panel tree is under test.
    fn test_channels() -> WorkspaceChannels {
        let (_worktree_tx, worktree_rx) = flume::unbounded();
        let (_buffer_tx, buffer_rx) = flume::unbounded();
        let (_nav_reply_tx, nav_rx) = flume::unbounded();
        let (open_file_tx, _open_file_rx) = flume::unbounded();
        let (save_file_tx, _save_file_rx) = flume::unbounded();
        let (buffer_change_tx, _buffer_change_rx) = flume::unbounded();
        let (nav_tx, _nav_request_rx) = flume::unbounded();
        WorkspaceChannels {
            worktree_rx,
            buffer_rx,
            nav_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
        }
    }

    /// Panel-tree construction (`docs/spec-ide-shell.md`, issue #324): the
    /// default layout must put the explorer in an open left dock, an
    /// editor|terminal horizontal split in the center, and collapsed (but
    /// real) right/bottom docks — the gate decision the spec pins.
    #[gpui::test]
    fn test_default_layout_has_left_explorer_center_split_and_collapsed_right_bottom(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(
                    cx.new(|cx| WorkspaceView::new(session_view, test_channels(), window, cx)),
                );
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap();
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        cx.update(|cx| {
            let view = workspace.read(cx);
            let dock_area = view.dock_area.read(cx);

            assert!(dock_area.left_dock().is_some(), "left dock must exist");
            assert!(
                dock_area.is_dock_open(DockPlacement::Left, cx),
                "explorer dock starts open"
            );

            assert!(dock_area.right_dock().is_some(), "right dock must exist");
            assert!(
                !dock_area.is_dock_open(DockPlacement::Right, cx),
                "right dock starts collapsed"
            );

            assert!(dock_area.bottom_dock().is_some(), "bottom dock must exist");
            assert!(
                !dock_area.is_dock_open(DockPlacement::Bottom, cx),
                "bottom dock starts collapsed"
            );

            match dock_area.center() {
                DockItem::Split { axis, items, .. } => {
                    assert_eq!(
                        *axis,
                        Axis::Horizontal,
                        "editor|terminal split is horizontal"
                    );
                    assert_eq!(items.len(), 2, "center split holds editor + terminal");
                }
                other => panic!("expected the center to be a horizontal split, got {other:?}"),
            }
        });
    }

    /// Dock interaction (`docs/spec-ide-shell.md`, issue #325): the explorer
    /// dock toggles hidden/shown via the native `DockArea::toggle_dock` — this
    /// exercises that wiring end-to-end against the app's own panel tree rather
    /// than trusting the vendored dock's own test suite.
    #[gpui::test]
    fn test_toggle_left_dock_flips_open_state(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(
                    cx.new(|cx| WorkspaceView::new(session_view, test_channels(), window, cx)),
                );
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let dock_area = workspace.read(cx).dock_area.clone();

                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "explorer dock starts open"
                );

                dock_area.update(cx, |dock_area, cx| {
                    dock_area.toggle_dock(DockPlacement::Left, window, cx);
                });
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "toggling the left dock once hides the explorer"
                );

                dock_area.update(cx, |dock_area, cx| {
                    dock_area.toggle_dock(DockPlacement::Left, window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "toggling the left dock again restores the explorer"
                );
            })
            .unwrap();
    }

    /// Dock interaction (`docs/spec-ide-shell.md`, issue #325): every surface
    /// stays zoomable to fill the shell and restore. None of `FileTree`,
    /// `EditorView`, `TerminalPanel`, or `ProblemsPanel` override
    /// `Panel::zoomable`, so the default reaches the dock's native
    /// zoom-in/zoom-out control for all four — this locks that invariant
    /// against an accidental future override.
    #[gpui::test]
    fn test_all_dock_surfaces_stay_zoomable(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(
                    cx.new(|cx| WorkspaceView::new(session_view, test_channels(), window, cx)),
                );
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap();
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        cx.update(|cx| {
            let (file_tree, editor, session_view, problems_panel) = {
                let view = workspace.read(cx);
                (
                    view.file_tree.clone(),
                    view.editor.clone(),
                    view.session_view.clone(),
                    view.problems_panel.clone(),
                )
            };

            assert!(
                file_tree.read(cx).zoomable(cx).is_some(),
                "the explorer stays zoomable"
            );
            assert!(
                editor.read(cx).zoomable(cx).is_some(),
                "the editor stays zoomable"
            );
            assert!(
                problems_panel.read(cx).zoomable(cx).is_some(),
                "the problems panel stays zoomable"
            );

            let terminal_panel = cx.new(|_| TerminalPanel::new(session_view));
            assert!(
                terminal_panel.read(cx).zoomable(cx).is_some(),
                "the terminal stays zoomable"
            );
        });
    }

    /// Problems panel (`docs/spec-problems-panel.md`, #342): the panel docks in
    /// the bottom zone and reads the *same* `WorktreeModel` the file tree
    /// mirrors — so a `Diagnostics` fold onto `file_tree`'s model (exactly what
    /// the daemon-stream bridge above performs) is immediately visible through
    /// the panel's summary, with no separate wiring. Live repaint-on-notify
    /// itself is a GPUI rendering concern verified visually at the milestone QA
    /// gate; this test locks the shared-model wiring the render depends on.
    #[gpui::test]
    fn test_problems_panel_docks_bottom_and_shares_the_file_tree_model(cx: &mut TestAppContext) {
        use crate::problems_panel::PROBLEMS_PANEL_NAME;
        use rift_protocol::{Diagnostic, DiagnosticSeverity, Position, Range};

        let mut workspace: Option<Entity<WorkspaceView>> = None;
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(
                    cx.new(|cx| WorkspaceView::new(session_view, test_channels(), window, cx)),
                );
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap();
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        cx.update(|cx| {
            let (file_tree, problems_panel, dock_area) = {
                let view = workspace.read(cx);
                (
                    view.file_tree.clone(),
                    view.problems_panel.clone(),
                    view.dock_area.clone(),
                )
            };

            assert_eq!(problems_panel.read(cx).panel_name(), PROBLEMS_PANEL_NAME);
            assert!(
                dock_area.read(cx).bottom_dock().is_some(),
                "the bottom dock exists and now carries the problems panel"
            );
            assert!(
                !dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                "the bottom dock starts collapsed, matching the other reserved docks"
            );
            assert!(
                problems_panel.read(cx).summary(cx).groups.is_empty(),
                "no diagnostics folded yet"
            );

            file_tree.update(cx, |tree, cx| {
                tree.model_mut()
                    .apply_snapshot_chunk("/proj".into(), Vec::new(), true);
                tree.model_mut().apply_diagnostics(
                    "a.rs".into(),
                    "rust-analyzer".into(),
                    vec![Diagnostic {
                        range: Range {
                            start: Position {
                                line: 0,
                                character: 0,
                            },
                            end: Position {
                                line: 0,
                                character: 1,
                            },
                        },
                        severity: DiagnosticSeverity::Error,
                        message: "mismatched types".into(),
                        source: None,
                        code: None,
                    }],
                );
                cx.notify();
            });

            let summary = problems_panel.read(cx).summary(cx);
            assert_eq!(summary.totals.errors, 1);
            assert_eq!(summary.groups.len(), 1);
            assert_eq!(summary.groups[0].path, "a.rs");
        });
    }

    /// Dock interaction (`docs/spec-ide-shell.md`, issue #325): focus keeps
    /// delegating to the terminal so agent keystrokes reach the tmux pane
    /// byte-identically to pre-refactor — the dock's own focus-follows-active-
    /// panel handling (native `TabPanel`) must not change where the workspace's
    /// own handed-off focus lands.
    #[gpui::test]
    fn test_workspace_focus_delegates_to_the_terminal(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace =
                    Some(cx.new(|cx| WorkspaceView::new(view, test_channels(), window, cx)));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap();
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");
        let session_view =
            session_view.expect("session view constructed inside the window callback");

        cx.update(|cx| {
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "workspace focus delegates to the terminal so keystrokes keep reaching the tmux pane"
            );
        });
    }
}
