use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable};
use termy_terminal_ui::TmuxSnapshot;
use tracing::debug;

use crate::keytable::{self, KeyTable, PrefixOptions};
use crate::layout::{self, LayoutNode};
use crate::pane_view::{measure_cell_size, PaneActivity, PaneView};
use crate::quote_tmux_arg;
use crate::{
    CaptureRequest, CaptureResult, ConnectionStatus, KeyTableQueryResult, PaneInput, PaneOutput,
    SelectWindow, SessionListItem, SessionOrderUpdate, SessionSwitchRequest, SubscriptionUpdate,
    TermSize,
};

const DEFAULT_FONT_SIZE: f32 = 14.0;
/// Lower bound of the whole-client font size, shared by the `Ctrl+=`/`Ctrl+-`
/// zoom path and the settings surface's font-scale field (#366).
pub const MIN_FONT_SIZE: f32 = 8.0;
/// Upper bound of the whole-client font size (see [`MIN_FONT_SIZE`]).
pub const MAX_FONT_SIZE: f32 = 40.0;
const FONT_SIZE_STEP: f32 = 1.0;
/// Height of each pane's header row (type glyph, command title, cwd, running
/// pill, split/zoom actions). Reserved from the reported tmux grid so stacked
/// panes never clip their bottom rows (see [`max_vertical_pane_count`] and the
/// #424 client-sizing invariant).
const PANE_HEADER_HEIGHT: f32 = 32.0;
/// How long the transient grid-size overlay stays visible after a resize
/// (`docs/spec-status-line.md`: the grid readout is resize feedback like an
/// OSD, not persistent status). `resize_client_to_area` arms a matching
/// one-shot timer that re-renders once the deadline passes, so the overlay
/// self-clears without a recurring poll.
const RESIZE_OVERLAY_DURATION: Duration = Duration::from_millis(900);
/// Height of one session chip (and the trailing new-session chip) in the
/// title-bar strip (#683, `docs/spec-session-management.md`).
const SESSION_CHIP_HEIGHT: f32 = 24.0;

/// A tmux layout snapshot paired with the per-pane `is_shell` flags (#510).
/// termy's `TmuxPaneState` is a vendored upstream type that cannot carry the
/// daemon-evaluated `is_shell` flag, so it rides beside the snapshot keyed by
/// tmux pane id (`%N`). A pane absent from the map reads as non-shell (process
/// glyph) — the legacy tmux path, which has no daemon flag, sends an empty map.
pub struct SessionSnapshot {
    pub snapshot: TmuxSnapshot,
    pub pane_is_shell: HashMap<String, bool>,
}

pub struct TerminalHandle {
    pub pane_output_tx: flume::Sender<PaneOutput>,
    pub input_rx: flume::Receiver<PaneInput>,
    pub size_changed_rx: flume::Receiver<TermSize>,
    pub snapshot_tx: flume::Sender<SessionSnapshot>,
    pub tmux_command_rx: flume::Receiver<String>,
    pub subscription_tx: flume::Sender<SubscriptionUpdate>,
    pub capture_request_rx: flume::Receiver<CaptureRequest>,
    pub capture_result_tx: flume::Sender<CaptureResult>,
    pub connection_status_tx: flume::Sender<ConnectionStatus>,
    /// An explicit key-table refresh request from the statusbar affordance —
    /// forwarded onto the protocol as `ClientMessage::QueryKeyTable`. A
    /// dispatched binding-mutating command's refresh is issued server-side
    /// instead (`spawn_command_bridge` in `crates/app`), not carried here.
    pub key_table_request_rx: flume::Receiver<()>,
    /// The parsed-ready `list-keys`/`show-options` reply for a refresh request
    /// (including the daemon's own unprompted attach-time query).
    pub key_table_result_tx: flume::Sender<KeyTableQueryResult>,
    /// The host's session list (`docs/spec-session-management.md`): every
    /// `SessionListReply` — the reply to an explicit refresh below or the
    /// daemon's unprompted churn-driven push — replaces the title-bar strip's
    /// whole list.
    pub session_list_tx: flume::Sender<Vec<SessionListItem>>,
    /// An explicit session-list refresh request (`open_session_switcher`) —
    /// forwarded onto the protocol as `ClientMessage::QuerySessionList`.
    /// Unused on the legacy tmux path (`RIFT_TERMINAL_LEGACY`): the receiver
    /// drops there and a request is a harmless no-op, so the strip stays on
    /// its single-row fallback (the legacy path is slated for removal, #285).
    pub session_list_request_rx: flume::Receiver<()>,
    /// A cockpit switch from the session strip — forwarded onto the
    /// protocol as `ClientMessage::Attach { session }` followed by a viewport
    /// re-assert (see [`SessionSwitchRequest`]). Same legacy-path caveat as
    /// `session_list_request_rx`.
    pub session_switch_rx: flume::Receiver<SessionSwitchRequest>,
    /// A session-order mutation from the strip — a drag-to-reorder commit or
    /// a rename's slot-preserving key rename (#686,
    /// `docs/spec-session-management.md`). Routed to `rift-app`'s
    /// `session_order` store, which persists it and re-sorts + re-pushes the
    /// current list on `session_list_tx`'s target channel. Unused on the
    /// legacy tmux path (the strip's drag/rename affordances still emit, but
    /// nothing is listening — harmless, the same caveat as
    /// `session_list_request_rx`).
    pub session_order_rx: flume::Receiver<SessionOrderUpdate>,
    /// The reconnect banner's Cancel (#476,
    /// `docs/spec-connection-robustness.md`): consumed by the SSH-level
    /// reconnect engine between attempts, which stops retrying and answers
    /// with `ConnectionStatus::Disconnected` — the visible not-connected
    /// state (the Connection screen once #477 lands).
    pub reconnect_cancel_rx: flume::Receiver<()>,
}

struct PaneEntry {
    entity: Entity<PaneView>,
    pty_tx: flume::Sender<Vec<u8>>,
    /// Whether this pane is sitting at its shell (tmux's own
    /// `#{==:#{pane_current_command},#{b:default-shell}}`, #510) — drives the
    /// pane header's type glyph. Refreshed from every snapshot; defaults to
    /// `false` (process glyph) when the flag is unavailable (the legacy tmux
    /// path, which sends an empty map).
    is_shell: bool,
    /// Keeps `SessionView`'s observation of this pane's activity alive for the
    /// pane's lifetime. A background pane's own `cx.notify()` re-renders only
    /// its own subtree, so the parent must observe it to refresh the tab-bar
    /// aggregate live off an OSC-133/bell transition. Dropped with the entry
    /// when the pane leaves the snapshot, so observations never leak across
    /// snapshots (`docs/spec-pane-activity-v2.md`).
    _activity_subscription: Subscription,
}

struct WindowState {
    id: String,
    name: String,
    index: i32,
    is_active: bool,
    pane_ids: Vec<String>,
    /// Whether this window's active pane is sitting at its shell (tmux's own
    /// `#{==:#{pane_current_command},#{b:default-shell}}`, #510) — drives the
    /// tab's type glyph: shell → prompt glyph, anything else → process glyph.
    /// Defaults to `false` (process glyph) when the flag is unavailable (the
    /// legacy tmux path, or a window with no active pane in the snapshot).
    is_shell: bool,
}

/// One window as the app's composite status line renders it
/// (`docs/spec-status-line.md`): the `index:name` chip, whether it is the
/// active window (rendered on a surface chip), and its folded pane activity
/// (a dot on busy/attention windows). Built by [`SessionView::status_windows`]
/// so the app reads a plain snapshot instead of reaching into the terminal's
/// live pane/window state. Clicks route back through
/// [`SessionView::select_window`] (the existing tmux command channel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusWindow {
    pub id: String,
    pub index: i32,
    pub name: String,
    pub is_active: bool,
    pub activity: PaneActivity,
}

/// An in-progress inline window rename. `window_id` targets the tmux window;
/// `input` holds the edit state and `_subscription` keeps the submit handler
/// alive for as long as the rename is active. The rename only emits
/// `rename-window`; the new name arrives via the next snapshot /
/// `rift_window_name` subscription, never an optimistic local mutation.
struct WindowRename {
    window_id: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// An in-progress new-session prompt in the switcher's footer row. Mirrors
/// [`WindowRename`]: `input` holds the edit state and `_subscription` keeps the
/// submit handler alive while the prompt is active. Enter sends a
/// [`SessionSwitchRequest`] for the typed name (the daemon child command is
/// attach-or-create, so a fresh name creates the session); the new list and
/// indicator arrive via the daemon's push and the fresh snapshot, never an
/// optimistic local mutation.
struct NewSessionPrompt {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// An in-progress inline session rename in the title-bar strip (#684,
/// `docs/spec-session-management.md`), dispatched from a chip's right-click
/// menu (see [`SessionView::render_session_strip`]). Mirrors [`WindowRename`]:
/// `session_id` is tmux's rename-stable session id (`SessionListItem::id`,
/// targeted as `$<id>`), `original_name` lets an unchanged submit no-op, and
/// `input`/`_subscription` hold the edit state and keep the submit handler
/// alive while the rename is active. The rename only emits `rename-session`;
/// the new name arrives via the next `SessionListReply` (and, for the
/// attached session, the `%session-renamed`-driven layout snapshot), never an
/// optimistic local mutation.
struct SessionRename {
    session_id: u32,
    original_name: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// An in-progress inline kill confirmation for a session chip (#685,
/// `docs/spec-session-management.md`), armed by a chip's right-click menu's
/// "Kill" item (see [`SessionView::render_session_strip`]). Two-step by
/// design: the menu item only arms this state (see
/// [`SessionView::start_session_kill_confirm`]) — nothing reaches tmux until
/// the confirm control commits (see [`SessionView::confirm_session_kill`]);
/// Escape or the cancel control aborts with no command sent
/// ([`SessionView::cancel_session_kill`]). `session_id` is tmux's
/// rename-stable session id (`SessionListItem::id`, targeted as `$<id>`);
/// `session_name` is display-only (the confirm affordance's label/tooltip).
/// `focus_handle` is moved onto the confirm row on arming (#686 drive-by fix)
/// so the row's own `on_key_down` actually receives Escape — unlike
/// [`SessionRename`], nothing here is an `InputState` that self-focuses, so
/// this state carries its own handle.
struct SessionKillConfirm {
    session_id: u32,
    session_name: String,
    focus_handle: FocusHandle,
}

/// The payload a dragged session chip carries (#686,
/// `docs/spec-session-management.md`): the session's NAME — the order
/// store's key, not the tmux id — read by the drop target's `on_drop` handler
/// ([`SessionView::reorder_sessions`]) via gpui's `on_drag`/`on_drop`, mirroring
/// the explorer tree's own drag payload (`file_tree.rs`'s `DraggedRow`).
#[derive(Clone)]
struct DraggedSession {
    name: String,
}

/// The floating preview that follows the cursor while a [`DraggedSession`] is
/// in flight, mirroring the explorer tree's `DragPreview`
/// (`file_tree.rs`) and gpui-component's own drag preview
/// (`dock/tab_panel.rs`'s `DragPanel`). Theme tokens only, never a hardcoded
/// hex.
struct SessionDragPreview {
    name: SharedString,
}

impl Render for SessionDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("session-chip-drag-preview")
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .text_size(px(13.0))
            .text_color(cx.theme().foreground)
            .child(self.name.clone())
    }
}

/// An in-progress border drag. `start` is the mouse position along the drag
/// axis at mouse-down; `emitted_cells` is the whole-cell offset already sent to
/// tmux, so each move only emits the incremental `resize-pane`.
struct BorderDrag {
    target_pane: String,
    horizontal: bool,
    start: Pixels,
    emitted_cells: i32,
}

/// Quote a window name for a tmux command line. tmux parses the command with
/// its own lexer, so a name containing spaces or metacharacters must be wrapped
/// in single quotes, with any embedded single quote escaped as `'\''`.
fn quote_tmux_name(name: &str) -> String {
    format!("'{}'", name.replace('\'', "'\\''"))
}

/// tmux `rename-session -t $<id> -- <name>` command for a session chip's
/// inline rename (#684, `docs/spec-session-management.md`). `id` targets
/// tmux's rename-stable session id (`SessionListItem::id`), never the
/// (possibly stale) name, so a concurrent external rename cannot race the
/// target; `new_name` is passed through [`quote_tmux_arg`] after `--`
/// (end-of-options), so an untrusted name containing whitespace or tmux
/// lexer metacharacters can never break the argument boundary or inject a
/// second command.
fn rename_session_command(id: u32, new_name: &str) -> String {
    format!("rename-session -t ${id} -- {}", quote_tmux_arg(new_name))
}

/// tmux `kill-session -t $<id>` command for a session chip's kill-confirm
/// (#685, `docs/spec-session-management.md`). `id` targets tmux's
/// rename-stable session id (`SessionListItem::id`), never the (possibly
/// stale) name; a numeric `$<id>` target needs no quoting helper — quoting
/// only matters for untrusted text arguments like a session name (see
/// [`rename_session_command`]).
fn kill_session_command(id: u32) -> String {
    format!("kill-session -t ${id}")
}

/// tmux `resize-pane` direction flag for a cell delta. Positive grows the
/// leading pane (right for a column split, down for a row split).
fn resize_direction(horizontal: bool, delta_positive: bool) -> &'static str {
    match (horizontal, delta_positive) {
        (true, true) => "R",
        (true, false) => "L",
        (false, true) => "D",
        (false, false) => "U",
    }
}

/// tmux `split-window` command for a pane header's split controls. The visual
/// divider is inverted vs. tmux's naming: a side-by-side split (vertical `|`
/// divider) is tmux `-h`; a stacked split (horizontal `-` divider) is `-v`.
fn split_command(side_by_side: bool, pane: &str) -> String {
    let direction = if side_by_side { "h" } else { "v" };
    format!("split-window -{} -t {}", direction, pane)
}

/// tmux `select-pane` command focusing the given pane.
fn select_pane_command(pane: &str) -> String {
    format!("select-pane -t {}", pane)
}

/// tmux `new-window -c <dir>` command opening a fresh window rooted at `dir`
/// — the explorer's "Reveal in terminal" context-menu action
/// (`docs/spec-explorer-context-menu.md`). `dir` is quoted with
/// [`quote_tmux_name`] so a path containing spaces or metacharacters still
/// parses as a single argument. Structural only: it never `send-keys` into an
/// existing pane and never inspects a pane's process, so it stays
/// agent-agnostic and injection-free.
fn new_window_at_command(dir: &str) -> String {
    format!("new-window -c {}", quote_tmux_name(dir))
}

/// tmux `resize-pane -Z` command toggling the given pane's zoom (full-window)
/// state — the pane header's zoom control.
fn zoom_pane_command(pane: &str) -> String {
    format!("resize-pane -Z -t {}", pane)
}

/// Rewrite a home-anchored absolute path to a `~`-relative one for display
/// (`/home/<user>/x` or `/root/x` -> `~/x`), leaving any other path unchanged.
/// Pure string math on the tmux-reported `pane_current_path` — no filesystem
/// access and no remote `$HOME` lookup — so it stays agent-agnostic and
/// unit-testable.
fn home_relative(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("/root") {
        if rest.is_empty() || rest.starts_with('/') {
            return format!("~{rest}");
        }
    }
    if let Some(rest) = path.strip_prefix("/home/") {
        return match rest.find('/') {
            Some(slash) => format!("~{}", &rest[slash..]),
            None if !rest.is_empty() => "~".to_string(),
            None => path.to_string(),
        };
    }
    path.to_string()
}

/// The largest number of panes stacked vertically along any path through the
/// layout tree — the count of pane headers that share a single column's height
/// in the worst case. A vertical split (`horizontal == false`, rows stacked)
/// sums its children; a horizontal split (columns side by side) takes the max;
/// a leaf is one. Drives the header-row reservation in [`grid_size_for`] so no
/// column clips its bottom rows (the #424 invariant). GPUI-free so the
/// per-shape recursion is unit-testable.
fn max_vertical_pane_count(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Pane(_) => 1,
        LayoutNode::Split {
            horizontal: true,
            children,
        } => children
            .iter()
            .map(|(_, child)| max_vertical_pane_count(child))
            .max()
            .unwrap_or(1),
        LayoutNode::Split {
            horizontal: false,
            children,
        } => children
            .iter()
            .map(|(_, child)| max_vertical_pane_count(child))
            .sum::<usize>()
            .max(1),
    }
}

/// The SSH-outage danger banner's body line (design contract in
/// `docs/spec-connection-robustness.md`): where the reconnect loop is
/// reconnecting to and which attempt it is on. GPUI-free so the format is
/// unit-testable.
fn reconnect_banner_message(ssh_label: &str, retry: u32) -> String {
    format!("reconnecting to {ssh_label} — retry {retry}")
}

/// Whole-cell grid dimensions that fit the given pane-area bounds after
/// reserving `reserved_height` for the stacked pane headers, clamped to at
/// least 1x1 so a degenerate (collapsed) layout never reports a zero-sized
/// tmux client. Reserving the header rows before the floor is the #424 grid
/// reconciliation: tmux then lays panes out in the rows that actually fit
/// below their headers, so a stacked split never clips its bottom rows.
/// GPUI-free math so the floor/clamp behaviour is unit-testable.
fn grid_size_for(area: Size<Pixels>, cell: Size<Pixels>, reserved_height: Pixels) -> TermSize {
    let usable_height = (area.height - reserved_height).max(px(0.0));
    TermSize {
        cols: ((area.width / cell.width).floor() as usize).max(1),
        rows: ((usable_height / cell.height).floor() as usize).max(1),
    }
}

/// Precedence rank of a [`PaneActivity`] for the per-window aggregate:
/// attention > busy > free.
fn activity_rank(activity: PaneActivity) -> u8 {
    match activity {
        PaneActivity::Free => 0,
        PaneActivity::Busy => 1,
        PaneActivity::Attention => 2,
    }
}

/// Fold a window's per-pane activities into `(dominant, active_count)`: the
/// dominant state by precedence (attention > busy > free) and the number of
/// panes that are busy or attention. GPUI-free so the precedence and count are
/// unit-testable in isolation, mirroring the per-pane `ActivityTracker`
/// (`docs/spec-pane-activity-v2.md`).
fn aggregate_activity(activities: impl Iterator<Item = PaneActivity>) -> (PaneActivity, usize) {
    let mut dominant = PaneActivity::Free;
    let mut active_count = 0;
    for activity in activities {
        if matches!(activity, PaneActivity::Busy | PaneActivity::Attention) {
            active_count += 1;
        }
        if activity_rank(activity) > activity_rank(dominant) {
            dominant = activity;
        }
    }
    (dominant, active_count)
}

/// Which shape a window tab's fixed state slot renders for a dominant
/// [`PaneActivity`]. Busy and attention are deliberately distinct shapes and
/// sizes (a small success dot vs a danger "!"-badge) so a glance tells them
/// apart (`docs/spec-cockpit-chrome.md`); `Idle` draws nothing but the slot
/// reserves its width so the lane stays aligned across tabs. GPUI-free so the
/// mapping is unit-testable without an app context; the render layer supplies
/// the theme colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabStateSlot {
    Idle,
    Busy,
    Attention,
}

fn tab_state_slot(activity: PaneActivity) -> TabStateSlot {
    match activity {
        PaneActivity::Free => TabStateSlot::Idle,
        PaneActivity::Busy => TabStateSlot::Busy,
        PaneActivity::Attention => TabStateSlot::Attention,
    }
}

/// The tab type glyph for a window whose active pane is (or is not) at its
/// shell: a terminal-prompt glyph for a shell, a process glyph otherwise.
/// Agent-agnostic — the distinction rides tmux's own `is_shell` flag (#510),
/// never any command taxonomy or agent-name list.
fn type_glyph(is_shell: bool) -> IconName {
    if is_shell {
        IconName::SquareTerminal
    } else {
        IconName::Cpu
    }
}

/// Emitted for whole-client state that window-state persistence needs to
/// observe but that lives inside `SessionView` (#225,
/// `docs/spec-window-state-persistence.md`): today, only a font-size zoom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionViewEvent {
    FontSizeChanged { font_size_px: f32 },
}

impl EventEmitter<SessionViewEvent> for SessionView {}

pub struct SessionView {
    panes: HashMap<String, PaneEntry>,
    early_output_buffer: HashMap<String, Vec<Vec<u8>>>,
    windows: Vec<WindowState>,
    layout: Option<LayoutNode>,
    active_pane_id: Option<String>,
    input_tx: flume::Sender<PaneInput>,
    size_changed_tx: flume::Sender<TermSize>,
    tmux_command_tx: flume::Sender<String>,
    capture_request_tx: flume::Sender<CaptureRequest>,
    font_zoom_tx: flume::Sender<i32>,
    font_size: Pixels,
    border_drag: Option<BorderDrag>,
    renaming_window: Option<WindowRename>,
    focus_handle: FocusHandle,
    needs_focus: bool,
    /// The last grid size sent for the tmux client, derived from the pane
    /// area's measured element bounds — not the window viewport — so the
    /// client tracks the terminal panel inside the dock split (#424).
    client_grid_size: TermSize,
    /// While set and unexpired, the transient grid-size overlay shows near the
    /// terminal after a resize (`docs/spec-status-line.md`); `None` (or past the
    /// deadline) hides it. Set by the pane-area grid observer on a size change.
    resize_overlay_deadline: Option<Instant>,
    /// Bumped on every resize; the deadline-clearing one-shot timer only
    /// notifies if this still matches its captured value, so a stale timer
    /// from an earlier resize no-ops once a newer resize has re-armed the
    /// overlay (mirrors `Editor::hover_move_generation`'s debounce guard).
    resize_overlay_generation: u64,
    ssh_label: SharedString,
    session_name: SharedString,
    connection_status: ConnectionStatus,
    /// The mirrored tmux key-table lookup and prefix/repeat options, pushed
    /// down to every pane and refreshed in place via
    /// [`Self::apply_key_table_result`] (`docs/spec-tmux-keytable-mirroring.md`).
    /// Default (empty table, stock `C-b` prefix) until the first reply lands —
    /// the daemon issues that query unprompted on attach.
    key_table: Arc<KeyTable>,
    prefix_options: PrefixOptions,
    /// Requests a key-table refresh (forwarded to `TerminalHandle`'s
    /// `key_table_request_rx`), driven by the command palette's "Refresh tmux
    /// key tables" entry via [`Self::request_key_table_refresh`] (the escape
    /// hatch the keytable-mirroring spec mandates, relocated off the removed
    /// statusbar in `docs/spec-status-line.md`). A dispatched binding-mutating
    /// command's refresh is issued server-side instead, ordered after the
    /// mutation on the same seam (`spawn_command_bridge` in `crates/app`) — not
    /// carried on this channel.
    key_table_request_tx: flume::Sender<()>,
    /// The host's tmux session list (`docs/spec-session-management.md`),
    /// replaced wholesale by every `SessionListReply` (explicit refresh or
    /// the daemon's unprompted churn-driven push). The ACTUAL attached
    /// session is NOT read from here — `session_name` (fed by the layout
    /// stream) owns that. Rendered as the always-visible title-bar chip strip
    /// (#683), which replaced the phase-19 click-to-open popover.
    sessions: Vec<SessionListItem>,
    /// The strip's in-progress inline new-session prompt (trailing "+ New
    /// session..." chip), when active.
    new_session_prompt: Option<NewSessionPrompt>,
    /// The strip's in-progress inline session rename (#684), dispatched from
    /// a chip's right-click menu; when active, that chip renders the edit
    /// input in place of its name.
    renaming_session: Option<SessionRename>,
    /// The strip's in-progress inline kill confirmation (#685), armed from a
    /// chip's right-click menu's "Kill" item; when active, that chip renders
    /// a compact confirm affordance ("Kill?" + confirm/cancel) in place of
    /// its normal row. Two-step by design — see [`SessionKillConfirm`].
    confirming_kill: Option<SessionKillConfirm>,
    /// Requests an on-demand session-list refresh (forwarded to
    /// `TerminalHandle`'s `session_list_request_rx`); between requests the
    /// daemon's churn-driven pushes keep the strip live.
    session_list_request_tx: flume::Sender<()>,
    /// Emits a cockpit switch (forwarded to `TerminalHandle`'s
    /// `session_switch_rx`) when a strip chip or the new-session prompt
    /// commits.
    session_switch_tx: flume::Sender<SessionSwitchRequest>,
    /// Emits a session-order mutation (forwarded to `TerminalHandle`'s
    /// `session_order_rx`) on a drag-to-reorder drop or a rename commit
    /// (#686, `docs/spec-session-management.md`).
    session_order_tx: flume::Sender<SessionOrderUpdate>,
    /// Emits the reconnect banner's Cancel to the SSH-level reconnect engine
    /// (forwarded to `TerminalHandle`'s `reconnect_cancel_rx`).
    reconnect_cancel_tx: flume::Sender<()>,
}

impl SessionView {
    pub fn new(cx: &mut Context<Self>) -> (Self, TerminalHandle) {
        let (pane_output_tx, pane_output_rx) = flume::unbounded::<PaneOutput>();
        let (input_tx, input_rx) = flume::unbounded::<PaneInput>();
        let (size_changed_tx, size_changed_rx) = flume::unbounded();
        let (snapshot_tx, snapshot_rx) = flume::unbounded::<SessionSnapshot>();
        let (tmux_command_tx, tmux_command_rx) = flume::unbounded::<String>();
        let (subscription_tx, subscription_rx) = flume::unbounded::<SubscriptionUpdate>();
        let (capture_request_tx, capture_request_rx) = flume::unbounded::<CaptureRequest>();
        let (capture_result_tx, capture_result_rx) = flume::unbounded::<CaptureResult>();
        let (connection_status_tx, connection_status_rx) = flume::unbounded::<ConnectionStatus>();
        let (font_zoom_tx, font_zoom_rx) = flume::unbounded::<i32>();
        let (key_table_request_tx, key_table_request_rx) = flume::unbounded::<()>();
        let (key_table_result_tx, key_table_result_rx) = flume::unbounded::<KeyTableQueryResult>();
        let (session_list_tx, session_list_rx) = flume::unbounded::<Vec<SessionListItem>>();
        let (session_list_request_tx, session_list_request_rx) = flume::unbounded::<()>();
        let (session_switch_tx, session_switch_rx) = flume::unbounded::<SessionSwitchRequest>();
        let (session_order_tx, session_order_rx) = flume::unbounded::<SessionOrderUpdate>();
        let (reconnect_cancel_tx, reconnect_cancel_rx) = flume::unbounded::<()>();

        {
            cx.spawn(async move |this, cx| loop {
                let Ok(output) = pane_output_rx.recv_async().await else {
                    break;
                };
                let result = cx.update(|cx| {
                    this.update(cx, |view, _cx| {
                        if let Some(entry) = view.panes.get(&output.pane_id) {
                            let _ = entry.pty_tx.send(output.bytes);
                        } else {
                            view.early_output_buffer
                                .entry(output.pane_id)
                                .or_default()
                                .push(output.bytes);
                        }
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            cx.spawn(async move |this, cx| loop {
                let Ok(snapshot) = snapshot_rx.recv_async().await else {
                    break;
                };
                let result = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        view.apply_snapshot(snapshot, cx);
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            // Phase 2d: format-subscription updates stream pane/window state
            // changes (cd, command, rename) end-to-end into the view layer.
            cx.spawn(async move |this, cx| loop {
                let Ok(update) = subscription_rx.recv_async().await else {
                    break;
                };
                let result = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        view.apply_subscription(update, cx);
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            cx.spawn(async move |this, cx| loop {
                let Ok(result) = capture_result_rx.recv_async().await else {
                    break;
                };
                let update = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        if let Some(entry) = view.panes.get(&result.pane_id) {
                            entry.entity.update(cx, |pane, cx| {
                                pane.apply_history(result.bytes, cx);
                            });
                        }
                    })
                });
                if update.is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            // SSH/tmux lifecycle drives the statusbar connection indicator
            // (event-driven, never polled). `Disconnected` is a visible
            // not-connected end state — an orderly tmux exit, a canceled
            // reconnect, or a non-retryable connect failure — never an app
            // quit: the SSH-level reconnect engine owns transport drops
            // (#476), and the Connection screen (#477) will own this state
            // once it lands.
            cx.spawn(async move |this, cx| loop {
                let Ok(status) = connection_status_rx.recv_async().await else {
                    break;
                };
                let result = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        if view.connection_status != status {
                            view.connection_status = status;
                            cx.notify();
                        }
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            cx.spawn(async move |this, cx| loop {
                let Ok(delta) = font_zoom_rx.recv_async().await else {
                    break;
                };
                let result = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        view.apply_font_zoom(delta, cx);
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            cx.spawn(async move |this, cx| loop {
                let Ok(result) = key_table_result_rx.recv_async().await else {
                    break;
                };
                let updated = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        view.apply_key_table_result(result, cx);
                    })
                });
                if updated.is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            // Session-list stream (`docs/spec-session-switch.md`): every reply
            // — the on-demand refresh or the daemon's unprompted churn-driven
            // push — replaces the switcher's whole list, so create/kill/rename
            // reflect without any manual refresh.
            cx.spawn(async move |this, cx| loop {
                let Ok(sessions) = session_list_rx.recv_async().await else {
                    break;
                };
                let updated = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        view.apply_session_list(sessions, cx);
                    })
                });
                if updated.is_err() {
                    break;
                }
            })
            .detach();
        }

        // Placeholder until the caller feeds the resolved host/user of the
        // actual connection via `set_ssh_label` (#494) — this view has no
        // connection of its own to resolve a default from, so it must not
        // guess one independently (that guess previously diverged from the
        // real `SshConfig` the app connects with: `localhost` vs
        // `127.0.0.1`, missing user vs `developer`).
        let ssh_label: SharedString = SharedString::default();

        let view = Self {
            panes: HashMap::new(),
            early_output_buffer: HashMap::new(),
            windows: Vec::new(),
            layout: None,
            active_pane_id: None,
            input_tx: input_tx.clone(),
            size_changed_tx: size_changed_tx.clone(),
            tmux_command_tx,
            capture_request_tx,
            font_zoom_tx,
            font_size: px(DEFAULT_FONT_SIZE),
            border_drag: None,
            renaming_window: None,
            focus_handle: cx.focus_handle(),
            needs_focus: true,
            client_grid_size: TermSize { cols: 80, rows: 24 },
            resize_overlay_deadline: None,
            resize_overlay_generation: 0,
            ssh_label,
            session_name: SharedString::default(),
            connection_status: ConnectionStatus::Connecting,
            key_table: Arc::new(KeyTable::default()),
            prefix_options: PrefixOptions::default(),
            key_table_request_tx,
            sessions: Vec::new(),
            new_session_prompt: None,
            renaming_session: None,
            confirming_kill: None,
            session_list_request_tx,
            session_switch_tx,
            session_order_tx,
            reconnect_cancel_tx,
        };

        let handle = TerminalHandle {
            pane_output_tx,
            input_rx,
            size_changed_rx,
            snapshot_tx,
            tmux_command_rx,
            subscription_tx,
            capture_request_rx,
            capture_result_tx,
            connection_status_tx,
            key_table_request_rx,
            key_table_result_tx,
            session_list_tx,
            session_list_request_rx,
            session_switch_rx,
            session_order_rx,
            reconnect_cancel_rx,
        };

        (view, handle)
    }

    /// Apply a font-zoom delta to the whole client. Font size is a single
    /// client render property: changing it shifts the cell metrics, so the next
    /// `render` recomputes cols/rows and pushes the new client size to tmux,
    /// which reflows every pane.
    fn apply_font_zoom(&mut self, delta: i32, cx: &mut Context<Self>) {
        let new_size = px((f32::from(self.font_size) + delta as f32 * FONT_SIZE_STEP)
            .clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
        self.apply_font_size(new_size, cx);
    }

    /// The whole-client font size (`docs/spec-window-state-persistence.md`'s
    /// "font scale"), read by the settings surface (#366) to seed its field.
    pub fn font_size(&self) -> Pixels {
        self.font_size
    }

    /// Set the whole-client font size directly, clamped to the same
    /// [`MIN_FONT_SIZE`]/[`MAX_FONT_SIZE`] bounds as the `Ctrl+=`/`Ctrl+-` zoom
    /// path. An absolute counterpart to [`Self::apply_font_zoom`]'s relative
    /// delta, for the settings surface's font-scale field (#366).
    pub fn set_font_size(&mut self, size: Pixels, cx: &mut Context<Self>) {
        let clamped = px(f32::from(size).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
        self.apply_font_size(clamped, cx);
    }

    /// Feed the statusbar host label from the resolved host/user the caller
    /// actually connects with, so it can never diverge from the real
    /// connection (#494). The label is display-only: callers format it
    /// (typically `user@host`), this just stores it.
    pub fn set_ssh_label(&mut self, label: impl Into<SharedString>) {
        self.ssh_label = label.into();
    }

    /// The `user@host` label, read by the title bar's connection group
    /// (#511, `docs/spec-cockpit-chrome.md`) — the same value this view's own
    /// statusbar renders.
    pub fn ssh_label(&self) -> &str {
        &self.ssh_label
    }

    /// The attached tmux session name (fed by the layout stream), read by the
    /// title bar's connection group (#511).
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    /// The SSH/tmux connection lifecycle state, read by the title bar's
    /// connection group (#511) to render the same status dot this view's own
    /// statusbar shows.
    pub fn connection_status(&self) -> ConnectionStatus {
        self.connection_status
    }

    /// Apply an absolute font size, notifying every live pane and arming the
    /// debounced window-state capture (#225) — the shared choke point for
    /// both the `Ctrl+=`/`Ctrl+-` zoom and the settings surface's font-scale
    /// field (#366), so either path emits [`SessionViewEvent::FontSizeChanged`]
    /// exactly once.
    fn apply_font_size(&mut self, new_size: Pixels, cx: &mut Context<Self>) {
        if new_size == self.font_size {
            return;
        }
        self.font_size = new_size;
        for entry in self.panes.values() {
            entry.entity.update(cx, |pane, cx| {
                pane.set_font_size(new_size);
                cx.notify();
            });
        }
        cx.emit(SessionViewEvent::FontSizeChanged {
            font_size_px: f32::from(new_size),
        });
        cx.notify();
    }

    /// Recompute the tmux client grid from the terminal pane-area `area` and
    /// `cell` metrics and, only when the whole-cell grid actually changes, cache
    /// it, push it onto the resize seam (`size_changed_tx` -> the daemon's
    /// `refresh-client -C`), and arm the transient grid-size overlay. Returns
    /// whether a resize was emitted, so the caller can `cx.notify()` exactly
    /// once.
    ///
    /// The stacked pane headers are reserved before flooring rows so a vertical
    /// split never clips its bottom rows (#424): the worst-case column loses one
    /// header per pane stacked in it. Deduped against `client_grid_size`, so a
    /// stable area (or a sub-cell wobble that floors to the same grid) sends
    /// nothing — a dock-layout change that leaves the grid unchanged never spams
    /// the seam (#596).
    fn resize_client_to_area(
        &mut self,
        area: Size<Pixels>,
        cell: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let header_rows = self
            .layout
            .as_ref()
            .map(max_vertical_pane_count)
            .unwrap_or(1);
        let reserved = px(PANE_HEADER_HEIGHT * header_rows as f32);
        let grid = grid_size_for(area, cell, reserved);
        if grid == self.client_grid_size {
            return false;
        }
        self.client_grid_size = grid;
        let _ = self.size_changed_tx.try_send(grid);
        self.resize_overlay_deadline = Some(Instant::now() + RESIZE_OVERLAY_DURATION);
        self.resize_overlay_generation = self.resize_overlay_generation.wrapping_add(1);
        let generation = self.resize_overlay_generation;
        cx.spawn(async move |this, cx| {
            smol::Timer::after(RESIZE_OVERLAY_DURATION).await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| {
                    if view.resize_overlay_generation == generation {
                        cx.notify();
                    }
                });
            });
        })
        .detach();
        true
    }

    /// Apply a refreshed `list-keys`/`show-options` reply: re-parse into the
    /// mirrored `KeyTable`/`PrefixOptions` and push the result down to every
    /// live pane (`apply_snapshot` only seeds new panes at creation — an
    /// already-open pane needs this to pick up a later refresh).
    fn apply_key_table_result(&mut self, result: KeyTableQueryResult, cx: &mut Context<Self>) {
        let key_table = Arc::new(keytable::parse_list_keys(&result.list_keys));
        let prefix_options = keytable::parse_options(&result.options);
        self.key_table = key_table.clone();
        self.prefix_options = prefix_options.clone();
        for entry in self.panes.values() {
            entry.entity.update(cx, |pane, cx| {
                pane.set_key_table(key_table.clone(), prefix_options.clone());
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Replace the strip's session list wholesale (replace semantics, like
    /// the layout stream) — every `SessionListReply` arrival lands here.
    fn apply_session_list(&mut self, sessions: Vec<SessionListItem>, cx: &mut Context<Self>) {
        if self.sessions != sessions {
            self.sessions = sessions;
            cx.notify();
        }
    }

    /// Request an on-demand session-list refresh — the target of the command
    /// palette's "Switch Session..." entry, routed through the workspace.
    /// #683 replaced the phase-19 click-to-open popover with the
    /// always-visible title-bar strip ([`Self::render_session_strip`]), which
    /// stays live via the daemon's own churn-driven `SessionListReply` push;
    /// this remains as a manual nudge (e.g. a stale first paint before the
    /// initial reply lands) rather than an open/close toggle.
    pub fn open_session_switcher(&self) {
        let _ = self.session_list_request_tx.try_send(());
    }

    /// Activate the strip's inline new-session prompt — the command palette's
    /// "New Session..." entry, routed through the workspace.
    pub fn open_new_session_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_new_session_prompt(window, cx);
    }

    /// Switch the cockpit to `session`: emit the switch request (the app seam
    /// re-sends `Attach { session }` — attach-or-create, so a fresh name
    /// creates the session). The indicator and the terminal model reset on
    /// the fresh `LayoutSnapshot`, never optimistically here. Switching to
    /// the already-attached session is a no-op — the strip stays visible
    /// either way, unlike the removed popover, which this used to close.
    fn switch_to_session(&mut self, session: &str, cx: &mut Context<Self>) {
        if session != self.session_name.as_ref() {
            if let Err(e) = self.session_switch_tx.try_send(SessionSwitchRequest {
                session: session.to_string(),
                size: self.client_grid_size,
            }) {
                debug!(error = %e, %session, "failed to send session switch request");
            }
        }
        cx.notify();
    }

    /// Commit a drag-to-reorder drop (#686, `docs/spec-session-management.md`,
    /// Prior decisions: "drag-to-order, a total user-set order"): take
    /// `dragged`'s current slot out of the live session list and reinsert it
    /// immediately before `target`, then emit the WHOLE resulting name
    /// sequence as one [`SessionOrderUpdate::Reorder`] — never a partial
    /// two-row patch. `rift-app`'s session-order store persists it and
    /// re-pushes the re-sorted list on the next `SessionListReply`-driven
    /// push, so the strip is never mutated optimistically here. A drop onto
    /// itself, or a name no longer in the live list (the daemon's churn push
    /// raced the drag), is a no-op.
    fn reorder_sessions(&mut self, dragged: &str, target: &str, cx: &mut Context<Self>) {
        if dragged == target {
            return;
        }
        let mut names: Vec<String> = self.sessions.iter().map(|s| s.name.clone()).collect();
        let Some(from) = names.iter().position(|n| n == dragged) else {
            return;
        };
        let dragged_name = names.remove(from);
        let to = names
            .iter()
            .position(|n| n == target)
            .unwrap_or(names.len());
        names.insert(to, dragged_name);
        if let Err(e) = self
            .session_order_tx
            .try_send(SessionOrderUpdate::Reorder(names))
        {
            debug!(error = %e, "failed to send session reorder update");
        }
        cx.notify();
    }

    /// Activate the strip's trailing new-session prompt: seed an empty input,
    /// focus it, and subscribe for submit/blur (mirroring the window-rename
    /// prompt). Enter with a non-empty name switches to it (attach-or-create);
    /// blur cancels.
    fn start_new_session_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("session name"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, _input, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_new_session_prompt(cx),
                InputEvent::Blur => this.cancel_new_session_prompt(cx),
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.new_session_prompt = Some(NewSessionPrompt {
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Commit the new-session prompt (Enter): a non-empty trimmed name switches
    /// to it — the daemon child command is attach-or-create (`new-session -A`),
    /// so a fresh name creates the session and a duplicate of an existing one
    /// simply attaches (issue #467). An empty name sends nothing and dismisses
    /// the prompt back to the trailing "+ New session..." chip. A second
    /// submit after the prompt already committed (or was cancelled) must not
    /// re-submit.
    fn submit_new_session_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.new_session_prompt.take() else {
            return;
        };
        let value = prompt.input.read(cx).value();
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            self.switch_to_session(&trimmed, cx);
        }
        cx.notify();
    }

    /// Cancel an in-progress new-session prompt without emitting anything,
    /// restoring the trailing "+ New session..." chip.
    fn cancel_new_session_prompt(&mut self, cx: &mut Context<Self>) {
        if self.new_session_prompt.is_some() {
            self.new_session_prompt = None;
            cx.notify();
        }
    }

    fn apply_snapshot(&mut self, session_snapshot: SessionSnapshot, cx: &mut Context<Self>) {
        use std::collections::HashSet;

        let SessionSnapshot {
            snapshot,
            pane_is_shell,
        } = session_snapshot;

        let snapshot_pane_ids: HashSet<&str> = snapshot
            .windows
            .iter()
            .flat_map(|w| w.panes.iter().map(|p| p.id.as_str()))
            .collect();

        self.panes
            .retain(|id, _| snapshot_pane_ids.contains(id.as_str()));

        let mut new_windows = Vec::with_capacity(snapshot.windows.len());
        let mut active_pane_id = None;

        for window in &snapshot.windows {
            let pane_ids: Vec<String> = window.panes.iter().map(|p| p.id.clone()).collect();
            let is_active_window = window.is_active;

            if is_active_window {
                active_pane_id = window.active_pane_id.clone();
            }

            for pane_state in &window.panes {
                // Each pane's own shell flag (not just the window's active pane)
                // feeds its header type glyph; absent from the map -> non-shell.
                let pane_shell = pane_is_shell.get(&pane_state.id).copied().unwrap_or(false);
                // The uncollapsed flag (kept as `Option`) is the authoritative
                // busy/free signal fed to the pane's activity tracker: `None` on
                // the legacy path must not read as `false` (busy)
                // (`docs/spec-pane-activity-v2.md`).
                let foreground_shell = pane_is_shell.get(&pane_state.id).copied();
                if self.panes.contains_key(&pane_state.id) {
                    if let Some(entry) = self.panes.get_mut(&pane_state.id) {
                        entry.is_shell = pane_shell;
                        entry.entity.update(cx, |pv, cx| {
                            pv.set_tmux_size(pane_state.width, pane_state.height);
                            // Push the window-active flag into the pane's tracker
                            // on every snapshot (not only on the is_active edge —
                            // a continuously active window has no edge): while
                            // active a bell never raises attention, and activation
                            // acknowledges pending attention, so tab clicks,
                            // Alt+1..9, and tmux-side selects clear it uniformly
                            // (`docs/spec-pane-activity-v2.md`).
                            pv.set_window_active(is_active_window);
                            // The tmux foreground-process flag (#510) drives the
                            // pane's busy/free indicator, pushed on every snapshot
                            // so a process start/exit flips it promptly
                            // (`docs/spec-pane-activity-v2.md`).
                            pv.set_foreground_shell(foreground_shell);
                            // CWD is subscription-driven (rift_pane_path); the
                            // snapshot seeds it only at pane creation below.
                            cx.notify();
                        });
                    }
                } else {
                    let (pty_tx, pty_rx) = flume::unbounded::<Vec<u8>>();
                    let input_tx = self.input_tx.clone();
                    let size_changed_tx = self.size_changed_tx.clone();
                    let capture_request_tx = self.capture_request_tx.clone();
                    let font_zoom_tx = self.font_zoom_tx.clone();
                    let tmux_command_tx = self.tmux_command_tx.clone();
                    let key_table = self.key_table.clone();
                    let prefix_options = self.prefix_options.clone();
                    let pane_id = pane_state.id.clone();
                    let font_size = self.font_size;

                    let entity = cx.new(|pane_cx| {
                        let mut pv = PaneView::new(
                            pane_cx,
                            pty_rx,
                            input_tx,
                            size_changed_tx,
                            capture_request_tx,
                            font_zoom_tx,
                            tmux_command_tx,
                            key_table,
                            prefix_options,
                        );
                        pv.set_pane_id(pane_id.clone());
                        pv.set_font_size(font_size);
                        pv.set_tmux_size(pane_state.width, pane_state.height);
                        if !pane_state.current_path.is_empty() {
                            pv.set_working_directory(pane_state.current_path.clone());
                        }
                        if !pane_state.current_command.is_empty() {
                            pv.set_current_command(pane_state.current_command.clone());
                        }
                        pv.set_window_active(is_active_window);
                        pv.set_foreground_shell(foreground_shell);
                        pv
                    });

                    // Observe the pane so its own OSC-133/bell `cx.notify()` (which
                    // re-renders only its own subtree) also re-renders this parent,
                    // keeping a background window's tab aggregate live. The handle
                    // lives in the entry and drops when the pane leaves the snapshot
                    // (`docs/spec-pane-activity-v2.md`).
                    let activity_subscription = cx.observe(&entity, |_this, _pane, cx| cx.notify());

                    if let Some(buffered) = self.early_output_buffer.remove(&pane_id) {
                        for bytes in buffered {
                            let _ = pty_tx.send(bytes);
                        }
                    }

                    debug!(pane_id = %pane_id, "created pane");
                    self.panes.insert(
                        pane_id,
                        PaneEntry {
                            entity,
                            pty_tx,
                            is_shell: pane_shell,
                            _activity_subscription: activity_subscription,
                        },
                    );
                    self.needs_focus = true;
                }
            }

            // The window's type glyph tracks its active pane's shell state; a
            // pane missing from the map (legacy path, or no active pane) reads
            // as non-shell. The flag refreshes on the layout-snapshot cadence,
            // so the glyph flips when a process starts or exits in that pane.
            let is_shell = window
                .active_pane_id
                .as_ref()
                .and_then(|id| pane_is_shell.get(id))
                .copied()
                .unwrap_or(false);

            new_windows.push(WindowState {
                id: window.id.clone(),
                name: window.name.clone(),
                index: window.index,
                is_active: window.is_active,
                pane_ids,
                is_shell,
            });
        }

        if !snapshot.session_name.is_empty() {
            self.session_name = SharedString::from(snapshot.session_name.clone());
        }

        self.windows = new_windows;
        if active_pane_id != self.active_pane_id {
            self.needs_focus = true;
        }
        self.active_pane_id = active_pane_id;

        self.layout = snapshot
            .windows
            .iter()
            .find(|w| w.is_active)
            .map(|w| layout::build_layout(&w.panes));

        cx.notify();
    }

    /// The folded activity of the window `window_id`: its dominant pane state
    /// (attention > busy > free) and the count of busy-or-attention panes, for
    /// the tab-bar indicator rendered by a later step. Read live at render so an
    /// observed pane transition reflects immediately. The active window never
    /// surfaces attention: its panes' trackers suppress bell raises while the
    /// window is active, and any pane whose flag is momentarily stale is read as
    /// its underlying busy/free (`docs/spec-pane-activity-v2.md`).
    pub fn window_activity(&self, window_id: &str, cx: &App) -> (PaneActivity, usize) {
        let Some(window) = self.windows.iter().find(|w| w.id == window_id) else {
            return (PaneActivity::Free, 0);
        };
        let suppress_attention = window.is_active;
        let activities = window.pane_ids.iter().filter_map(|id| {
            self.panes.get(id).map(|entry| {
                let pane = entry.entity.read(cx);
                if suppress_attention {
                    pane.underlying_activity()
                } else {
                    pane.activity()
                }
            })
        });
        aggregate_activity(activities)
    }

    /// The window list the app's composite status line renders
    /// (`docs/spec-status-line.md`): one [`StatusWindow`] per window in tab
    /// order, each carrying its `index:name`, active flag, and folded pane
    /// activity (the dominant of its panes). Read live so create/close/rename/
    /// select/activity reflect on the next render without a refresh.
    pub fn status_windows(&self, cx: &App) -> Vec<StatusWindow> {
        self.windows
            .iter()
            .map(|w| StatusWindow {
                id: w.id.clone(),
                index: w.index,
                name: w.name.clone(),
                is_active: w.is_active,
                activity: self.window_activity(&w.id, cx).0,
            })
            .collect()
    }

    /// Whether the focused pane is mid-chord after the tmux prefix, for the
    /// composite status line's transient PREFIX indicator. Reads the active
    /// pane's own prefix state (`crate::prefix`); `false` when no pane is
    /// active or the chord has been dispatched/cancelled.
    pub fn prefix_pending(&self, cx: &App) -> bool {
        self.active_pane_id
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .is_some_and(|entry| entry.entity.read(cx).prefix_pending())
    }

    /// Select `window_id` through the existing tmux command channel
    /// (`select-window`), acknowledging its bell attention locally so the badge
    /// clears without waiting for the confirming snapshot — the same path the
    /// tab-bar click and `Alt+1..9` take. Called by the composite status line's
    /// window-list click (`docs/spec-status-line.md`).
    pub fn select_window(&self, window_id: &str, cx: &mut Context<Self>) {
        if let Err(e) = self
            .tmux_command_tx
            .try_send(format!("select-window -t {window_id}"))
        {
            debug!(error = %e, "failed to send window switch command");
        }
        self.acknowledge_window_attention(window_id, cx);
    }

    /// Open a fresh tmux window rooted at `dir` through the existing tmux
    /// command channel (`new-window -c <dir>`) — the explorer row context
    /// menu's "Reveal in terminal" action (`docs/spec-explorer-context-menu.md`),
    /// routed here by `workspace.rs`. Structural only, mirroring the shipped
    /// pane-header split / new-window controls: it never `send-keys` into an
    /// existing pane and never reads a pane's process, so it neither disturbs
    /// a running agent nor detects one.
    pub fn open_terminal_at(&self, dir: &str) {
        if let Err(e) = self.tmux_command_tx.try_send(new_window_at_command(dir)) {
            debug!(error = %e, %dir, "failed to send reveal-in-terminal command");
        }
    }

    /// Request a manual key-table refresh (the escape hatch the
    /// keytable-mirroring spec mandates), driven by the command palette's
    /// "Refresh tmux key tables" entry (`docs/spec-status-line.md`). Forwards on
    /// the same `key_table_request_tx` the removed statusbar affordance used.
    pub fn request_key_table_refresh(&self) {
        if let Err(e) = self.key_table_request_tx.try_send(()) {
            debug!(error = %e, "failed to send key-table refresh request");
        }
    }

    /// Acknowledge bell attention on every pane of `window_id`, immediately.
    /// Called from the local window-select paths (tab click, Alt+1..9) so the
    /// attention badge clears without waiting for the confirming snapshot's
    /// round trip through tmux; the snapshot then re-asserts the same state via
    /// `set_window_active` (`docs/spec-pane-activity-v2.md`).
    fn acknowledge_window_attention(&self, window_id: &str, cx: &mut Context<Self>) {
        let Some(window) = self.windows.iter().find(|w| w.id == window_id) else {
            return;
        };
        for pane_id in &window.pane_ids {
            if let Some(entry) = self.panes.get(pane_id) {
                entry.entity.update(cx, |pv, cx| {
                    pv.acknowledge_attention();
                    cx.notify();
                });
            }
        }
    }

    fn apply_subscription(&mut self, update: SubscriptionUpdate, cx: &mut Context<Self>) {
        match update.name.as_str() {
            // `rift_pane_path` (`#{pane_current_path}`, scope `%*`): live CWD per
            // pane. Drives the statusbar within ~1s of `cd`; the snapshot only
            // seeds initial state at pane creation.
            "rift_pane_path" => {
                if let Some(entry) = self.panes.get(&update.pane) {
                    entry.entity.update(cx, |pv, cx| {
                        pv.set_working_directory(update.value);
                        cx.notify();
                    });
                    cx.notify();
                }
            }
            // `rift_pane_command` (`#{pane_current_command}`, scope `%*`): the
            // foreground command per pane. Same live-driver pattern as the CWD;
            // the snapshot only seeds it at pane creation.
            "rift_pane_command" => {
                if let Some(entry) = self.panes.get(&update.pane) {
                    entry.entity.update(cx, |pv, cx| {
                        pv.set_current_command(update.value);
                        cx.notify();
                    });
                    cx.notify();
                }
            }
            // `rift_window_name` (`#{window_name}`, scope `@*`): live window
            // title per window. Updates the tab label within ~1s of
            // `rename-window`; the snapshot seeds it otherwise.
            "rift_window_name" => {
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == update.window) {
                    if win.name != update.value {
                        win.name = update.value;
                        cx.notify();
                    }
                }
            }
            other => {
                debug!(name = %other, "unhandled tmux subscription");
            }
        }
    }

    /// Begin an inline rename of `window_id`: seed a text input with the current
    /// window name, focus it, and subscribe for submit/blur. Enter emits
    /// `rename-window`; blur cancels. The snapshot remains the source of truth
    /// for the resulting name.
    fn start_window_rename(
        &mut self,
        window_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .windows
            .iter()
            .find(|w| w.id == window_id)
            .map(|w| w.name.clone())
            .unwrap_or_default();

        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    if let Some(rename) = this.renaming_window.take() {
                        let value = input.read(cx).value();
                        let trimmed = value.trim();
                        if !trimmed.is_empty() {
                            if let Err(e) = this.tmux_command_tx.try_send(format!(
                                "rename-window -t {} {}",
                                rename.window_id,
                                quote_tmux_name(trimmed)
                            )) {
                                debug!(error = %e, "failed to send window rename command");
                            }
                        }
                        this.needs_focus = true;
                        cx.notify();
                    }
                }
                InputEvent::Blur => this.cancel_window_rename(cx),
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.renaming_window = Some(WindowRename {
            window_id: window_id.to_string(),
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Cancel an in-progress rename without emitting a command, restoring pane
    /// focus on the next render.
    fn cancel_window_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming_window.is_some() {
            self.renaming_window = None;
            self.needs_focus = true;
            cx.notify();
        }
    }

    /// Begin an inline rename of the session chip `id` (current name
    /// `current_name`), dispatched by the chip's right-click menu (#684,
    /// `docs/spec-session-management.md`): seed a text input with the
    /// current name, focus it, and subscribe for submit/blur — mirrors
    /// [`Self::start_window_rename`]. Enter commits (see
    /// [`Self::submit_session_rename`]); Escape/blur cancels. The strip
    /// remains the source of truth for the resulting name (via the next
    /// `SessionListReply`), never an optimistic local mutation.
    fn start_session_rename(
        &mut self,
        id: u32,
        current_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current_name.clone()));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, _input, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_session_rename(cx),
                InputEvent::Blur => this.cancel_session_rename(cx),
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.renaming_session = Some(SessionRename {
            session_id: id,
            original_name: current_name,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Commit the in-progress session rename (Enter): an empty trimmed name,
    /// or one unchanged from the original, is a no-op (the spec's "empty or
    /// unchanged name is a no-op"); otherwise sends the quoted
    /// `rename-session` over the existing raw tmux-command seam
    /// (`tmux_command_tx`, the same channel the pane-header split/zoom/
    /// select-pane controls use), and a matching [`SessionOrderUpdate::Rename`]
    /// on `session_order_tx` so the order store renames its key in the SAME
    /// action, preserving this session's reordered slot (#686,
    /// `docs/spec-session-management.md`; only an external CLI rename
    /// re-slots). A second submit after the rename already committed (or was
    /// cancelled) must not re-submit.
    fn submit_session_rename(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.renaming_session.take() else {
            return;
        };
        let value = rename.input.read(cx).value();
        let trimmed = value.trim();
        if !trimmed.is_empty() && trimmed != rename.original_name {
            if let Err(e) = self
                .tmux_command_tx
                .try_send(rename_session_command(rename.session_id, trimmed))
            {
                debug!(error = %e, "failed to send session rename command");
            }
            if let Err(e) = self.session_order_tx.try_send(SessionOrderUpdate::Rename {
                old: rename.original_name,
                new: trimmed.to_string(),
            }) {
                debug!(error = %e, "failed to send session order rename update");
            }
        }
        // The inline input unmounts now; return keyboard focus to the pane
        // (mirrors window rename) so the terminal is not left keyboard-dead.
        self.needs_focus = true;
        cx.notify();
    }

    /// Cancel an in-progress session rename without emitting a command.
    fn cancel_session_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming_session.is_some() {
            self.renaming_session = None;
            self.needs_focus = true;
            cx.notify();
        }
    }

    /// Arm the kill confirmation for chip `id` (name `name`), dispatched by
    /// the chip's right-click menu's "Kill" item (#685,
    /// `docs/spec-session-management.md`): two-step by design, so a stray
    /// click can never kill a session outright. No command is sent here —
    /// only [`Self::confirm_session_kill`] sends one. Moves keyboard focus
    /// onto a fresh handle for the confirm row (#686 drive-by fix): without
    /// this, nothing in the confirm row ever had focus, so its own
    /// `on_key_down` Escape handler (see [`Self::render_session_strip`]) never
    /// fired — Escape kept reaching whatever the terminal pane was doing
    /// instead. Click-cancel ([`Self::cancel_session_kill`]) is unaffected —
    /// it never depended on focus.
    fn start_session_kill_confirm(
        &mut self,
        id: u32,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        self.confirming_kill = Some(SessionKillConfirm {
            session_id: id,
            session_name: name,
            focus_handle,
        });
        cx.notify();
    }

    /// Commit an armed kill confirmation: send `kill-session -t $<id>` over
    /// the existing raw tmux-command seam (`tmux_command_tx`, the same
    /// channel rename and the pane-header split/zoom/select-pane controls
    /// use). The strip drops the session live via the daemon's
    /// `%sessions-changed` churn push (`SessionListReply`); killing the
    /// ATTACHED session surfaces the existing `TerminalExit` path — no new
    /// teardown here. A second confirm after the kill already committed (or
    /// was cancelled) must not re-send.
    fn confirm_session_kill(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.confirming_kill.take() else {
            return;
        };
        if let Err(e) = self
            .tmux_command_tx
            .try_send(kill_session_command(confirm.session_id))
        {
            debug!(error = %e, "failed to send session kill command");
        }
        // The inline confirm unmounts now; return keyboard focus to the pane
        // (mirrors session/window rename) so the terminal is not left
        // keyboard-dead.
        self.needs_focus = true;
        cx.notify();
    }

    /// Cancel an armed kill confirmation without emitting a command
    /// (Escape or the cancel control).
    fn cancel_session_kill(&mut self, cx: &mut Context<Self>) {
        if self.confirming_kill.is_some() {
            self.confirming_kill = None;
            self.needs_focus = true;
            cx.notify();
        }
    }

    fn render_layout(
        &self,
        node: &LayoutNode,
        border_color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            LayoutNode::Pane(id) => match self.panes.get(id) {
                Some(entry) => {
                    let pane = entry.entity.clone();
                    let header = self.render_pane_header(id, cx);
                    v_flex()
                        .size_full()
                        .child(header)
                        .child(div().flex_1().min_h_0().child(pane))
                        .into_any_element()
                }
                None => div().flex_grow().into_any_element(),
            },
            LayoutNode::Split {
                horizontal,
                children,
            } => {
                let horizontal = *horizontal;
                let mut container = div().flex().size_full();
                container = if horizontal {
                    container.flex_row()
                } else {
                    container.flex_col()
                };
                let last = children.len().saturating_sub(1);
                for (i, (proportion, child)) in children.iter().enumerate() {
                    let inner = self.render_layout(child, border_color, cx);
                    let wrapper = div()
                        .flex_1()
                        .flex_basis(relative(*proportion))
                        .size_full()
                        .child(inner);
                    container = container.child(wrapper);
                    if i < last {
                        // The seam before this border resizes the leading child;
                        // target a representative pane inside it.
                        let target = layout::first_pane_id(child).map(str::to_string);
                        container = container.child(self.resize_handle(
                            horizontal,
                            border_color,
                            target,
                            cx,
                        ));
                    }
                }
                container.into_any_element()
            }
        }
    }

    /// A draggable seam between two split children. The 7px hit area wraps a
    /// centered 1px line; mouse-down records the drag so the root element's move
    /// handler can emit incremental `resize-pane` commands. The cursor stays the
    /// default arrow (no resize cursor).
    fn resize_handle(
        &self,
        horizontal: bool,
        border_color: Hsla,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let line = if horizontal {
            div().w(px(1.0)).h_full().bg(border_color)
        } else {
            div().h(px(1.0)).w_full().bg(border_color)
        };
        let mut handle = div().flex().items_center().justify_center().flex_none();
        handle = if horizontal {
            handle.w(px(7.0)).h_full()
        } else {
            handle.h(px(7.0)).w_full()
        };
        handle = handle.child(line);

        if let Some(target) = target {
            handle = handle.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let start = if horizontal {
                        event.position.x
                    } else {
                        event.position.y
                    };
                    this.border_drag = Some(BorderDrag {
                        target_pane: target.clone(),
                        horizontal,
                        start,
                        emitted_cells: 0,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        }

        handle.into_any_element()
    }

    /// The always-visible session strip (#683, `docs/spec-session-management.md`),
    /// anchored to the title bar's connection group (#512,
    /// `docs/spec-cockpit-chrome.md`) — `app::workspace::WorkspaceView` embeds
    /// this via `title_bar::ConnectionGroup::connected`, so the strip lives
    /// beside the plain `user@host` label rather than the statusbar, which
    /// shows a plain (non-interactive) session name. REPLACES the phase-19
    /// click-to-open popover: every host session renders as a chip (mono
    /// name, a fixed attached-dot lane) with no open/close step; the current
    /// session's chip keeps the 2px primary left bar on a surface
    /// background — the same tokens the popover's current row used. A
    /// trailing "+ New session..." chip reuses the popover's own inline-prompt
    /// flow ([`Self::start_new_session_prompt`]/[`Self::submit_new_session_prompt`]).
    /// All colors are theme tokens. A per-chip right-click menu
    /// ([`gpui_component::menu::ContextMenuExt`], the same widget the
    /// explorer row/editor context menus already ship with) holds "Rename",
    /// which opens the inline edit below (#684), and "Kill" (#685), which
    /// arms the inline confirm. Each real chip is also a drag source and drop
    /// target (#686): dropping one onto another resequences the whole visible
    /// list and commits it via [`Self::reorder_sessions`], mirroring the
    /// explorer tree's own drag-to-move (`file_tree.rs`). No `Window` param
    /// needed here (unlike the spec's draft architecture): every GPUI
    /// interactive-element closure attached below (`on_click`, `on_drag`,
    /// `on_drop`, the popup menu's `on_click`) already receives its own
    /// `&mut Window` at dispatch time — see [`Self::start_session_kill_confirm`]'s
    /// call site, which reuses the "Kill" menu item's own closure-scoped
    /// `window` instead of a value threaded from here.
    pub fn render_session_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let current = self.session_name.clone();
        // The live host list; before the first reply (or on the legacy tmux
        // path, where the list channel is inert) fall back to the attached
        // session alone so the strip never renders empty.
        let mut rows = self.sessions.clone();
        if rows.is_empty() && !current.is_empty() {
            rows.push(SessionListItem {
                // No real tmux session id is known client-side before the
                // first `SessionListReply` arrives; this synthesized row is
                // display-only (never a rename/kill target), so `0` is a
                // harmless placeholder.
                id: 0,
                name: current.to_string(),
                windows: self.windows.len() as u32,
                attached: true,
            });
        }
        let prompt_input = self.new_session_prompt.as_ref().map(|p| p.input.clone());

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let current_bg = cx.theme().list_active;
        let primary = cx.theme().primary;
        let attached_dot = cx.theme().success;
        let danger = cx.theme().danger;

        let mut strip = h_flex()
            .items_center()
            .gap(px(4.0))
            .text_size(px(13.0))
            .font_family("JetBrainsMono Nerd Font Mono");

        // A right-click menu targets a real tmux session id; the synthesized
        // fallback row (`id: 0` above) is display-only and must stay inert,
        // so the menu is only attached once the host list has actually
        // loaded.
        let has_real_sessions = !self.sessions.is_empty();

        for row in &rows {
            let is_current = row.name.as_str() == current.as_ref();
            let name = row.name.clone();
            let row_entity = entity.clone();

            let renaming_input = self
                .renaming_session
                .as_ref()
                .filter(|rename| rename.session_id == row.id)
                .map(|rename| rename.input.clone());

            let kill_confirm = self
                .confirming_kill
                .as_ref()
                .filter(|confirm| confirm.session_id == row.id)
                .map(|confirm| (confirm.session_name.clone(), confirm.focus_handle.clone()));

            let chip = if let Some(input) = renaming_input {
                let cancel_entity = entity.clone();
                h_flex()
                    .items_center()
                    .h(px(SESSION_CHIP_HEIGHT))
                    .px(px(6.0))
                    .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                        if event.keystroke.key.as_str() == "escape" {
                            cancel_entity.update(cx, |view, cx| {
                                view.cancel_session_rename(cx);
                            });
                            cx.stop_propagation();
                        }
                    })
                    .child(Input::new(&input).xsmall())
                    .into_any_element()
            } else if let Some((kill_name, kill_focus_handle)) = kill_confirm {
                let confirm_entity = entity.clone();
                let cancel_entity = entity.clone();
                let key_cancel_entity = entity.clone();
                let confirm_id = row.id;
                h_flex()
                    .items_center()
                    .gap(px(4.0))
                    .h(px(SESSION_CHIP_HEIGHT))
                    .px(px(6.0))
                    // Moved onto this row when the confirm was armed
                    // (`Self::start_session_kill_confirm`, #686 drive-by
                    // fix) so `on_key_down` below actually receives Escape
                    // — without this, keyboard focus stayed on the terminal
                    // pane and Escape never reached this handler.
                    .track_focus(&kill_focus_handle)
                    .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                        if event.keystroke.key.as_str() == "escape" {
                            key_cancel_entity.update(cx, |view, cx| {
                                view.cancel_session_kill(cx);
                            });
                            cx.stop_propagation();
                        }
                    })
                    .child(div().text_color(danger).child("Kill?"))
                    .child(
                        Button::new(("session-kill-confirm", confirm_id as usize))
                            .xsmall()
                            .danger()
                            .icon(IconName::Check)
                            .tooltip(SharedString::from(format!("Kill \"{kill_name}\"")))
                            .on_click(move |_event, _window, cx| {
                                confirm_entity.update(cx, |view, cx| {
                                    view.confirm_session_kill(cx);
                                });
                            }),
                    )
                    .child(
                        Button::new(("session-kill-cancel", confirm_id as usize))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Cancel")
                            .on_click(move |_event, _window, cx| {
                                cancel_entity.update(cx, |view, cx| {
                                    view.cancel_session_kill(cx);
                                });
                            }),
                    )
                    .into_any_element()
            } else {
                let base = h_flex()
                    .id(("session-chip", row.id as usize))
                    .items_center()
                    .gap(px(6.0))
                    .h(px(SESSION_CHIP_HEIGHT))
                    .px(px(8.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    // Every chip carries the 2px left-bar slot (the current
                    // chip colors it primary), so the current chip's content
                    // never shifts against the others.
                    .border_l_2()
                    .border_color(if is_current {
                        primary
                    } else {
                        transparent_black()
                    })
                    .when(is_current, |el| el.bg(current_bg))
                    .hover(move |s| s.bg(current_bg))
                    .text_color(if is_current { fg } else { muted })
                    .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                        row_entity.update(cx, |view, cx| {
                            view.switch_to_session(&name, cx);
                        });
                    })
                    .child(
                        div()
                            .max_w(px(140.0))
                            .truncate()
                            .child(SharedString::from(row.name.clone())),
                    )
                    .children(
                        row.attached
                            .then(|| div().size(px(6.0)).rounded_full().bg(attached_dot)),
                    );

                if has_real_sessions {
                    let menu_entity = entity.clone();
                    let session_id = row.id;
                    let session_name = row.name.clone();

                    // Drag-to-reorder (#686, `docs/spec-session-management.md`,
                    // Prior decisions: "drag-to-order, a total user-set
                    // order"): every real chip is both a drag source (a
                    // themed floating preview, `SessionDragPreview`) and a
                    // drop target, mirroring the explorer tree's own
                    // drag/drop (`file_tree.rs`'s `DraggedRow`/`DragPreview`).
                    // Dropping one chip onto another resequences the WHOLE
                    // visible list (never just the two dragged/target rows)
                    // and commits it via `Self::reorder_sessions`, which
                    // sends a `SessionOrderUpdate::Reorder` — `rift-app`
                    // persists it and re-pushes the re-sorted list, so the
                    // strip never mutates `self.sessions` optimistically.
                    // `can_drop` refuses a chip dropped onto itself.
                    let drag_payload = DraggedSession {
                        name: session_name.clone(),
                    };
                    let preview_name = SharedString::from(session_name.clone());
                    let can_drop_name = session_name.clone();
                    let drop_target_name = session_name.clone();
                    let base = base
                        .on_drag(drag_payload, move |_drag, _point, _window, cx| {
                            cx.new(|_| SessionDragPreview {
                                name: preview_name.clone(),
                            })
                        })
                        .drag_over::<DraggedSession>(|style, _drag, _window, cx| {
                            style.bg(cx.theme().list_active)
                        })
                        .can_drop(move |drag: &dyn std::any::Any, _window, _cx| {
                            drag.downcast_ref::<DraggedSession>()
                                .is_some_and(|dragged| dragged.name != can_drop_name)
                        })
                        .on_drop(
                            cx.listener(move |this, drag: &DraggedSession, _window, cx| {
                                this.reorder_sessions(&drag.name, &drop_target_name, cx);
                            }),
                        );

                    base.context_menu(move |menu, _window, _cx| {
                        let rename_entity = menu_entity.clone();
                        let rename_name = session_name.clone();
                        let kill_entity = menu_entity.clone();
                        let kill_name = session_name.clone();
                        menu.item(PopupMenuItem::new("Rename").on_click(
                            move |_event, window, cx| {
                                rename_entity.update(cx, |view, cx| {
                                    view.start_session_rename(
                                        session_id,
                                        rename_name.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            },
                        ))
                        .item(
                            // "Kill" only ARMS the chip's inline confirm
                            // (#685, `docs/spec-session-management.md`); the
                            // danger token marks it destructive at the menu
                            // level too, matching the confirm affordance.
                            PopupMenuItem::element(|_window, cx| {
                                div().text_color(cx.theme().danger).child("Kill")
                            })
                            .on_click(move |_event, window, cx| {
                                kill_entity.update(cx, |view, cx| {
                                    view.start_session_kill_confirm(
                                        session_id,
                                        kill_name.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                        )
                    })
                    .into_any_element()
                } else {
                    base.into_any_element()
                }
            };

            strip = strip.child(chip);
        }

        let new_session = match &prompt_input {
            Some(input) => {
                let cancel_entity = entity.clone();
                h_flex()
                    .items_center()
                    .h(px(SESSION_CHIP_HEIGHT))
                    .px(px(6.0))
                    .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                        if event.keystroke.key.as_str() == "escape" {
                            cancel_entity.update(cx, |view, cx| {
                                view.cancel_new_session_prompt(cx);
                            });
                            cx.stop_propagation();
                        }
                    })
                    .child(Input::new(input).xsmall())
                    .into_any_element()
            }
            None => {
                let prompt_entity = entity.clone();
                h_flex()
                    .items_center()
                    .h(px(SESSION_CHIP_HEIGHT))
                    .px(px(8.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(move |s| s.text_color(fg))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        prompt_entity.update(cx, |view, cx| {
                            view.start_new_session_prompt(window, cx);
                        });
                    })
                    .child("+ New session...")
                    .into_any_element()
            }
        };

        strip.child(new_session)
    }

    /// The reconnect banner's Cancel action: ask the SSH-level reconnect
    /// engine to stop retrying. The engine answers with `Disconnected` on the
    /// status channel — the banner clears through that status change, never
    /// through an optimistic local mutation.
    fn cancel_reconnect(&self) {
        let _ = self.reconnect_cancel_tx.try_send(());
    }

    /// The SSH-outage danger banner (#476, design contract in
    /// `docs/spec-connection-robustness.md`): shown across the top of the
    /// terminal panel while the SSH-level reconnect engine retries. Danger
    /// tint + icon, 13/600 title, 12 muted body carrying the target and the
    /// retry counter — all colors theme tokens — and a Cancel button that
    /// stops the loop via [`Self::cancel_reconnect`].
    fn render_reconnect_banner(&self, retry: u32, cx: &mut Context<Self>) -> impl IntoElement {
        let danger = cx.theme().danger;
        h_flex()
            .id("ssh-reconnect-banner")
            .w_full()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(6.0))
            .bg(danger.opacity(0.12))
            .border_b_1()
            .border_color(danger.opacity(0.35))
            .child(Icon::new(IconName::TriangleAlert).text_color(danger))
            .child(
                div()
                    .flex_none()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child("SSH connection lost"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(reconnect_banner_message(
                        &self.ssh_label,
                        retry,
                    ))),
            )
            .child(
                Button::new("cancel-ssh-reconnect")
                    .danger()
                    .outline()
                    .xsmall()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _event, _window, _cx| {
                        this.cancel_reconnect();
                    })),
            )
    }

    /// The 32px header above each pane: type glyph (from tmux's own `is_shell`,
    /// #510), the `pane_current_command` title (mono), a home-relative cwd
    /// (muted mono), a "running" pill while the pane is busy — hidden when free
    /// (attention is unreachable for a visible pane under the #428 gating) — and
    /// split-h / split-v / zoom controls that each emit a tmux command over the
    /// shared command seam. The bg lifts for the active pane; a click anywhere
    /// on the header focuses the pane (`select-pane`), replacing the removed
    /// sidebar's mouse pane-select. Every control only emits a command; the next
    /// snapshot redraws the result.
    fn render_pane_header(&self, pane_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.panes.get(pane_id) else {
            return div().into_any_element();
        };
        let pane = entry.entity.read(cx);
        let command = pane.current_command().unwrap_or("").to_string();
        let cwd = pane
            .working_directory()
            .map(home_relative)
            .unwrap_or_default();
        // The pill tracks the pane's own busy state; attention never surfaces on
        // a visible pane (its window is active, so the tracker suppresses it).
        let running = matches!(pane.activity(), PaneActivity::Busy);
        let is_shell = entry.is_shell;
        let is_active = self.active_pane_id.as_deref() == Some(pane_id);

        let tab_bar = cx.theme().tab_bar;
        let active_bg = cx.theme().list_active;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let success = cx.theme().success;

        let header_bg = if is_active { active_bg } else { tab_bar };
        // `pane_current_command` is normally populated; fall back to the pane id
        // only so the title is never blank.
        let title = if command.is_empty() {
            pane_id.to_string()
        } else {
            command
        };
        let focus_target = pane_id.to_string();

        let info = h_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(8.0))
            .child(Icon::new(type_glyph(is_shell)).size_3().text_color(muted))
            .child(
                div()
                    .flex_none()
                    .font_family("JetBrainsMono Nerd Font Mono")
                    .text_size(px(13.0))
                    .text_color(fg)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family("JetBrainsMono Nerd Font Mono")
                    .text_size(px(12.0))
                    .text_color(muted)
                    .child(SharedString::from(cwd)),
            )
            .children(running.then(|| {
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.0))
                    .child(div().size(px(7.0)).rounded_full().bg(success))
                    .child(
                        div()
                            .font_family("JetBrainsMono Nerd Font Mono")
                            .text_size(px(11.0))
                            .text_color(muted)
                            .child("running"),
                    )
            }));

        let actions = h_flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .child(self.header_action(
                IconName::PanelRight,
                split_command(true, pane_id),
                muted,
                fg,
                active_bg,
                cx,
            ))
            .child(self.header_action(
                IconName::PanelBottom,
                split_command(false, pane_id),
                muted,
                fg,
                active_bg,
                cx,
            ))
            .child(self.header_action(
                IconName::Maximize,
                zoom_pane_command(pane_id),
                muted,
                fg,
                active_bg,
                cx,
            ));

        h_flex()
            .flex_none()
            .h(px(PANE_HEADER_HEIGHT))
            .w_full()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .bg(header_bg)
            .border_b_1()
            .border_color(border)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, _window, _cx| {
                    if let Err(e) = this
                        .tmux_command_tx
                        .try_send(select_pane_command(&focus_target))
                    {
                        debug!(error = %e, "failed to send select-pane command");
                    }
                }),
            )
            .child(info)
            .child(actions)
            .into_any_element()
    }

    /// A square icon button in a pane header. Emits its tmux command on click
    /// and stops propagation so the header's own focus click does not also fire.
    /// The icon inherits the button's text color, so it dims idle (`muted`) and
    /// brightens to `hover_fg` on hover.
    fn header_action(
        &self,
        icon: IconName,
        command: String,
        muted: Hsla,
        hover_fg: Hsla,
        hover_bg: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(22.0))
            .rounded(px(4.0))
            .text_color(muted)
            .hover(|s| s.bg(hover_bg).text_color(hover_fg))
            .child(Icon::new(icon).size_3())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                    if let Err(e) = this.tmux_command_tx.try_send(command.clone()) {
                        debug!(error = %e, command = %command, "failed to send pane-header command");
                    }
                    cx.stop_propagation();
                }),
            )
    }
}

impl Focusable for SessionView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.active_pane_id
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .or_else(|| self.panes.values().next())
            .map(|entry| entry.entity.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl Render for SessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Skip pane auto-focus while an inline rename, the switcher's
        // new-session prompt, or an armed kill confirm (#686 drive-by fix)
        // owns focus, so a snapshot arriving mid-edit does not steal the
        // keystroke stream from it — the kill confirm has no input of its own
        // to notice the theft, so without this guard a stray steal-back would
        // silently break its Escape handler again.
        if self.needs_focus
            && self.renaming_window.is_none()
            && self.new_session_prompt.is_none()
            && self.renaming_session.is_none()
            && self.confirming_kill.is_none()
        {
            let entity_to_focus = self
                .active_pane_id
                .as_ref()
                .and_then(|id| self.panes.get(id))
                .or_else(|| self.panes.values().next())
                .map(|entry| entry.entity.clone());

            if let Some(entity) = entity_to_focus {
                cx.focus_view(&entity, window);
                self.needs_focus = false;
            }
        }

        let focused_pane_id = self.panes.iter().find_map(|(id, entry)| {
            let fh = entry.entity.read(cx).focus_handle(cx);
            fh.is_focused(window).then_some(id.as_str())
        });
        if let Some(id) = focused_pane_id {
            if self.active_pane_id.as_deref() != Some(id) {
                debug!(pane_id = %id, "focus changed");
                let _ = self
                    .tmux_command_tx
                    .try_send(format!("select-pane -t {}", id));
                self.active_pane_id = Some(id.to_string());
            }
        }

        let font_size = self.font_size;
        let cell_size = measure_cell_size(window, font_size);

        // The transient grid-size overlay (`docs/spec-status-line.md`): the
        // client grid, shown near the terminal for a moment after a resize and
        // hidden once the deadline passes. Session name, cwd, connection dot,
        // and PREFIX no longer live here — they relocated to the title bar, the
        // pane headers, and the app's composite status line respectively.
        let resize_overlay = self
            .resize_overlay_deadline
            .filter(|deadline| Instant::now() < *deadline)
            .map(|_| {
                format!(
                    "{}x{}",
                    self.client_grid_size.cols, self.client_grid_size.rows
                )
            });

        // SSH-outage danger banner (#476): only the SSH-level engine's state
        // shows it; a daemon-stream recovery (plain `Reconnecting`, #475)
        // surfaces through the warning dot alone.
        let reconnect_banner = match self.connection_status {
            ConnectionStatus::SshReconnecting { retry } => {
                Some(self.render_reconnect_banner(retry, cx))
            }
            _ => None,
        };

        let selected_index = self.windows.iter().position(|w| w.is_active).unwrap_or(0);
        // Tab affordances reuse the theme tokens (zero hardcoded hex): the index
        // caption and type glyph idle muted, the busy dot is success-tinted, the
        // attention badge is danger with a white "!", the close x reddens on
        // hover, and the new-window glyph brightens.
        let muted = cx.theme().muted_foreground;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let danger_foreground = cx.theme().danger_foreground;
        let close_idle = cx.theme().muted_foreground;
        let close_hover = cx.theme().danger;
        let new_idle = cx.theme().muted_foreground;
        let new_hover = cx.theme().foreground;

        // Fold each window's panes to its `(dominant, active_count)` before the tab
        // loop: `window_activity` reads `self.panes` while the loop below borrows
        // `self.windows`, so pre-computing keeps the two shared borrows from
        // overlapping. Read live here (not cached) so an observed pane transition
        // reflects on the next render (`docs/spec-pane-activity-v2.md`).
        let mut window_activities: Vec<(PaneActivity, usize)> =
            Vec::with_capacity(self.windows.len());
        for w in &self.windows {
            window_activities.push(self.window_activity(&w.id, cx));
        }

        // One Tab per window, rendered to the design anatomy (`docs/spec-cockpit-chrome.md`):
        // muted index caption, type glyph (prompt vs process, from tmux's own
        // `is_shell`), name, and a fixed state slot (busy dot / attention badge /
        // empty). Single click selects the window, double click opens an inline
        // rename input in place of the anatomy; this dispatch lives on the
        // per-`Tab` `on_click` (not the bar) because it needs the click count to
        // tell the two apart. The close x (fading in over the state slot on hover)
        // and middle-click both kill the window (own mouse-down + stop_propagation
        // so they do not also select); the editing tab shows the input instead.
        let mut tabs: Vec<Tab> = Vec::with_capacity(self.windows.len());
        for (ix, (w, &(dominant, active_count))) in
            self.windows.iter().zip(&window_activities).enumerate()
        {
            let window_id = w.id.clone();
            let editing = self
                .renaming_window
                .as_ref()
                .filter(|rename| rename.window_id == w.id)
                .map(|rename| rename.input.clone());

            let tab = match editing {
                Some(input) => Tab::new().child(
                    div()
                        .w(px(160.0))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                            if event.keystroke.key.as_str() == "escape" {
                                this.cancel_window_rename(cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(Input::new(&input).xsmall()),
                ),
                None => {
                    // Anatomy child: muted index caption (mono, a numeric), the
                    // type glyph, then the window name (Inter 13/500, inheriting
                    // the tab's fg so the active tab reads brighter).
                    let inner = h_flex()
                        .gap(px(6.0))
                        .items_center()
                        .child(
                            div()
                                .font_family("JetBrainsMono Nerd Font Mono")
                                .text_size(px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted)
                                .child(SharedString::from(w.index.to_string())),
                        )
                        .child(Icon::new(type_glyph(w.is_shell)).size_3().text_color(muted))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(w.name.clone())),
                        );

                    // Fixed state slot (busy dot / attention "!"-badge / empty),
                    // sharing its lane with the close x — the x fades in on tab
                    // hover and the state fades out, so hovering never shifts the
                    // layout and an idle tab reserves the same lane.
                    let group_name = SharedString::from(format!("window-tab-{}", w.id));
                    let state = match tab_state_slot(dominant) {
                        TabStateSlot::Idle => div().into_any_element(),
                        TabStateSlot::Busy => {
                            let mut busy = h_flex()
                                .gap(px(3.0))
                                .items_center()
                                .child(div().size(px(7.0)).rounded_full().bg(success));
                            // Count only when more than one pane is busy — a lone
                            // busy pane needs no "1".
                            if active_count > 1 {
                                busy = busy.child(
                                    div()
                                        .font_family("JetBrainsMono Nerd Font Mono")
                                        .text_size(px(11.0))
                                        .text_color(muted)
                                        .child(SharedString::from(active_count.to_string())),
                                );
                            }
                            busy.into_any_element()
                        }
                        TabStateSlot::Attention => h_flex()
                            .size(px(16.0))
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(danger)
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(danger_foreground)
                                    .child("!"),
                            )
                            .into_any_element(),
                    };

                    let close_target = w.id.clone();
                    let close = div()
                        .id(("tab-close", ix))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .invisible()
                        .group_hover(group_name.clone(), |s| s.visible())
                        .text_color(close_idle)
                        .hover(move |this| this.text_color(close_hover))
                        .child(Icon::new(IconName::Close).size_3())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                                if let Err(e) = this
                                    .tmux_command_tx
                                    .try_send(format!("kill-window -t {}", close_target))
                                {
                                    debug!(error = %e, "failed to send kill-window command");
                                }
                                cx.stop_propagation();
                            }),
                        );

                    let suffix = div()
                        .relative()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .min_w(px(16.0))
                        .h_full()
                        .child(
                            div()
                                .group_hover(group_name.clone(), |s| s.invisible())
                                .child(state),
                        )
                        .child(close);

                    let middle_target = w.id.clone();
                    Tab::new()
                        .group(group_name)
                        .child(inner)
                        .suffix(suffix)
                        .on_mouse_down(
                            MouseButton::Middle,
                            cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                                if let Err(e) = this
                                    .tmux_command_tx
                                    .try_send(format!("kill-window -t {}", middle_target))
                                {
                                    debug!(error = %e, "failed to send kill-window command");
                                }
                                cx.stop_propagation();
                            }),
                        )
                }
            };

            tabs.push(
                tab.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.start_window_rename(&window_id, window, cx);
                    } else {
                        if let Err(e) = this
                            .tmux_command_tx
                            .try_send(format!("select-window -t {}", window_id))
                        {
                            debug!(error = %e, "failed to send window switch command");
                        }
                        this.acknowledge_window_attention(&window_id, cx);
                    }
                })),
            );
        }

        let new_window = div()
            .id("tab-new-window")
            .px(px(8.0))
            .text_color(new_idle)
            .hover(move |this| this.text_color(new_hover))
            .child("+")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                    if let Err(e) = this.tmux_command_tx.try_send("new-window".into()) {
                        debug!(error = %e, "failed to send new-window command");
                    }
                    cx.stop_propagation();
                }),
            );

        let tab_bar = TabBar::new("tab-bar")
            .selected_index(selected_index)
            .children(tabs)
            .suffix(new_window);

        let border_color = cx.theme().border;
        let pane_area = if let Some(ref layout) = self.layout {
            self.render_layout(layout, border_color, cx)
        } else {
            div().flex_grow().into_any_element()
        };

        // The tmux client is sized from the pane area's measured bounds — the
        // flex slot below the tab bar, minus the stacked pane headers — never
        // from the window viewport: with the editor split open, the terminal
        // panel only gets a slice of the window, and a viewport-derived grid
        // would overshoot and clip/mis-wrap every pane (#424). The canvas
        // overlays the pane area (absolute, zero layout impact) and its prepaint
        // sees the post-layout bounds whenever this view renders, so a resize is
        // re-sent as the panel geometry changes. Because gpui-component caches
        // the dock panel, this view only re-renders when it is marked dirty; the
        // app re-asserts that on every dock layout change so a dock toggle or
        // splitter drag reaches here even while the terminal is idle (#596).
        let entity = cx.entity().clone();
        let grid_observer = canvas(
            move |bounds: Bounds<Pixels>, _window: &mut Window, cx: &mut App| {
                entity.update(cx, |view: &mut Self, cx| {
                    // On a size change: notify so at least one more frame paints
                    // the transient grid readout even for a single discrete
                    // resize. `resize_client_to_area` arms its own one-shot
                    // timer to clear the overlay once the deadline passes.
                    if view.resize_client_to_area(bounds.size, cell_size, cx) {
                        cx.notify();
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        // Transient grid-size overlay (`docs/spec-status-line.md`): a small
        // pill near the terminal's bottom-right, shown briefly after a resize.
        let resize_overlay = resize_overlay.map(|label| {
            div()
                .absolute()
                .bottom(px(8.0))
                .right(px(8.0))
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .bg(cx.theme().tab_bar)
                .border_1()
                .border_color(cx.theme().border)
                .text_size(px(11.0))
                .text_color(cx.theme().muted_foreground)
                .font_family("JetBrainsMono Nerd Font Mono")
                .child(SharedString::from(label))
        });

        let mut root = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            // Alt+1..9 window switch. The action is dispatched here (an ancestor
            // of the focused pane) before the keystroke reaches the PTY; the
            // 1-based number selects the Nth window in tab order, mirroring the
            // tab-bar click handler. When N exceeds the window count, create a
            // single new window (tmux selects it automatically), completing the
            // affordance like the `+` tab button — never N-M windows.
            .on_action(cx.listener(|this, action: &SelectWindow, _window, cx| {
                if let Some(win) = this.windows.get(action.0.saturating_sub(1)) {
                    if let Err(e) = this
                        .tmux_command_tx
                        .try_send(format!("select-window -t {}", win.id))
                    {
                        debug!(error = %e, "failed to send window switch command");
                    }
                    this.acknowledge_window_attention(&win.id, cx);
                } else if let Err(e) = this.tmux_command_tx.try_send("new-window".into()) {
                    debug!(error = %e, "failed to create new window");
                }
            }))
            .children(reconnect_banner)
            .child(tab_bar)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(pane_area)
                    .child(grid_observer)
                    .children(resize_overlay),
            );

        // While dragging a border, a full-window overlay captures all mouse
        // events (`occlude`) so the underlying pane does not start a text
        // selection, and translates the drag into incremental `resize-pane`.
        if self.border_drag.is_some() {
            let cell_width = cell_size.width;
            let cell_height = cell_size.height;
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .on_mouse_move(cx.listener(
                        move |this, event: &MouseMoveEvent, _window, _cx| {
                            let Some(drag) = this.border_drag.as_mut() else {
                                return;
                            };
                            let (pos, extent) = if drag.horizontal {
                                (event.position.x, cell_width)
                            } else {
                                (event.position.y, cell_height)
                            };
                            let total = ((pos - drag.start) / extent).round() as i32;
                            let delta = total - drag.emitted_cells;
                            if delta != 0 {
                                let dir = resize_direction(drag.horizontal, delta > 0);
                                let _ = this.tmux_command_tx.try_send(format!(
                                    "resize-pane -t {} -{} {}",
                                    drag.target_pane,
                                    dir,
                                    delta.unsigned_abs()
                                ));
                                drag.emitted_cells = total;
                            }
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                            this.border_drag = None;
                            cx.notify();
                        }),
                    ),
            );
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::{
        aggregate_activity, grid_size_for, home_relative, kill_session_command,
        max_vertical_pane_count, new_window_at_command, quote_tmux_name, reconnect_banner_message,
        rename_session_command, resize_direction, select_pane_command, split_command,
        tab_state_slot, zoom_pane_command, PaneActivity, SessionListItem, SessionOrderUpdate,
        SessionSnapshot, SessionView, TabStateSlot, TermSize, TerminalHandle, DEFAULT_FONT_SIZE,
        MAX_FONT_SIZE, MIN_FONT_SIZE,
    };
    use crate::layout::LayoutNode;
    use gpui::{
        px, size, AnyWindowHandle, App, AppContext as _, Entity, SharedString, TestAppContext,
    };
    use gpui_component::input::InputState;
    use std::collections::HashMap;
    use std::time::Instant;
    use termy_terminal_ui::{TmuxPaneState, TmuxSnapshot, TmuxWindowState};

    #[test]
    fn test_grid_size_for_exact_multiple_returns_full_grid() {
        assert_eq!(
            grid_size_for(
                size(px(800.0), px(600.0)),
                size(px(10.0), px(20.0)),
                px(0.0)
            ),
            TermSize { cols: 80, rows: 30 }
        );
    }

    #[test]
    fn test_grid_size_for_partial_cells_floors_to_whole_cells() {
        assert_eq!(
            grid_size_for(
                size(px(805.0), px(619.0)),
                size(px(10.0), px(20.0)),
                px(0.0)
            ),
            TermSize { cols: 80, rows: 30 }
        );
    }

    #[test]
    fn test_grid_size_for_collapsed_area_clamps_to_one_by_one() {
        assert_eq!(
            grid_size_for(size(px(0.0), px(0.0)), size(px(10.0), px(20.0)), px(0.0)),
            TermSize { cols: 1, rows: 1 }
        );
    }

    #[test]
    fn test_grid_size_for_reserves_header_rows_before_flooring() {
        // 600px tall at 20px cells is 30 rows without headers; reserving two
        // 32px headers (64px) leaves 536px -> 26 rows. Columns are unaffected.
        assert_eq!(
            grid_size_for(
                size(px(800.0), px(600.0)),
                size(px(10.0), px(20.0)),
                px(64.0)
            ),
            TermSize { cols: 80, rows: 26 }
        );
    }

    #[test]
    fn test_grid_size_for_reserve_exceeding_height_clamps_rows_to_one() {
        assert_eq!(
            grid_size_for(
                size(px(800.0), px(40.0)),
                size(px(10.0), px(20.0)),
                px(64.0)
            ),
            TermSize { cols: 80, rows: 1 }
        );
    }

    /// The pane-area resize decision (#596) emits a client resize only when the
    /// whole-cell grid actually changes: a repeated area sends nothing (no
    /// dock-layout resize spam), and a real change sends exactly one new grid.
    /// The default (no-layout) path reserves one 32px pane header before
    /// flooring rows.
    #[gpui::test]
    fn test_resize_client_to_area_sends_once_per_grid_change(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);
            let cell = size(px(10.0), px(20.0));

            session.update(cx, |view, cx| {
                // Initial client grid is 80x24; an area that floors to 80x24
                // after the reserved 32px header is unchanged -> nothing emitted.
                // cols: 800/10 = 80; rows: (512 - 32)/20 = 24.
                assert!(!view.resize_client_to_area(size(px(800.0), px(512.0)), cell, cx));

                // A taller area floors to 80x34 -> a real change, one emit.
                // rows: (712 - 32)/20 = 34.
                assert!(view.resize_client_to_area(size(px(800.0), px(712.0)), cell, cx));

                // The same area again is now the cached grid -> no second emit.
                assert!(!view.resize_client_to_area(size(px(800.0), px(712.0)), cell, cx));
            });

            let emitted = handle
                .size_changed_rx
                .try_recv()
                .expect("one resize emitted on the grid change");
            assert_eq!(emitted, TermSize { cols: 80, rows: 34 });
            assert!(
                handle.size_changed_rx.try_recv().is_err(),
                "a stable area emits no further resize"
            );
        });
    }

    /// With the recurring activity idle tick removed, the resize overlay must
    /// self-clear via its own one-shot timer: every real grid change re-arms
    /// the deadline and bumps the generation guard (so a stale in-flight timer
    /// from an earlier resize no-ops); a no-op resize (unchanged grid) must not
    /// spawn a redundant timer.
    #[gpui::test]
    fn test_resize_client_to_area_arms_overlay_only_on_real_change(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, _handle) = session_and_handle(cx);
            let cell = size(px(10.0), px(20.0));

            session.update(cx, |view, cx| {
                assert_eq!(view.resize_overlay_generation, 0);
                assert!(view.resize_overlay_deadline.is_none());

                // Unchanged grid (matches the seeded 80x24 default): no overlay,
                // no generation bump.
                assert!(!view.resize_client_to_area(size(px(800.0), px(512.0)), cell, cx));
                assert_eq!(view.resize_overlay_generation, 0);
                assert!(view.resize_overlay_deadline.is_none());

                // A real change arms the overlay and bumps the generation.
                assert!(view.resize_client_to_area(size(px(800.0), px(712.0)), cell, cx));
                assert_eq!(view.resize_overlay_generation, 1);
                let first_deadline = view.resize_overlay_deadline.expect("overlay armed");
                assert!(first_deadline > Instant::now());

                // A second real change re-arms with a fresh generation, so the
                // first timer's captured generation is now stale.
                assert!(view.resize_client_to_area(size(px(900.0), px(712.0)), cell, cx));
                assert_eq!(view.resize_overlay_generation, 2);
            });
        });
    }

    #[test]
    fn test_quote_tmux_name_plain_wraps_in_single_quotes() {
        assert_eq!(quote_tmux_name("build"), "'build'");
    }

    #[test]
    fn test_quote_tmux_name_with_space_stays_single_argument() {
        assert_eq!(quote_tmux_name("my window"), "'my window'");
    }

    #[test]
    fn test_quote_tmux_name_with_single_quote_is_escaped() {
        assert_eq!(quote_tmux_name("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_resize_direction_horizontal() {
        assert_eq!(resize_direction(true, true), "R");
        assert_eq!(resize_direction(true, false), "L");
    }

    #[test]
    fn test_resize_direction_vertical() {
        assert_eq!(resize_direction(false, true), "D");
        assert_eq!(resize_direction(false, false), "U");
    }

    #[test]
    fn test_split_command_side_by_side_uses_h() {
        assert_eq!(split_command(true, "%3"), "split-window -h -t %3");
    }

    #[test]
    fn test_split_command_stacked_uses_v() {
        assert_eq!(split_command(false, "%3"), "split-window -v -t %3");
    }

    #[test]
    fn test_select_pane_command_targets_pane() {
        assert_eq!(select_pane_command("%7"), "select-pane -t %7");
    }

    #[test]
    fn test_zoom_pane_command_targets_pane() {
        assert_eq!(zoom_pane_command("%7"), "resize-pane -Z -t %7");
    }

    #[test]
    fn test_new_window_at_command_targets_plain_dir() {
        assert_eq!(
            new_window_at_command("/proj/src"),
            "new-window -c '/proj/src'"
        );
    }

    #[test]
    fn test_rename_session_command_plain_name_targets_dollar_id() {
        assert_eq!(
            rename_session_command(2, "build"),
            "rename-session -t $2 -- 'build'"
        );
    }

    #[test]
    fn test_rename_session_command_quotes_a_name_with_a_space() {
        assert_eq!(
            rename_session_command(0, "my session"),
            "rename-session -t $0 -- 'my session'"
        );
    }

    #[test]
    fn test_rename_session_command_quotes_a_name_with_a_single_quote() {
        assert_eq!(
            rename_session_command(5, "it's"),
            "rename-session -t $5 -- 'it'\\''s'"
        );
    }

    #[test]
    fn test_kill_session_command_targets_dollar_id() {
        assert_eq!(kill_session_command(3), "kill-session -t $3");
    }

    #[test]
    fn test_new_window_at_command_quotes_dir_with_spaces() {
        assert_eq!(
            new_window_at_command("/proj/my docs"),
            "new-window -c '/proj/my docs'"
        );
    }

    #[test]
    fn test_home_relative_rewrites_home_prefix() {
        assert_eq!(home_relative("/home/dev/proj"), "~/proj");
        assert_eq!(home_relative("/home/dev/a/b"), "~/a/b");
        assert_eq!(home_relative("/home/dev"), "~");
    }

    #[test]
    fn test_home_relative_rewrites_root_prefix() {
        assert_eq!(home_relative("/root"), "~");
        assert_eq!(home_relative("/root/src"), "~/src");
    }

    #[test]
    fn test_home_relative_leaves_other_paths_unchanged() {
        assert_eq!(home_relative("/var/log"), "/var/log");
        // A path that merely starts with the literal "/root" text is not home.
        assert_eq!(home_relative("/rootfs/x"), "/rootfs/x");
    }

    #[test]
    fn test_max_vertical_pane_count_single_pane_is_one() {
        assert_eq!(max_vertical_pane_count(&LayoutNode::Pane("%0".into())), 1);
    }

    #[test]
    fn test_max_vertical_pane_count_vertical_split_sums_children() {
        // Two panes stacked -> two headers share the column height.
        let node = LayoutNode::Split {
            horizontal: false,
            children: vec![
                (0.5, LayoutNode::Pane("%0".into())),
                (0.5, LayoutNode::Pane("%1".into())),
            ],
        };
        assert_eq!(max_vertical_pane_count(&node), 2);
    }

    #[test]
    fn test_max_vertical_pane_count_horizontal_split_takes_max() {
        // Side-by-side columns -> one header per column height, not the sum.
        let node = LayoutNode::Split {
            horizontal: true,
            children: vec![
                (0.5, LayoutNode::Pane("%0".into())),
                (0.5, LayoutNode::Pane("%1".into())),
            ],
        };
        assert_eq!(max_vertical_pane_count(&node), 1);
    }

    #[test]
    fn test_max_vertical_pane_count_row_over_pane_is_two() {
        // | A | B |  over  C  -> each column stacks two headers.
        let node = LayoutNode::Split {
            horizontal: false,
            children: vec![
                (
                    0.5,
                    LayoutNode::Split {
                        horizontal: true,
                        children: vec![
                            (0.5, LayoutNode::Pane("%0".into())),
                            (0.5, LayoutNode::Pane("%1".into())),
                        ],
                    },
                ),
                (0.5, LayoutNode::Pane("%2".into())),
            ],
        };
        assert_eq!(max_vertical_pane_count(&node), 2);
    }

    #[test]
    fn test_aggregate_activity_empty_window_is_free_and_zero() {
        let (dominant, count) = aggregate_activity(std::iter::empty());
        assert_eq!(dominant, PaneActivity::Free);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_aggregate_activity_single_pane_reports_its_own_state() {
        assert_eq!(
            aggregate_activity([PaneActivity::Free].into_iter()),
            (PaneActivity::Free, 0)
        );
        assert_eq!(
            aggregate_activity([PaneActivity::Busy].into_iter()),
            (PaneActivity::Busy, 1)
        );
        assert_eq!(
            aggregate_activity([PaneActivity::Attention].into_iter()),
            (PaneActivity::Attention, 1)
        );
    }

    #[test]
    fn test_aggregate_activity_precedence_attention_over_busy_over_free() {
        // Attention dominates regardless of order.
        let (dominant, _) = aggregate_activity(
            [
                PaneActivity::Free,
                PaneActivity::Busy,
                PaneActivity::Attention,
            ]
            .into_iter(),
        );
        assert_eq!(dominant, PaneActivity::Attention);
        // Busy dominates free.
        let (dominant, _) = aggregate_activity(
            [PaneActivity::Free, PaneActivity::Busy, PaneActivity::Free].into_iter(),
        );
        assert_eq!(dominant, PaneActivity::Busy);
        // All free stays free.
        let (dominant, _) =
            aggregate_activity([PaneActivity::Free, PaneActivity::Free].into_iter());
        assert_eq!(dominant, PaneActivity::Free);
    }

    #[test]
    fn test_aggregate_activity_count_is_busy_or_attention_panes() {
        let (_, count) = aggregate_activity(
            [
                PaneActivity::Free,
                PaneActivity::Busy,
                PaneActivity::Attention,
                PaneActivity::Free,
                PaneActivity::Busy,
            ]
            .into_iter(),
        );
        assert_eq!(count, 3);
    }

    #[test]
    fn test_tab_state_slot_free_is_idle() {
        assert_eq!(tab_state_slot(PaneActivity::Free), TabStateSlot::Idle);
    }

    #[test]
    fn test_tab_state_slot_busy_and_attention_are_distinct_shapes() {
        let busy = tab_state_slot(PaneActivity::Busy);
        let attention = tab_state_slot(PaneActivity::Attention);
        assert_eq!(busy, TabStateSlot::Busy);
        assert_eq!(attention, TabStateSlot::Attention);
        // Distinct shapes so the dot and the "!"-badge never read alike.
        assert_ne!(busy, attention);
    }

    #[test]
    fn test_reconnect_banner_message_includes_target_and_retry() {
        assert_eq!(
            reconnect_banner_message("developer@100.64.0.1", 3),
            "reconnecting to developer@100.64.0.1 — retry 3"
        );
    }

    /// A `SessionView` plus its channel handle, so switcher tests can assert
    /// what actually crossed the render seam.
    fn session_and_handle(cx: &mut App) -> (Entity<SessionView>, TerminalHandle) {
        let mut handle = None;
        let session = cx.new(|cx| {
            let (view, h) = SessionView::new(cx);
            handle = Some(h);
            view
        });
        (session, handle.expect("handle built by SessionView::new"))
    }

    /// A minimal one-window, one-pane snapshot with the given `pane_is_shell`
    /// map, for exercising `apply_snapshot`'s foreground-shell wiring.
    fn snapshot_with_pane_is_shell(pane_is_shell: HashMap<String, bool>) -> SessionSnapshot {
        SessionSnapshot {
            snapshot: TmuxSnapshot {
                session_name: "rift".to_owned(),
                session_id: None,
                windows: vec![TmuxWindowState {
                    id: "@0".to_owned(),
                    index: 0,
                    name: "win".to_owned(),
                    layout: String::new(),
                    is_active: true,
                    automatic_rename: false,
                    active_pane_id: Some("%0".to_owned()),
                    panes: vec![TmuxPaneState {
                        id: "%0".to_owned(),
                        window_id: "@0".to_owned(),
                        session_id: "$0".to_owned(),
                        is_active: true,
                        left: 0,
                        top: 0,
                        width: 80,
                        height: 24,
                        cursor_x: 0,
                        cursor_y: 0,
                        current_path: String::new(),
                        current_command: String::new(),
                    }],
                }],
            },
            pane_is_shell,
        }
    }

    /// Regression (`docs/spec-pane-activity-v2.md`): the legacy tmux path
    /// (`RIFT_TERMINAL_LEGACY`) sends an empty `pane_is_shell` map (`main.rs`).
    /// `apply_snapshot` must resolve a pane absent from that map to `None`,
    /// not a collapsed `Some(false)` — otherwise every legacy pane would read
    /// forced-busy instead of falling back to the client structural
    /// classifier (alt-screen / OSC-133), which reads free here.
    #[gpui::test]
    fn test_apply_snapshot_legacy_empty_map_reads_pane_free_not_busy(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, _handle) = session_and_handle(cx);

            session.update(cx, |view, cx| {
                view.apply_snapshot(snapshot_with_pane_is_shell(HashMap::new()), cx)
            });

            let activity = session.read(cx).panes["%0"].entity.read(cx).activity();
            assert_eq!(
                activity,
                PaneActivity::Free,
                "pane absent from an empty legacy pane_is_shell map must fall \
                 back to the structural classifier, not read forced-busy"
            );
        });
    }

    /// Same wiring on the daemon-default path: a pane present in the map with
    /// `is_shell: false` (a command is running) reads busy.
    #[gpui::test]
    fn test_apply_snapshot_daemon_path_maps_is_shell_to_busy(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, _handle) = session_and_handle(cx);

            let mut pane_is_shell = HashMap::new();
            pane_is_shell.insert("%0".to_owned(), false);
            session.update(cx, |view, cx| {
                view.apply_snapshot(snapshot_with_pane_is_shell(pane_is_shell), cx)
            });

            let activity = session.read(cx).panes["%0"].entity.read(cx).activity();
            assert_eq!(activity, PaneActivity::Busy);
        });
    }

    /// The banner's Cancel action crosses the render seam as exactly one
    /// signal on the cancel channel; the status change back to `Disconnected`
    /// comes from the engine, never optimistically from the view.
    #[gpui::test]
    fn test_cancel_reconnect_sends_one_cancel_signal(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);

            session.read(cx).cancel_reconnect();

            assert!(
                handle.reconnect_cancel_rx.try_recv().is_ok(),
                "cancel crossed the seam"
            );
            assert!(
                handle.reconnect_cancel_rx.try_recv().is_err(),
                "exactly one cancel signal"
            );
        });
    }

    /// `open_session_switcher` (the command palette's "Switch Session..."
    /// entry) is a manual refresh nudge now that the strip is always visible
    /// (#683) — unlike the removed popover's open/close toggle, every call
    /// issues its own on-demand list refresh.
    #[gpui::test]
    fn test_open_session_switcher_requests_a_list_refresh_each_call(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);

            session.read(cx).open_session_switcher();
            assert!(
                handle.session_list_request_rx.try_recv().is_ok(),
                "requests an on-demand list refresh"
            );

            session.read(cx).open_session_switcher();
            assert!(
                handle.session_list_request_rx.try_recv().is_ok(),
                "a second call requests another refresh (no open/close state to dedupe against)"
            );
        });
    }

    /// Selecting another session emits one switch request carrying the current
    /// client grid (so the fresh control child reflows to the live viewport);
    /// the indicator itself only updates on the fresh snapshot, never
    /// optimistically.
    #[gpui::test]
    fn test_switch_to_session_sends_the_request(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);

            session.update(cx, |view, cx| {
                view.session_name = SharedString::from("rift");
                view.switch_to_session("agent", cx);
            });

            let request = handle
                .session_switch_rx
                .try_recv()
                .expect("switch request sent");
            assert_eq!(request.session, "agent");
            assert_eq!(
                request.size,
                TermSize { cols: 80, rows: 24 },
                "carries the current client grid"
            );
            assert_eq!(
                session.read(cx).session_name.as_ref(),
                "rift",
                "indicator stays on the attached session until the fresh snapshot"
            );
        });
    }

    /// Selecting the already-attached session sends nothing — no pointless
    /// re-attach crosses the seam.
    #[gpui::test]
    fn test_switch_to_session_attached_session_sends_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);

            session.update(cx, |view, cx| {
                view.session_name = SharedString::from("rift");
                view.switch_to_session("rift", cx);
            });

            assert!(
                handle.session_switch_rx.try_recv().is_err(),
                "no switch request for the attached session"
            );
        });
    }

    /// Every session-list arrival replaces the whole list (replace semantics,
    /// like the layout stream), so create/kill/rename reflect without any
    /// merge logic or manual refresh.
    #[gpui::test]
    fn test_apply_session_list_replaces_the_whole_list(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, _handle) = session_and_handle(cx);

            let first = vec![SessionListItem {
                id: 0,
                name: "rift".into(),
                windows: 3,
                attached: true,
            }];
            let second = vec![
                SessionListItem {
                    id: 0,
                    name: "rift".into(),
                    windows: 3,
                    attached: true,
                },
                SessionListItem {
                    id: 1,
                    name: "agent".into(),
                    windows: 1,
                    attached: false,
                },
            ];

            session.update(cx, |view, cx| view.apply_session_list(first.clone(), cx));
            assert_eq!(session.read(cx).sessions, first);

            session.update(cx, |view, cx| view.apply_session_list(second.clone(), cx));
            assert_eq!(session.read(cx).sessions, second);

            session.update(cx, |view, cx| view.apply_session_list(Vec::new(), cx));
            assert!(
                session.read(cx).sessions.is_empty(),
                "an empty reply clears the list rather than merging"
            );
        });
    }

    /// A chip dropped onto another emits the WHOLE resulting name sequence
    /// (#686), not just the two rows involved — `reorder_sessions` reinserts
    /// the dragged name immediately before the target and resequences every
    /// other known session around it.
    #[gpui::test]
    fn test_reorder_sessions_emits_full_sequence_with_dragged_before_target(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);
            session.update(cx, |view, cx| {
                view.apply_session_list(
                    vec![
                        SessionListItem {
                            id: 0,
                            name: "rift".into(),
                            windows: 1,
                            attached: true,
                        },
                        SessionListItem {
                            id: 1,
                            name: "agent".into(),
                            windows: 1,
                            attached: false,
                        },
                        SessionListItem {
                            id: 2,
                            name: "tests".into(),
                            windows: 1,
                            attached: false,
                        },
                    ],
                    cx,
                );
            });

            session.update(cx, |view, cx| view.reorder_sessions("tests", "rift", cx));

            assert_eq!(
                handle
                    .session_order_rx
                    .try_recv()
                    .expect("reorder update sent"),
                SessionOrderUpdate::Reorder(vec![
                    "tests".to_string(),
                    "rift".to_string(),
                    "agent".to_string(),
                ])
            );
        });
    }

    /// A chip dropped onto itself is a no-op — no update crosses the seam.
    #[gpui::test]
    fn test_reorder_sessions_dropped_onto_itself_sends_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);
            session.update(cx, |view, cx| {
                view.apply_session_list(
                    vec![SessionListItem {
                        id: 0,
                        name: "rift".into(),
                        windows: 1,
                        attached: true,
                    }],
                    cx,
                );
            });

            session.update(cx, |view, cx| view.reorder_sessions("rift", "rift", cx));

            assert!(
                handle.session_order_rx.try_recv().is_err(),
                "a drop onto itself sends no reorder update"
            );
        });
    }

    /// A dragged name no longer in the live list (the daemon's churn push
    /// raced the drag) is a no-op rather than inserting a stale entry.
    #[gpui::test]
    fn test_reorder_sessions_unknown_dragged_name_sends_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);
            session.update(cx, |view, cx| {
                view.apply_session_list(
                    vec![SessionListItem {
                        id: 0,
                        name: "rift".into(),
                        windows: 1,
                        attached: true,
                    }],
                    cx,
                );
            });

            session.update(cx, |view, cx| {
                view.reorder_sessions("gone", "rift", cx);
            });

            assert!(
                handle.session_order_rx.try_recv().is_err(),
                "an unknown dragged name sends nothing"
            );
        });
    }

    /// A windowed [`session_and_handle`]: the new-session prompt needs a live
    /// window for its `InputState`, so prompt tests build the view inside an
    /// open test window.
    fn windowed_session_and_handle(
        cx: &mut TestAppContext,
    ) -> (AnyWindowHandle, Entity<SessionView>, TerminalHandle) {
        let mut built = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |_window, cx| {
                let (session, handle) = session_and_handle(cx);
                built = Some((session.clone(), handle));
                session
            })
            .unwrap()
        });
        let (session, handle) = built.expect("session constructed inside the window callback");
        (window.into(), session, handle)
    }

    /// The active prompt's input entity, for driving its value in tests.
    fn prompt_input(session: &Entity<SessionView>, cx: &App) -> Entity<InputState> {
        session
            .read(cx)
            .new_session_prompt
            .as_ref()
            .expect("new-session prompt active")
            .input
            .clone()
    }

    /// The palette's "New Session..." entry activates the strip's trailing
    /// prompt directly (#683 dropped the popover open/close step).
    #[gpui::test]
    fn test_open_new_session_prompt_activates_the_prompt(cx: &mut TestAppContext) {
        let (window, session, _handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| view.open_new_session_prompt(window, cx));

            assert!(
                session.read(cx).new_session_prompt.is_some(),
                "new-session prompt active"
            );

            session.update(cx, |view, cx| view.cancel_new_session_prompt(cx));
            assert!(
                session.read(cx).new_session_prompt.is_none(),
                "cancel restores the trailing chip"
            );
        })
        .unwrap();
    }

    /// Submitting a fresh name sends one switch request carrying the trimmed
    /// name (the daemon's attach-or-create child creates the session) and
    /// clears the prompt; a second submit after the commit must not re-attach
    /// (issue #467).
    #[gpui::test]
    fn test_submit_new_session_prompt_fresh_name_sends_trimmed_switch_and_clears(
        cx: &mut TestAppContext,
    ) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.session_name = SharedString::from("rift");
                view.open_new_session_prompt(window, cx);
            });
            let input = prompt_input(&session, cx);
            input.update(cx, |state, cx| state.set_value("  agent  ", window, cx));

            session.update(cx, |view, cx| view.submit_new_session_prompt(cx));

            let request = handle
                .session_switch_rx
                .try_recv()
                .expect("switch request sent");
            assert_eq!(request.session, "agent", "name is trimmed");
            assert!(
                session.read(cx).new_session_prompt.is_none(),
                "prompt cleared"
            );

            session.update(cx, |view, cx| view.submit_new_session_prompt(cx));
            assert!(
                handle.session_switch_rx.try_recv().is_err(),
                "a second submit after the commit sends nothing"
            );
        })
        .unwrap();
    }

    /// An empty (or whitespace-only) name sends nothing across the seam: the
    /// prompt dismisses back to the "+ New session..." chip — no dead state
    /// (issue #467).
    #[gpui::test]
    fn test_submit_new_session_prompt_empty_name_sends_nothing_and_dismisses(
        cx: &mut TestAppContext,
    ) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| view.open_new_session_prompt(window, cx));
            let input = prompt_input(&session, cx);
            input.update(cx, |state, cx| state.set_value("   ", window, cx));

            session.update(cx, |view, cx| view.submit_new_session_prompt(cx));

            assert!(
                handle.session_switch_rx.try_recv().is_err(),
                "no switch request for an empty name"
            );
            assert!(
                session.read(cx).new_session_prompt.is_none(),
                "prompt dismissed back to the trailing chip"
            );
        })
        .unwrap();
    }

    /// A name duplicating the already-attached session sends no pointless
    /// re-attach; any other existing name takes the same path as a fresh one
    /// (attach-or-create attaches instead of creating), so duplicates can
    /// never error (issue #467).
    #[gpui::test]
    fn test_submit_new_session_prompt_attached_session_name_sends_nothing(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.session_name = SharedString::from("rift");
                view.apply_session_list(
                    vec![SessionListItem {
                        id: 0,
                        name: "rift".into(),
                        windows: 2,
                        attached: true,
                    }],
                    cx,
                );
                view.open_new_session_prompt(window, cx);
            });
            let input = prompt_input(&session, cx);
            input.update(cx, |state, cx| state.set_value("rift", window, cx));

            session.update(cx, |view, cx| view.submit_new_session_prompt(cx));

            assert!(
                handle.session_switch_rx.try_recv().is_err(),
                "no re-attach for the already-attached session"
            );
            assert!(
                session.read(cx).new_session_prompt.is_none(),
                "prompt cleared"
            );
        })
        .unwrap();
    }

    /// A name duplicating another (non-attached) existing session switches to
    /// it exactly like a fresh name — attach-or-create resolves the duplicate
    /// by attaching (issue #467).
    #[gpui::test]
    fn test_submit_new_session_prompt_existing_other_session_name_switches_to_it(
        cx: &mut TestAppContext,
    ) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.session_name = SharedString::from("rift");
                view.apply_session_list(
                    vec![
                        SessionListItem {
                            id: 0,
                            name: "rift".into(),
                            windows: 2,
                            attached: true,
                        },
                        SessionListItem {
                            id: 1,
                            name: "agent".into(),
                            windows: 1,
                            attached: false,
                        },
                    ],
                    cx,
                );
                view.open_new_session_prompt(window, cx);
            });
            let input = prompt_input(&session, cx);
            input.update(cx, |state, cx| state.set_value("agent", window, cx));

            session.update(cx, |view, cx| view.submit_new_session_prompt(cx));

            let request = handle
                .session_switch_rx
                .try_recv()
                .expect("duplicate of a non-attached session is a plain switch");
            assert_eq!(request.session, "agent");
        })
        .unwrap();
    }

    /// The chip's right-click "Rename" (#684) commits a changed, non-empty
    /// name as one quoted `rename-session` on the raw tmux-command seam —
    /// the same channel the pane-header split/zoom/select-pane controls use
    /// — targeted by the tmux session id, not the name; a name with a space
    /// survives the quoting helper. A second submit after the commit must
    /// not re-send (mirrors issue #467's new-session-prompt guard).
    #[gpui::test]
    fn test_submit_session_rename_changed_name_sends_one_quoted_command(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.apply_session_list(
                    vec![SessionListItem {
                        id: 3,
                        name: "rift".into(),
                        windows: 1,
                        attached: false,
                    }],
                    cx,
                );
                view.start_session_rename(3, "rift".to_string(), window, cx);
            });
            let input = session
                .read(cx)
                .renaming_session
                .as_ref()
                .expect("rename active")
                .input
                .clone();
            input.update(cx, |state, cx| state.set_value("my agent", window, cx));

            session.update(cx, |view, cx| view.submit_session_rename(cx));

            assert_eq!(
                handle.tmux_command_rx.try_recv().expect("command sent"),
                "rename-session -t $3 -- 'my agent'"
            );
            assert!(
                session.read(cx).renaming_session.is_none(),
                "rename cleared after commit"
            );

            session.update(cx, |view, cx| view.submit_session_rename(cx));
            assert!(
                handle.tmux_command_rx.try_recv().is_err(),
                "a second submit after the commit sends nothing"
            );
        })
        .unwrap();
    }

    /// A committed rename also emits a matching
    /// `SessionOrderUpdate::Rename { old, new }` on `session_order_rx` (#686,
    /// `docs/spec-session-management.md`): the order store renames its key in
    /// the SAME action, so a reordered session keeps its slot instead of
    /// falling back to the unknown-name default order.
    #[gpui::test]
    fn test_submit_session_rename_emits_matching_order_rename_update(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.apply_session_list(
                    vec![SessionListItem {
                        id: 3,
                        name: "rift".into(),
                        windows: 1,
                        attached: false,
                    }],
                    cx,
                );
                view.start_session_rename(3, "rift".to_string(), window, cx);
            });
            let input = session
                .read(cx)
                .renaming_session
                .as_ref()
                .expect("rename active")
                .input
                .clone();
            input.update(cx, |state, cx| state.set_value("my agent", window, cx));

            session.update(cx, |view, cx| view.submit_session_rename(cx));

            assert_eq!(
                handle
                    .session_order_rx
                    .try_recv()
                    .expect("order update sent"),
                SessionOrderUpdate::Rename {
                    old: "rift".to_string(),
                    new: "my agent".to_string(),
                }
            );
        })
        .unwrap();
    }

    /// An unchanged/empty submit (a no-op rename) sends no order update
    /// either — mirrors `test_submit_session_rename_empty_or_unchanged_name_sends_nothing`.
    #[gpui::test]
    fn test_submit_session_rename_unchanged_name_sends_no_order_update(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.start_session_rename(1, "rift".to_string(), window, cx);
            });
            session.update(cx, |view, cx| view.submit_session_rename(cx));

            assert!(
                handle.session_order_rx.try_recv().is_err(),
                "an unchanged name sends no order update"
            );
        })
        .unwrap();
    }

    /// An empty (or whitespace-only) or unchanged name is a no-op (the
    /// spec's "empty or unchanged name is a no-op"): nothing is sent and the
    /// rename clears.
    #[gpui::test]
    fn test_submit_session_rename_empty_or_unchanged_name_sends_nothing(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.start_session_rename(1, "rift".to_string(), window, cx);
            });
            session.update(cx, |view, cx| view.submit_session_rename(cx));
            assert!(
                handle.tmux_command_rx.try_recv().is_err(),
                "an unchanged name sends nothing"
            );

            session.update(cx, |view, cx| {
                view.start_session_rename(1, "rift".to_string(), window, cx);
            });
            let input = session
                .read(cx)
                .renaming_session
                .as_ref()
                .expect("rename active")
                .input
                .clone();
            input.update(cx, |state, cx| state.set_value("   ", window, cx));
            session.update(cx, |view, cx| view.submit_session_rename(cx));
            assert!(
                handle.tmux_command_rx.try_recv().is_err(),
                "a whitespace-only name sends nothing"
            );
            assert!(
                session.read(cx).renaming_session.is_none(),
                "rename cleared either way"
            );
        })
        .unwrap();
    }

    /// Escape/blur cancels an in-progress session rename without sending a
    /// command.
    #[gpui::test]
    fn test_cancel_session_rename_sends_nothing(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.start_session_rename(1, "rift".to_string(), window, cx);
            });
            assert!(session.read(cx).renaming_session.is_some());

            session.update(cx, |view, cx| view.cancel_session_rename(cx));

            assert!(
                session.read(cx).renaming_session.is_none(),
                "cancel clears the in-progress rename"
            );
            assert!(
                handle.tmux_command_rx.try_recv().is_err(),
                "cancel sends no command"
            );
        })
        .unwrap();
    }

    /// The chip's right-click "Kill" (#685) only ARMS the two-step confirm —
    /// nothing is sent to tmux yet, and needs_focus is left untouched by
    /// arming (only clearing the confirm, on either path, restores it). #686
    /// drive-by fix: arming also moves keyboard focus onto the confirm's own
    /// handle, so the row's `on_key_down` Escape actually fires — without
    /// this, focus stayed on the terminal pane and Escape never reached it.
    #[gpui::test]
    fn test_start_session_kill_confirm_arms_without_sending(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.start_session_kill_confirm(3, "rift".to_string(), window, cx);
            });

            let confirm = session
                .read(cx)
                .confirming_kill
                .as_ref()
                .expect("Kill arms the inline confirm")
                .focus_handle
                .clone();
            assert!(
                confirm.is_focused(window),
                "arming moves focus onto the confirm row so Escape reaches it"
            );
            assert!(
                handle.tmux_command_rx.try_recv().is_err(),
                "arming the confirm sends no command"
            );
        })
        .unwrap();
    }

    /// Confirming an armed kill sends exactly one `kill-session -t $<id>` —
    /// targeted by the numeric tmux session id, so no quoting is needed —
    /// clears the confirm state, and restores pane focus (mirrors
    /// `submit_session_rename`). A second confirm after the commit must not
    /// re-send.
    #[gpui::test]
    fn test_confirm_session_kill_sends_one_kill_command_and_restores_focus(
        cx: &mut TestAppContext,
    ) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.start_session_kill_confirm(3, "rift".to_string(), window, cx);
                view.needs_focus = false;
            });

            session.update(cx, |view, cx| view.confirm_session_kill(cx));

            assert_eq!(
                handle.tmux_command_rx.try_recv().expect("command sent"),
                "kill-session -t $3"
            );
            assert!(
                session.read(cx).confirming_kill.is_none(),
                "confirm cleared after commit"
            );
            assert!(session.read(cx).needs_focus, "focus restored after confirm");

            session.update(cx, |view, cx| view.confirm_session_kill(cx));
            assert!(
                handle.tmux_command_rx.try_recv().is_err(),
                "a second confirm after the commit sends nothing"
            );
        })
        .unwrap();
    }

    /// Cancel (mirrors Escape, which dispatches the same handler) sends no
    /// command, clears the armed confirm, and restores pane focus.
    #[gpui::test]
    fn test_cancel_session_kill_sends_nothing_and_restores_focus(cx: &mut TestAppContext) {
        let (window, session, handle) = windowed_session_and_handle(cx);

        cx.update_window(window, |_, window, cx| {
            session.update(cx, |view, cx| {
                view.start_session_kill_confirm(1, "rift".to_string(), window, cx);
                view.needs_focus = false;
            });
            assert!(session.read(cx).confirming_kill.is_some());

            session.update(cx, |view, cx| view.cancel_session_kill(cx));

            assert!(
                session.read(cx).confirming_kill.is_none(),
                "cancel clears the in-progress kill confirm"
            );
            assert!(
                handle.tmux_command_rx.try_recv().is_err(),
                "cancel sends no command"
            );
            assert!(session.read(cx).needs_focus, "focus restored after cancel");
        })
        .unwrap();
    }

    /// `font_size()` starts at the same default `apply_font_zoom`'s delta path
    /// has always used — the settings surface's font-scale field (#366) seeds
    /// from this getter.
    #[gpui::test]
    fn test_font_size_defaults_to_the_starting_client_font_size(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let session = cx.new(|cx| SessionView::new(cx).0);
            assert_eq!(session.read(cx).font_size(), px(DEFAULT_FONT_SIZE));
        });
    }

    /// `set_font_size` is the absolute counterpart to the `Ctrl+=`/`Ctrl+-`
    /// delta path (`apply_font_zoom`): an in-range value applies directly.
    #[gpui::test]
    fn test_set_font_size_applies_an_in_range_value(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let session = cx.new(|cx| SessionView::new(cx).0);

            session.update(cx, |session, cx| {
                session.set_font_size(px(20.0), cx);
            });

            assert_eq!(session.read(cx).font_size(), px(20.0));
        });
    }

    /// A value outside `[MIN_FONT_SIZE, MAX_FONT_SIZE]` clamps rather than
    /// applying verbatim — the same bounds `apply_font_zoom` enforces, so the
    /// settings surface's field can never push the client font out of range.
    #[gpui::test]
    fn test_set_font_size_clamps_to_the_zoom_bounds(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let session = cx.new(|cx| SessionView::new(cx).0);

            session.update(cx, |session, cx| {
                session.set_font_size(px(MIN_FONT_SIZE - 5.0), cx);
            });
            assert_eq!(session.read(cx).font_size(), px(MIN_FONT_SIZE));

            session.update(cx, |session, cx| {
                session.set_font_size(px(MAX_FONT_SIZE + 5.0), cx);
            });
            assert_eq!(session.read(cx).font_size(), px(MAX_FONT_SIZE));
        });
    }

    /// Before the caller feeds a real connection's host/user, the label is
    /// empty rather than guessed — the guessed default (env-var resolution)
    /// is exactly what caused it to diverge from the actual connection
    /// (#494).
    #[gpui::test]
    fn test_ssh_label_defaults_to_empty_until_the_caller_sets_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let session = cx.new(|cx| SessionView::new(cx).0);
            assert_eq!(session.read(cx).ssh_label, SharedString::default());
        });
    }

    /// `set_ssh_label` is the single seam through which the statusbar label
    /// is fed — the caller passes the already-resolved host/user of the
    /// connection it actually establishes, so the label can never diverge
    /// from it (#494).
    #[gpui::test]
    fn test_set_ssh_label_stores_the_given_label(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let session = cx.new(|cx| SessionView::new(cx).0);

            session.update(cx, |session, _cx| {
                session.set_ssh_label("developer@100.64.0.1");
            });

            assert_eq!(
                session.read(cx).ssh_label,
                SharedString::from("developer@100.64.0.1")
            );
        });
    }
}
