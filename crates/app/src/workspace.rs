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
//!   but the model carries them already.) After each fold, every **open tab's**
//!   path is checked against the fresh snapshot `mtime` and handed to the editor
//!   as the **concurrent-write signal** (#188, fanned out per tab — #353): a
//!   clean tab auto-reloads, a dirty one surfaces a conflict, independent of
//!   which tab is active. After a `Diagnostics` fold every open tab's set is
//!   re-pushed to the editor as **inline markers** (#189, #353) — consuming the
//!   existing client diagnostics model (#178), so a fixed error clears its
//!   marker on the tab that owns it, even while that tab sits in the background.
//!   After an `UpdateGitStatus` fold, the diff view's open path (if any) is
//!   checked against `changed`/`cleared` — **live diff refresh** (#339): a tick
//!   marking it still changed re-requests its diff, one dropping it from the
//!   changed set (e.g. a commit) closes the view.
//! - **Buffer reply** (`buffer_rx`): a `FileContent { path, content, mtime }`
//!   loads into the [`EditorView`] (and that tab's inline diagnostics are
//!   re-applied, since opening rebuilt its input); a `SaveResult` / `SaveConflict`
//!   resolves a save on whichever tab holds `path` (commit the new base `mtime`,
//!   or surface the conflict without losing the buffer) — not necessarily the
//!   active tab, since a background dirty tab can save concurrently.
//!
//! The reverse path runs through three request channels. `open_file_tx` issues an
//! `OpenFile` read: when the tree emits [`FileTreeEvent::OpenFile`] (or the editor
//! auto-reloads), this view tells the editor to begin opening (arming its timeout)
//! and sends the path to the tokio side. `save_file_tx` carries a `SaveFile` the
//! editor builds from the active tab's buffer on a [`crate::editor::Save`].
//! `buffer_change_tx` carries the **live-buffer feed** (#189), driven **per dirty
//! tab** (#353) — the daemon holds one live buffer per path
//! (`crates/lsp/src/document.rs`), so several tabs can be live at once: the
//! editor sends `BufferChanged` on a debounced edit and `BufferClosed` only on an
//! actual tab close or a successful save, never on a mere tab switch, so the
//! daemon feeds the LSP the live buffer and an unsaved error surfaces without a
//! save first. The daemon's read/write replies return on `buffer_rx`; diagnostics
//! return on `worktree_rx` as `Diagnostics`. Nav replies return on `nav_rx` and
//! are routed to whichever tab's request `id` they answer (#196/#197/#198,
//! #351) — never the merely-active tab, so a stale response for a superseded
//! request can never land on the wrong tab.

use std::path::{Path, PathBuf};
use std::time::Duration;

use flume::{Receiver, Sender};
use gpui::{
    div, point, px, size, App, AppContext as _, Axis, Bounds, Context, Entity, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    Styled as _, Window, WindowBounds,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement};
use gpui_component::{ActiveTheme as _, Root};
use rift_protocol::{ClientMessage, DaemonMessage};
use rift_terminal::{SessionView, SessionViewEvent};
use tracing::{debug, warn};

use crate::command_palette::{CommandPalette, OpenCommandPalette};
use crate::diff_view::DiffView;
use crate::editor::EditorView;
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::problems_panel::{ProblemsPanel, ProblemsPanelEvent};
use crate::settings::{OpenSettings, SettingsView};
use crate::source_control::{SourceControlEvent, SourceControlPanel};
use crate::status_bar;
use crate::terminal_panel::TerminalPanel;
use crate::window_state;
use crate::{
    SelectCatppuccinMochaTheme, SelectDefaultDarkTheme, SelectDefaultLightTheme, ToggleThemeMode,
    CATPPUCCIN_MOCHA_THEME_NAME, DEFAULT_DARK_THEME_NAME, DEFAULT_LIGHT_THEME_NAME,
};

/// Debounce cadence for the window-state save timer — move/resize/maximize
/// (`observe_window_bounds`) and font-size changes
/// ([`SessionViewEvent::FontSizeChanged`]) all arm it (#225,
/// `docs/spec-window-state-persistence.md`). Zed's own precedent order
/// (~100-250ms), matching `editor::BUFFER_FEED_DEBOUNCE`'s scale.
const WINDOW_STATE_SAVE_DEBOUNCE: Duration = Duration::from_millis(200);

// ── Actions ───────────────────────────────────────────────────────────────────
//
// Shell command actions (`docs/spec-command-palette.md`, issue #358): Phase 10
// (`docs/spec-ide-shell.md`) delivers dock/panel toggling and zoom via the
// `DockArea`'s own mouse-driven controls, not as dispatchable `#[action]`
// types, so the command palette has nothing to bind these to. Defined here,
// beside the `dock_area` they target, and wired to it in
// [`WorkspaceView::render`]'s `on_action` handlers.

/// Toggle the explorer (left) dock hidden/shown.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ToggleExplorer;

/// Toggle the problems dock (bottom, home to the problems panel, #342)
/// hidden/shown.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ToggleProblems;

/// Toggle the source control dock (right, reserved for the Phase 12 source
/// control panel) hidden/shown.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ToggleSourceControl;

/// Move focus to the terminal, so keystrokes reach the active tmux pane.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct FocusTerminal;

/// Toggle zoom on the currently focused panel. Forwards to `gpui_component`'s
/// own `dock::ToggleZoom` — the action its built-in zoom button already
/// dispatches — so there is exactly one zoom code path (no parallel
/// execution mechanism).
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ZoomActivePanel;

/// Open the session switcher (`docs/spec-session-switch.md`): the popover
/// anchored to the statusbar session label, listing every host tmux session.
/// Handled at the workspace root (not inside the terminal) so the palette's
/// dispatch reaches it regardless of which surface holds focus.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SwitchSession;

/// Open the session switcher with its new-session prompt active
/// (`docs/spec-session-switch.md`): naming a fresh session attach-creates it
/// (the daemon child command is `new-session -A -s <name>`).
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct NewSession;

/// Initial width of the left (explorer) dock, in pixels. Purely a starting
/// point for the user's first resize — `DockArea` owns the size afterward,
/// replacing the old fixed explorer column (`docs/spec-ide-shell.md`, #324).
const LEFT_DOCK_WIDTH: f32 = 240.0;

/// Initial width of the (collapsed) right dock.
const RIGHT_DOCK_WIDTH: f32 = 240.0;

/// Initial height of the (collapsed) bottom dock.
const BOTTOM_DOCK_HEIGHT: f32 = 200.0;

/// Initial height of the source-control panel within the right dock's
/// top/bottom split (#338) — a compact changed-file list, leaving the
/// remaining right-dock height to the diff view below it.
const SOURCE_CONTROL_SPLIT_HEIGHT: f32 = 180.0;

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
    /// `StatusLineReply` replies to fold into the mirrored tmux status line's
    /// render model (#221, `docs/spec-tmux-statusline-mirroring.md`).
    pub status_line_rx: Receiver<DaemonMessage>,
    /// `FileDiff` replies to route to the diff view (#338).
    pub diff_rx: Receiver<DaemonMessage>,
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
    /// Diff pull requests: the root-relative path of a changed file to diff
    /// (#338). The tokio side turns each into a `ClientMessage::RequestDiff`.
    pub request_diff_tx: Sender<String>,
}

/// The composed app root.
pub struct WorkspaceView {
    file_tree: Entity<FileTree>,
    editor: Entity<EditorView>,
    session_view: Entity<SessionView>,
    /// The problems panel (`docs/spec-problems-panel.md`, #342): a read-only
    /// mirror of `file_tree`'s model, docked in the bottom zone. Kept as its
    /// own field (mirroring `file_tree`/`editor`) so tests can reach it
    /// directly rather than reaching into the dock's private panel tree. The
    /// jump-to-location wiring (#343) subscribes to the local `problems_panel`
    /// value at construction time rather than through this field, so the
    /// field itself is still read only by tests.
    // Use `allow` (not `expect`) since the field IS read under `cfg(test)`,
    // which would make an `expect(dead_code)` unfulfilled on the
    // `--all-targets` build.
    #[allow(dead_code)]
    problems_panel: Entity<ProblemsPanel>,
    /// Which status-bar render mode is active (#221): resolved once at
    /// startup from `RIFT_STATUSLINE_MIRROR` and never changed at runtime —
    /// the spec's v1 opt-in toggle, mutually exclusive with the native fields.
    status_bar_mode: status_bar::StatusBarMode,
    /// The mirrored tmux status line's current render model (#221), folded
    /// from the daemon's `StatusLineReply` stream. Read only when
    /// `status_bar_mode` is `Mirrored`; kept at its default (empty, no
    /// leftover state) otherwise.
    mirrored_status_line: status_bar::MirroredStatusLine,
    /// The diff view (`docs/spec-source-control.md`, #338): renders the
    /// `FileDiff` streamed for the source-control panel's selection. Kept as
    /// its own field for the same reason as `problems_panel` above; the
    /// open-diff subscription below reaches it through this field.
    #[allow(dead_code)]
    diff_view: Entity<DiffView>,
    /// Read-request sender: a tree open turns into a path on this channel, which
    /// the tokio side emits as `ClientMessage::OpenFile`.
    open_file_tx: Sender<String>,
    /// The dock shell (`docs/spec-ide-shell.md`, issue #324): explorer in the
    /// left dock, editor|terminal split in the center, source control + diff
    /// view (#338) split in the (collapsed by default) right dock, problems
    /// panel in the (collapsed by default) bottom dock. Holds its own strong
    /// references to the panel entities; this view keeps its own handles too
    /// (`file_tree`, `editor`, `problems_panel`, `diff_view` above) so the
    /// daemon-stream bridges keep working unchanged after the layout refactor.
    dock_area: Entity<DockArea>,
    /// The command palette (`docs/spec-command-palette.md`, issue #359): owns
    /// its `ListState` entity for the workspace's lifetime, so reopening it
    /// (`Ctrl+Shift+P` / `Cmd+Shift+P`) reuses the same list rather than
    /// rebuilding the registry each time.
    command_palette: CommandPalette,
    /// Where this instance's channel-keyed window-state file lives (#225).
    /// `None` when no platform state directory could be resolved
    /// (`window_state::state_path`'s failure mode) — capture then silently
    /// no-ops rather than crashing, matching the store's own contract.
    window_state_path: Option<PathBuf>,
    /// Monotonic generation fencing the debounced window-state save timer
    /// (mirrors `EditorView::arm_buffer_feed`'s `buffer_generation`): each
    /// arm bumps it, so an in-flight timer from an earlier move/resize sees
    /// the mismatch and no-ops instead of writing stale-but-superseded state.
    window_state_save_generation: u64,
    /// The settings surface (`docs/spec-theme-settings.md`, issue #366):
    /// theme mode, named theme, and font scale, hosted as a `Root` dialog
    /// like `command_palette` above.
    settings_view: SettingsView,
}

impl WorkspaceView {
    /// Build the workspace around an already-created [`SessionView`] entity (the
    /// terminal, created in `main.rs` so it keeps owning the SSH/daemon session
    /// thread). Creates the explorer and editor, mounts all three, and starts the
    /// daemon-stream bridges.
    pub fn new(
        session_view: Entity<SessionView>,
        channels: WorkspaceChannels,
        window_state_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let WorkspaceChannels {
            worktree_rx,
            buffer_rx,
            nav_rx,
            status_line_rx,
            diff_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            request_diff_tx,
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
                    let is_repo_state = matches!(msg, DaemonMessage::RepoState { .. });
                    // Live diff refresh (#339): pull the changed/cleared paths out
                    // before `msg` moves into the fold below, so the diff view can
                    // be reacted to afterward without a second pass over the model.
                    let git_status_update = match &msg {
                        DaemonMessage::UpdateGitStatus { changed, cleared } => Some((
                            changed
                                .iter()
                                .map(|entry| entry.path.clone())
                                .collect::<Vec<String>>(),
                            cleared.clone(),
                        )),
                        _ => None,
                    };
                    view.file_tree.update(cx, |tree, cx| {
                        apply_worktree_message(tree, msg);
                        cx.notify();
                    });
                    // Status bar (#347, #348): a `RepoState` fold changes the
                    // branch/ahead-behind segment and a `Diagnostics` fold changes
                    // the error/warning counts segment, both read inline in
                    // `WorkspaceView::render`, so the workspace view itself must
                    // notify too — the file tree's own notify above only repaints
                    // the tree.
                    if is_repo_state || is_diagnostics {
                        cx.notify();
                    }
                    // Concurrent-write signal (#188), fanned out per open tab
                    // (#353): after folding the structure update, hand every open
                    // tab its own path's new snapshot `mtime`. Each tab compares it
                    // against its own buffer's base `mtime` and auto-reloads
                    // (clean) or surfaces a conflict (dirty), independent of which
                    // tab is active. Tapping the model the tree already mirrors
                    // keeps the comparison base-vs-snapshot — never an independent
                    // stat.
                    view.notify_editor_of_open_paths_mtime(window, cx);
                    // Inline diagnostics (#189), fanned out per open tab (#353):
                    // after a diagnostics fold, re-push every open tab's full set
                    // to the editor so each tab's inline markers converge with the
                    // model (fixing an error clears the marker on the tab that
                    // owns it, even while that tab sits in the background). Only
                    // on a diagnostics message — the structure folds do not touch
                    // the diagnostics map.
                    if is_diagnostics {
                        view.push_diagnostics_for_open_tabs(cx);
                    }
                    // Live diff refresh (#339): a status tick that marks the open
                    // diff's path changed re-requests it; one that drops it from
                    // the changed set (e.g. a commit) closes the view instead.
                    if let Some((changed, cleared)) = git_status_update {
                        view.diff_view.update(cx, |diff_view, cx| {
                            diff_view.apply_git_update(&changed, &cleared, cx);
                        });
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
                        DaemonMessage::SaveConflict { path, disk_mtime } => {
                            editor.apply_save_conflict(path, disk_mtime, cx)
                        }
                        _ => {}
                    });
                    // After a load the editor rebuilt that tab's input with an
                    // empty `DiagnosticSet`, so re-apply every open tab's inline
                    // markers (#189, #353) from the model the tree mirrors — a
                    // no-op for tabs whose diagnostics did not change.
                    if loaded {
                        view.push_diagnostics_for_open_tabs(cx);
                        // Reveal active file (#331): a `FileContent` load means
                        // the editor just finished opening or switching to a
                        // file — whether the open originated from a tree click
                        // or a cross-file go-to-definition jump, both request
                        // it over `open_file_tx` and land here.
                        view.reveal_open_file_in_tree(cx);
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

        // Mirrored status line stream -> render model (#221): each
        // `StatusLineReply` rebuilds the parsed left/right runs and the
        // resolved base style, then a notify repaints the status bar (folded
        // regardless of `status_bar_mode`, mirroring the nav/buffer loops
        // above — the render branch, not this fold, is what makes the two
        // modes exclusive). Routed through this view's weak handle so a
        // closed window ends the loop gracefully.
        {
            cx.spawn(async move |this, cx| loop {
                let Ok(msg) = status_line_rx.recv_async().await else {
                    break;
                };
                let result = this.update(cx, |view, cx| {
                    let DaemonMessage::StatusLineReply {
                        options,
                        status_left,
                        status_right,
                    } = msg
                    else {
                        return;
                    };
                    view.mirrored_status_line = status_bar::build_mirrored_status_line(
                        &options,
                        &status_left,
                        &status_right,
                    );
                    cx.notify();
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        // Diff reply stream -> diff view (#338): `FileDiff` routes to
        // `apply_file_diff`, which drops a reply for a path no longer open (the
        // user moved on before it arrived). Routed through this view's weak
        // handle so a closed window ends the loop gracefully, mirroring the nav
        // reply bridge above.
        {
            cx.spawn_in(window, async move |this, cx| loop {
                let Ok(msg) = diff_rx.recv_async().await else {
                    break;
                };
                let result = this.update_in(cx, |view, _window, cx| {
                    let DaemonMessage::FileDiff { path, diff } = msg else {
                        return;
                    };
                    view.diff_view.update(cx, |diff_view, cx| {
                        diff_view.apply_file_diff(path, diff, cx);
                    });
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        // Dock shell (`docs/spec-ide-shell.md`, issue #324): explorer (left) +
        // editor|terminal (center split) + source control + diff view split
        // (right, #337, #338) + problems panel (bottom, #342). Built after the
        // daemon-stream bridges above so the panel entities they close over
        // already exist.
        let dock_area = cx.new(|cx| DockArea::new("rift-dock", Some(1), window, cx));
        let weak_dock_area = dock_area.downgrade();

        let terminal_panel = cx.new(|_| TerminalPanel::new(session_view.clone()));
        // Reads (never mutates) the file tree's mirrored `WorktreeModel` for its
        // git status — the existing client git-status model, not a re-derived
        // copy (`docs/spec-source-control.md`).
        let source_control = cx.new(|cx| SourceControlPanel::new(file_tree.clone(), cx));
        let diff_view = cx.new(|cx| DiffView::new(request_diff_tx, cx));
        let problems_panel = cx.new(|cx| ProblemsPanel::new(file_tree.clone(), cx));

        // Open-diff wiring (#338): selecting a changed file in the
        // source-control panel opens its diff in the diff view — the clean
        // signal the panel emits (`SourceControlEvent::OpenDiff`), routed here
        // so the diff view issues the `RequestDiff` and renders the reply.
        // Mirrors the file tree's `OpenFile` subscription above.
        cx.subscribe_in(
            &source_control,
            window,
            |this, _panel, event: &SourceControlEvent, _window, cx| {
                let SourceControlEvent::OpenDiff { path } = event;
                this.diff_view.update(cx, |diff_view, cx| {
                    diff_view.open_diff(path.clone(), cx);
                });
            },
        )
        .detach();

        // Jump-to-location (#343): selecting a diagnostic emits `OpenLocation`,
        // routed to the editor's thin `open_at_range` wrapper around the same
        // LSP-nav jump machinery go-to-definition already drives. Mirrors the
        // file tree's `OpenFile` subscription above, minus the `open_file_tx`
        // send — `EditorView::open_at_range` already issues that itself
        // (via `jump_to_location`) on a cross-file jump.
        cx.subscribe_in(
            &problems_panel,
            window,
            |this, _panel, event: &ProblemsPanelEvent, window, cx| {
                let ProblemsPanelEvent::OpenLocation { path, range } = event;
                this.editor.update(cx, |editor, cx| {
                    editor.open_at_range(path.clone(), *range, window, cx);
                });
            },
        )
        .detach();

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
        // Both real, collapsed docks (not placeholder views) — a single-tab
        // `TabPanel`, collapsed by default until the user opens it. The right
        // dock is a vertical split (#338): the changed-file list stays compact
        // on top, the diff view takes the remaining height below it — both
        // signal panels visible together, matching the review flow (select a
        // file, read its diff, without switching tabs).
        let right_item = DockItem::split_with_sizes(
            Axis::Vertical,
            vec![
                DockItem::tab(source_control, &weak_dock_area, window, cx),
                DockItem::tab(diff_view.clone(), &weak_dock_area, window, cx),
            ],
            vec![Some(px(SOURCE_CONTROL_SPLIT_HEIGHT)), None],
            &weak_dock_area,
            window,
            cx,
        );
        let bottom_item = DockItem::tab(problems_panel.clone(), &weak_dock_area, window, cx);

        dock_area.update(cx, |dock, cx| {
            dock.set_center(center_item, window, cx);
            dock.set_left_dock(left_item, Some(px(LEFT_DOCK_WIDTH)), true, window, cx);
            dock.set_right_dock(right_item, Some(px(RIGHT_DOCK_WIDTH)), false, window, cx);
            dock.set_bottom_dock(bottom_item, Some(px(BOTTOM_DOCK_HEIGHT)), false, window, cx);
        });

        let command_palette = CommandPalette::new(window, cx);
        let settings_view = SettingsView::new(session_view.clone());

        // Window-state capture (#225, docs/spec-window-state-persistence.md):
        // observe move/resize/maximize and the terminal's font-size changes,
        // and flush on a clean close. Registered unconditionally;
        // `arm_window_state_save` and the close handler both no-op when
        // `window_state_path` is `None`, so persistence degrades to silently
        // doing nothing rather than crashing (the store's own contract).
        cx.observe_window_bounds(window, |this, window, cx| {
            this.arm_window_state_save(window, cx);
        })
        .detach();
        cx.subscribe_in(
            &session_view,
            window,
            |this, _session_view, _event: &SessionViewEvent, window, cx| {
                this.arm_window_state_save(window, cx);
            },
        )
        .detach();
        {
            let session_view = session_view.clone();
            let window_state_path = window_state_path.clone();
            window.on_window_should_close(cx, move |window, cx| {
                if let Some(path) = &window_state_path {
                    save_window_state(path, window, &session_view, cx);
                }
                true
            });
        }

        Self {
            file_tree,
            editor,
            session_view,
            problems_panel,
            status_bar_mode: status_bar::StatusBarMode::from_env(),
            mirrored_status_line: status_bar::MirroredStatusLine::default(),
            diff_view,
            open_file_tx,
            dock_area,
            command_palette,
            window_state_path,
            window_state_save_generation: 0,
            settings_view,
        }
    }

    /// Arm (or re-arm) the debounced window-state save (#225): bumps the
    /// fencing generation so an in-flight timer from an earlier move/resize
    /// sees the mismatch and no-ops instead of writing stale-but-superseded
    /// state (mirrors `EditorView::arm_buffer_feed`). A no-op when no state
    /// path was resolved at startup.
    fn arm_window_state_save(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.window_state_path.is_none() {
            return;
        }
        self.window_state_save_generation = self.window_state_save_generation.wrapping_add(1);
        let generation = self.window_state_save_generation;
        cx.spawn_in(window, async move |this, cx| {
            smol::Timer::after(WINDOW_STATE_SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |view, window, cx| {
                if view.window_state_save_generation != generation {
                    return;
                }
                let Some(path) = view.window_state_path.as_deref() else {
                    return;
                };
                save_window_state(path, window, &view.session_view, cx);
            });
        })
        .detach();
    }

    /// Feed every open tab its path's current snapshot `mtime` as the
    /// concurrent-write signal (#188), fanned out per tab (#353): each open
    /// path is looked up independently in the file tree's mirrored model —
    /// the same `mtime` that tab's buffer base is compared against — and,
    /// when present, handed to [`EditorView::note_external_change_for_path`].
    /// A path not (yet) in the tree is left untouched; the editor itself
    /// ignores a path with no open tab.
    fn notify_editor_of_open_paths_mtime(&self, window: &mut Window, cx: &mut Context<Self>) {
        let open_paths: Vec<String> = self
            .editor
            .read(cx)
            .open_paths()
            .map(str::to_owned)
            .collect();
        for path in open_paths {
            let Some(mtime) = self.file_tree.read(cx).model().get(&path).map(|e| e.mtime) else {
                continue;
            };
            self.editor.update(cx, |editor, cx| {
                editor.note_external_change_for_path(&path, mtime, window, cx);
            });
        }
    }

    /// Push every open tab's full diagnostic set (aggregated across servers)
    /// from the file tree's model onto its inline markers (#189), fanned out
    /// per tab (#353). Looks each open tab's path up in the same model the
    /// tree mirrors — the existing client diagnostics layer (#178), consumed,
    /// not redesigned — and hands a flattened `Vec<Diagnostic>` to
    /// [`EditorView::set_diagnostics_for_path`]. An open tab with no
    /// diagnostics applies an empty set, which clears its inline markers (so
    /// a fixed error clears on the tab that owns it).
    fn push_diagnostics_for_open_tabs(&self, cx: &mut Context<Self>) {
        let open_paths: Vec<String> = self
            .editor
            .read(cx)
            .open_paths()
            .map(str::to_owned)
            .collect();
        for path in open_paths {
            // Flatten the per-server sets into one list — the editor renders
            // all servers' diagnostics for the file together (a linter + a
            // type-checker aggregate), mirroring the model's
            // per-`(file, server)` keying.
            let items: Vec<rift_protocol::Diagnostic> = self
                .file_tree
                .read(cx)
                .model()
                .diagnostics(&path)
                .map(|by_server| by_server.values().flatten().cloned().collect())
                .unwrap_or_default();
            self.editor.update(cx, |editor, cx| {
                editor.set_diagnostics_for_path(&path, &items, cx);
            });
        }
    }

    /// Reveal the editor's currently open file in the explorer tree (#331):
    /// expand its ancestor directories, select its row, and scroll it into
    /// view. Reads the *active* tab's path (unlike the per-tab fan-out above,
    /// this is a single-target UI concern — which file to reveal), rather
    /// than from the triggering daemon message, so it always reflects the tab
    /// that is actually active once the load lands. A no-op (via
    /// [`FileTree::reveal`]) when no file is open or the path is not (yet)
    /// present in the tree's mirrored model.
    fn reveal_open_file_in_tree(&self, cx: &mut Context<Self>) {
        let Some(open_path) = self
            .editor
            .read(cx)
            .open_path()
            .map(std::borrow::ToOwned::to_owned)
        else {
            return;
        };
        self.file_tree.update(cx, |tree, cx| {
            tree.reveal(&open_path);
            cx.notify();
        });
    }

    /// Toggle the explorer (left) dock hidden/shown (issue #358).
    fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.toggle_dock(DockPlacement::Left, window, cx);
        });
    }

    /// Toggle the problems dock (bottom) hidden/shown (issue #358).
    fn toggle_problems(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.toggle_dock(DockPlacement::Bottom, window, cx);
        });
    }

    /// Toggle the source control dock (right) hidden/shown (issue #358).
    fn toggle_source_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.toggle_dock(DockPlacement::Right, window, cx);
        });
    }

    /// Move focus to the terminal (issue #358), so keystrokes reach the tmux
    /// pane exactly as they do when the terminal is clicked directly.
    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_view.focus_handle(cx).focus(window, cx);
    }

    /// Toggle zoom on the currently focused panel (issue #358): re-dispatches
    /// `gpui_component`'s own `ToggleZoom`, which bubbles from the focused
    /// element up to whichever `TabPanel` contains it — the same path its
    /// built-in zoom button already drives.
    fn zoom_active_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.dispatch_action(Box::new(gpui_component::dock::ToggleZoom), cx);
    }

    /// Open the command palette (issue #359) as a `Root` dialog.
    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_palette.open(window, cx);
    }

    /// Open the settings surface (issue #366) as a `Root` dialog.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_view.open(window, cx);
    }

    /// Toggle light/dark mode (issue #367), keeping whichever named theme is
    /// currently assigned to each slot.
    fn toggle_theme_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::toggle_theme_mode_persisted(Some(window), cx);
    }

    /// Switch the active theme by name (issue #367), restyling the running UI
    /// live.
    fn select_theme(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        crate::set_theme_persisted(name, Some(window), cx);
    }
}

/// Capture the live window/font geometry and write it to `path` (#225) — the
/// single write call both the debounced save (`arm_window_state_save`) and
/// the close-flush (`WorkspaceView::new`'s `on_window_should_close`) funnel
/// through. Read-modify-write via [`window_state::save_geometry`]: the theme
/// fields are a separate concern (`crate::set_theme_persisted` et al., via
/// `window_state::save_theme`) and must survive an unrelated geometry save
/// untouched. `window.window_bounds()` — not `window.bounds()` — is
/// deliberate: it carries the *restore* size for a maximized/fullscreen
/// window, so persisting it never overwrites the saved size with the
/// full-display dimensions on every maximized capture.
fn save_window_state(path: &Path, window: &Window, session_view: &Entity<SessionView>, cx: &App) {
    let bounds = window.window_bounds().get_bounds();
    let rect = window_state::Rect {
        x: f64::from(bounds.origin.x),
        y: f64::from(bounds.origin.y),
        width: f64::from(bounds.size.width),
        height: f64::from(bounds.size.height),
    };
    let maximized = window.is_maximized();
    let font_size_px = f32::from(session_view.read(cx).font_size());
    if let Err(e) = window_state::save_geometry(path, rect, maximized, font_size_px) {
        warn!(%e, "failed to save window state");
    }
}

/// Convert the platform's active displays into the store's plain `Rect`s for
/// [`window_state::clamp_bounds`] — `window_state` is deliberately GPUI-free
/// (its own module doc), so this seam does the one conversion GPUI-side
/// (#225). Called from `main.rs` before the window is created.
pub fn display_rects(cx: &App) -> Vec<window_state::Rect> {
    cx.displays()
        .iter()
        .map(|display| {
            let bounds = display.bounds();
            window_state::Rect {
                x: f64::from(bounds.origin.x),
                y: f64::from(bounds.origin.y),
                width: f64::from(bounds.size.width),
                height: f64::from(bounds.size.height),
            }
        })
        .collect()
}

/// Convert a clamped store `Rect` into GPUI's `Bounds<Pixels>`.
fn gpui_bounds(rect: window_state::Rect) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(rect.x as f32), px(rect.y as f32)),
        size: size(px(rect.width as f32), px(rect.height as f32)),
    }
}

/// Decide the `WindowOptions.window_bounds` to open with from restored state
/// (#225): clamp the persisted bounds against the live display topology, then
/// carry the maximized flag through as the matching `WindowBounds` variant —
/// `Maximized` wraps the *restore* bounds, mirroring how
/// [`save_window_state`] itself reads `window.window_bounds()`, so a
/// maximized restart reopens maximized instead of at the un-maximized size.
/// Called from `main.rs` before the window is created.
pub fn initial_window_bounds(
    state: &window_state::WindowState,
    displays: &[window_state::Rect],
) -> WindowBounds {
    let bounds = gpui_bounds(window_state::clamp_bounds(state.bounds, displays));
    if state.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The dock shell fills the window under the current OS chrome; the
        // `flex_col` mirrors `examples/dock.rs` at the pinned gpui-component rev.
        // The status bar (#347, #348, `docs/spec-status-bar.md`) is a plain
        // `flex_col` sibling below the dock — bottom chrome, not a dock `Panel` —
        // reading the file tree's mirrored `WorktreeModel` inline (repainted by
        // the `RepoState`/`Diagnostics`-fold `cx.notify()` above).
        //
        // The shell command actions (issue #358) are handled here, at the
        // workspace root, rather than scoped to a key context: they are
        // global commands the command palette dispatches regardless of which
        // panel currently has focus.
        //
        // Render mode (#221, `docs/spec-tmux-statusline-mirroring.md`): the
        // native fields and the mirrored tmux status line are mutually
        // exclusive branches, never composed. `status_bar_mode` is fixed for
        // the process's lifetime (resolved once from an env var at
        // construction), so this never toggles mid-session.
        let status_bar = match self.status_bar_mode {
            status_bar::StatusBarMode::Native => {
                let model = self.file_tree.read(cx).model();
                status_bar::render(
                    model.repo_state_received(),
                    model.branch(),
                    model.ahead_behind(),
                    model.all_diagnostics(),
                    cx,
                )
                .into_any_element()
            }
            status_bar::StatusBarMode::Mirrored => {
                status_bar::render_mirrored(&self.mirrored_status_line, cx).into_any_element()
            }
        };

        // `Root`'s overlay layers (issue #359, `docs/spec-command-palette.md`):
        // only the gallery rendered these before this issue, so no modal —
        // including the command palette below — ever appeared in the shipped
        // app. Read here, at the workspace root, mirroring `examples/dock.rs`
        // at the pinned gpui-component rev.
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &ToggleExplorer, window, cx| {
                this.toggle_explorer(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleProblems, window, cx| {
                this.toggle_problems(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSourceControl, window, cx| {
                this.toggle_source_control(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusTerminal, window, cx| {
                this.focus_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomActivePanel, window, cx| {
                this.zoom_active_panel(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchSession, _window, cx| {
                this.session_view.update(cx, |session, cx| {
                    session.open_session_switcher(cx);
                });
            }))
            .on_action(cx.listener(|this, _: &NewSession, window, cx| {
                this.session_view.update(cx, |session, cx| {
                    session.open_new_session_prompt(window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                this.open_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleThemeMode, window, cx| {
                this.toggle_theme_mode(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &SelectDefaultLightTheme, window, cx| {
                    this.select_theme(DEFAULT_LIGHT_THEME_NAME, window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &SelectDefaultDarkTheme, window, cx| {
                this.select_theme(DEFAULT_DARK_THEME_NAME, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &SelectCatppuccinMochaTheme, window, cx| {
                    this.select_theme(CATPPUCCIN_MOCHA_THEME_NAME, window, cx);
                }),
            )
            .child(self.dock_area.clone())
            .child(status_bar)
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Axis, TestAppContext};
    use gpui_component::dock::{DockPlacement, Panel};
    use gpui_component::WindowExt as _;

    // --- window-state restore decision (#225) --------------------------------
    // Headless: `initial_window_bounds` and the types it returns
    // (`WindowBounds`/`Bounds<Pixels>`) are plain data, so these run as
    // ordinary `#[test]`s — no GPUI test harness needed.

    fn display(x: f64, y: f64, width: f64, height: f64) -> window_state::Rect {
        window_state::Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn test_initial_window_bounds_windowed_state_restores_windowed() {
        let state = window_state::WindowState {
            maximized: false,
            bounds: window_state::Rect {
                x: 100.0,
                y: 100.0,
                width: 900.0,
                height: 600.0,
            },
            ..window_state::WindowState::default()
        };
        let displays = [display(0.0, 0.0, 1920.0, 1080.0)];

        let bounds = initial_window_bounds(&state, &displays);

        assert!(matches!(bounds, WindowBounds::Windowed(_)));
    }

    #[test]
    fn test_initial_window_bounds_maximized_state_restores_maximized() {
        let state = window_state::WindowState {
            maximized: true,
            ..window_state::WindowState::default()
        };
        let displays = [display(0.0, 0.0, 1920.0, 1080.0)];

        let bounds = initial_window_bounds(&state, &displays);

        assert!(matches!(bounds, WindowBounds::Maximized(_)));
    }

    #[test]
    fn test_initial_window_bounds_clamps_off_screen_state_onto_the_display() {
        let state = window_state::WindowState {
            maximized: false,
            bounds: window_state::Rect {
                x: -5000.0,
                y: -5000.0,
                width: 800.0,
                height: 600.0,
            },
            ..window_state::WindowState::default()
        };
        let displays = [display(0.0, 0.0, 1920.0, 1080.0)];

        let bounds = initial_window_bounds(&state, &displays).get_bounds();

        assert!(f32::from(bounds.origin.x) >= 0.0);
        assert!(f32::from(bounds.origin.y) >= 0.0);
    }

    /// A `WorkspaceChannels` wired to throwaway flume endpoints — no daemon is
    /// attached in this test, only the dock's panel tree is under test.
    fn test_channels() -> WorkspaceChannels {
        let (_worktree_tx, worktree_rx) = flume::unbounded();
        let (_buffer_tx, buffer_rx) = flume::unbounded();
        let (_nav_reply_tx, nav_rx) = flume::unbounded();
        let (_status_line_tx, status_line_rx) = flume::unbounded();
        let (_diff_reply_tx, diff_rx) = flume::unbounded();
        let (open_file_tx, _open_file_rx) = flume::unbounded();
        let (save_file_tx, _save_file_rx) = flume::unbounded();
        let (buffer_change_tx, _buffer_change_rx) = flume::unbounded();
        let (nav_tx, _nav_request_rx) = flume::unbounded();
        let (request_diff_tx, _request_diff_rx) = flume::unbounded();
        WorkspaceChannels {
            worktree_rx,
            buffer_rx,
            nav_rx,
            status_line_rx,
            diff_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            request_diff_tx,
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
                workspace =
                    Some(cx.new(|cx| {
                        WorkspaceView::new(session_view, test_channels(), None, window, cx)
                    }));
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

            let right_dock = dock_area.right_dock().expect("right dock must exist");
            assert!(
                !dock_area.is_dock_open(DockPlacement::Right, cx),
                "right dock starts collapsed"
            );
            match right_dock.read(cx).panel() {
                DockItem::Split { axis, items, .. } => {
                    assert_eq!(
                        *axis,
                        Axis::Vertical,
                        "source-control/diff split is vertical (#338)"
                    );
                    assert_eq!(
                        items.len(),
                        2,
                        "right dock holds source-control + diff view"
                    );

                    match &items[0] {
                        DockItem::Tabs { items, .. } => assert_eq!(
                            items[0].panel_name(cx),
                            crate::source_control::SOURCE_CONTROL_PANEL_NAME,
                            "right dock's top split is the source-control panel"
                        ),
                        other => panic!("expected a tabs item, got {other:?}"),
                    }
                    match &items[1] {
                        DockItem::Tabs { items, .. } => assert_eq!(
                            items[0].panel_name(cx),
                            crate::diff_view::DIFF_VIEW_PANEL_NAME,
                            "right dock's bottom split is the diff view"
                        ),
                        other => panic!("expected a tabs item, got {other:?}"),
                    }
                }
                other => panic!("expected the right dock to hold a split, got {other:?}"),
            }

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
                workspace =
                    Some(cx.new(|cx| {
                        WorkspaceView::new(session_view, test_channels(), None, window, cx)
                    }));
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
                workspace =
                    Some(cx.new(|cx| {
                        WorkspaceView::new(session_view, test_channels(), None, window, cx)
                    }));
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
                workspace =
                    Some(cx.new(|cx| {
                        WorkspaceView::new(session_view, test_channels(), None, window, cx)
                    }));
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
                    Some(cx.new(|cx| WorkspaceView::new(view, test_channels(), None, window, cx)));
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

    /// Shell command action (`docs/spec-command-palette.md`, issue #358): the
    /// `ToggleExplorer` handler reaches the same `DockArea::toggle_dock` call
    /// `test_toggle_left_dock_flips_open_state` already exercises directly —
    /// this proves the action is wired to the dock, not just defined.
    #[gpui::test]
    fn test_toggle_explorer_flips_left_dock_open_state(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace =
                    Some(cx.new(|cx| {
                        WorkspaceView::new(session_view, test_channels(), None, window, cx)
                    }));
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

                workspace.update(cx, |view, cx| {
                    view.toggle_explorer(window, cx);
                });
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "ToggleExplorer hides the explorer dock"
                );
            })
            .unwrap();
    }

    /// Shell command action (issue #358): `ToggleProblems` reaches the bottom
    /// dock (home to the problems panel, #342), which starts collapsed.
    #[gpui::test]
    fn test_toggle_problems_flips_bottom_dock_open_state(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace =
                    Some(cx.new(|cx| {
                        WorkspaceView::new(session_view, test_channels(), None, window, cx)
                    }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let dock_area = workspace.read(cx).dock_area.clone();
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                    "problems dock starts collapsed"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_problems(window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                    "ToggleProblems opens the bottom dock"
                );
            })
            .unwrap();
    }

    /// Shell command action (issue #358): `ToggleSourceControl` reaches the
    /// right dock (reserved for the Phase 12 source control panel), which
    /// starts collapsed.
    #[gpui::test]
    fn test_toggle_source_control_flips_right_dock_open_state(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace =
                    Some(cx.new(|cx| {
                        WorkspaceView::new(session_view, test_channels(), None, window, cx)
                    }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let dock_area = workspace.read(cx).dock_area.clone();
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "source control dock starts collapsed"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_source_control(window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "ToggleSourceControl opens the right dock"
                );
            })
            .unwrap();
    }

    /// Shell command action (issue #358): `FocusTerminal` moves focus to the
    /// terminal, exactly as `WorkspaceView`'s own handed-off focus does.
    #[gpui::test]
    fn test_focus_terminal_moves_focus_to_the_terminal(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace =
                    Some(cx.new(|cx| WorkspaceView::new(view, test_channels(), None, window, cx)));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");
        let session_view =
            session_view.expect("session view constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                workspace.update(cx, |view, cx| {
                    view.focus_terminal(window, cx);
                });
                assert!(
                    session_view.focus_handle(cx).is_focused(window),
                    "FocusTerminal moves focus to the terminal"
                );
            })
            .unwrap();
    }

    /// Command palette (`docs/spec-command-palette.md`, issue #359): opening
    /// sets an active `Root` dialog (the modal wired via `render_dialog_layer`
    /// actually appears) and closing it clears that state, without disturbing
    /// the workspace's own focus delegation to the terminal — "dismissing the
    /// palette leaves terminal/editor state untouched" from the spec.
    #[gpui::test]
    fn test_open_command_palette_opens_a_dialog_and_close_clears_it(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace =
                    Some(cx.new(|cx| WorkspaceView::new(view, test_channels(), None, window, cx)));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");
        let session_view =
            session_view.expect("session view constructed inside the window callback");

        // `window.update` (`WindowHandle<Root>::update`) itself leases the
        // `Root` entity for the closure's duration — but `has_active_dialog` /
        // `open_dialog` / `close_dialog` all read or update that same `Root`
        // entity internally, which double-leases and panics. `update_window`
        // (`AppContext`, imported above via `super::*`) hands back the raw
        // window state without leasing `Root`, so those calls nest safely.
        cx.update_window(window.into(), |_, window, cx| {
            assert!(
                !window.has_active_dialog(cx),
                "no dialog is open before the shortcut fires"
            );

            workspace.update(cx, |view, cx| {
                view.open_command_palette(window, cx);
            });
            assert!(
                window.has_active_dialog(cx),
                "OpenCommandPalette opens a Root dialog"
            );
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening the palette does not move the workspace's terminal focus delegation"
            );

            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "closing the dialog clears the active-dialog state"
            );
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "dismissing the palette leaves the terminal focus delegation untouched"
            );
        })
        .unwrap();
    }

    /// Settings surface (`docs/spec-theme-settings.md`, issue #366): opening
    /// sets an active `Root` dialog, mirroring the command palette above, and
    /// closing it clears that state without disturbing the workspace's own
    /// focus delegation to the terminal.
    #[gpui::test]
    fn test_open_settings_opens_a_dialog_and_close_clears_it(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace =
                    Some(cx.new(|cx| WorkspaceView::new(view, test_channels(), None, window, cx)));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");
        let session_view =
            session_view.expect("session view constructed inside the window callback");

        cx.update_window(window.into(), |_, window, cx| {
            assert!(
                !window.has_active_dialog(cx),
                "no dialog is open before the shortcut fires"
            );

            workspace.update(cx, |view, cx| {
                view.open_settings(window, cx);
            });
            assert!(
                window.has_active_dialog(cx),
                "OpenSettings opens a Root dialog"
            );
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening settings does not move the workspace's terminal focus delegation"
            );

            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "closing the dialog clears the active-dialog state"
            );
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "dismissing settings leaves the terminal focus delegation untouched"
            );
        })
        .unwrap();
    }
}
