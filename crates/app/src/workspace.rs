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
//!   active tab, since a background dirty tab can save concurrently. An
//!   `OpenError` / `SaveError` (`docs/spec-v1-hardening.md`) names why the
//!   daemon refused the read/write, surfaced immediately instead of waiting
//!   out the editor's own `OPEN_TIMEOUT` / `SAVE_TIMEOUT`.
//!
//! `daemon_unavailable_rx` is not a `DaemonMessage` and does not come from
//! `consume_daemon_messages` — no daemon client exists yet when it fires. It
//! fires from `run_ssh_session` itself (#619) the moment the daemon terminal
//! is selected but provisioning came back empty, and folds into a persistent
//! `Root` notification rather than any panel model.
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use flume::{Receiver, Sender};
use gpui::{
    div, point, px, size, App, AppContext as _, Axis, Bounds, Context, Entity, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    SharedString, Styled as _, Subscription, Window, WindowBounds,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{AlertDialog, DialogButtonProps};
use gpui_component::dock::{Dock, DockArea, DockItem, DockPlacement, PanelView};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme as _, IconName, Root, Sizable as _, WindowExt as _};
use rift_protocol::{
    ClientMessage, DaemonMessage, DirBrowseError, DirEntry, EntryKind, LspServerState,
};
use rift_terminal::{SessionView, SessionViewEvent};
use tracing::{debug, warn};

use crate::activity_rail;
use crate::command_palette::{CommandPalette, OpenCommandPalette};
use crate::diff_view::DiffView;
use crate::editor::{EditorEvent, EditorView};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::outline_panel::{OutlinePanel, OutlinePanelEvent};
use crate::problems_panel::{ProblemsPanel, ProblemsPanelEvent};
use crate::quick_open::{OpenQuickOpen, QuickOpen};
use crate::results_panel::{ResultsPanel, ResultsPanelEvent};
use crate::root_picker::{self, RootPicker, RootPickerEvent};
use crate::settings::{OpenSettings, SettingsView};
use crate::source_control::{SourceControlEvent, SourceControlPanel};
use crate::status_bar;
use crate::terminal_panel::TerminalPanel;
use crate::title_bar;
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

/// Toggle the outline panel (left dock, alongside the explorer) shown/hidden
/// (`docs/spec-editor-chrome.md`, issue #530). Unlike `ToggleExplorer` this
/// does not toggle the whole left dock's open state — the outline panel is a
/// second tab added to (or removed from) the left dock's `TabPanel` via
/// `DockArea::add_panel`/`remove_panel`, opening the dock too when shown. The
/// activity-rail icon for this is a phase-21 follow-up.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ToggleOutline;

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

/// Request an on-demand session-list refresh (`docs/spec-session-management.md`).
/// The always-visible title-bar session strip (#683) replaced the phase-19
/// click-to-open popover this used to open, so this is now a manual nudge
/// rather than a toggle. Handled at the workspace root (not inside the
/// terminal) so the palette's dispatch reaches it regardless of which surface
/// holds focus.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SwitchSession;

/// Activate the session strip's trailing new-session prompt
/// (`docs/spec-session-management.md`): naming a fresh session attach-creates
/// it (the daemon child command is `new-session -A -s <name>`).
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct NewSession;

/// Manually refresh the mirrored tmux key tables (the escape hatch the
/// keytable-mirroring spec mandates, relocated off the removed statusbar to a
/// command-palette entry, `docs/spec-status-line.md`). Handled at the workspace
/// root, driving the terminal's `key_table_request_tx`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct RefreshKeyTables;

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

/// Width of the in-cockpit root-picker dialog (issue #769,
/// `docs/spec-session-root-picker.md`) — `RootPicker`'s own card is 470px
/// (`root_picker::CARD_WIDTH`, private to that module); this leaves it a
/// small margin inside the dialog chrome rather than matching exactly.
const ROOT_PICKER_DIALOG_WIDTH: f32 = 520.0;

/// Notification id marker for the daemon-unavailable banner (#619) — pushing
/// with this type keeps a repeat signal (e.g. a reconnect that still finds no
/// daemon) from stacking a second notification instead of replacing the first.
struct DaemonUnavailableNotification;

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
    /// `LspStatus` pushes to fold into the composite status line's
    /// language-server health map (`docs/spec-status-line.md`).
    pub lsp_status_rx: Receiver<DaemonMessage>,
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
    /// Git write ops the source-control panel emits — `StageFile`,
    /// `UnstageFile`, `DiscardFile`, `Commit` (#546). The tokio side forwards
    /// each onto the protocol verbatim; the daemon's `ok`/`error` reply is not
    /// echoed into state, the resulting git change arrives on the push
    /// recompute (`docs/spec-source-control-write.md`).
    pub git_op_tx: Sender<ClientMessage>,
    /// File ops the file tree emits — `CreateFile`, `CreateDir`,
    /// `RenamePath`, `DeletePath` (`docs/spec-explorer-file-ops.md`, #675).
    /// The tokio side forwards each onto the protocol verbatim, bridged
    /// exactly like `git_op_tx`.
    pub file_op_tx: Sender<ClientMessage>,
    /// `FileOpResult` replies routed to the file tree for UX transitions only
    /// (`docs/spec-explorer-file-ops.md`): unlike the git-write channel, this
    /// reply IS routed back — the tree never mutates `WorktreeModel` from it
    /// (the push-only `UpdateWorktree` is the single writer), but it does
    /// close the rename editor on success or re-open it with an error.
    pub file_op_result_rx: Receiver<DaemonMessage>,
    /// Fires once whenever the tokio side selected the daemon terminal but no
    /// daemon came up (#619, `docs/spec-v1-hardening.md`): the session still
    /// runs over the legacy tmux path, but every daemon-backed IDE feature is
    /// dead. Folded into a persistent, dismissible notification rather than
    /// only a log line, so the degraded session is visible on screen.
    pub daemon_unavailable_rx: Receiver<()>,
    /// Root-picker browse requests (`docs/spec-session-root-picker.md`, issue
    /// #769): every `ClientMessage::QueryDirEntries` the in-cockpit root
    /// picker issues. The tokio side forwards it onto the protocol verbatim,
    /// sharing the bridge the pre-cockpit root picker's own clone of this
    /// sender also feeds (`main.rs`'s single directory-browse transport). The
    /// `DirEntriesReply` reply does NOT come back on a matching receiver
    /// here — `main.rs`'s `Shell` owns that single reply loop and calls
    /// [`WorkspaceView::apply_dir_entries_reply`] directly, since only one of
    /// the pre-/in-cockpit pickers is ever showing at a time.
    pub dir_browse_tx: Sender<ClientMessage>,
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
    /// The outline panel (`docs/spec-editor-chrome.md`, #530): a virtualized
    /// read of the active editor tab's document-symbol cache, toggled into
    /// the left dock alongside `file_tree` by `toggle_outline`, which reads
    /// this field. The jump-to-location wiring subscribes to the local
    /// `outline_panel` value at construction time instead.
    outline_panel: Entity<OutlinePanel>,
    /// Whether the outline panel is currently added to the left dock's
    /// `TabPanel` (`ToggleOutline`, #530) — `add_panel`/`remove_panel` carry
    /// no query API, so this is the source of truth `toggle_outline` flips.
    /// Starts `false`: the outline panel is opt-in via the palette, not
    /// shown by default (the rail icon that would surface it is a phase-21
    /// follow-up).
    outline_open: bool,
    /// The results panel (`docs/spec-editor-chrome.md` §3, #529): the right-dock
    /// list of find-references / multi-target go-to-definition results. Toggled
    /// into the right dock on demand by [`WorkspaceView::show_results_panel`]
    /// when the editor emits [`EditorEvent::ShowResults`]. The jump-to-location
    /// and close wiring subscribes to the local `results_panel` value at
    /// construction time instead.
    results_panel: Entity<ResultsPanel>,
    /// Whether the results panel is currently added to the right dock — the
    /// source of truth its show/hide toggles flip (`add_panel`/`remove_panel`
    /// carry no query API). Starts `false`: the panel is opened only by a nav
    /// result, never by default.
    results_open: bool,
    /// Whether [`WorkspaceView::show_results_panel`] was the one to open the
    /// (collapsed-by-default) right dock — so closing the panel re-collapses it
    /// only when the panel is why it opened, leaving a user-opened source-control
    /// dock alone.
    results_opened_dock: bool,
    /// Language-server health for the composite status line's LSP segment
    /// (`docs/spec-status-line.md`), keyed by stable server name and folded
    /// from the daemon's `LspStatus` push (replayed behind Welcome). Read
    /// inline in [`WorkspaceView::render`].
    lsp: BTreeMap<String, LspServerState>,
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
    /// Jump-to-file quick-open (`docs/spec-explorer-search.md`, Phase 31,
    /// issue #681): owns its `ListState` entity for the workspace's lifetime,
    /// hosted beside `command_palette` above (`Ctrl+Shift+O` / `Cmd+Shift+O`).
    quick_open: QuickOpen,
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
    /// Root-picker browse requests (`docs/spec-session-root-picker.md`, issue
    /// #769): the tokio side forwards each onto the protocol verbatim,
    /// sharing `main.rs`'s single directory-browse bridge with the
    /// pre-cockpit picker.
    dir_browse_tx: Sender<ClientMessage>,
    /// The in-cockpit root picker's live state (issue #769), `None` when no
    /// "+ New session..." dialog is open. A fresh [`RootPicker`] entity is
    /// built on every open (mirroring `main.rs`'s `Shell` — never reused
    /// across opens), so `pending_browse` — the owner-side correlation guard
    /// [`root_picker::browse_reply_matches`] checks against every incoming
    /// `DirEntriesReply` — always starts clean.
    root_picker_session: Option<RootPickerSession>,
}

/// [`WorkspaceView::root_picker_session`]'s payload, mirroring `main.rs`'s
/// `RootPickerScreen`: the live picker entity, the outstanding-request guard,
/// and the subscription that keeps `picker`'s event stream alive for as long
/// as the dialog is open.
struct RootPickerSession {
    picker: Entity<RootPicker>,
    pending_browse: Option<String>,
    _subscription: Subscription,
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
            lsp_status_rx,
            diff_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            request_diff_tx,
            git_op_tx,
            file_op_tx,
            file_op_result_rx,
            daemon_unavailable_rx,
            dir_browse_tx,
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
        // The header/root-row `RevealActiveRequested` action (#604) re-triggers
        // the existing reveal path via the already-present
        // `reveal_open_file_in_tree` — no new protocol, no new coupling. The
        // row context menu's `RevealInTerminalRequested`
        // (`docs/spec-explorer-context-menu.md`) routes to the existing
        // `SessionView` this view already owns — a new public method enqueues
        // a structural `new-window -c <dir>` on the shipped tmux command
        // channel; no new protocol message. `RenameRequested`
        // (`docs/spec-explorer-file-ops.md`, #675) is the tree's inline-rename
        // commit: turned into a `ClientMessage::RenamePath` on `file_op_tx`.
        // `CreateRequested` / `DeleteRequested` (`docs/spec-explorer-file-ops.md`,
        // #676 — the context-menu write group) turn into
        // `ClientMessage::CreateFile` / `CreateDir` / `DeletePath` the same
        // way. The tree itself holds no protocol channel.
        {
            let file_op_tx = file_op_tx.clone();
            cx.subscribe_in(
                &file_tree,
                window,
                move |this, _tree, event: &FileTreeEvent, window, cx| match event {
                    FileTreeEvent::OpenFile { path } => {
                        this.editor.update(cx, |editor, cx| {
                            editor.begin_open(path.clone(), false, window, cx);
                        });
                        if let Err(e) = this.open_file_tx.try_send(path.clone()) {
                            debug!(error = %e, %path, "failed to enqueue open-file request");
                        }
                    }
                    FileTreeEvent::RevealActiveRequested => {
                        this.reveal_open_file_in_tree(cx);
                    }
                    FileTreeEvent::RevealInTerminalRequested { dir } => {
                        this.session_view
                            .update(cx, |session, _cx| session.open_terminal_at(dir));
                    }
                    FileTreeEvent::RenameRequested { from, to } => {
                        if let Err(e) = file_op_tx.try_send(ClientMessage::RenamePath {
                            from: from.clone(),
                            to: to.clone(),
                        }) {
                            debug!(error = %e, %from, %to, "failed to enqueue rename");
                        }
                    }
                    FileTreeEvent::CreateRequested { path, kind } => {
                        let op = match kind {
                            EntryKind::File => ClientMessage::CreateFile { path: path.clone() },
                            EntryKind::Dir => ClientMessage::CreateDir { path: path.clone() },
                        };
                        if let Err(e) = file_op_tx.try_send(op) {
                            debug!(error = %e, %path, "failed to enqueue create");
                        }
                    }
                    FileTreeEvent::DeleteRequested { path } => {
                        if let Err(e) =
                            file_op_tx.try_send(ClientMessage::DeletePath { path: path.clone() })
                        {
                            debug!(error = %e, %path, "failed to enqueue delete");
                        }
                    }
                },
            )
            .detach();
        }

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
                    // Content-only diff refresh (#488): pull the changed paths
                    // out of an `UpdateWorktree` before `msg` moves into the
                    // fold below, mirroring `git_status_update` above — this
                    // fires on every disk write, catching the content-only
                    // edits `UpdateGitStatus` never reports (status unchanged).
                    let worktree_content_update = match &msg {
                        DaemonMessage::UpdateWorktree { changed, .. } => Some(
                            changed
                                .iter()
                                .map(|entry| entry.path.clone())
                                .collect::<Vec<String>>(),
                        ),
                        _ => None,
                    };
                    // Cross-project write safety (#738,
                    // `docs/spec-per-session-project-root.md`, "Detach open
                    // buffers on re-root"): capture the model's root BEFORE
                    // folding this message, so a completed `WorktreeSnapshot`
                    // that lands on a DIFFERENT root than before (a session
                    // switch, not the initial connect or a same-root
                    // reconnect) is detected below. `apply_snapshot_chunk`
                    // only commits `root` on the FINAL chunk of a snapshot,
                    // so comparing before/after this fold — rather than
                    // matching on the message type — naturally fires exactly
                    // once per completed snapshot, multi-chunk-safe.
                    let previous_root = view.file_tree.read(cx).model().root().map(str::to_owned);
                    view.file_tree.update(cx, |tree, cx| {
                        apply_worktree_message(tree, msg);
                        cx.notify();
                    });
                    let new_root = view.file_tree.read(cx).model().root().map(str::to_owned);
                    if worktree_root_switched(previous_root.as_deref(), new_root.as_deref()) {
                        // The reactive layer just re-rooted to a different
                        // project: force-close every open tab so a buffer
                        // left open against the OLD root can never resolve a
                        // later save against the new one (the daemon detaches
                        // its own live-buffer feed symmetrically on the same
                        // re-root, `reroot_connection`/`detach_open_buffers`
                        // in `crates/daemon/src/lib.rs`).
                        view.editor.update(cx, |editor, cx| {
                            editor.close_all_tabs_for_project_switch(window, cx);
                        });
                    }
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
                    // Live diff refresh (#488): a content-only edit to the open
                    // diff's path (`M` stays `M`, no `UpdateGitStatus` above)
                    // still re-requests the diff here, on the `UpdateWorktree`
                    // every disk write produces.
                    if let Some(changed) = worktree_content_update {
                        view.diff_view.update(cx, |diff_view, cx| {
                            diff_view.apply_content_update(&changed, cx);
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
        // new base `mtime`, `SaveConflict` surfaces the conflict, and the typed
        // refusals `OpenError` / `SaveError` (`docs/spec-v1-hardening.md`) name
        // why a read or write was refused. Each routed method ignores a reply
        // for a path no longer open. Routed through this view's weak handle so
        // a load can be followed by re-pushing the freshly opened file's inline
        // diagnostics (#189) — the `begin_open` recreated the input with an
        // empty `DiagnosticSet`, so the open file's existing set must be
        // re-applied once its content has loaded.
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
                            editor.apply_save_conflict(path, disk_mtime, window, cx)
                        }
                        DaemonMessage::OpenError { path, reason } => {
                            editor.apply_open_error(path, reason, cx)
                        }
                        DaemonMessage::SaveError { path, reason } => {
                            editor.apply_save_error(path, reason, cx)
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
        // `apply_references_response` (#198); `DocumentSymbolResponse` routes to
        // `apply_document_symbol_response` (editor-chrome breadcrumb). Routed
        // through this view's weak handle so a closed window ends the loop
        // gracefully.
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
                        DaemonMessage::DocumentSymbolResponse { id, symbols } => {
                            editor.apply_document_symbol_response(id, symbols, cx);
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

        // Language-server health stream -> composite status line
        // (`docs/spec-status-line.md`): each `LspStatus` push replaces the
        // server's health by name (replayed behind Welcome so a reattach sees
        // current health), then a notify repaints the status bar. Routed
        // through this view's weak handle so a closed window ends the loop
        // gracefully.
        {
            cx.spawn(async move |this, cx| loop {
                let Ok(msg) = lsp_status_rx.recv_async().await else {
                    break;
                };
                let result = this.update(cx, |view, cx| {
                    let DaemonMessage::LspStatus { server, state } = msg else {
                        return;
                    };
                    view.lsp.insert(server, state);
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

        // File-op reply stream -> file tree, UX transitions only
        // (`docs/spec-explorer-file-ops.md`, #675): `FileOpResult` routes to
        // `FileTree::apply_file_op_result`, which never mutates
        // `WorktreeModel` — the tree structure change (if any) arrives
        // separately, through the worktree-structure bridge above, as the
        // ordinary push-only `UpdateWorktree`. Routed through this view's
        // weak handle so a closed window ends the loop gracefully, mirroring
        // the diff reply bridge above.
        {
            cx.spawn_in(window, async move |this, cx| loop {
                let Ok(msg) = file_op_result_rx.recv_async().await else {
                    break;
                };
                let result = this.update_in(cx, |view, window, cx| {
                    let DaemonMessage::FileOpResult { op, ok, error } = msg else {
                        return;
                    };
                    view.file_tree.update(cx, |tree, cx| {
                        tree.apply_file_op_result(op, ok, error, window, cx);
                    });
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        // Daemon-unavailable signal -> a persistent, dismissible notification
        // (#619): the tokio side selected the daemon terminal but no daemon
        // came up, so the legacy tmux path is carrying the session with every
        // daemon-backed feature dead. Pushed through the existing `Root`
        // notification layer (rendered below in `render`) rather than a new
        // primitive; a stable id keeps a reconnect's repeat signal from
        // stacking a second copy. `autohide(false)` plus the notification's own
        // close button is the "persistent, dismissible" the issue asks for.
        {
            cx.spawn_in(window, async move |this, cx| loop {
                let Ok(()) = daemon_unavailable_rx.recv_async().await else {
                    break;
                };
                let result = this.update_in(cx, |_view, window, cx| {
                    window.push_notification(
                        Notification::warning("Daemon unavailable - IDE features disabled")
                            .id::<DaemonUnavailableNotification>()
                            .autohide(false),
                        cx,
                    );
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
        let source_control =
            cx.new(|cx| SourceControlPanel::new(file_tree.clone(), git_op_tx.clone(), window, cx));
        // The diff header's `+ Stage hunk` button (#547) shares the same
        // git-op channel the source-control panel's stage/unstage/discard
        // actions use, and its Split|Unified toggle persists into the same
        // window-state file the workspace itself restores from.
        let diff_view =
            cx.new(|cx| DiffView::new(request_diff_tx, git_op_tx, window_state_path.clone(), cx));
        let problems_panel = cx.new(|cx| ProblemsPanel::new(file_tree.clone(), cx));
        // Reads (never mutates) the editor's active-tab document-symbol cache
        // (`docs/spec-editor-chrome.md`, #530) — the same cache the
        // breadcrumb resolves the enclosing symbol against (#527).
        let outline_panel = cx.new(|cx| OutlinePanel::new(editor.clone(), cx));
        // The right-dock results panel (`docs/spec-editor-chrome.md` §3, #529):
        // fed by the editor's nav responses, opened on demand.
        let results_panel = cx.new(ResultsPanel::new);

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

        // Jump-to-location (#530): selecting an outline row emits
        // `OpenLocation`, routed the same way the problems panel's is above.
        cx.subscribe_in(
            &outline_panel,
            window,
            |this, _panel, event: &OutlinePanelEvent, window, cx| {
                let OutlinePanelEvent::OpenLocation { path, range } = event;
                this.editor.update(cx, |editor, cx| {
                    editor.open_at_range(path.clone(), *range, window, cx);
                });
            },
        )
        .detach();

        // Results panel (#529): the editor emits `ShowResults` when a
        // find-references / multi-target definition response lands — feed the
        // panel and open it in the right dock — and `CloseResults` when Escape
        // closes it from the editor. `ActiveTabChanged` (#404) fires when
        // `open_or_switch` lands on an already-open tab (cross-file
        // go-to-definition or `go_back`) — the only switch path with no
        // `FileContent` reply to key the FileContent-load reveal off (below),
        // so it re-triggers the same `reveal_open_file_in_tree` the header's
        // `RevealActiveRequested` action uses. Mirrors the panel-toggle wiring
        // above.
        cx.subscribe_in(
            &editor,
            window,
            |this, _editor, event: &EditorEvent, window, cx| match event {
                EditorEvent::ShowResults {
                    kind,
                    symbol,
                    locations,
                } => {
                    this.results_panel.update(cx, |panel, cx| {
                        panel.set_results(*kind, symbol.clone(), locations.clone(), cx);
                    });
                    this.show_results_panel(window, cx);
                }
                EditorEvent::CloseResults => {
                    this.hide_results_panel(window, cx);
                }
                EditorEvent::ActiveTabChanged => {
                    this.reveal_open_file_in_tree(cx);
                }
            },
        )
        .detach();

        // Jump-to-location (#529): selecting a result row emits `OpenLocation`
        // with the full `NavLocation` (preserving the out-of-root read-only
        // carve-out, unlike the problems/outline panels' in-worktree jumps);
        // its × affordance emits `Close`, hiding the panel.
        cx.subscribe_in(
            &results_panel,
            window,
            |this, _panel, event: &ResultsPanelEvent, window, cx| match event {
                ResultsPanelEvent::OpenLocation { location } => {
                    this.editor.update(cx, |editor, cx| {
                        editor.jump_to_nav_location(location.clone(), window, cx);
                    });
                }
                ResultsPanelEvent::Close => {
                    this.hide_results_panel(window, cx);
                }
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

        // Terminal reflow on dock layout change (#596): toggling or dragging a
        // dock resizes the center terminal panel, but gpui-component notifies
        // only the dock being changed. The terminal is a center sibling — never
        // an ancestor of that dock — so GPUI never marks it dirty, and its
        // cached panel prepaint (the pane-area grid observer that pushes the
        // client resize onto `refresh-client -C`) is reused, leaving tmux at the
        // old size and the content clipped behind the panels. Observing each
        // dock and notifying the session forces it to re-render against its new
        // bounds and re-emit the resize; the observer's own grid dedup keeps a
        // no-op layout change from spamming the seam.
        let docks: Vec<Entity<Dock>> = {
            let dock_area = dock_area.read(cx);
            [
                dock_area.left_dock(),
                dock_area.right_dock(),
                dock_area.bottom_dock(),
            ]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
        };
        for dock in docks {
            cx.observe(&dock, |this, _dock, cx| {
                this.session_view.update(cx, |_session, cx| cx.notify());
            })
            .detach();
        }

        let command_palette = CommandPalette::new(window, cx);
        let quick_open = QuickOpen::new(file_tree.clone(), window, cx);
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
            |this, _session_view, event: &SessionViewEvent, window, cx| match event {
                SessionViewEvent::FontSizeChanged { .. } => {
                    this.arm_window_state_save(window, cx);
                }
                // The strip's "+ New session..." chip / the command
                // palette's "New Session..." entry (issue #769,
                // `docs/spec-session-root-picker.md`): open the root picker
                // as a modal over the workspace.
                SessionViewEvent::NewSessionRequested => {
                    this.open_root_picker(window, cx);
                }
            },
        )
        .detach();
        {
            let session_view = session_view.clone();
            let window_state_path = window_state_path.clone();
            let editor = editor.clone();
            window.on_window_should_close(cx, move |window, cx| {
                // Unsaved-changes guard (`docs/spec-v1-hardening.md`): a dirty
                // editor tab must not be silently lost on quit. If any tab has
                // unsaved edits, keep the window open (`false`) and raise the
                // aggregated confirm/discard dialog — the same `AlertDialog`
                // the per-tab close flow uses (`EditorView::confirm_close_tab`),
                // at workspace scope. Confirming discards every dirty tab and
                // closes the window; the window-state save still runs on that
                // eventual clean close, exactly as on a clean quit below.
                let dirty_count = editor.read(cx).dirty_tab_count();
                if dirty_count > 0 {
                    // Guard against stacking a second dialog when the OS fires
                    // `should_close` again while one is already up (mirrors
                    // `EditorView::open_conflict_dialog`).
                    if !window.has_active_dialog(cx) {
                        let session_view = session_view.clone();
                        let window_state_path = window_state_path.clone();
                        let message = unsaved_quit_message(dirty_count);
                        window.open_alert_dialog(cx, move |alert: AlertDialog, _, _| {
                            let session_view = session_view.clone();
                            let window_state_path = window_state_path.clone();
                            let message = message.clone();
                            alert
                                .title("Unsaved Changes")
                                .description(message)
                                .button_props(
                                    DialogButtonProps::default()
                                        .ok_text("Discard and quit")
                                        .cancel_text("Cancel")
                                        .show_cancel(true)
                                        .on_ok(move |_, window, cx| {
                                            if let Some(path) = &window_state_path {
                                                save_window_state(path, window, &session_view, cx);
                                            }
                                            window.remove_window();
                                            true
                                        }),
                                )
                        });
                    }
                    return false;
                }
                if let Some(path) = &window_state_path {
                    save_window_state(path, window, &session_view, cx);
                }
                true
            });
        }

        // Composite status line reactivity (`docs/spec-status-line.md`): the
        // window list, activity dots, and PREFIX segment read `session_view`'s
        // live state, and the Ln/Col segment reads the editor's cursor, so this
        // view must repaint when either notifies. `cx.observe` fires on every
        // notify of the observed entity — the same signal that already redraws
        // the terminal's own tab bar and the editor.
        cx.observe(&session_view, |_this, _session_view, cx| cx.notify())
            .detach();
        cx.observe(&editor, |_this, _editor, cx| cx.notify())
            .detach();

        // Minute clock (`docs/spec-status-line.md`): repaint once per minute so
        // the clock's `HH:MM` advances, aligned to the minute boundary (delay to
        // the next boundary, then every 60s) — at most one wakeup per minute, no
        // per-second poll. The value itself is read from `chrono::Local::now()`
        // in `render`; this loop only drives the repaint.
        cx.spawn(async move |this, cx| loop {
            smol::Timer::after(duration_to_next_minute()).await;
            if this.update(cx, |_view, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();

        Self {
            file_tree,
            editor,
            session_view,
            problems_panel,
            outline_panel,
            outline_open: false,
            results_panel,
            results_open: false,
            results_opened_dock: false,
            lsp: BTreeMap::new(),
            diff_view,
            open_file_tx,
            dock_area,
            command_palette,
            quick_open,
            window_state_path,
            window_state_save_generation: 0,
            settings_view,
            dir_browse_tx,
            root_picker_session: None,
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

    /// Toggle the outline panel shown/hidden in the left dock (#530): adds it
    /// as a tab alongside the explorer (opening the dock too, since
    /// `DockArea::add_panel` does not do that for an already-existing dock)
    /// or removes it, per `outline_open`'s current state — the panel-level
    /// counterpart to `toggle_explorer`'s whole-dock toggle.
    fn toggle_outline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let opening = !self.outline_open;
        let panel: Arc<dyn PanelView> = Arc::new(self.outline_panel.clone());
        self.dock_area.update(cx, |dock_area, cx| {
            if opening {
                dock_area.add_panel(panel, DockPlacement::Left, None, window, cx);
                if let Some(left_dock) = dock_area.left_dock().cloned() {
                    left_dock.update(cx, |dock, cx| {
                        dock.set_open(true, window, cx);
                    });
                }
            } else {
                dock_area.remove_panel(panel, DockPlacement::Left, window, cx);
            }
        });
        self.outline_open = opening;
    }

    /// Show the results panel in the right dock (`docs/spec-editor-chrome.md`
    /// §3, #529): add it as a tab (opening the dock, since `DockArea::add_panel`
    /// does not do that for an already-existing dock) and remember whether we
    /// were the one to open the dock, so [`WorkspaceView::hide_results_panel`]
    /// can re-collapse it only when the panel is why it opened. A no-op layout
    /// change when the panel is already shown (a fresh nav result just repaints
    /// the already-visible panel).
    fn show_results_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.results_open {
            return;
        }
        let was_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        let panel: Arc<dyn PanelView> = Arc::new(self.results_panel.clone());
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.add_panel(panel, DockPlacement::Right, None, window, cx);
            if let Some(right_dock) = dock_area.right_dock().cloned() {
                right_dock.update(cx, |dock, cx| {
                    dock.set_open(true, window, cx);
                });
            }
        });
        self.results_opened_dock = !was_open;
        self.results_open = true;
        // `add_panel` moves keyboard focus to the new tab; hand it back to the
        // editor buffer so a following `Escape` reaches the editor key context.
        self.editor.update(cx, |editor, cx| {
            editor.focus_active_input(window, cx);
        });
    }

    /// Hide the results panel: remove it from the right dock, re-collapse the
    /// dock if this panel is why it opened, clear the panel's data, and tell the
    /// editor so its `Escape` gate stays in sync (#529). A no-op when the panel
    /// is not shown.
    fn hide_results_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.results_open {
            return;
        }
        let collapse = self.results_opened_dock;
        let panel: Arc<dyn PanelView> = Arc::new(self.results_panel.clone());
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.remove_panel(panel, DockPlacement::Right, window, cx);
            if collapse {
                if let Some(right_dock) = dock_area.right_dock().cloned() {
                    right_dock.update(cx, |dock, cx| {
                        dock.set_open(false, window, cx);
                    });
                }
            }
        });
        self.results_open = false;
        self.results_opened_dock = false;
        self.results_panel.update(cx, |panel, cx| panel.clear(cx));
        self.editor
            .update(cx, |editor, _cx| editor.mark_results_closed());
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

    /// Open the jump-to-file quick-open (`docs/spec-explorer-search.md`,
    /// Phase 31, issue #681) as a `Root` dialog.
    fn open_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_open.open(window, cx);
    }

    /// Open the settings surface (issue #366) as a `Root` dialog.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_view.open(window, cx);
    }

    /// Open the in-cockpit root picker (issue #769,
    /// `docs/spec-session-root-picker.md`) as a modal `Root` dialog over the
    /// workspace — the session strip's "+ New session..." chip and the
    /// command palette's "New Session..." entry both route here via
    /// [`SessionViewEvent::NewSessionRequested`]. A fresh [`RootPicker`]
    /// entity is built on every open (never reused, mirroring `main.rs`'s
    /// `Shell`), so its correlation guard always starts clean; its start
    /// level seeds from the phase-9 recents-of-roots store. On `Picked`, the
    /// name is disambiguated against `session_view`'s live list before
    /// [`SessionView::create_session_at_root`] sends the create — the
    /// single create-with-root transport this and the pre-cockpit picker
    /// both use.
    fn open_root_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let recent_roots = self
            .window_state_path
            .as_deref()
            .map(|path| window_state::load(path).recent_roots)
            .unwrap_or_default();
        let start = root_picker::start_path(&recent_roots);
        let picker = cx.new(|cx| RootPicker::new(window, cx));

        let dir_browse_tx = self.dir_browse_tx.clone();
        let window_state_path = self.window_state_path.clone();
        let session_view = self.session_view.clone();
        let subscription = cx.subscribe_in(
            &picker,
            window,
            move |this, _picker, event: &RootPickerEvent, window, cx| match event {
                RootPickerEvent::Browse(path) => {
                    let _ = dir_browse_tx
                        .try_send(ClientMessage::QueryDirEntries { path: path.clone() });
                    if let Some(session) = this.root_picker_session.as_mut() {
                        session.pending_browse = Some(path.clone());
                    }
                }
                RootPickerEvent::Picked { root, name } => {
                    let existing: Vec<String> = session_view
                        .read(cx)
                        .sessions()
                        .iter()
                        .map(|session| session.name.clone())
                        .collect();
                    let session_name = root_picker::disambiguate_session_name(name, &existing);
                    if let Some(path) = &window_state_path {
                        if let Err(e) = window_state::record_recent_root(path, root) {
                            warn!(%e, "failed to record recent root");
                        }
                    }
                    session_view.update(cx, |session, cx| {
                        session.create_session_at_root(session_name, root.clone(), cx);
                    });
                    this.root_picker_session = None;
                    window.close_dialog(cx);
                }
            },
        );

        // Set the session BEFORE kicking off the first browse below: the
        // `Browse` handler above updates `this.root_picker_session`, so it
        // must already exist by the time that emit is observed.
        self.root_picker_session = Some(RootPickerSession {
            picker: picker.clone(),
            pending_browse: Some(start.clone()),
            _subscription: subscription,
        });
        picker.update(cx, |picker, cx| picker.browse(start, cx));

        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("New session")
                .w(px(ROOT_PICKER_DIALOG_WIDTH))
                .child(picker.clone())
        });
    }

    /// Route a `DirEntriesReply` to the in-cockpit root picker, if one is
    /// open (issue #769) — called directly by `main.rs`'s `Shell`, which owns
    /// the single reply-receiving loop shared with the pre-cockpit picker
    /// (only one of the two is ever showing at a time). Applies the same
    /// [`root_picker::browse_reply_matches`] correlation guard
    /// `Shell::apply_dir_entries_reply` applies to its own picker.
    pub fn apply_dir_entries_reply(
        &mut self,
        path: String,
        parent: Option<String>,
        entries: Vec<DirEntry>,
        error: Option<DirBrowseError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.root_picker_session.as_mut() else {
            return;
        };
        if !root_picker::browse_reply_matches(session.pending_browse.as_deref(), &path) {
            return;
        }
        session.pending_browse = None;
        let picker = session.picker.clone();
        picker.update(cx, |picker, cx| {
            picker.apply_dir_entries_reply(path, parent, entries, error, window, cx);
        });
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

/// The aggregated window-close confirm dialog's message for `count` dirty tabs
/// (`docs/spec-v1-hardening.md`). `count` is always `>= 1` at the call site
/// (the guard only opens the dialog when a tab is dirty); singular vs plural
/// keeps the copy natural.
fn unsaved_quit_message(count: usize) -> SharedString {
    if count == 1 {
        SharedString::from("1 file has unsaved changes. Discard it and quit?")
    } else {
        SharedString::from(format!(
            "{count} files have unsaved changes. Discard them and quit?"
        ))
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

/// The composite status line's client-local minute clock as `HH:MM`
/// (`docs/spec-status-line.md`). Read at render; `chrono::Local` resolves the
/// wall-clock hour/minute and [`status_bar::format_clock`] zero-pads them.
fn current_clock() -> String {
    use chrono::Timelike as _;
    let now = chrono::Local::now();
    status_bar::format_clock(now.hour(), now.minute())
}

/// Time from now until the next wall-clock minute boundary, so the clock timer
/// wakes once per minute aligned to `:00` seconds rather than drifting. Falls
/// back to a full minute if the current second reads at the boundary.
fn duration_to_next_minute() -> Duration {
    use chrono::Timelike as _;
    let secs = u64::from(chrono::Local::now().second());
    Duration::from_secs(60u64.saturating_sub(secs).max(1))
}

/// Whether folding a worktree message just completed a project switch
/// (#738, `docs/spec-per-session-project-root.md`, "Detach open buffers on
/// re-root"): `previous` is the file tree model's root immediately BEFORE
/// the fold, `new` is its root immediately AFTER. `true` only when there
/// WAS a previous root and the fold committed a DIFFERENT one — never on
/// the very first snapshot after connecting (`previous` is `None`, nothing
/// is open yet to close) and never mid-accumulation of a multi-chunk
/// snapshot or a same-root resend/reconnect (`WorktreeModel::
/// apply_snapshot_chunk` only commits `root` on the snapshot's FINAL chunk,
/// so `new` stays equal to `previous` until then).
fn worktree_root_switched(previous: Option<&str>, new: Option<&str>) -> bool {
    previous.is_some() && previous != new
}

/// Fold one worktree-family daemon message into the file tree's model. Only the
/// structure-path messages are routed here; any other variant is ignored (the
/// tokio side forwards only this family on `worktree_rx`). An `UpdateWorktree`
/// fold also drives the pending-reveal follow-up
/// (`docs/spec-explorer-file-ops.md`, #675): a successful create/rename arms
/// `FileTree::pending_reveal` with the new path, and this is where that path
/// actually turns into a select + reveal, once the push-only recompute has
/// added the row to `model` — never before, and never mutating the model a
/// second time to do it.
fn apply_worktree_message(tree: &mut FileTree, msg: DaemonMessage) {
    let model = tree.model_mut();
    let added_paths = match msg {
        DaemonMessage::WorktreeSnapshot {
            root,
            entries,
            final_chunk,
        } => {
            model.apply_snapshot_chunk(root, entries, final_chunk);
            None
        }
        DaemonMessage::UpdateWorktree {
            added,
            changed,
            removed,
        } => {
            let added_paths: Vec<String> = added.iter().map(|entry| entry.path.clone()).collect();
            model.apply_update(added, changed, removed);
            Some(added_paths)
        }
        DaemonMessage::UpdateGitStatus { changed, cleared } => {
            model.apply_git_update(changed, cleared);
            None
        }
        DaemonMessage::RepoState {
            branch,
            ahead_behind,
            lines_added,
            lines_removed,
        } => {
            model.apply_repo_state(branch, ahead_behind, lines_added, lines_removed);
            None
        }
        DaemonMessage::Diagnostics {
            path,
            server,
            items,
        } => {
            model.apply_diagnostics(path, server, items);
            None
        }
        _ => None,
    };
    if let Some(added_paths) = added_paths {
        tree.apply_pending_reveal(&added_paths);
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
        // The custom title bar (#511/#512, `docs/spec-cockpit-chrome.md`): the
        // connection group reads the live `SessionView` fields the terminal
        // crate's own statusbar shows, so the two never disagree, and hosts
        // the always-visible session strip itself (#683, relocated from the
        // interim statusbar anchor) once a live session names it. The
        // settings gear dispatches through the same `open_settings` path as
        // the `OpenSettings` action below.
        let (connection, session_strip) = {
            let (dot_color, ssh_label, has_session) = {
                let session = self.session_view.read(cx);
                let (_, dot_color) = session.connection_status().status_dot(cx);
                (
                    dot_color,
                    SharedString::from(session.ssh_label().to_string()),
                    !session.session_name().is_empty(),
                )
            };
            let session_strip = has_session.then(|| {
                self.session_view.update(cx, |session, cx| {
                    session.render_session_strip(cx).into_any_element()
                })
            });
            (
                title_bar::ConnectionGroup::connected(dot_color, ssh_label),
                session_strip,
            )
        };
        let settings_button = Button::new("title-bar-settings")
            .ghost()
            .xsmall()
            .icon(IconName::Settings)
            .on_click(cx.listener(|this, _event, window, cx| {
                this.open_settings(window, cx);
            }))
            .into_any_element();
        let title_bar = title_bar::render(connection, session_strip, Some(settings_button), cx);

        // The activity rail (#513, `docs/spec-cockpit-chrome.md`): active
        // state tracks live dock visibility and the badges read the same
        // `WorktreeModel` the status bar reads below — both live views over
        // one model, no separate rail-owned state.
        let rail = {
            let model = self.file_tree.read(cx).model();
            let dock_area = self.dock_area.read(cx);
            activity_rail::render(
                activity_rail::RailState {
                    explorer_open: dock_area.is_dock_open(DockPlacement::Left, cx),
                    source_control_open: dock_area.is_dock_open(DockPlacement::Right, cx),
                    problems_open: dock_area.is_dock_open(DockPlacement::Bottom, cx),
                    changed_count: model.git_statuses().len(),
                    worst_diagnostic: activity_rail::worst_severity(model.all_diagnostics()),
                },
                cx,
            )
        };

        // The dock shell fills the window below the custom title bar (the
        // native OS chrome is gone, #511); the `flex_col` mirrors
        // `examples/dock.rs` at the pinned gpui-component rev.
        //
        // The composite status line (`docs/spec-status-line.md`) is a plain
        // `flex_col` sibling below the dock — bottom chrome, not a dock `Panel`.
        // Its left segments read the terminal's live window/prefix state, its
        // right segments the file tree's `WorktreeModel` (branch/ahead-behind/
        // line totals/diagnostics), the folded `lsp` health, the editor cursor,
        // and a client-local minute clock — repainted by the folds' `cx.notify`
        // and the `session_view`/`editor` observes above.
        //
        // The shell command actions (issue #358) are handled here, at the
        // workspace root, rather than scoped to a key context: they are
        // global commands the command palette dispatches regardless of which
        // panel currently has focus.
        let windows = self.session_view.read(cx).status_windows(cx);
        let prefix_pending = self.session_view.read(cx).prefix_pending(cx);
        let cursor = self.editor.read(cx).cursor_position(cx);
        let clock = current_clock();
        let status_bar = {
            let model = self.file_tree.read(cx).model();
            let (lines_added, lines_removed) = model.line_totals();
            status_bar::render(
                status_bar::StatusLineModel {
                    windows: &windows,
                    prefix_pending,
                    repo_state_received: model.repo_state_received(),
                    branch: model.branch(),
                    ahead_behind: model.ahead_behind(),
                    lines_added,
                    lines_removed,
                    diagnostics: model.all_diagnostics(),
                    lsp: &self.lsp,
                    cursor,
                    clock: &clock,
                },
                &self.session_view,
                cx,
            )
            .into_any_element()
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
            .on_action(cx.listener(|this, _: &ToggleOutline, window, cx| {
                this.toggle_outline(window, cx);
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
                this.session_view.read(cx).open_session_switcher();
            }))
            .on_action(cx.listener(|this, _: &NewSession, _window, cx| {
                this.session_view.update(cx, |session, cx| {
                    session.open_new_session_prompt(cx);
                });
            }))
            .on_action(cx.listener(|this, _: &RefreshKeyTables, _window, cx| {
                this.session_view.read(cx).request_key_table_refresh();
            }))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                this.open_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenQuickOpen, window, cx| {
                this.open_quick_open(window, cx);
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
            .child(title_bar)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .size_full()
                    .child(rail)
                    .child(self.dock_area.clone()),
            )
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

    // --- unsaved-changes window-close guard message (spec-v1-hardening) -------

    #[test]
    fn test_unsaved_quit_message_is_singular_for_one_dirty_tab() {
        let message = unsaved_quit_message(1);
        assert!(message.contains("1 file has"), "singular copy: {message}");
        assert!(message.contains("Discard it"), "singular copy: {message}");
    }

    #[test]
    fn test_unsaved_quit_message_is_plural_and_counts_for_many_dirty_tabs() {
        let message = unsaved_quit_message(3);
        assert!(
            message.contains("3 files have"),
            "plural copy names the count: {message}"
        );
        assert!(message.contains("Discard them"), "plural copy: {message}");
    }

    // --- project-switch buffer detach (#738) ----------------------------------
    // Headless: `worktree_root_switched` is plain data logic, no GPUI test
    // harness needed.

    #[test]
    fn test_worktree_root_switched_true_when_the_committed_root_differs() {
        assert!(worktree_root_switched(Some("/proj/a"), Some("/proj/b")));
    }

    #[test]
    fn test_worktree_root_switched_false_when_the_root_is_unchanged() {
        assert!(!worktree_root_switched(Some("/proj/a"), Some("/proj/a")));
    }

    #[test]
    fn test_worktree_root_switched_false_on_the_first_ever_snapshot() {
        // `previous` is `None` before the daemon's first `WorktreeSnapshot`
        // ever completes — nothing was open yet, so this must not be treated
        // as a switch.
        assert!(!worktree_root_switched(None, Some("/proj/a")));
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
        let (_lsp_status_tx, lsp_status_rx) = flume::unbounded();
        let (_diff_reply_tx, diff_rx) = flume::unbounded();
        let (open_file_tx, _open_file_rx) = flume::unbounded();
        let (save_file_tx, _save_file_rx) = flume::unbounded();
        let (buffer_change_tx, _buffer_change_rx) = flume::unbounded();
        let (nav_tx, _nav_request_rx) = flume::unbounded();
        let (request_diff_tx, _request_diff_rx) = flume::unbounded();
        let (git_op_tx, _git_op_rx) = flume::unbounded();
        let (file_op_tx, _file_op_rx) = flume::unbounded();
        let (_file_op_result_tx, file_op_result_rx) = flume::unbounded();
        let (_daemon_unavailable_tx, daemon_unavailable_rx) = flume::unbounded();
        let (dir_browse_tx, _dir_browse_rx) = flume::unbounded();
        WorkspaceChannels {
            worktree_rx,
            buffer_rx,
            nav_rx,
            lsp_status_rx,
            diff_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            request_diff_tx,
            git_op_tx,
            file_op_tx,
            file_op_result_rx,
            daemon_unavailable_rx,
            dir_browse_tx,
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
    /// stays zoomable to fill the shell and restore. `FileTree`, `EditorView`,
    /// `TerminalPanel`, and `ProblemsPanel` all override `Panel::zoomable` to
    /// `Some(PanelControl::Toolbar)` (`docs/spec-dogfooding-fixes.md`, #716)
    /// so the zoom control renders as a direct header button instead of the
    /// "..." overflow menu — this locks the "stays zoomable" invariant
    /// against an accidental future override that drops it to `None`.
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

            for (name, control) in [
                ("the explorer", file_tree.read(cx).zoomable(cx)),
                ("the editor", editor.read(cx).zoomable(cx)),
                ("the problems panel", problems_panel.read(cx).zoomable(cx)),
            ] {
                let control = control.unwrap_or_else(|| panic!("{name} stays zoomable"));
                assert!(
                    control.toolbar_visible(),
                    "{name}'s zoom renders as a direct header button, not the \"...\" menu"
                );
                assert!(
                    !control.menu_visible(),
                    "{name}'s zoom is pulled out of the \"...\" overflow menu"
                );
            }

            let terminal_panel = cx.new(|_| TerminalPanel::new(session_view));
            let terminal_control = terminal_panel
                .read(cx)
                .zoomable(cx)
                .expect("the terminal stays zoomable");
            assert!(
                terminal_control.toolbar_visible(),
                "the terminal's zoom renders as a direct header button, not the \"...\" menu"
            );
            assert!(
                !terminal_control.menu_visible(),
                "the terminal's zoom is pulled out of the \"...\" overflow menu"
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

    /// Shell command action (`docs/spec-editor-chrome.md`, #530):
    /// `ToggleOutline` adds the outline panel as a second tab in the left
    /// dock — opening it if collapsed, unlike a plain `DockArea::add_panel`,
    /// which does not — and removes it again on a second toggle, leaving the
    /// explorer tab untouched throughout.
    #[gpui::test]
    fn test_toggle_outline_adds_and_removes_the_outline_tab_and_opens_the_dock(
        cx: &mut TestAppContext,
    ) {
        // `DockItem::Tabs { items, .. }` is a construction-time snapshot: only
        // `DockItem::add_panel` (`&mut self`) keeps it in sync, while
        // `DockItem::remove_panel` (`&self`) mutates just the live
        // `TabPanel.panels` it delegates to — so `items` is reliable right
        // after an add but stale after a remove. `left_tab_names` (add) and
        // `left_active_tab_name` (remove, via the live `TabPanel`) are each
        // used only where they are accurate.
        fn left_tab_names(dock_area: &Entity<DockArea>, cx: &App) -> Vec<&'static str> {
            let left = dock_area.read(cx).left_dock().expect("left dock exists");
            match left.read(cx).panel() {
                DockItem::Tabs { items, .. } => {
                    items.iter().map(|item| item.panel_name(cx)).collect()
                }
                other => panic!("expected the left dock to hold tabs, got {other:?}"),
            }
        }

        fn left_active_tab_name(dock_area: &Entity<DockArea>, cx: &App) -> Option<&'static str> {
            let left = dock_area.read(cx).left_dock().expect("left dock exists");
            match left.read(cx).panel() {
                DockItem::Tabs { view, .. } => view
                    .read(cx)
                    .active_panel(cx)
                    .map(|panel| panel.panel_name(cx)),
                other => panic!("expected the left dock to hold tabs, got {other:?}"),
            }
        }

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

                assert_eq!(
                    left_tab_names(&dock_area, cx),
                    vec![crate::file_tree::FILE_TREE_PANEL_NAME],
                    "the outline panel is not shown by default"
                );

                // Collapse the left dock first, to prove ToggleOutline opens
                // it rather than merely adding an invisible tab.
                workspace.update(cx, |view, cx| {
                    view.toggle_explorer(window, cx);
                });
                assert!(!dock_area.read(cx).is_dock_open(DockPlacement::Left, cx));

                workspace.update(cx, |view, cx| {
                    view.toggle_outline(window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "ToggleOutline opens the left dock"
                );
                assert_eq!(
                    left_tab_names(&dock_area, cx),
                    vec![
                        crate::file_tree::FILE_TREE_PANEL_NAME,
                        crate::outline_panel::OUTLINE_PANEL_NAME,
                    ],
                    "ToggleOutline adds the outline panel beside the explorer"
                );
                assert_eq!(
                    left_active_tab_name(&dock_area, cx),
                    Some(crate::outline_panel::OUTLINE_PANEL_NAME),
                    "adding the outline panel activates its tab"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_outline(window, cx);
                });
                assert_eq!(
                    left_active_tab_name(&dock_area, cx),
                    Some(crate::file_tree::FILE_TREE_PANEL_NAME),
                    "a second ToggleOutline removes the outline panel, leaving the explorer active"
                );
            })
            .unwrap();
    }

    /// Results panel (`docs/spec-editor-chrome.md` §3, #529): showing it adds
    /// its tab to the right dock and opens the (collapsed-by-default) dock;
    /// hiding it removes the tab and re-collapses the dock, since the panel is
    /// why the dock opened. The live inner `TabPanel` (not the stale
    /// construction-time `DockItem::Tabs { items }` snapshot) is read to see the
    /// added tab, per the outline test's note.
    #[gpui::test]
    fn test_show_and_hide_results_panel_toggles_the_right_dock(cx: &mut TestAppContext) {
        fn right_top_active(dock_area: &Entity<DockArea>, cx: &App) -> Option<&'static str> {
            let right = dock_area.read(cx).right_dock().expect("right dock exists");
            match right.read(cx).panel() {
                DockItem::Split { items, .. } => match items.first() {
                    Some(DockItem::Tabs { view, .. }) => {
                        view.read(cx).active_panel(cx).map(|p| p.panel_name(cx))
                    }
                    other => panic!("expected the first split item to be tabs, got {other:?}"),
                },
                other => panic!("expected the right dock to hold a split, got {other:?}"),
            }
        }

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
                    "right dock starts collapsed"
                );

                workspace.update(cx, |view, cx| view.show_results_panel(window, cx));
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "showing the results panel opens the right dock"
                );
                assert!(workspace.read(cx).results_open);
                assert_eq!(
                    right_top_active(&dock_area, cx),
                    Some(crate::results_panel::RESULTS_PANEL_NAME),
                    "the results tab becomes active in the right dock"
                );

                workspace.update(cx, |view, cx| view.hide_results_panel(window, cx));
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "hiding re-collapses the right dock it opened"
                );
                assert!(!workspace.read(cx).results_open);
                assert_ne!(
                    right_top_active(&dock_area, cx),
                    Some(crate::results_panel::RESULTS_PANEL_NAME),
                    "the results tab is removed on hide"
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

    /// Jump-to-file quick-open (`docs/spec-explorer-search.md`, Phase 31,
    /// issue #681): opening sets an active `Root` dialog, mirroring the
    /// command palette and settings surface above, and closing it clears
    /// that state without disturbing the workspace's own focus delegation to
    /// the terminal.
    #[gpui::test]
    fn test_open_quick_open_opens_a_dialog_and_close_clears_it(cx: &mut TestAppContext) {
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
                view.open_quick_open(window, cx);
            });
            assert!(
                window.has_active_dialog(cx),
                "OpenQuickOpen opens a Root dialog"
            );
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening quick-open does not move the workspace's terminal focus delegation"
            );

            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "closing the dialog clears the active-dialog state"
            );
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "dismissing quick-open leaves the terminal focus delegation untouched"
            );
        })
        .unwrap();
    }

    /// The in-cockpit root picker (issue #769,
    /// `docs/spec-session-root-picker.md`): opening sets an active `Root`
    /// dialog and an in-flight browse of the seeded start path, mirroring
    /// the command palette / settings / quick-open dialogs above; closing it
    /// leaves the workspace's focus delegation untouched.
    #[gpui::test]
    fn test_open_root_picker_opens_a_dialog_with_a_pending_seed_browse(cx: &mut TestAppContext) {
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
            assert!(!window.has_active_dialog(cx));

            workspace.update(cx, |view, cx| {
                view.open_root_picker(window, cx);
            });

            assert!(
                window.has_active_dialog(cx),
                "opening the root picker opens a Root dialog"
            );
            assert_eq!(
                workspace
                    .read(cx)
                    .root_picker_session
                    .as_ref()
                    .unwrap()
                    .pending_browse,
                Some(String::new()),
                "no recent roots yet: the seed browse targets \"\" ($HOME)"
            );
            assert_eq!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening the root picker does not move the terminal focus delegation"
            );

            window.close_dialog(cx);
            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
    }

    /// The session strip's "+ New session..." chip and the command
    /// palette's "New Session..." entry both call
    /// `SessionView::open_new_session_prompt`, which only emits
    /// `SessionViewEvent::NewSessionRequested` — `WorkspaceView`'s
    /// subscription (wired in `new`) reacts by opening the root picker
    /// (issue #769, `docs/spec-session-root-picker.md`).
    #[gpui::test]
    fn test_new_session_requested_event_opens_the_root_picker(cx: &mut TestAppContext) {
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

        cx.update_window(window.into(), |_, _window, cx| {
            assert!(workspace.read(cx).root_picker_session.is_none());
            session_view.update(cx, |session, cx| {
                session.open_new_session_prompt(cx);
            });
        })
        .unwrap();

        cx.update_window(window.into(), |_, _window, cx| {
            assert!(
                workspace.read(cx).root_picker_session.is_some(),
                "NewSessionRequested opens the root picker"
            );
        })
        .unwrap();
    }

    /// [`WorkspaceView::apply_dir_entries_reply`]'s correlation guard (issue
    /// #769, `docs/spec-session-root-picker.md`): a reply whose path does
    /// not match the outstanding browse is dropped without clearing
    /// `pending_browse`; a matching reply clears it. The seed browse
    /// (`""`) matches any resolved path (`root_picker::browse_reply_matches`),
    /// so this drives the picker one level past it first — into a browse
    /// whose requested path IS the reply's resolved path — to exercise a
    /// genuine mismatch.
    #[gpui::test]
    fn test_apply_dir_entries_reply_drops_a_non_matching_path(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                workspace =
                    Some(cx.new(|cx| WorkspaceView::new(view, test_channels(), None, window, cx)));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        // Every step below runs as its own `update_window` call — subscriber
        // callbacks (the `RootPickerEvent::Browse` handler that updates
        // `pending_browse`) are only guaranteed observed once effects flush
        // at the end of a top-level update, so triggering an emit and
        // asserting its effect inside the SAME call (as the earlier dialog
        // tests above do for state that needs no cross-entity round trip)
        // would race the flush.
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |view, cx| {
                view.open_root_picker(window, cx);
            });
        })
        .unwrap();

        // Resolve the seed level first, then descend into a child —
        // `select_entry` is private to `root_picker`, so this reaches
        // through the picker's own public `browse` instead, mirroring what a
        // row click would do.
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |view, cx| {
                view.apply_dir_entries_reply(
                    "/home/dev".to_string(),
                    None,
                    Vec::new(),
                    None,
                    window,
                    cx,
                );
            });
        })
        .unwrap();
        cx.update_window(window.into(), |_, _window, cx| {
            let picker = workspace
                .read(cx)
                .root_picker_session
                .as_ref()
                .unwrap()
                .picker
                .clone();
            picker.update(cx, |picker, cx| {
                picker.browse("/home/dev/project".to_string(), cx);
            });
        })
        .unwrap();
        cx.update_window(window.into(), |_, _window, cx| {
            assert_eq!(
                workspace
                    .read(cx)
                    .root_picker_session
                    .as_ref()
                    .unwrap()
                    .pending_browse,
                Some("/home/dev/project".to_string()),
                "the child browse is now the outstanding request"
            );
        })
        .unwrap();

        // A stale/mismatched reply must not clobber it.
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |view, cx| {
                view.apply_dir_entries_reply(
                    "/some/other/path".to_string(),
                    None,
                    Vec::new(),
                    None,
                    window,
                    cx,
                );
            });
        })
        .unwrap();
        cx.update_window(window.into(), |_, _window, cx| {
            assert_eq!(
                workspace
                    .read(cx)
                    .root_picker_session
                    .as_ref()
                    .unwrap()
                    .pending_browse,
                Some("/home/dev/project".to_string()),
                "a non-matching reply is dropped, the outstanding request survives"
            );
        })
        .unwrap();

        // The matching reply clears it.
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |view, cx| {
                view.apply_dir_entries_reply(
                    "/home/dev/project".to_string(),
                    None,
                    Vec::new(),
                    None,
                    window,
                    cx,
                );
            });
        })
        .unwrap();
        cx.update_window(window.into(), |_, _window, cx| {
            assert_eq!(
                workspace
                    .read(cx)
                    .root_picker_session
                    .as_ref()
                    .unwrap()
                    .pending_browse,
                None,
                "the matching reply clears the outstanding request"
            );
        })
        .unwrap();
    }

    /// Daemon-unavailable banner (#619, `docs/spec-v1-hardening.md`): the
    /// tokio side's signal must surface as exactly one `Root` notification,
    /// a repeat signal (SSH-level reconnect resending it) must replace rather
    /// than stack a second copy, and the notification must not autohide —
    /// it stays until the user dismisses it.
    #[gpui::test]
    fn test_daemon_unavailable_signal_pushes_one_persistent_notification(cx: &mut TestAppContext) {
        let (daemon_unavailable_tx, daemon_unavailable_rx) = flume::unbounded();
        let channels = WorkspaceChannels {
            daemon_unavailable_rx,
            ..test_channels()
        };
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace =
                    Some(cx.new(|cx| WorkspaceView::new(session_view, channels, None, window, cx)));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let _workspace = workspace.expect("workspace constructed inside the window callback");

        let _ = daemon_unavailable_tx.send(());
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "the daemon-unavailable signal must surface exactly one notification"
            );
        })
        .unwrap();

        // Reconnect churn resending the same signal must replace, not stack,
        // the notification (`DaemonUnavailableNotification`'s stable id).
        let _ = daemon_unavailable_tx.send(());
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "a repeat signal must replace, not stack, the notification"
            );
        })
        .unwrap();

        // Persistent (`autohide(false)`): survives past the 5s autohide window
        // `NotificationList::push` arms for every other notification.
        cx.background_executor.advance_clock(Duration::from_secs(6));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "the banner must not autohide -- it stays until the user dismisses it"
            );
        })
        .unwrap();
    }

    /// Explorer reveal on tab switch (#404): switching to an already-open
    /// tab (`EditorView::open_or_switch`'s already-open branch) has no
    /// `FileContent` reply to key the existing reveal off, so the editor's
    /// `ActiveTabChanged` event must drive `reveal_open_file_in_tree` itself
    /// — mirroring the header's `RevealActiveRequested` wiring above.
    #[gpui::test]
    fn test_switching_to_an_already_open_tab_reveals_it_in_the_tree(cx: &mut TestAppContext) {
        use rift_protocol::WorktreeEntry;
        use std::time::SystemTime;

        fn file(path: &str) -> WorktreeEntry {
            WorktreeEntry {
                path: path.to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH,
            }
        }

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
        let (editor, file_tree) = cx.update(|cx| {
            let view = workspace.read(cx);
            (view.editor.clone(), view.file_tree.clone())
        });

        // Seed the tree with both paths so `reveal` can find them, and open
        // two fresh tabs (no daemon reply simulated, so no reveal happens
        // yet): a.rs then b.rs, landing active on b.rs. Subscriber-driven
        // effects (the `ActiveTabChanged` reveal included) only flush once
        // this outermost `window.update` returns, so assertions on them sit
        // after it, not inside its closure (`docs/patterns.md`).
        window
            .update(cx, |_, window, cx| {
                file_tree.update(cx, |tree, cx| {
                    apply_worktree_message(
                        tree,
                        DaemonMessage::WorktreeSnapshot {
                            root: "/proj".into(),
                            entries: vec![file("a.rs"), file("b.rs")],
                            final_chunk: true,
                        },
                    );
                    cx.notify();
                });
                editor.update(cx, |editor, cx| {
                    editor.begin_open("a.rs".into(), false, window, cx);
                    editor.begin_open("b.rs".into(), false, window, cx);
                });
            })
            .unwrap();
        cx.update(|cx| {
            assert_eq!(
                file_tree.read(cx).selected(),
                None,
                "a fresh open alone does not reveal (that path rides the FileContent load)"
            );
        });

        // Switching back to the already-open a.rs must reveal it.
        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.begin_open("a.rs".into(), false, window, cx);
                });
            })
            .unwrap();
        cx.update(|cx| {
            assert_eq!(
                file_tree.read(cx).selected(),
                Some("a.rs"),
                "switching to the already-open a.rs tab reveals it in the tree"
            );
        });
    }
}
