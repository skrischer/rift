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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use flume::{Receiver, Sender};
use gpui::{
    div, point, px, size, App, AppContext as _, Axis, Bounds, ClickEvent, Context, Entity,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Render, SharedString, Styled as _, Subscription, Window, WindowBounds,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{AlertDialog, DialogButtonProps};
use gpui_component::dock::{Dock, DockArea, DockItem, DockPlacement, PanelView};
use gpui_component::{ActiveTheme as _, IconName, Root, Sizable as _, WindowExt as _};
use rift_protocol::{
    ClientMessage, CloneError, DaemonMessage, DirBrowseError, DirEntry, EntryKind, LspServerState,
};
use rift_terminal::{SessionView, SessionViewEvent};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::activity_rail;
use crate::command_palette::{CommandPalette, OpenCommandPalette};
use crate::diff_view::DiffView;
use crate::editor::{EditorEvent, EditorView};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::outline_panel::{OutlinePanel, OutlinePanelEvent};
use crate::problems_panel::{ProblemsPanel, ProblemsPanelEvent};
use crate::quick_open::{OpenQuickOpen, QuickOpen};
use crate::recents::{self, RecentTarget};
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

/// Toggle the Terminal area hidden/shown — the rail's icon for `Area::
/// Terminal` (issue #821, "Terminal: fully symmetric"). Mirrors
/// `ToggleExplorer`/`ToggleProblems`/`ToggleSourceControl` exactly: the
/// Terminal is a normal peer in the rift-owned visible set, never demoted or
/// re-arranged by this toggle — only its render-level visibility changes.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ToggleTerminal;

/// Move focus to the terminal, so keystrokes reach the active tmux pane.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct FocusTerminal;

/// Solo whichever area currently holds focus (issue #820): the
/// command-palette / keyboard path onto [`WorkspaceView::toggle_solo_area`].
/// Superseded from forwarding `gpui_component`'s own `dock::ToggleZoom` —
/// that built-in path flips `TabPanel.zoomed` + `DockArea.zoom_view`
/// independently of the rift-owned visible set, the exact divergence
/// `docs/spec-workspace-visibility-rail.md`'s "Single source of truth for
/// solo" constraint rules out. The per-area header button (`Solo*` below) is
/// the primary, always-correct trigger, since it names its own area
/// explicitly rather than inferring it from focus.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ZoomActivePanel;

/// Solo the Explorer+Editor area, dispatched by `FileTree`/`EditorView`'s
/// `toolbar_buttons()` header button (issue #820) — see [`ZoomActivePanel`].
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SoloExplorerEditor;

/// Solo the Terminal area, dispatched by `TerminalPanel`'s `toolbar_buttons()`
/// header button (issue #820) — see [`ZoomActivePanel`].
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SoloTerminal;

/// Solo the Diagnostics area, dispatched by `ProblemsPanel`'s
/// `toolbar_buttons()` header button (issue #820) — see [`ZoomActivePanel`].
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SoloDiagnostics;

/// Solo the Git area, dispatched by `SourceControlPanel`'s `toolbar_buttons()`
/// header button (issue #820) — see [`ZoomActivePanel`].
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SoloGit;

/// Builds the per-area solo header button (issue #820): `FileTree`,
/// `EditorView`, `TerminalPanel`, `ProblemsPanel`, and `SourceControlPanel`
/// each call this from their `Panel::toolbar_buttons()` with a closure
/// dispatching their own `Solo*` action above, replacing gpui-component's
/// native zoom button (disabled via each panel's `Panel::zoomable() ->
/// None`) so the header control is unambiguously a rift-owned solo trigger
/// rather than a second surface reaching into `TabPanel.zoomed` /
/// `DockArea.zoom_view` (`docs/spec-workspace-visibility-rail.md`, "Single
/// source of truth for solo"). Presentational only, mirroring
/// `activity_rail::rail_button`: no live "currently soloed" indicator on the
/// button itself — the activity rail (already solo-aware through
/// `Visibility::is_visible`) is the authoritative visual state, so this is
/// purely the action trigger.
pub(crate) fn solo_button(
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new("solo-area")
        .icon(IconName::Maximize)
        .xsmall()
        .ghost()
        .tab_stop(false)
        .tooltip("Solo (show only this area)")
        .on_click(on_click)
}

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
    /// `HostMetrics` pushes to fold into the composite status line's MEM/CPU
    /// segment (`docs/spec-host-telemetry.md`).
    pub host_metrics_rx: Receiver<DaemonMessage>,
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

/// The four fixed workspace areas the activity rail carries one icon each for
/// (`docs/spec-workspace-visibility-rail.md`), replacing gpui-component's own
/// per-dock open/close as the source of truth for what renders.
/// Explorer+Editor are one area: the left-dock file tree and the center
/// editor half toggle together. The Terminal is a fully symmetric peer
/// (issue #821, "Terminal: fully symmetric"): hiding it removes it from the
/// center `h_split` entirely (the Editor expands to fill the freed half, or
/// the center goes empty if both are hidden), and it never re-arranges or
/// takes the Explorer+Editor's place — the rail only ever governs visibility
/// and solo, never layout order. `Diagnostics`/`Git` are the existing
/// bottom/right docks.
///
/// `Serialize`/`Deserialize` (issue #822, `window_state.rs`) use
/// `rename_all = "snake_case"` tags (`"explorer_editor"`, `"terminal"`, ...)
/// so the persisted JSON stays readable; `window_state.rs` deserializes each
/// entry independently and drops one that fails to match a known variant
/// (a future/older schema's area) instead of failing the whole array — see
/// `window_state::deserialize_tolerant_areas`.
// `pub`, not `pub(crate)`: `window_state::WindowState`'s `visible_areas`/
// `solo_area` fields are `pub` (mirroring every other `WindowState` field,
// e.g. `DiffViewMode`), and a public field cannot expose a less-visible
// type (`private_interfaces`, denied workspace-wide).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    ExplorerEditor,
    Terminal,
    Diagnostics,
    Git,
}

impl Area {
    /// Every area, in the rail's left-to-right order — also
    /// `window_state::default_visible_areas`'s source of truth for
    /// `WindowState::default`'s all-visible seed (issue #822).
    pub(crate) const ALL: [Area; 4] = [
        Area::ExplorerEditor,
        Area::Terminal,
        Area::Diagnostics,
        Area::Git,
    ];
}

/// The rift-owned visible-area set + solo target
/// (`docs/spec-workspace-visibility-rail.md`), authoritative over dock
/// rendering: [`WorkspaceView`] reads this — not gpui-component's own
/// `Dock::is_open` — for the rail's active-icon state and which panels get
/// built. A pure state machine (no GPUI dependency), unit-testable directly;
/// [`WorkspaceView`]'s `apply_*_visibility` methods translate a change here
/// into the actual dock tree (`toggle_area`).
///
/// Solo (routing the zoom control through `solo`, reconciling
/// gpui-component's two zoom states) is issue #820: the per-area header
/// button (`Solo*` actions, dispatched from each panel's `toolbar_buttons`)
/// drives [`Visibility::toggle_solo`]; the rail's own [`Visibility::toggle`]
/// clears solo when clicked while soloed, rather than blindly flipping a
/// membership bit solo made irrelevant. Persisting this across restart is
/// issue #822.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Visibility {
    visible: BTreeSet<Area>,
    solo: Option<Area>,
}

impl Visibility {
    /// Build from a persisted visible-area list + solo target
    /// (`WindowState::visible_areas`/`solo_area`, issue #822):
    /// `window_state`'s tolerant deserializer has already dropped any
    /// unrecognized `Area` entry before this runs, so this only rebuilds the
    /// set from what is left — an empty `visible_areas` (a corrupted-but-
    /// parseable store) starts with nothing visible rather than silently
    /// substituting `Area::ALL`'s all-visible default, since that degenerate
    /// case is indistinguishable from a deliberately-cleared set at this
    /// layer. `WorkspaceView::new` reaches the all-visible/no-solo default
    /// through this same constructor: `window_state::load` already degrades
    /// an absent/unreadable state file to `WindowState::default`'s
    /// `visible_areas: Area::ALL.to_vec(), solo_area: None`.
    fn from_persisted(visible_areas: &[Area], solo_area: Option<Area>) -> Self {
        Self {
            visible: visible_areas.iter().copied().collect(),
            solo: solo_area,
        }
    }

    /// The current visible-area list + solo target in the persisted store's
    /// shape (`Vec`/`Option`, not `BTreeSet`) — the write half of
    /// [`Visibility::from_persisted`], read by [`WorkspaceView::
    /// persist_visibility`] after every [`WorkspaceView::toggle_area`] /
    /// [`WorkspaceView::toggle_solo_area`] mutation.
    fn to_persisted(&self) -> (Vec<Area>, Option<Area>) {
        (self.visible.iter().copied().collect(), self.solo)
    }

    /// Whether `area` renders right now: the solo target alone when solo is
    /// set (soloing any area hides every other area, including the Terminal —
    /// the spec's "Terminal: fully symmetric" decision), otherwise plain
    /// membership in the visible set.
    fn is_visible(&self, area: Area) -> bool {
        match self.solo {
            Some(solo) => solo == area,
            None => self.visible.contains(&area),
        }
    }

    /// Flip `area`'s membership in the visible set — the rail's click
    /// handler. While solo is active, every area but the soloed one reads as
    /// hidden (`is_visible`), regardless of its own membership bit; a rail
    /// click in that state must not blindly XOR that bit (it could leave the
    /// clicked area hidden after solo clears, if it happened to already be a
    /// member). Instead it exits solo and re-adds (inserts, never removes)
    /// the clicked area, restoring the pre-solo set with that area
    /// guaranteed visible — "re-toggling any area from the rail exits solo
    /// by re-adding that area" (`docs/spec-workspace-visibility-rail.md`,
    /// issue #820). Outside solo this is the plain #819 toggle.
    fn toggle(&mut self, area: Area) {
        if self.solo.take().is_some() {
            self.visible.insert(area);
            return;
        }
        if !self.visible.remove(&area) {
            self.visible.insert(area);
        }
    }

    /// Toggle `area` as the solo target — the per-area header button's
    /// solo/zoom trigger (issue #820). Soloing the already-soloed area exits
    /// solo (mirroring a zoom-out); soloing any other area switches the
    /// target directly, without an intermediate exit. Never touches
    /// `visible` membership: solo is a pure rendering override over it (see
    /// `is_visible`), so exiting solo — this way or via `toggle` — always
    /// restores exactly the set from before solo engaged.
    fn toggle_solo(&mut self, area: Area) {
        self.solo = if self.solo == Some(area) {
            None
        } else {
            Some(area)
        };
    }
}

/// The composed app root.
pub struct WorkspaceView {
    file_tree: Entity<FileTree>,
    editor: Entity<EditorView>,
    session_view: Entity<SessionView>,
    /// The terminal's dock panel wrapper (`docs/spec-ide-shell.md`, #324):
    /// kept as its own field (mirroring `file_tree`/`editor`) so
    /// [`Self::apply_center_visibility`] can rebuild the center `h_split`
    /// around the same live entity on every Explorer+Editor or Terminal
    /// toggle (issue #821), instead of only being reachable through the
    /// (rebuilt) `dock_area` tree. Never dropped or recreated across a
    /// hide/show — this is what keeps the wrapped `SessionView`'s tmux
    /// control-mode subscription alive with no reconnect while the Terminal
    /// is unrendered.
    terminal_panel: Entity<TerminalPanel>,
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
    /// The host's latest resource sample for the composite status line's
    /// MEM/CPU segment (`docs/spec-host-telemetry.md`), folded from the
    /// daemon's `HostMetrics` push (replayed behind Welcome). `None` until
    /// the first sample arrives, which hides the segment entirely (mirroring
    /// the LSP dot before a server is known). Read inline in
    /// [`WorkspaceView::render`].
    host_metrics: Option<status_bar::HostMetrics>,
    /// The diff view (`docs/spec-source-control.md`, #338): renders the
    /// `FileDiff` streamed for the source-control panel's selection. Kept as
    /// its own field for the same reason as `problems_panel` above; the
    /// open-diff subscription below reaches it through this field, and
    /// [`WorkspaceView::zoom_active_panel`] reads its focus state (issue
    /// #820).
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
    /// The rift-owned area-visibility set + solo target
    /// (`docs/spec-workspace-visibility-rail.md`), authoritative over dock
    /// rendering: the rail's active-icon state and `toggle_area`'s dock-tree
    /// reconciliation both read/mutate this rather than `dock_area`'s own
    /// per-dock `open` bool. Seeded from the loaded `WindowState`'s
    /// `visible_areas`/`solo_area` (issue #822, `window_state.rs`) — an
    /// absent or unreadable state file degrades to `WindowState::default`'s
    /// all-visible/no-solo. Every mutation in `toggle_area` is persisted
    /// back through the same store.
    visibility: Visibility,
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
    /// The recents file + current connection identity (issue #873,
    /// `docs/spec-host-scoped-root-recents.md`), for the in-cockpit "+ New
    /// session..." root picker's host-scoped seed/record — the same pair
    /// `main.rs`'s `Shell` threads through `RootPickerLaunch.recents`. `None`
    /// when either half is unavailable (no recents-file path resolved, or no
    /// connection identity given — every non-test `WorkspaceView::new` call
    /// site supplies both): the picker then seeds `""` and a pick simply
    /// records nothing, exactly like a `main.rs` launch with no recents path.
    recents: Option<(PathBuf, RecentTarget)>,
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
    /// The workspace's own, always-rendered focus anchor
    /// (`docs/spec-visibility-rail-focus.md`, issue #847) — see the
    /// `Focusable` impl below for why this exists.
    focus_handle: FocusHandle,
}

/// [`WorkspaceView::root_picker_session`]'s payload, mirroring `main.rs`'s
/// `RootPickerScreen`: the live picker entity, the outstanding-request guard,
/// and the subscription that keeps `picker`'s event stream alive for as long
/// as the dialog is open.
struct RootPickerSession {
    picker: Entity<RootPicker>,
    pending_browse: Option<String>,
    /// The `<parent>/<name>` last sent on `ClientMessage::CloneRepo` (issue
    /// #829, `docs/spec-clone-repo.md`), the clone-channel counterpart of
    /// `pending_browse` — checked against an incoming `CloneResult`'s echoed
    /// `path` via [`root_picker::browse_reply_matches`] (issue #839) before
    /// routing it into `picker`.
    pending_clone: Option<String>,
    _subscription: Subscription,
}

impl WorkspaceView {
    /// Build the workspace around an already-created [`SessionView`] entity (the
    /// terminal, created in `main.rs` so it keeps owning the SSH/daemon session
    /// thread). Creates the explorer and editor, mounts all three, and starts the
    /// daemon-stream bridges.
    ///
    /// `recents_path`/`recent_target` (issue #873, `docs/spec-host-scoped-
    /// root-recents.md`) give the in-cockpit "+ New session..." root picker
    /// the same host-scoped recents store `main.rs`'s pre-cockpit picker
    /// uses; both are `None` in the existing test call sites, which seeds
    /// `""` and makes a pick's root-record a no-op, exactly like a `main.rs`
    /// launch with no recents path.
    pub fn new(
        session_view: Entity<SessionView>,
        channels: WorkspaceChannels,
        window_state_path: Option<PathBuf>,
        recents_path: Option<PathBuf>,
        recent_target: Option<RecentTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let WorkspaceChannels {
            worktree_rx,
            buffer_rx,
            nav_rx,
            lsp_status_rx,
            host_metrics_rx,
            diff_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            request_diff_tx,
            git_op_tx,
            file_op_tx,
            file_op_result_rx,
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
                    let snapshot_completed = view.file_tree.update(cx, |tree, cx| {
                        let completed = apply_worktree_message(tree, msg);
                        cx.notify();
                        completed
                    });
                    let new_root = view.file_tree.read(cx).model().root().map(str::to_owned);
                    let switched =
                        worktree_root_switched(previous_root.as_deref(), new_root.as_deref());
                    if switched {
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
                    // Default the explorer to collapsed on the snapshot that
                    // just completed (#795, `docs/spec-dogfooding-fixes.md`)
                    // — the very first one ever, or the first one for a
                    // newly switched-to project. `switched` implies
                    // `snapshot_completed` (both derive from the same
                    // FINAL-chunk root commit above), so resetting the
                    // per-project seed here — BEFORE the seed call in the
                    // same update — always leaves a fresh guard for it to
                    // fire against; without the reset, the switched-to
                    // project would inherit the OLD project's `collapsed`
                    // set (same-named directories, e.g. both projects having
                    // `src`, reading as collapsed by accident) and the
                    // already-set guard would no-op the reseed. A no-op on
                    // any later same-project snapshot: a directory the user
                    // has since expanded stays expanded.
                    if snapshot_completed {
                        view.file_tree.update(cx, |tree, cx| {
                            if switched {
                                tree.reset_collapse_seed();
                            }
                            tree.seed_collapsed_on_first_snapshot();
                            cx.notify();
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

        // Host resource stream -> composite status line
        // (`docs/spec-host-telemetry.md`): each daemon-global `HostMetrics`
        // push replaces the latest sample wholesale (replayed behind Welcome
        // so a reattach sees current state), then a notify repaints the
        // status bar. `None` before the first sample, which hides the
        // segment (mirroring the LSP fold above). Routed through this view's
        // weak handle so a closed window ends the loop gracefully.
        {
            cx.spawn(async move |this, cx| loop {
                let Ok(msg) = host_metrics_rx.recv_async().await else {
                    break;
                };
                let result = this.update(cx, |view, cx| {
                    let DaemonMessage::HostMetrics {
                        cpu,
                        mem_total,
                        mem_available,
                        ..
                    } = msg
                    else {
                        return;
                    };
                    view.host_metrics = Some(status_bar::HostMetrics {
                        cpu,
                        mem_total,
                        mem_available,
                    });
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

        // Persisted workspace visibility (`docs/spec-workspace-visibility-
        // rail.md`, issue #822): loaded up front so the dock construction
        // below seeds each dock's open state and the center split directly
        // from it, replacing the former hardcoded "left open, right/bottom
        // collapsed" construction-time seeding. `window_state::load` already
        // degrades a missing/corrupt/pre-#822 state file (or no
        // `window_state_path` at all) to `WindowState::default`'s
        // all-visible/no-solo.
        let persisted_state = window_state_path
            .as_deref()
            .map(window_state::load)
            .unwrap_or_default();
        let visibility =
            Visibility::from_persisted(&persisted_state.visible_areas, persisted_state.solo_area);
        let explorer_editor_visible = visibility.is_visible(Area::ExplorerEditor);
        let terminal_visible = visibility.is_visible(Area::Terminal);
        let diagnostics_visible = visibility.is_visible(Area::Diagnostics);
        let git_visible = visibility.is_visible(Area::Git);

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
        // The Explorer+Editor and Terminal areas (issue #822 seeding, issue
        // #821 making the Terminal a fully symmetric peer too): the center
        // starts as the editor|terminal split only when both are visible,
        // either side alone when just one is, or an empty tab strip when the
        // loaded state left both hidden — mirroring
        // `Self::apply_center_visibility`'s own branches exactly.
        let center_item = match (explorer_editor_visible, terminal_visible) {
            (true, true) => DockItem::h_split(
                vec![
                    DockItem::tab(editor.clone(), &weak_dock_area, window, cx),
                    DockItem::tab(terminal_panel.clone(), &weak_dock_area, window, cx),
                ],
                &weak_dock_area,
                window,
                cx,
            ),
            (true, false) => DockItem::tab(editor.clone(), &weak_dock_area, window, cx),
            (false, true) => DockItem::tab(terminal_panel.clone(), &weak_dock_area, window, cx),
            (false, false) => DockItem::tabs(vec![], &weak_dock_area, window, cx),
        };
        // Both real docks (not placeholder views) — a single-tab `TabPanel`,
        // open/collapsed per the loaded visibility below rather than always
        // collapsed. The right dock is a vertical split (#338): the
        // changed-file list stays compact on top, the diff view takes the
        // remaining height below it — both signal panels visible together,
        // matching the review flow (select a file, read its diff, without
        // switching tabs).
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
            dock.set_left_dock(
                left_item,
                Some(px(LEFT_DOCK_WIDTH)),
                explorer_editor_visible,
                window,
                cx,
            );
            dock.set_right_dock(
                right_item,
                Some(px(RIGHT_DOCK_WIDTH)),
                git_visible,
                window,
                cx,
            );
            dock.set_bottom_dock(
                bottom_item,
                Some(px(BOTTOM_DOCK_HEIGHT)),
                diagnostics_visible,
                window,
                cx,
            );
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
        // no-op layout change from spamming the seam. Issue #821 extends this
        // same re-assertion to the Terminal's own visible-set/solo
        // transitions, which never touch these three docks directly — see
        // `Self::apply_center_visibility`'s own explicit `session_view`
        // notify at the end of its body.
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
            terminal_panel,
            problems_panel,
            outline_panel,
            outline_open: false,
            results_panel,
            results_open: false,
            results_opened_dock: false,
            lsp: BTreeMap::new(),
            host_metrics: None,
            diff_view,
            open_file_tx,
            dock_area,
            visibility,
            command_palette,
            quick_open,
            window_state_path,
            recents: recents_path.zip(recent_target),
            window_state_save_generation: 0,
            settings_view,
            dir_browse_tx,
            root_picker_session: None,
            focus_handle: cx.focus_handle(),
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

    /// Toggle `area`'s membership in the rift-owned visible set, reconcile
    /// the dock tree to match, and best-effort persist the new set (issue
    /// #822) — the rail's `on_action` handlers below call this per [`Area`],
    /// replacing the old direct `dock_area.toggle_dock` forwarding.
    /// `Visibility::toggle` exits solo when called while soloed (issue
    /// #820), which changes every area's effective visibility at once, not
    /// just `area`'s — so that case reconciles all of them via
    /// [`Self::reconcile_visibility`] rather than only the clicked one.
    fn toggle_area(&mut self, area: Area, window: &mut Window, cx: &mut Context<Self>) {
        let was_soloed = self.visibility.solo.is_some();
        self.visibility.toggle(area);
        if was_soloed {
            self.reconcile_visibility(window, cx);
        } else {
            // Captured before `apply_area_visibility` unrenders `area` (gpui
            // does not clear focus on unmount, so the window's focus handle
            // still names whatever held it going in) — re-homed below only
            // if that is `area` itself and it just became hidden (issue #847).
            let focused_area = self.focused_area(window, cx);
            self.apply_area_visibility(area, self.visibility.is_visible(area), window, cx);
            self.rehome_focus_if_hidden(focused_area, window, cx);
        }
        self.persist_visibility();
    }

    /// Toggle `area` as the solo target and best-effort persist the new solo
    /// target (issue #822) — the per-area header button's solo/zoom trigger
    /// (issue #820, dispatched as a `Solo*` action from each panel's
    /// `toolbar_buttons()`) and [`Self::zoom_active_panel`]'s focus-based
    /// command-palette path. Entering or exiting solo changes every area's
    /// effective visibility at once, so this always reconciles all of them
    /// rather than only the target.
    fn toggle_solo_area(&mut self, area: Area, window: &mut Window, cx: &mut Context<Self>) {
        self.visibility.toggle_solo(area);
        self.reconcile_visibility(window, cx);
        self.persist_visibility();
    }

    /// Reconcile every area's dock rendering with its current
    /// [`Visibility::is_visible`] — unlike a plain rail toggle (which only
    /// ever changes one area's own effective visibility and can call just
    /// its matching `apply_*_visibility`), a solo transition changes what
    /// every area's `is_visible` returns at once. Calls each of the three
    /// underlying `apply_*_visibility` functions directly (not through
    /// [`Self::apply_area_visibility`]'s per-`Area` dispatch) since
    /// `ExplorerEditor` and `Terminal` both map to the same
    /// [`Self::apply_center_visibility`] (issue #821) — looping `Area::ALL`
    /// through the dispatcher would rebuild the center twice per
    /// reconciliation. Callers persist once themselves afterward.
    fn reconcile_visibility(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Captured before the `apply_*_visibility` calls below unrender
        // whichever area just lost visibility — re-homed afterward only if
        // that area held focus and is now hidden (issue #847).
        let focused_area = self.focused_area(window, cx);
        self.apply_center_visibility(window, cx);
        self.apply_diagnostics_visibility(
            self.visibility.is_visible(Area::Diagnostics),
            window,
            cx,
        );
        self.apply_git_visibility(self.visibility.is_visible(Area::Git), window, cx);
        self.rehome_focus_if_hidden(focused_area, window, cx);
    }

    /// Dispatch to the one `apply_*_visibility` matching `area` — the plain
    /// rail-click path (`Self::toggle_area`), which only ever changes one
    /// area at a time. `ExplorerEditor` and `Terminal` (issue #821) both
    /// route to [`Self::apply_center_visibility`], which reads both areas'
    /// current visibility itself rather than trusting the single `visible`
    /// passed in for whichever one of the pair triggered this — the center's
    /// shape depends on both.
    fn apply_area_visibility(
        &self,
        area: Area,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match area {
            Area::ExplorerEditor | Area::Terminal => self.apply_center_visibility(window, cx),
            Area::Diagnostics => self.apply_diagnostics_visibility(visible, window, cx),
            Area::Git => self.apply_git_visibility(visible, window, cx),
        }
    }

    /// Best-effort persist the current visibility set + solo target into the
    /// window-state store (issue #822), mirroring `DiffView::set_view_mode`'s
    /// "the live change already applied regardless, only log on failure"
    /// contract. A missing `window_state_path` (no platform state directory
    /// resolved at startup) silently no-ops, like every other save site.
    fn persist_visibility(&self) {
        let Some(path) = self.window_state_path.as_deref() else {
            return;
        };
        let (visible_areas, solo_area) = self.visibility.to_persisted();
        if let Err(e) = window_state::save_visibility(path, visible_areas, solo_area) {
            warn!(%e, "failed to persist workspace visibility");
        }
    }

    /// Reconcile the dock tree's center region with the Explorer+Editor
    /// *and* Terminal areas' visibility together (issue #821 makes the
    /// Terminal a fully symmetric peer, no longer an always-rendered floor —
    /// see the spec's "Terminal: fully symmetric" decision): the
    /// editor|terminal `h_split` when both are visible, either side alone —
    /// filling the freed half — when exactly one is, or an empty tab strip
    /// (a zero-panel `TabPanel`, mirroring `apply_diagnostics_visibility`'s
    /// "hidden = zero tabs, not merely collapsed" contract) when both are
    /// hidden, e.g. while a non-Terminal area is soloed. Also open/closes the
    /// left dock with the Explorer+Editor flag (closed, a Left `Dock` skips
    /// its whole subtree in gpui-component's own `Dock::render`, so this
    /// alone makes the explorer "not rendered"). The rail never re-arranges
    /// or demotes the terminal — it stays the prominent center peer
    /// side-by-side with the Editor whenever both show, never merged into a
    /// shared tab strip.
    ///
    /// Rebuilds `center` from the same live `editor`/`terminal_panel`
    /// entities (never dropping or recreating them) so their reactive state
    /// and daemon-stream/tmux bindings survive a hide/show — only the
    /// surrounding `TabPanel`/`StackPanel` chrome is rebuilt.
    ///
    /// Explicitly notifies `session_view` afterward — issue #821 extending
    /// the #596 dock-toggle reflow observer (below, in `Self::new`) to
    /// visible-set/solo transitions. That observer only watches the
    /// left/right/bottom `Dock` entities, which a pure Terminal/
    /// Explorer+Editor visibility change never touches; without this, a
    /// re-shown Terminal's render-coupled tmux grid resize
    /// (`resize_client_to_area`, `rift-terminal`'s `grid_observer` prepaint)
    /// might not re-fire, leaving tmux at the stale pre-hide grid.
    fn apply_center_visibility(&self, window: &mut Window, cx: &mut Context<Self>) {
        let explorer_editor_visible = self.visibility.is_visible(Area::ExplorerEditor);
        let terminal_visible = self.visibility.is_visible(Area::Terminal);
        let weak_dock_area = self.dock_area.downgrade();
        let center = match (explorer_editor_visible, terminal_visible) {
            (true, true) => DockItem::h_split(
                vec![
                    DockItem::tab(self.editor.clone(), &weak_dock_area, window, cx),
                    DockItem::tab(self.terminal_panel.clone(), &weak_dock_area, window, cx),
                ],
                &weak_dock_area,
                window,
                cx,
            ),
            (true, false) => DockItem::tab(self.editor.clone(), &weak_dock_area, window, cx),
            (false, true) => {
                DockItem::tab(self.terminal_panel.clone(), &weak_dock_area, window, cx)
            }
            (false, false) => DockItem::tabs(vec![], &weak_dock_area, window, cx),
        };
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.set_center(center, window, cx);
            if let Some(left_dock) = dock_area.left_dock().cloned() {
                left_dock.update(cx, |dock, cx| {
                    dock.set_open(explorer_editor_visible, window, cx)
                });
            }
        });
        self.session_view.update(cx, |_session, cx| cx.notify());
    }

    /// Reconcile the dock tree with the Diagnostics (bottom/problems) area's
    /// visibility: add/remove the problems panel's tab — so a hidden panel's
    /// `render` is never invoked (`TabPanel::render_active_panel` falls back
    /// to `Empty` with zero tabs), unlike merely collapsing the dock, which
    /// gpui-component keeps a slim title strip always rendered for the
    /// bottom placement — and open/close the dock. The actually-rendered set
    /// (`TabPanel::panels`) is idempotent (no-op on an already-present/
    /// already-absent entity id), so the add/remove itself is safe
    /// regardless of the panel's current attachment; `reset_bottom_dock_tabs_
    /// items` below additionally works around a separate gpui-component
    /// bookkeeping leak on top of that.
    fn apply_diagnostics_visibility(
        &self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel: Arc<dyn PanelView> = Arc::new(self.problems_panel.clone());
        self.dock_area.update(cx, |dock_area, cx| {
            if visible {
                dock_area.add_panel(panel.clone(), DockPlacement::Bottom, None, window, cx);
            } else {
                dock_area.remove_panel(panel.clone(), DockPlacement::Bottom, window, cx);
            }
            if let Some(bottom_dock) = dock_area.bottom_dock().cloned() {
                bottom_dock.update(cx, |dock, cx| {
                    dock.set_open(visible, window, cx);
                    Self::reset_bottom_dock_tabs_items(dock, visible, &panel, window, cx);
                });
            }
        });
    }

    /// Works around a gpui-component bookkeeping leak (review note carried
    /// from #819/PR #826 into issue #820): `DockItem::add_panel`'s `Tabs`
    /// branch (`gpui-component` `dock/mod.rs`) unconditionally pushes onto
    /// `items: Vec<Arc<dyn PanelView>>` on every show, and `remove_panel`'s
    /// `Tabs` branch never trims `items` on hide. The actually-rendered set
    /// (`TabPanel::panels`, deduped/pruned internally by entity id) stays
    /// correct — this shadow list is read only by `DockArea::dump`, which
    /// rift never calls — but left alone it grows one stale `Arc` per
    /// show/hide cycle forever. Rewrites `items` to exactly what the bottom
    /// dock holds today (the problems panel alone, or nothing — the only
    /// panel ever placed there); `Dock::set_panel` is a plain field
    /// assignment with no re-subscription, so this is safe to call after
    /// every visibility change.
    fn reset_bottom_dock_tabs_items(
        dock: &mut Dock,
        visible: bool,
        panel: &Arc<dyn PanelView>,
        window: &mut Window,
        cx: &mut Context<Dock>,
    ) {
        let DockItem::Tabs {
            size,
            active_ix,
            view,
            ..
        } = dock.panel().clone()
        else {
            return;
        };
        let items = if visible { vec![panel.clone()] } else { vec![] };
        dock.set_panel(
            DockItem::Tabs {
                size,
                items,
                active_ix,
                view,
            },
            window,
            cx,
        );
    }

    /// Reconcile the dock tree with the Git (source-control + diff) area's
    /// visibility: open/close the right dock only — unlike Diagnostics, never
    /// add_panel/remove_panel the right dock's two tabs directly. The right
    /// dock's panel is a vertical `Split` of two separate `Tabs` (source
    /// control, diff view, #338), and `DockItem::add_panel`'s `Split` branch
    /// always re-inserts into the *first* `Tabs` it finds regardless of which
    /// one a panel came from — a remove/re-add round trip would merge both
    /// tabs into one and lose the split. Closing the (non-bottom) right dock
    /// already skips its whole subtree in `Dock::render`, reaching the same
    /// "not rendered" outcome without that hazard.
    fn apply_git_visibility(&self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_area.update(cx, |dock_area, cx| {
            if let Some(right_dock) = dock_area.right_dock().cloned() {
                right_dock.update(cx, |dock, cx| dock.set_open(visible, window, cx));
            }
        });
    }

    /// Toggle the outline panel shown/hidden in the left dock (#530): adds it
    /// as a tab alongside the explorer (opening the dock too, since
    /// `DockArea::add_panel` does not do that for an already-existing dock)
    /// or removes it, per `outline_open`'s current state — independent of the
    /// Explorer+Editor area toggle above (the outline panel is not one of the
    /// rail's four areas).
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

    /// Move focus to the terminal (issue #358), so keystrokes reach the tmux
    /// pane exactly as they do when the terminal is clicked directly. `pub`
    /// (not `pub(crate)`) so `main.rs::enter_workspace` — a separate binary
    /// crate depending on this one — can call it directly as the explicit
    /// startup-focus call `WorkspaceView::focus_handle` no longer provides by
    /// delegation (`docs/spec-visibility-rail-focus.md`, issue #847).
    pub fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_view.focus_handle(cx).focus(window, cx);
    }

    /// Which [`Area`] currently holds keyboard focus, if any — shared by
    /// [`Self::zoom_active_panel`] (the focused-panel solo command) and the
    /// hide/solo focus re-home helpers below
    /// (`docs/spec-visibility-rail-focus.md`, issue #847), so the two
    /// focus-detection paths can never diverge. Source Control's own tab (as
    /// opposed to the diff view) has no dedicated field to check here — it
    /// stays reachable via its own header button, unrelated to this
    /// detection.
    fn focused_area(&self, window: &Window, cx: &App) -> Option<Area> {
        // `Focusable::focus_handle` disambiguated: `FileTree`/`EditorView`/
        // `ProblemsPanel`/`DiffView` all also implement gpui-component's
        // `Panel`, which gives `Entity<T>` a second, differently-scoped
        // `focus_handle` (via its blanket `PanelView` impl) — plain method
        // syntax is ambiguous between the two.
        if Focusable::focus_handle(&self.file_tree, cx).contains_focused(window, cx)
            || Focusable::focus_handle(&self.editor, cx).contains_focused(window, cx)
        {
            Some(Area::ExplorerEditor)
        } else if self
            .session_view
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            Some(Area::Terminal)
        } else if Focusable::focus_handle(&self.problems_panel, cx).contains_focused(window, cx) {
            Some(Area::Diagnostics)
        } else if Focusable::focus_handle(&self.diff_view, cx).contains_focused(window, cx) {
            Some(Area::Git)
        } else {
            None
        }
    }

    /// Solo whichever area currently holds focus (issue #820), superseding
    /// the old direct `gpui_component::dock::ToggleZoom` forward (issue
    /// #358): that built-in path flips `TabPanel.zoomed` + `DockArea.
    /// zoom_view` independently of the rift-owned visible set — exactly the
    /// divergence `docs/spec-workspace-visibility-rail.md` rules out. A
    /// best-effort command-palette/keyboard entry point, not the primary
    /// trigger (the per-area header button names its area explicitly); a
    /// no-op if focus is not inside a recognized surface.
    fn zoom_active_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(area) = self.focused_area(window, cx) {
            self.toggle_solo_area(area, window, cx);
        }
    }

    /// Re-home focus after a visibility/solo transition, when
    /// `previously_focused` names the area that held focus *before* the
    /// transition and that area is now hidden (`docs/spec-visibility-rail-
    /// focus.md`, issue #847) — a no-op when nothing was focused there, or
    /// when it is still visible, so an unaffected focus is never disturbed.
    /// gpui does not clear focus on unmount, so leaving this unhandled would
    /// strand the window's focus on a now-unrendered panel, dropping every
    /// subsequent `window.dispatch_action` (the Phase-390 QA freeze). Moves
    /// focus to [`Self::preferred_focus_area`]'s pick, or to the workspace's
    /// own root anchor when nothing is visible.
    fn rehome_focus_if_hidden(
        &mut self,
        previously_focused: Option<Area>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = previously_focused else {
            return;
        };
        if self.visibility.is_visible(area) {
            return;
        }
        match Self::preferred_focus_area(&self.visibility) {
            Some(Area::Terminal) => self.focus_terminal(window, cx),
            Some(Area::ExplorerEditor) => self.editor.update(cx, |editor, cx| {
                editor.focus_active_input(window, cx);
            }),
            Some(Area::Diagnostics) => {
                Focusable::focus_handle(&self.problems_panel, cx).focus(window, cx);
            }
            Some(Area::Git) => {
                Focusable::focus_handle(&self.diff_view, cx).focus(window, cx);
            }
            None => self.focus_handle.focus(window, cx),
        }
    }

    /// Pick the area focus should re-home to, preferring **Terminal ->
    /// Explorer+Editor -> Diagnostics -> Git** (`docs/vision.md`: the
    /// terminal is rift's primary surface, restore focus there first when
    /// visible) — pure state-machine logic over [`Visibility`], no GPUI
    /// dependency, directly unit-testable. `None` means no area is visible
    /// (the degenerate all-hidden state); callers fall back to the
    /// workspace's own root focus anchor.
    fn preferred_focus_area(visibility: &Visibility) -> Option<Area> {
        const PREFERENCE: [Area; 4] = [
            Area::Terminal,
            Area::ExplorerEditor,
            Area::Diagnostics,
            Area::Git,
        ];
        PREFERENCE
            .into_iter()
            .find(|&area| visibility.is_visible(area))
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
    /// level seeds from the current connection target's own recorded roots
    /// (`self.recents`, issue #873, `docs/spec-host-scoped-root-recents.md`),
    /// never a different host's. On `Picked`, the name is disambiguated
    /// against `session_view`'s live list before
    /// [`SessionView::create_session_at_root`] sends the create — the
    /// single create-with-root transport this and the pre-cockpit picker
    /// both use.
    fn open_root_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let recent_roots = self
            .recents
            .as_ref()
            .map(|(path, target)| recents::target_recent_roots(path, target))
            .unwrap_or_default();
        let start = root_picker::start_path(&recent_roots);
        let picker = cx.new(|cx| RootPicker::new(window, cx));

        let dir_browse_tx = self.dir_browse_tx.clone();
        let recents = self.recents.clone();
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
                RootPickerEvent::Clone { url, parent, name } => {
                    let target = root_picker::join_child(parent, name);
                    let _ = dir_browse_tx.try_send(ClientMessage::CloneRepo {
                        url: url.clone(),
                        parent: parent.clone(),
                        name: name.clone(),
                    });
                    if let Some(session) = this.root_picker_session.as_mut() {
                        session.pending_clone = Some(target);
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
                    if let Some((path, target)) = &recents {
                        if let Err(e) = recents::merge_recent_root(path, target, root) {
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
            pending_clone: None,
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

    /// Route a `CloneResult` to the in-cockpit root picker, if one is open
    /// (issue #829/#839, `docs/spec-clone-repo.md`) — called directly by
    /// `main.rs`'s `Shell`, mirroring [`Self::apply_dir_entries_reply`]'s
    /// dispatch shape, now with the **same** [`root_picker::browse_reply_matches`]
    /// tolerant check `pending_browse` uses (issue #839's fix): the daemon
    /// echoes the RESOLVED `<parent>/<name>` (`~` expanded), which never
    /// exact-matches a `~`-prefixed `pending_clone`, and echoes the resolved
    /// path (never empty) on an early-rejected clone too, so this clears
    /// `pending_clone` and surfaces the error instead of a stuck spinner.
    pub fn apply_clone_result(
        &mut self,
        path: String,
        error: Option<CloneError>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.root_picker_session.as_mut() else {
            return;
        };
        if !root_picker::browse_reply_matches(session.pending_clone.as_deref(), &path) {
            return;
        }
        session.pending_clone = None;
        let picker = session.picker.clone();
        picker.update(cx, |picker, cx| {
            picker.apply_clone_result(path, error, cx);
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
///
/// Returns whether this fold just completed a worktree snapshot (the final
/// chunk of a `WorktreeSnapshot`) — the caller uses this to drive the
/// default-collapsed seed (#795, `docs/spec-dogfooding-fixes.md`) AFTER it
/// has had a chance to reset that seed on a project re-root, so the reset
/// and the seed for the same switch-completing snapshot land in the right
/// order (never both inside this one call, which would seed against a
/// not-yet-reset guard).
fn apply_worktree_message(tree: &mut FileTree, msg: DaemonMessage) -> bool {
    let model = tree.model_mut();
    let mut snapshot_completed = false;
    let added_paths = match msg {
        DaemonMessage::WorktreeSnapshot {
            root,
            entries,
            final_chunk,
        } => {
            snapshot_completed = model.apply_snapshot_chunk(root, entries, final_chunk);
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
    snapshot_completed
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        // The workspace's own always-rendered anchor (`docs/spec-visibility-
        // rail-focus.md`, issue #847) — no longer delegated to the terminal.
        // Delegating meant a hidden (unrendered) focused terminal stranded the
        // window's focus off the dispatch tree entirely, since the root `div`
        // registering the `on_action` handlers below was itself non-focusable;
        // `window.dispatch_action` then silently dropped every rail/keyboard
        // toggle. Startup still hands focus to the terminal explicitly (#358,
        // `main.rs::enter_workspace`'s `focus_terminal` call), and hide/solo
        // re-homes focus to a still-visible surface (`Self::rehome_focus_if_hidden`)
        // rather than relying on this delegation.
        self.focus_handle.clone()
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

        // The activity rail (#513, `docs/spec-cockpit-chrome.md`; rewired to
        // area visibility by `docs/spec-workspace-visibility-rail.md`): active
        // state reads the rift-owned `visibility` set — not `dock.is_open` —
        // and the badges read the same `WorktreeModel` the status bar reads
        // below — both live views over one model, no separate rail-owned
        // state. The four `on_toggle_*` callbacks are `cx.listener`s bound to
        // this `Entity<WorkspaceView>` (a weak reference, no retain cycle),
        // calling `Self::toggle_area` directly rather than round-tripping
        // through `window.dispatch_action`'s focused-node routing
        // (`docs/spec-visibility-rail-focus.md`, issue #848) — the rail click
        // path is now focus-immune by construction. The `Toggle*` actions +
        // their `on_action` handlers below stay in place for the keyboard,
        // command palette, and agent-driven dispatch.
        let rail = {
            let model = self.file_tree.read(cx).model();
            activity_rail::render(
                activity_rail::RailState {
                    explorer_editor_visible: self.visibility.is_visible(Area::ExplorerEditor),
                    terminal_visible: self.visibility.is_visible(Area::Terminal),
                    git_visible: self.visibility.is_visible(Area::Git),
                    diagnostics_visible: self.visibility.is_visible(Area::Diagnostics),
                    solo: self.visibility.solo,
                    changed_count: model.git_statuses().len(),
                    worst_diagnostic: activity_rail::worst_severity(model.all_diagnostics()),
                },
                cx.listener(|this, _event: &ClickEvent, window, cx| {
                    this.toggle_area(Area::ExplorerEditor, window, cx);
                }),
                cx.listener(|this, _event: &ClickEvent, window, cx| {
                    this.toggle_area(Area::Terminal, window, cx);
                }),
                cx.listener(|this, _event: &ClickEvent, window, cx| {
                    this.toggle_area(Area::Git, window, cx);
                }),
                cx.listener(|this, _event: &ClickEvent, window, cx| {
                    this.toggle_area(Area::Diagnostics, window, cx);
                }),
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
                    host_metrics: self.host_metrics.as_ref(),
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
            // Stable focus anchor (`docs/spec-visibility-rail-focus.md`, issue
            // #847): keeps this node — the one registering the `on_action`
            // handlers below — in `window`'s dispatch tree every frame, even
            // when no panel is focused/rendered, so rail/keyboard/command-
            // palette actions always reach a live handler.
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ToggleExplorer, window, cx| {
                this.toggle_area(Area::ExplorerEditor, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleOutline, window, cx| {
                this.toggle_outline(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleProblems, window, cx| {
                this.toggle_area(Area::Diagnostics, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSourceControl, window, cx| {
                this.toggle_area(Area::Git, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                this.toggle_area(Area::Terminal, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusTerminal, window, cx| {
                this.focus_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomActivePanel, window, cx| {
                this.zoom_active_panel(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SoloExplorerEditor, window, cx| {
                this.toggle_solo_area(Area::ExplorerEditor, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SoloTerminal, window, cx| {
                this.toggle_solo_area(Area::Terminal, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SoloDiagnostics, window, cx| {
                this.toggle_solo_area(Area::Diagnostics, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SoloGit, window, cx| {
                this.toggle_solo_area(Area::Git, window, cx);
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
    use gpui_component::dock::{DockPlacement, Panel, PanelControl};

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

    // --- workspace visibility set/solo state machine ---------------------
    // (`docs/spec-workspace-visibility-rail.md`, issue #819): `Visibility` has
    // no GPUI dependency, so these run as ordinary `#[test]`s exercising the
    // pure state machine directly, independent of the dock-tree reconciliation
    // the `#[gpui::test]`s further below cover.

    #[test]
    fn test_visibility_all_visible_marks_every_area_visible() {
        let visibility = Visibility::from_persisted(&Area::ALL, None);
        for area in Area::ALL {
            assert!(visibility.is_visible(area), "{area:?} should start visible");
        }
    }

    #[test]
    fn test_toggle_hides_one_area_and_a_second_toggle_restores_it() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);

        visibility.toggle(Area::Git);
        assert!(!visibility.is_visible(Area::Git), "Git is now hidden");
        for area in [Area::ExplorerEditor, Area::Terminal, Area::Diagnostics] {
            assert!(
                visibility.is_visible(area),
                "toggling Git leaves {area:?} visible"
            );
        }

        visibility.toggle(Area::Git);
        assert!(
            visibility.is_visible(Area::Git),
            "a second toggle restores it"
        );
    }

    /// Issue #821: the Terminal toggles exactly like any other area — this
    /// exercises it as the toggle TARGET (not a bystander, unlike the Git
    /// case above), the rail's own `ToggleTerminal` click path.
    #[test]
    fn test_toggle_terminal_hides_it_and_a_second_toggle_restores_it() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);

        visibility.toggle(Area::Terminal);
        assert!(
            !visibility.is_visible(Area::Terminal),
            "Terminal is now hidden"
        );
        for area in [Area::ExplorerEditor, Area::Diagnostics, Area::Git] {
            assert!(
                visibility.is_visible(area),
                "toggling Terminal leaves {area:?} visible"
            );
        }

        visibility.toggle(Area::Terminal);
        assert!(
            visibility.is_visible(Area::Terminal),
            "a second toggle restores it"
        );
    }

    #[test]
    fn test_is_visible_solo_shows_only_the_target_area() {
        let visibility = Visibility {
            visible: Area::ALL.into_iter().collect(),
            solo: Some(Area::Diagnostics),
        };

        assert!(
            visibility.is_visible(Area::Diagnostics),
            "the soloed area renders"
        );
        for area in [Area::ExplorerEditor, Area::Terminal, Area::Git] {
            assert!(
                !visibility.is_visible(area),
                "{area:?} is hidden while another area is soloed, even though it is \
                 still in the visible set"
            );
        }
    }

    /// Issue #820: `toggle_solo` entering solo on a non-Terminal area hides
    /// every other area, including the Terminal — the spec's "Terminal:
    /// fully symmetric" decision, exercised here through the mutating
    /// solo-toggle entry point rather than a hand-built `Visibility`.
    #[test]
    fn test_toggle_solo_on_a_non_terminal_area_hides_the_terminal_too() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);

        visibility.toggle_solo(Area::Diagnostics);

        assert!(
            visibility.is_visible(Area::Diagnostics),
            "the target area renders"
        );
        for area in [Area::ExplorerEditor, Area::Terminal, Area::Git] {
            assert!(
                !visibility.is_visible(area),
                "{area:?}, including the Terminal, is hidden while Diagnostics is soloed"
            );
        }
    }

    /// Issue #820: soloing the Terminal itself shows only the Terminal,
    /// hiding every other area — the same full-symmetry rule applied to the
    /// Terminal as the target rather than the odd one out.
    #[test]
    fn test_toggle_solo_on_the_terminal_shows_only_the_terminal() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);

        visibility.toggle_solo(Area::Terminal);

        assert!(visibility.is_visible(Area::Terminal));
        for area in [Area::ExplorerEditor, Area::Diagnostics, Area::Git] {
            assert!(!visibility.is_visible(area), "{area:?} is hidden");
        }
    }

    /// Issue #820: soloing the already-soloed area exits solo (a zoom-out),
    /// restoring the pre-solo visible set exactly — solo never touches
    /// `visible` membership, so nothing here needed re-adding.
    #[test]
    fn test_toggle_solo_on_the_soloed_area_again_exits_solo() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);
        visibility.toggle(Area::Git); // Git starts hidden, pre-solo.

        visibility.toggle_solo(Area::Diagnostics);
        assert!(visibility.is_visible(Area::Diagnostics));

        visibility.toggle_solo(Area::Diagnostics);

        assert!(
            !visibility.is_visible(Area::Git),
            "exiting solo restores the pre-solo set, where Git was still hidden"
        );
        for area in [Area::ExplorerEditor, Area::Terminal, Area::Diagnostics] {
            assert!(visibility.is_visible(area), "{area:?} is visible again");
        }
    }

    /// Issue #820: soloing a second area switches the target directly —
    /// no intermediate "exit solo" step, and the previous target is hidden
    /// exactly like every other non-target area.
    #[test]
    fn test_toggle_solo_on_a_different_area_switches_the_target() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);
        visibility.toggle_solo(Area::Diagnostics);

        visibility.toggle_solo(Area::Git);

        assert!(visibility.is_visible(Area::Git), "the new target renders");
        assert!(
            !visibility.is_visible(Area::Diagnostics),
            "the previous target is hidden like any other non-soloed area"
        );
    }

    /// Issue #820: "re-toggling any area from the rail exits solo by
    /// re-adding that area" — clicking the rail's own (currently soloed)
    /// area exits solo, keeping that area visible among the restored set.
    #[test]
    fn test_toggle_area_on_the_soloed_area_exits_solo_and_keeps_it_visible() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);
        visibility.toggle_solo(Area::Diagnostics);

        visibility.toggle(Area::Diagnostics);

        assert!(visibility.solo.is_none(), "solo is cleared");
        for area in Area::ALL {
            assert!(visibility.is_visible(area), "{area:?} is visible again");
        }
    }

    /// Issue #820: a rail click on a DIFFERENT (non-soloed) area while
    /// soloed also exits solo, and forces that area visible even if it had
    /// been toggled off before solo engaged — the affordance read as
    /// "hidden" (via `is_visible`) while soloed, so the click means "show
    /// it", not a blind flip of a membership bit solo made irrelevant.
    #[test]
    fn test_toggle_area_on_a_different_area_while_soloed_exits_solo_and_shows_it() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);
        visibility.toggle(Area::Git); // Git hidden before solo engages.
        visibility.toggle_solo(Area::Diagnostics);

        visibility.toggle(Area::Git);

        assert!(visibility.solo.is_none(), "solo is cleared");
        assert!(
            visibility.is_visible(Area::Git),
            "the clicked area is shown, overriding its pre-solo hidden state"
        );
        assert!(
            visibility.is_visible(Area::Diagnostics),
            "the previously-soloed area stays visible too (never removed from the set)"
        );
    }

    // --- persisted-shape adapter (issue #822) -----------------------------

    #[test]
    fn test_from_persisted_rebuilds_the_visible_set_and_solo() {
        let visibility =
            Visibility::from_persisted(&[Area::Terminal, Area::Git], Some(Area::Terminal));

        assert!(visibility.is_visible(Area::Terminal), "the solo target");
        for area in [Area::ExplorerEditor, Area::Diagnostics, Area::Git] {
            assert!(
                !visibility.is_visible(area),
                "{area:?} is hidden while Terminal is soloed"
            );
        }
    }

    #[test]
    fn test_from_persisted_empty_list_starts_with_nothing_visible() {
        let visibility = Visibility::from_persisted(&[], None);

        for area in Area::ALL {
            assert!(!visibility.is_visible(area), "{area:?} should be hidden");
        }
    }

    #[test]
    fn test_to_persisted_round_trips_through_from_persisted() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);
        visibility.toggle(Area::Diagnostics);

        let (visible_areas, solo_area) = visibility.to_persisted();
        let rebuilt = Visibility::from_persisted(&visible_areas, solo_area);

        assert_eq!(rebuilt, visibility);
    }

    // --- focus re-home target selection (`docs/spec-visibility-rail-focus.md`,
    // issue #847): `WorkspaceView::preferred_focus_area` has no GPUI
    // dependency (pure logic over `Visibility`), so these run as ordinary
    // `#[test]`s directly, independent of the `#[gpui::test]`s that exercise
    // the actual focus movement further below.

    #[test]
    fn test_preferred_focus_area_all_visible_picks_the_terminal_first() {
        let visibility = Visibility::from_persisted(&Area::ALL, None);

        assert_eq!(
            WorkspaceView::preferred_focus_area(&visibility),
            Some(Area::Terminal),
            "Terminal is the primary surface (docs/vision.md) and wins when everything is visible"
        );
    }

    #[test]
    fn test_preferred_focus_area_falls_back_down_the_preference_order() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);
        visibility.toggle(Area::Terminal);
        assert_eq!(
            WorkspaceView::preferred_focus_area(&visibility),
            Some(Area::ExplorerEditor),
            "Terminal hidden: Explorer+Editor is next"
        );

        visibility.toggle(Area::ExplorerEditor);
        assert_eq!(
            WorkspaceView::preferred_focus_area(&visibility),
            Some(Area::Diagnostics),
            "Terminal and Explorer+Editor hidden: Diagnostics is next"
        );

        visibility.toggle(Area::Diagnostics);
        assert_eq!(
            WorkspaceView::preferred_focus_area(&visibility),
            Some(Area::Git),
            "only Git is left visible"
        );
    }

    #[test]
    fn test_preferred_focus_area_nothing_visible_yields_the_root_anchor_fallback() {
        let visibility = Visibility::from_persisted(&[], None);

        assert_eq!(
            WorkspaceView::preferred_focus_area(&visibility),
            None,
            "the degenerate all-hidden state has no area to re-home to: callers fall back to the \
             workspace's own root focus anchor"
        );
    }

    #[test]
    fn test_preferred_focus_area_solo_picks_only_the_soloed_area() {
        let mut visibility = Visibility::from_persisted(&Area::ALL, None);
        visibility.toggle_solo(Area::Git);

        assert_eq!(
            WorkspaceView::preferred_focus_area(&visibility),
            Some(Area::Git),
            "solo hides every other area (including the Terminal), so the soloed area is the \
             only one `is_visible` reports"
        );
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
        let (_host_metrics_tx, host_metrics_rx) = flume::unbounded();
        let (_diff_reply_tx, diff_rx) = flume::unbounded();
        let (open_file_tx, _open_file_rx) = flume::unbounded();
        let (save_file_tx, _save_file_rx) = flume::unbounded();
        let (buffer_change_tx, _buffer_change_rx) = flume::unbounded();
        let (nav_tx, _nav_request_rx) = flume::unbounded();
        let (request_diff_tx, _request_diff_rx) = flume::unbounded();
        let (git_op_tx, _git_op_rx) = flume::unbounded();
        let (file_op_tx, _file_op_rx) = flume::unbounded();
        let (_file_op_result_tx, file_op_result_rx) = flume::unbounded();
        let (dir_browse_tx, _dir_browse_rx) = flume::unbounded();
        WorkspaceChannels {
            worktree_rx,
            buffer_rx,
            nav_rx,
            lsp_status_rx,
            host_metrics_rx,
            diff_rx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            request_diff_tx,
            git_op_tx,
            file_op_tx,
            file_op_result_rx,
            dir_browse_tx,
        }
    }

    /// Panel-tree construction (`docs/spec-ide-shell.md`, issue #324;
    /// updated by `docs/spec-workspace-visibility-rail.md`, issue #822): with
    /// no persisted state (`window_state_path: None`), the workspace seeds
    /// from `WindowState::default`'s all-areas-visible `visible_areas` — the
    /// explorer, the editor|terminal split, and real, *open* right/bottom
    /// docks, not the old hardcoded collapsed right/bottom.
    #[gpui::test]
    fn test_default_layout_has_left_explorer_center_split_and_all_areas_visible(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                dock_area.is_dock_open(DockPlacement::Right, cx),
                "right dock starts open (Git is visible by default)"
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
                dock_area.is_dock_open(DockPlacement::Bottom, cx),
                "bottom dock starts open (Diagnostics is visible by default)"
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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

    /// Dock interaction (`docs/spec-ide-shell.md`, issue #325), superseded by
    /// `docs/spec-workspace-visibility-rail.md` (issue #820): every dock
    /// surface used to expose gpui-component's own native zoom button
    /// (`Panel::zoomable` -> `Some(PanelControl::Toolbar)`, #716); #820
    /// disables that (`-> None`) on all five — `FileTree`, `EditorView`,
    /// `TerminalPanel`, `ProblemsPanel`, `SourceControlPanel` — so the
    /// built-in `ToggleZoom` -> `PanelEvent` path can never flip
    /// `TabPanel.zoomed` + `DockArea.zoom_view` behind the rift-owned
    /// visible set's back ("Single source of truth for solo"), and each
    /// supplies exactly one `toolbar_buttons()` entry (the solo trigger) in
    /// its place.
    #[gpui::test]
    fn test_all_dock_surfaces_disable_native_zoom_and_supply_one_solo_button(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let (file_tree, editor, session_view, problems_panel) = {
                    let view = workspace.read(cx);
                    (
                        view.file_tree.clone(),
                        view.editor.clone(),
                        view.session_view.clone(),
                        view.problems_panel.clone(),
                    )
                };

                fn assert_solo_only(
                    name: &str,
                    zoomable: Option<PanelControl>,
                    buttons: Option<Vec<Button>>,
                ) {
                    assert!(zoomable.is_none(), "{name} disables native zoom");
                    let buttons = buttons.unwrap_or_else(|| panic!("{name} supplies a header button"));
                    assert_eq!(
                        buttons.len(),
                        1,
                        "{name}'s toolbar carries exactly the solo button, not a native zoom button too"
                    );
                }

                file_tree.update(cx, |view, cx| {
                    assert_solo_only(
                        "the explorer",
                        view.zoomable(cx),
                        view.toolbar_buttons(window, cx),
                    );
                });
                editor.update(cx, |view, cx| {
                    assert_solo_only(
                        "the editor",
                        view.zoomable(cx),
                        view.toolbar_buttons(window, cx),
                    );
                });
                problems_panel.update(cx, |view, cx| {
                    assert_solo_only(
                        "the problems panel",
                        view.zoomable(cx),
                        view.toolbar_buttons(window, cx),
                    );
                });

                let (git_op_tx, _git_op_rx) = flume::unbounded();
                let source_control =
                    cx.new(|cx| SourceControlPanel::new(file_tree, git_op_tx, window, cx));
                source_control.update(cx, |view, cx| {
                    assert_solo_only(
                        "the source control panel",
                        view.zoomable(cx),
                        view.toolbar_buttons(window, cx),
                    );
                });

                let terminal_panel = cx.new(|_| TerminalPanel::new(session_view));
                terminal_panel.update(cx, |view, cx| {
                    assert_solo_only(
                        "the terminal",
                        view.zoomable(cx),
                        view.toolbar_buttons(window, cx),
                    );
                });
            })
            .unwrap();
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                "the bottom dock starts open (Diagnostics is visible by default, issue #822)"
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

    /// Stable workspace focus anchor (`docs/spec-visibility-rail-focus.md`,
    /// issue #847): `WorkspaceView::focus_handle` is the workspace's own,
    /// always-rendered root anchor — no longer delegated to the terminal
    /// (superseding this test's pre-#847 name/assertion) — so the node
    /// carrying the root `on_action` handlers (`.track_focus`'d on the same
    /// `div` in `render`) stays in `window`'s dispatch path even while no
    /// panel is focused. Startup focus still lands on the terminal via an
    /// explicit `focus_terminal` call from `main.rs::enter_workspace` (issue
    /// #358), exercised directly by `test_focus_terminal_moves_focus_to_the_terminal`
    /// below.
    #[gpui::test]
    fn test_workspace_focus_handle_is_its_own_focusable_anchor(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");
        let session_view =
            session_view.expect("session view constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let anchor = workspace.read(cx).focus_handle(cx);
                assert_ne!(
                    anchor,
                    session_view.read(cx).focus_handle(cx),
                    "the workspace's focus handle is its own root anchor, not delegated to the terminal"
                );

                anchor.focus(window, cx);
                assert!(
                    anchor.is_focused(window),
                    "the root anchor is focusable, so it stays reachable even with no panel focused"
                );
            })
            .unwrap();
    }

    /// Shell command action (`docs/spec-command-palette.md`, issue #358;
    /// rewired by `docs/spec-workspace-visibility-rail.md`, issue #819): the
    /// `ToggleExplorer` handler now flips `Area::ExplorerEditor`'s membership
    /// in the rift-owned visibility set, closes the left dock (the same
    /// `DockArea::toggle_dock` wiring `test_toggle_left_dock_flips_open_state`
    /// exercises directly), and collapses the center split down to the
    /// Terminal alone — restoring the editor|terminal split on the next
    /// toggle.
    #[gpui::test]
    fn test_toggle_area_explorer_editor_hides_left_dock_and_collapses_center_to_terminal(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                assert!(
                    workspace
                        .read(cx)
                        .visibility
                        .is_visible(Area::ExplorerEditor),
                    "the area starts visible"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::ExplorerEditor, window, cx);
                });
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "ToggleExplorer hides the explorer dock"
                );
                assert!(
                    !workspace
                        .read(cx)
                        .visibility
                        .is_visible(Area::ExplorerEditor),
                    "the area is now hidden in the rift-owned set"
                );
                match dock_area.read(cx).center() {
                    DockItem::Tabs { items, .. } => assert_eq!(
                        items[0].panel_name(cx),
                        crate::terminal_panel::TERMINAL_PANEL_NAME,
                        "the terminal expands to fill the center alone"
                    ),
                    other => {
                        panic!("expected the center to collapse to a single tab, got {other:?}")
                    }
                }

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::ExplorerEditor, window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "toggling again restores the explorer dock"
                );
                assert!(
                    workspace
                        .read(cx)
                        .visibility
                        .is_visible(Area::ExplorerEditor),
                    "the area is visible again"
                );
                match dock_area.read(cx).center() {
                    DockItem::Split { axis, items, .. } => {
                        assert_eq!(*axis, Axis::Horizontal);
                        assert_eq!(items.len(), 2, "center split holds editor + terminal again");
                    }
                    other => {
                        panic!("expected the center to be a horizontal split again, got {other:?}")
                    }
                }
            })
            .unwrap();
    }

    /// Shell command action (`docs/spec-workspace-visibility-rail.md`, issue
    /// #821, "Terminal: fully symmetric"): `ToggleTerminal` flips
    /// `Area::Terminal`'s membership in the rift-owned visibility set and
    /// collapses the center split down to the Editor alone — the mirror
    /// image of the Explorer+Editor toggle above, proving the Terminal is a
    /// real render-level peer rather than the permanent floor #820 left it
    /// as.
    #[gpui::test]
    fn test_toggle_area_terminal_collapses_center_to_editor_and_restores_the_split(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                    workspace.read(cx).visibility.is_visible(Area::Terminal),
                    "the Terminal starts visible"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Terminal, window, cx);
                });
                assert!(
                    !workspace.read(cx).visibility.is_visible(Area::Terminal),
                    "ToggleTerminal hides the Terminal in the rift-owned set"
                );
                match dock_area.read(cx).center() {
                    DockItem::Tabs { items, .. } => assert_eq!(
                        items[0].panel_name(cx),
                        crate::editor::EDITOR_PANEL_NAME,
                        "the editor expands to fill the center alone; the Terminal is gone, \
                         not merely collapsed"
                    ),
                    other => {
                        panic!("expected the center to collapse to a single tab, got {other:?}")
                    }
                }
                // The rail never re-arranges the layout: the left (Explorer)
                // dock is untouched by a Terminal-only toggle.
                assert!(dock_area.read(cx).is_dock_open(DockPlacement::Left, cx));

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Terminal, window, cx);
                });
                assert!(workspace.read(cx).visibility.is_visible(Area::Terminal));
                match dock_area.read(cx).center() {
                    DockItem::Split { axis, items, .. } => {
                        assert_eq!(*axis, Axis::Horizontal);
                        assert_eq!(items.len(), 2, "center split holds editor + terminal again");
                    }
                    other => {
                        panic!("expected the center to be a horizontal split again, got {other:?}")
                    }
                }
            })
            .unwrap();
    }

    /// Issue #821, "Terminal: fully symmetric": soloing the Terminal shows it
    /// alone in the center and closes every other dock, exactly like soloing
    /// any other area — exercised end to end through `SoloTerminal`'s
    /// handler (`Self::toggle_solo_area`), not just the pure `Visibility`
    /// state machine.
    #[gpui::test]
    fn test_toggle_solo_terminal_shows_it_alone_and_closes_every_other_dock(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let dock_area = workspace.read(cx).dock_area.clone();

                workspace.update(cx, |view, cx| {
                    view.toggle_solo_area(Area::Terminal, window, cx);
                });

                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "soloing the Terminal closes the Explorer dock too"
                );
                assert!(!dock_area.read(cx).is_dock_open(DockPlacement::Right, cx));
                assert!(!dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx));
                match dock_area.read(cx).center() {
                    DockItem::Tabs { items, .. } => assert_eq!(
                        items[0].panel_name(cx),
                        crate::terminal_panel::TERMINAL_PANEL_NAME,
                        "the Terminal alone fills the center"
                    ),
                    other => panic!("expected a single-tab center, got {other:?}"),
                }

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Terminal, window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "re-toggling the Terminal from the rail exits solo"
                );
                match dock_area.read(cx).center() {
                    DockItem::Split { items, .. } => {
                        assert_eq!(items.len(), 2, "the editor|terminal split is restored")
                    }
                    other => panic!("expected the split to be restored, got {other:?}"),
                }
            })
            .unwrap();
    }

    /// Issue #821: soloing a NON-Terminal area hides the Terminal too — the
    /// spec's "Terminal: fully symmetric" decision — which drives the
    /// center's `apply_center_visibility` all the way down to an empty tab
    /// strip (zero panels) since both center-contributing areas
    /// (Explorer+Editor and Terminal) are hidden at once. Zero panels, not a
    /// leftover single tab, is what actually makes the Terminal "not
    /// rendered" while soloed away.
    #[gpui::test]
    fn test_toggle_solo_a_non_terminal_area_empties_the_center(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let dock_area = workspace.read(cx).dock_area.clone();

                workspace.update(cx, |view, cx| {
                    view.toggle_solo_area(Area::Diagnostics, window, cx);
                });

                assert!(
                    !workspace.read(cx).visibility.is_visible(Area::Terminal),
                    "soloing Diagnostics hides the Terminal too"
                );
                match dock_area.read(cx).center() {
                    DockItem::Tabs { items, .. } => assert!(
                        items.is_empty(),
                        "neither the editor nor the terminal render while Diagnostics is soloed"
                    ),
                    other => panic!("expected an empty tab strip, got {other:?}"),
                }

                workspace.update(cx, |view, cx| {
                    view.toggle_solo_area(Area::Diagnostics, window, cx);
                });
                assert!(workspace.read(cx).visibility.is_visible(Area::Terminal));
                match dock_area.read(cx).center() {
                    DockItem::Split { items, .. } => assert_eq!(items.len(), 2),
                    other => panic!("expected the split to be restored, got {other:?}"),
                }
            })
            .unwrap();
    }

    /// Constraint ("Bindings are entity-lifetime, not render-lifetime",
    /// `docs/spec-workspace-visibility-rail.md`): hiding the Terminal never
    /// drops or recreates the `SessionView`/`TerminalPanel` entities — this
    /// is what lets a re-show re-attach the live tmux control-mode
    /// subscription with no reconnect. `Entity<T>`'s `PartialEq` compares by
    /// entity id, so this fails if `apply_center_visibility` ever rebuilt
    /// the underlying session instead of reusing `self.session_view`/
    /// `self.terminal_panel`.
    #[gpui::test]
    fn test_hiding_and_reshowing_the_terminal_keeps_the_same_session_entity(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");
        let session_view =
            session_view.expect("session view constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let terminal_panel_before = workspace.read(cx).terminal_panel.clone();

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Terminal, window, cx);
                });
                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Terminal, window, cx);
                });

                assert_eq!(
                    workspace.read(cx).session_view,
                    session_view,
                    "hide/show never replaces the SessionView entity (no reconnect)"
                );
                assert_eq!(
                    workspace.read(cx).terminal_panel,
                    terminal_panel_before,
                    "hide/show never replaces the TerminalPanel wrapper entity either"
                );
            })
            .unwrap();
    }

    /// Focus re-home on hide (`docs/spec-visibility-rail-focus.md`, issue
    /// #847). Two cases in one window, both starting from the Terminal
    /// focused: (1) hiding an unrelated area (Git) never fires the re-home —
    /// the Terminal still holds focus, since it never lost visibility; (2)
    /// hiding the focused Terminal itself — the Phase-390 QA freeze's actual
    /// trigger (clicking the Terminal rail toggle broke every other toggle,
    /// since focus is on the Terminal at click time far more often than on
    /// any other area) — moves focus to the next-preferred still-visible
    /// area (Explorer+Editor) instead of stranding it on the now-unrendered
    /// Terminal.
    #[gpui::test]
    fn test_toggle_area_rehomes_focus_only_when_the_focused_area_is_hidden(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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

                // Case 1: hiding Git, an area unrelated to focus, is a no-op
                // for focus.
                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Git, window, cx);
                });
                assert!(
                    !workspace.read(cx).visibility.is_visible(Area::Git),
                    "Git is now hidden"
                );
                assert!(
                    session_view.focus_handle(cx).is_focused(window),
                    "the Terminal still holds focus: it never lost visibility, so re-home never fires"
                );

                // Case 2: hiding the focused Terminal re-homes focus to the
                // next-preferred still-visible area.
                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Terminal, window, cx);
                });
                assert!(
                    !workspace.read(cx).visibility.is_visible(Area::Terminal),
                    "the Terminal is now hidden"
                );
                assert!(
                    !session_view.focus_handle(cx).contains_focused(window, cx),
                    "focus moved off the now-hidden Terminal"
                );
                assert!(
                    Focusable::focus_handle(&workspace.read(cx).editor, cx)
                        .contains_focused(window, cx),
                    "focus re-homed to Explorer+Editor, the next-preferred still-visible area"
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                    view.toggle_area(Area::ExplorerEditor, window, cx);
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
    /// its tab to the right dock and opens the dock if collapsed; hiding it
    /// removes the tab and re-collapses the dock, since the panel is why the
    /// dock opened. The right dock is visible by default (issue #822), so the
    /// Git area is explicitly hidden first — mirroring the outline test's
    /// left-dock note — to exercise `show_results_panel`'s own open/collapse
    /// tracking rather than reusing an already-open dock. The live inner
    /// `TabPanel` (not the stale construction-time `DockItem::Tabs { items }`
    /// snapshot) is read to see the added tab.
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                    dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "right dock starts open (Git is visible by default)"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Git, window, cx);
                });
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "hiding Git collapses the right dock"
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

    /// Shell command action (issue #358; rewired by `docs/spec-workspace-
    /// visibility-rail.md`, issue #819; default flipped to visible by #822):
    /// `ToggleProblems` flips `Area::Diagnostics`'s membership in the
    /// rift-owned visibility set, opens/closes the bottom dock (home to the
    /// problems panel, #342 — visible by default), and attaches/detaches the
    /// panel's tab itself so a hidden Diagnostics area is not rendered
    /// (`TabPanel` falls back to `Empty` with zero tabs) rather than merely
    /// collapsed.
    #[gpui::test]
    fn test_toggle_area_diagnostics_attaches_and_detaches_the_problems_panel(
        cx: &mut TestAppContext,
    ) {
        use crate::problems_panel::PROBLEMS_PANEL_NAME;

        fn bottom_active_tab_name(dock_area: &Entity<DockArea>, cx: &App) -> Option<&'static str> {
            let bottom = dock_area
                .read(cx)
                .bottom_dock()
                .expect("bottom dock exists");
            match bottom.read(cx).panel() {
                DockItem::Tabs { view, .. } => {
                    view.read(cx).active_panel(cx).map(|p| p.panel_name(cx))
                }
                other => panic!("expected the bottom dock to hold tabs, got {other:?}"),
            }
        }

        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                    dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                    "problems dock starts open (Diagnostics is visible by default, issue #822)"
                );
                assert!(
                    workspace.read(cx).visibility.is_visible(Area::Diagnostics),
                    "the area starts visible, matching the open dock"
                );
                assert_eq!(
                    bottom_active_tab_name(&dock_area, cx),
                    Some(PROBLEMS_PANEL_NAME),
                    "the problems panel's tab is attached from the start"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Diagnostics, window, cx);
                });
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                    "ToggleProblems closes the bottom dock"
                );
                assert!(!workspace.read(cx).visibility.is_visible(Area::Diagnostics));
                assert_eq!(
                    bottom_active_tab_name(&dock_area, cx),
                    None,
                    "hiding the area detaches the problems panel's tab, so it is not rendered"
                );

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Diagnostics, window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                    "toggling again re-opens the bottom dock"
                );
                assert!(workspace.read(cx).visibility.is_visible(Area::Diagnostics));
                assert_eq!(
                    bottom_active_tab_name(&dock_area, cx),
                    Some(PROBLEMS_PANEL_NAME),
                    "showing the area re-attaches the problems panel's tab"
                );
            })
            .unwrap();
    }

    /// Shell command action (issue #358; rewired by `docs/spec-workspace-
    /// visibility-rail.md`, issue #819; default flipped to visible by #822):
    /// `ToggleSourceControl` flips `Area::Git`'s membership in the
    /// rift-owned visibility set and opens/closes the right dock (reserved
    /// for the source control + diff panels, #338), which is visible by
    /// default.
    #[gpui::test]
    fn test_toggle_area_git_flips_right_dock_open_state(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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
                    dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "source control dock starts open (Git is visible by default, issue #822)"
                );
                assert!(workspace.read(cx).visibility.is_visible(Area::Git));

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Git, window, cx);
                });
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "ToggleSourceControl closes the right dock"
                );
                assert!(!workspace.read(cx).visibility.is_visible(Area::Git));

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Git, window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "toggling again re-opens the right dock"
                );
                assert!(workspace.read(cx).visibility.is_visible(Area::Git));
            })
            .unwrap();
    }

    /// Solo (`docs/spec-workspace-visibility-rail.md`, issue #820): soloing
    /// an area closes every OTHER area's dock — including one the user had
    /// just explicitly opened — and re-toggling the soloed area from the
    /// rail (the same click path #819 wired) exits solo, restoring each
    /// dock to exactly its pre-solo open/closed state.
    #[gpui::test]
    fn test_toggle_solo_area_closes_every_other_dock_and_toggle_area_restores(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                let dock_area = workspace.read(cx).dock_area.clone();

                // The default (no persisted state, issue #822): every area
                // starts visible/open. Turn Diagnostics off explicitly for a
                // mixed pre-solo state, so restoring it is a real assertion.
                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Diagnostics, window, cx);
                });
                assert!(dock_area.read(cx).is_dock_open(DockPlacement::Left, cx));
                assert!(dock_area.read(cx).is_dock_open(DockPlacement::Right, cx));
                assert!(!dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx));

                workspace.update(cx, |view, cx| {
                    view.toggle_solo_area(Area::Git, window, cx);
                });
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "soloing Git closes the left (Explorer) dock"
                );
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "the soloed area's own dock opens"
                );
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                    "the bottom dock, already closed pre-solo, stays closed"
                );
                assert!(workspace.read(cx).visibility.is_visible(Area::Git));

                workspace.update(cx, |view, cx| {
                    view.toggle_area(Area::Git, window, cx);
                });
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Left, cx),
                    "exiting solo restores the left dock"
                );
                assert!(
                    dock_area.read(cx).is_dock_open(DockPlacement::Right, cx),
                    "exiting solo restores the right dock to its pre-solo (open) state"
                );
                assert!(
                    !dock_area.read(cx).is_dock_open(DockPlacement::Bottom, cx),
                    "the bottom dock stays at its pre-solo (closed) state, not re-added"
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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
    /// the workspace's own root focus anchor (`docs/spec-visibility-rail-
    /// focus.md`, issue #847) — "dismissing the palette leaves terminal/editor
    /// state untouched" from the spec.
    #[gpui::test]
    fn test_open_command_palette_opens_a_dialog_and_close_clears_it(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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
            assert_ne!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening the palette does not touch the workspace's own root focus anchor"
            );

            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "closing the dialog clears the active-dialog state"
            );
            assert_ne!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "dismissing the palette leaves the workspace's root focus anchor untouched"
            );
        })
        .unwrap();
    }

    /// Settings surface (`docs/spec-theme-settings.md`, issue #366): opening
    /// sets an active `Root` dialog, mirroring the command palette above, and
    /// closing it clears that state without disturbing the workspace's own
    /// root focus anchor (`docs/spec-visibility-rail-focus.md`, issue #847).
    #[gpui::test]
    fn test_open_settings_opens_a_dialog_and_close_clears_it(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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
            assert_ne!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening settings does not touch the workspace's own root focus anchor"
            );

            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "closing the dialog clears the active-dialog state"
            );
            assert_ne!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "dismissing settings leaves the workspace's root focus anchor untouched"
            );
        })
        .unwrap();
    }

    /// Jump-to-file quick-open (`docs/spec-explorer-search.md`, Phase 31,
    /// issue #681): opening sets an active `Root` dialog, mirroring the
    /// command palette and settings surface above, and closing it clears
    /// that state without disturbing the workspace's own root focus anchor
    /// (`docs/spec-visibility-rail-focus.md`, issue #847).
    #[gpui::test]
    fn test_open_quick_open_opens_a_dialog_and_close_clears_it(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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
            assert_ne!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening quick-open does not touch the workspace's own root focus anchor"
            );

            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "closing the dialog clears the active-dialog state"
            );
            assert_ne!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "dismissing quick-open leaves the workspace's root focus anchor untouched"
            );
        })
        .unwrap();
    }

    /// The in-cockpit root picker (issue #769,
    /// `docs/spec-session-root-picker.md`): opening sets an active `Root`
    /// dialog and an in-flight browse of the seeded start path, mirroring
    /// the command palette / settings / quick-open dialogs above; closing it
    /// leaves the workspace's own root focus anchor
    /// (`docs/spec-visibility-rail-focus.md`, issue #847) untouched.
    #[gpui::test]
    fn test_open_root_picker_opens_a_dialog_with_a_pending_seed_browse(cx: &mut TestAppContext) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let mut session_view: Option<Entity<SessionView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                session_view = Some(view.clone());
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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
            assert_ne!(
                workspace.read(cx).focus_handle(cx),
                session_view.read(cx).focus_handle(cx),
                "opening the root picker does not touch the workspace's own root focus anchor"
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
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

    /// [`WorkspaceView::apply_clone_result`]'s `~`-tolerant correlation
    /// (issue #839): the picker records `pending_clone` as the RAW,
    /// unresolved `<parent>/<name>` it sent (`~/code/repo`), but the daemon
    /// echoes the RESOLVED path (`~` expanded to the remote `$HOME`) — an
    /// exact-match check (the pre-fix behavior) would never match this and
    /// drop the reply forever, leaving the spinner stuck. The
    /// `root_picker::browse_reply_matches` tolerant check clears
    /// `pending_clone` and lets the `Picked` flow (session created at the
    /// reply's resolved root) proceed instead.
    #[gpui::test]
    fn test_apply_clone_result_tilde_parent_correlates_and_closes_the_picker(
        cx: &mut TestAppContext,
    ) {
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(view, test_channels(), None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");

        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |view, cx| {
                view.open_root_picker(window, cx);
            });
        })
        .unwrap();

        // Emit the `Clone` request the picker's Clone-mode action would send
        // for a `~`-prefixed parent (bypassing the private
        // `RootPicker::start_clone`, unreachable from this owner module).
        cx.update_window(window.into(), |_, _window, cx| {
            let picker = workspace
                .read(cx)
                .root_picker_session
                .as_ref()
                .unwrap()
                .picker
                .clone();
            picker.update(cx, |_picker, cx| {
                cx.emit(RootPickerEvent::Clone {
                    url: "https://example.com/org/repo.git".to_string(),
                    parent: "~/code".to_string(),
                    name: "repo".to_string(),
                });
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
                    .pending_clone,
                Some("~/code/repo".to_string()),
                "the raw, unresolved target is the outstanding request"
            );
        })
        .unwrap();

        // The daemon's reply carries the RESOLVED path (`~` expanded to the
        // remote `$HOME`) — never an exact match against `~/code/repo`.
        cx.update_window(window.into(), |_, _window, cx| {
            workspace.update(cx, |view, cx| {
                view.apply_clone_result("/home/dev/code/repo".to_string(), None, cx);
            });
        })
        .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            assert!(
                !window.has_active_dialog(cx),
                "a tolerant-matched success clears the picker instead of leaving it stuck"
            );
            assert!(
                workspace.read(cx).root_picker_session.is_none(),
                "the Picked flow ran through to completion"
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
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, test_channels(), None, None, None, window, cx)
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

    /// Default-collapsed is per-project (#795 follow-up, #738 re-root): a
    /// project switch's fresh snapshot must default the NEW project's tree
    /// to collapsed too, never inherit the OLD project's per-path
    /// `collapsed` state. Regression for the reviewer-flagged gap: without
    /// resetting the seed guard at the re-root hook, a directory the user
    /// had expanded in the old project (here `src`, deliberately same-named
    /// across both projects) would carry over as expanded in the new one.
    #[gpui::test]
    fn test_worktree_snapshot_after_a_project_switch_defaults_the_new_tree_to_collapsed(
        cx: &mut TestAppContext,
    ) {
        use rift_protocol::WorktreeEntry;
        use std::time::SystemTime;

        fn dir(path: &str) -> WorktreeEntry {
            WorktreeEntry {
                path: path.to_owned(),
                kind: EntryKind::Dir,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH,
            }
        }

        fn file(path: &str) -> WorktreeEntry {
            WorktreeEntry {
                path: path.to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH,
            }
        }

        let (worktree_tx, worktree_rx) = flume::unbounded();
        let channels = WorkspaceChannels {
            worktree_rx,
            ..test_channels()
        };
        let mut workspace: Option<Entity<WorkspaceView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let session_view = cx.new(|cx| SessionView::new(cx).0);
                workspace = Some(cx.new(|cx| {
                    WorkspaceView::new(session_view, channels, None, None, None, window, cx)
                }));
                cx.new(|cx| Root::new(workspace.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let workspace = workspace.expect("workspace constructed inside the window callback");
        let file_tree = cx.update(|cx| workspace.read(cx).file_tree.clone());

        // Project A's first (and only) snapshot defaults to collapsed.
        let _ = worktree_tx.send(DaemonMessage::WorktreeSnapshot {
            root: "/proj-a".into(),
            entries: vec![dir("src"), file("src/main.rs"), dir("docs")],
            final_chunk: true,
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let tree = file_tree.read(cx);
            assert!(tree.is_collapsed("src"), "project A defaults to collapsed");
            assert!(tree.is_collapsed("docs"));
        });

        // The user opens `src` in project A.
        cx.update_window(window.into(), |_, _window, cx| {
            file_tree.update(cx, |tree, cx| {
                tree.reveal("src/main.rs");
                cx.notify();
            });
        })
        .unwrap();
        cx.update(|cx| {
            assert!(
                !file_tree.read(cx).is_collapsed("src"),
                "revealing src/main.rs expands its ancestor"
            );
        });

        // Switching to project B -- a same-named `src` directory, plus one
        // the user never touched -- must default BOTH to collapsed, not
        // inherit project A's now-expanded `src`.
        let _ = worktree_tx.send(DaemonMessage::WorktreeSnapshot {
            root: "/proj-b".into(),
            entries: vec![dir("src"), file("src/lib.rs"), dir("vendor")],
            final_chunk: true,
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let tree = file_tree.read(cx);
            assert_eq!(tree.model().root(), Some("/proj-b"));
            assert!(
                tree.is_collapsed("src"),
                "the new project's same-named src defaults to collapsed, no stale carry-over"
            );
            assert!(tree.is_collapsed("vendor"));
        });
    }
}
