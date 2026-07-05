use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::popover::Popover;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{h_flex, v_flex, ActiveTheme, Sizable};
use termy_terminal_ui::TmuxSnapshot;
use tracing::debug;

use crate::keytable::{self, KeyTable, PrefixOptions};
use crate::layout::{self, LayoutNode};
use crate::pane_view::{measure_cell_size, statusbar_height, PaneActivity, PaneView};
use crate::{
    CaptureRequest, CaptureResult, ConnectionStatus, KeyTableQueryResult, PaneInput, PaneOutput,
    SelectWindow, SessionListItem, SessionSwitchRequest, SubscriptionUpdate, TermSize,
};

const DEFAULT_FONT_SIZE: f32 = 14.0;
/// Lower bound of the whole-client font size, shared by the `Ctrl+=`/`Ctrl+-`
/// zoom path and the settings surface's font-scale field (#366).
pub const MIN_FONT_SIZE: f32 = 8.0;
/// Upper bound of the whole-client font size (see [`MIN_FONT_SIZE`]).
pub const MAX_FONT_SIZE: f32 = 40.0;
const FONT_SIZE_STEP: f32 = 1.0;
/// Width of the always-visible pane sidebar. The tmux client size no longer
/// subtracts it: the client grid is derived from the pane area's measured
/// bounds, which the sidebar (a flex sibling) is already outside of.
const PANE_SIDEBAR_WIDTH: f32 = 160.0;
/// Recurring re-render cadence that ages the per-pane output-recency fallback
/// from busy back to free for the window-tab aggregate. Only the recency
/// fallback needs it; OSC-133 and bell transitions stay event-driven (they
/// re-render via the per-pane observation) (`docs/spec-pane-activity-indicators.md`).
const ACTIVITY_IDLE_TICK: Duration = Duration::from_millis(1000);
/// Width of the session-switcher popover content
/// (`docs/spec-session-switch.md` UI contract).
const SESSION_SWITCHER_WIDTH: f32 = 260.0;
/// Height of one session row (and the new-session footer row) in the switcher.
const SESSION_SWITCHER_ROW_HEIGHT: f32 = 30.0;

pub struct TerminalHandle {
    pub pane_output_tx: flume::Sender<PaneOutput>,
    pub input_rx: flume::Receiver<PaneInput>,
    pub size_changed_rx: flume::Receiver<TermSize>,
    pub snapshot_tx: flume::Sender<TmuxSnapshot>,
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
    /// The host's session list (`docs/spec-session-switch.md`): every
    /// `SessionListReply` — the reply to an explicit refresh below or the
    /// daemon's unprompted churn-driven push — replaces the switcher's whole
    /// list.
    pub session_list_tx: flume::Sender<Vec<SessionListItem>>,
    /// An explicit session-list refresh request (opening the switcher) —
    /// forwarded onto the protocol as `ClientMessage::QuerySessionList`.
    /// Unused on the legacy tmux path (`RIFT_TERMINAL_LEGACY`): the receiver
    /// drops there and a request is a harmless no-op, so the switcher is
    /// inert (the legacy path is slated for removal, #285).
    pub session_list_request_rx: flume::Receiver<()>,
    /// A cockpit switch from the session switcher — forwarded onto the
    /// protocol as `ClientMessage::Attach { session }` followed by a viewport
    /// re-assert (see [`SessionSwitchRequest`]). Same legacy-path caveat as
    /// `session_list_request_rx`.
    pub session_switch_rx: flume::Receiver<SessionSwitchRequest>,
}

struct PaneEntry {
    entity: Entity<PaneView>,
    pty_tx: flume::Sender<Vec<u8>>,
    /// Keeps `SessionView`'s observation of this pane's activity alive for the
    /// pane's lifetime. A background pane's own `cx.notify()` re-renders only
    /// its own subtree, so the parent must observe it to refresh the tab-bar
    /// aggregate live off an OSC-133/bell transition. Dropped with the entry
    /// when the pane leaves the snapshot, so observations never leak across
    /// snapshots (`docs/spec-pane-activity-indicators.md`).
    _activity_subscription: Subscription,
}

struct WindowState {
    id: String,
    name: String,
    index: i32,
    is_active: bool,
    pane_ids: Vec<String>,
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

/// tmux `split-window` command for the sidebar's split controls. The visual
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

/// tmux `kill-pane` command closing the given pane.
fn kill_pane_command(pane: &str) -> String {
    format!("kill-pane -t {}", pane)
}

/// Whole-cell grid dimensions that fit the given pane-area bounds, clamped to
/// at least 1x1 so a degenerate (collapsed) layout never reports a zero-sized
/// tmux client. GPUI-free math so the floor/clamp behaviour is unit-testable.
fn grid_size_for(area: Size<Pixels>, cell: Size<Pixels>) -> TermSize {
    TermSize {
        cols: ((area.width / cell.width).floor() as usize).max(1),
        rows: ((area.height / cell.height).floor() as usize).max(1),
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
/// (`docs/spec-pane-activity-indicators.md`).
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

/// Catppuccin Mocha dot color for a window's dominant [`PaneActivity`], reusing
/// the connection-indicator palette (hardcoded literals, not gpui-component theme
/// tokens): busy is green, attention is peach. `Free` returns `None` so an idle
/// window draws no dot and does not compete for attention. GPUI-free (`Hsla` is a
/// plain value) so the mapping is unit-testable without an app context
/// (`docs/spec-pane-activity-indicators.md`).
fn activity_dot_color(activity: PaneActivity) -> Option<Hsla> {
    match activity {
        PaneActivity::Free => None,
        PaneActivity::Busy => Some(rgb(0xa6e3a1).into()),
        PaneActivity::Attention => Some(rgb(0xfab387).into()),
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
    ssh_label: SharedString,
    session_name: SharedString,
    working_directory: Option<String>,
    connection_status: ConnectionStatus,
    /// The mirrored tmux key-table lookup and prefix/repeat options, pushed
    /// down to every pane and refreshed in place via
    /// [`Self::apply_key_table_result`] (`docs/spec-tmux-keytable-mirroring.md`).
    /// Default (empty table, stock `C-b` prefix) until the first reply lands —
    /// the daemon issues that query unprompted on attach.
    key_table: Arc<KeyTable>,
    prefix_options: PrefixOptions,
    /// Requests a key-table refresh (forwarded to `TerminalHandle`'s
    /// `key_table_request_rx`), driven by the statusbar's explicit refresh
    /// affordance. A dispatched binding-mutating command's refresh is issued
    /// server-side instead, ordered after the mutation on the same seam
    /// (`spawn_command_bridge` in `crates/app`) — not carried on this channel.
    key_table_request_tx: flume::Sender<()>,
    /// The host's tmux session list (`docs/spec-session-switch.md`), replaced
    /// wholesale by every `SessionListReply` (explicit refresh or the daemon's
    /// unprompted churn-driven push). The ACTUAL attached session is NOT read
    /// from here — `session_name` (fed by the layout stream) owns that.
    sessions: Vec<SessionListItem>,
    /// Whether the session-switcher popover (anchored to the statusbar session
    /// label) is open. Controlled state, so the command palette can open the
    /// same switcher programmatically.
    switcher_open: bool,
    /// The switcher footer's in-progress new-session prompt, when active.
    new_session_prompt: Option<NewSessionPrompt>,
    /// Requests an on-demand session-list refresh (forwarded to
    /// `TerminalHandle`'s `session_list_request_rx`), sent when the switcher
    /// opens; between opens the daemon's churn-driven pushes keep the list live.
    session_list_request_tx: flume::Sender<()>,
    /// Emits a cockpit switch (forwarded to `TerminalHandle`'s
    /// `session_switch_rx`) when a switcher row or the new-session prompt
    /// commits.
    session_switch_tx: flume::Sender<SessionSwitchRequest>,
}

impl SessionView {
    pub fn new(cx: &mut Context<Self>) -> (Self, TerminalHandle) {
        let (pane_output_tx, pane_output_rx) = flume::unbounded::<PaneOutput>();
        let (input_tx, input_rx) = flume::unbounded::<PaneInput>();
        let (size_changed_tx, size_changed_rx) = flume::unbounded();
        let (snapshot_tx, snapshot_rx) = flume::unbounded::<TmuxSnapshot>();
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
            // (event-driven, never polled).
            cx.spawn(async move |this, cx| loop {
                let Ok(status) = connection_status_rx.recv_async().await else {
                    break;
                };
                let result = cx.update(|cx| {
                    let updated = this.update(cx, |view, cx| {
                        if view.connection_status != status {
                            view.connection_status = status;
                            cx.notify();
                        }
                    });
                    // `Disconnected` arrives only after `run_ssh_session`
                    // returns, i.e. the tmux session itself ended. That is the
                    // single genuine app-shutdown signal; a closing pane is not.
                    if status == ConnectionStatus::Disconnected {
                        cx.quit();
                    }
                    updated
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

        {
            // Idle tick: one recurring re-render so the per-window activity
            // aggregate ages the output-recency fallback from busy back to free.
            // OSC-133 and bell transitions stay event-driven — they re-render
            // via the per-pane observation set up in `apply_snapshot`; this tick
            // only drives the time-based recency decay
            // (`docs/spec-pane-activity-indicators.md`).
            cx.spawn(async move |this, cx| loop {
                smol::Timer::after(ACTIVITY_IDLE_TICK).await;
                let result = cx.update(|cx| {
                    this.update(cx, |_view, cx| {
                        cx.notify();
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        let ssh_user = std::env::var("RIFT_SSH_USER").unwrap_or_default();
        let ssh_host = std::env::var("RIFT_SSH_HOST").unwrap_or_else(|_| "localhost".into());
        let ssh_label: SharedString = if ssh_user.is_empty() {
            ssh_host.into()
        } else {
            format!("{}@{}", ssh_user, ssh_host).into()
        };

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
            ssh_label,
            session_name: SharedString::default(),
            working_directory: None,
            connection_status: ConnectionStatus::Connecting,
            key_table: Arc::new(KeyTable::default()),
            prefix_options: PrefixOptions::default(),
            key_table_request_tx,
            sessions: Vec::new(),
            switcher_open: false,
            new_session_prompt: None,
            session_list_request_tx,
            session_switch_tx,
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

    /// Replace the switcher's session list wholesale (replace semantics, like
    /// the layout stream) — every `SessionListReply` arrival lands here.
    fn apply_session_list(&mut self, sessions: Vec<SessionListItem>, cx: &mut Context<Self>) {
        if self.sessions != sessions {
            self.sessions = sessions;
            cx.notify();
        }
    }

    /// Open the session-switcher popover (`docs/spec-session-switch.md`) —
    /// the target of the command palette's "Switch Session..." entry, routed
    /// through the workspace. The statusbar label's own click toggles the
    /// popover directly and syncs back via [`Self::set_switcher_open`].
    pub fn open_session_switcher(&mut self, cx: &mut Context<Self>) {
        self.set_switcher_open(true, cx);
    }

    /// Open the switcher with the new-session prompt already active — the
    /// command palette's "New Session..." entry, routed through the workspace.
    pub fn open_new_session_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_session_switcher(cx);
        self.start_new_session_prompt(window, cx);
    }

    /// The single open/close choke point: the palette entry, the statusbar
    /// trigger's own toggle, and the popover's dismiss paths (click-out,
    /// Escape) all land here. Opening requests an on-demand list refresh
    /// (`docs/protocol.md`); between opens the daemon's churn-driven pushes
    /// keep the list live, so no polling anywhere.
    fn set_switcher_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.switcher_open == open {
            return;
        }
        self.switcher_open = open;
        if open {
            let _ = self.session_list_request_tx.try_send(());
        } else {
            self.new_session_prompt = None;
        }
        cx.notify();
    }

    /// Switch the cockpit to `session`: emit the switch request (the app seam
    /// re-sends `Attach { session }` — attach-or-create, so a fresh name
    /// creates the session) and close the switcher. The indicator and the
    /// terminal model reset on the fresh `LayoutSnapshot`, never optimistically
    /// here. Switching to the already-attached session only closes the popover.
    fn switch_to_session(&mut self, session: &str, cx: &mut Context<Self>) {
        if session != self.session_name.as_ref() {
            if let Err(e) = self.session_switch_tx.try_send(SessionSwitchRequest {
                session: session.to_string(),
                size: self.client_grid_size,
            }) {
                debug!(error = %e, %session, "failed to send session switch request");
            }
        }
        self.set_switcher_open(false, cx);
        cx.notify();
    }

    /// Activate the switcher footer's new-session prompt: seed an empty input,
    /// focus it, and subscribe for submit/blur (mirroring the window-rename
    /// prompt). Enter with a non-empty name switches to it (attach-or-create);
    /// blur cancels.
    fn start_new_session_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("session name"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    // A second Enter after the prompt already committed (or was
                    // cancelled) must not re-submit.
                    if this.new_session_prompt.take().is_none() {
                        return;
                    }
                    let value = input.read(cx).value();
                    let trimmed = value.trim().to_string();
                    if !trimmed.is_empty() {
                        this.switch_to_session(&trimmed, cx);
                    }
                    cx.notify();
                }
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

    /// Cancel an in-progress new-session prompt without emitting anything,
    /// restoring the footer's "+ New session..." row (the popover stays open).
    fn cancel_new_session_prompt(&mut self, cx: &mut Context<Self>) {
        if self.new_session_prompt.is_some() {
            self.new_session_prompt = None;
            cx.notify();
        }
    }

    fn apply_snapshot(&mut self, snapshot: TmuxSnapshot, cx: &mut Context<Self>) {
        use std::collections::HashSet;

        let snapshot_pane_ids: HashSet<&str> = snapshot
            .windows
            .iter()
            .flat_map(|w| w.panes.iter().map(|p| p.id.as_str()))
            .collect();

        self.panes
            .retain(|id, _| snapshot_pane_ids.contains(id.as_str()));

        let mut new_windows = Vec::with_capacity(snapshot.windows.len());
        let mut active_pane_id = None;
        let mut active_cwd = None;

        for window in &snapshot.windows {
            let pane_ids: Vec<String> = window.panes.iter().map(|p| p.id.clone()).collect();
            let is_active_window = window.is_active;

            if is_active_window {
                active_pane_id = window.active_pane_id.clone();
                if let Some(pane) = window.panes.iter().find(|p| p.is_active) {
                    if !pane.current_path.is_empty() {
                        active_cwd = Some(pane.current_path.clone());
                    }
                }
            }

            for pane_state in &window.panes {
                if self.panes.contains_key(&pane_state.id) {
                    if let Some(entry) = self.panes.get(&pane_state.id) {
                        entry.entity.update(cx, |pv, cx| {
                            pv.set_tmux_size(pane_state.width, pane_state.height);
                            // Push the window-active flag into the pane's tracker
                            // on every snapshot (not only on the is_active edge —
                            // a continuously active window has no edge): while
                            // active a bell never raises attention, and activation
                            // acknowledges pending attention, so tab clicks,
                            // Alt+1..9, and tmux-side selects clear it uniformly
                            // (`docs/spec-pane-activity-indicators.md`).
                            pv.set_window_active(is_active_window);
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
                        pv
                    });

                    // Observe the pane so its own OSC-133/bell `cx.notify()` (which
                    // re-renders only its own subtree) also re-renders this parent,
                    // keeping a background window's tab aggregate live. The handle
                    // lives in the entry and drops when the pane leaves the snapshot
                    // (`docs/spec-pane-activity-indicators.md`).
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
                            _activity_subscription: activity_subscription,
                        },
                    );
                    self.needs_focus = true;
                }
            }

            new_windows.push(WindowState {
                id: window.id.clone(),
                name: window.name.clone(),
                index: window.index,
                is_active: window.is_active,
                pane_ids,
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
        if let Some(cwd) = active_cwd {
            self.working_directory = Some(cwd);
        }

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
    /// its underlying busy/free (`docs/spec-pane-activity-indicators.md`).
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

    /// Acknowledge bell attention on every pane of `window_id`, immediately.
    /// Called from the local window-select paths (tab click, Alt+1..9) so the
    /// attention badge clears without waiting for the confirming snapshot's
    /// round trip through tmux; the snapshot then re-asserts the same state via
    /// `set_window_active` (`docs/spec-pane-activity-indicators.md`).
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

    fn render_layout(
        &self,
        node: &LayoutNode,
        border_color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            LayoutNode::Pane(id) => {
                if let Some(entry) = self.panes.get(id) {
                    entry.entity.clone().into_any_element()
                } else {
                    div().flex_grow().into_any_element()
                }
            }
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

    /// The statusbar session label wrapped in the session-switcher popover
    /// (`docs/spec-session-switch.md`, interim placement until the phase-21
    /// custom title bar relocates the indicator group). The trigger is the
    /// session name (ghost button); the popover lists every host session —
    /// mono name, "N windows" muted caption, a fixed attached-dot lane — with
    /// the current session marked by a 2px primary left bar on a surface
    /// background, and a "+ New session..." footer. All colors are theme
    /// tokens. Controlled open state, so the command palette opens the same
    /// switcher; the popover's own toggle/dismiss paths sync back through
    /// [`Self::set_switcher_open`].
    fn render_session_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let current = self.session_name.clone();
        // The live host list; before the first reply (or on the legacy tmux
        // path, where the list channel is inert) fall back to the attached
        // session alone so the picker never renders an empty shell.
        let mut rows = self.sessions.clone();
        if rows.is_empty() && !current.is_empty() {
            rows.push(SessionListItem {
                name: current.to_string(),
                windows: self.windows.len() as u32,
                attached: true,
            });
        }
        let prompt_input = self.new_session_prompt.as_ref().map(|p| p.input.clone());

        let on_open_entity = entity.clone();
        Popover::new("session-switcher")
            .anchor(Anchor::BottomLeft)
            .trigger(
                Button::new("session-switcher-trigger")
                    .ghost()
                    .xsmall()
                    .label(current.clone()),
            )
            .open(self.switcher_open)
            .on_open_change(move |open, _window, cx| {
                on_open_entity.update(cx, |view, cx| view.set_switcher_open(*open, cx));
            })
            .content(move |_state, _window, cx| {
                let border = cx.theme().border;
                let fg = cx.theme().popover_foreground;
                let muted = cx.theme().muted_foreground;
                let current_bg = cx.theme().list_active;
                let primary = cx.theme().primary;
                let attached_dot = cx.theme().success;

                let mut list = v_flex()
                    .w(px(SESSION_SWITCHER_WIDTH))
                    .text_size(px(13.0))
                    .text_color(fg)
                    .font_family("JetBrainsMono Nerd Font Mono");

                for row in &rows {
                    let is_current = row.name.as_str() == current.as_ref();
                    let name = row.name.clone();
                    let row_entity = entity.clone();
                    list =
                        list.child(
                            h_flex()
                                .w_full()
                                .h(px(SESSION_SWITCHER_ROW_HEIGHT))
                                .items_center()
                                .gap(px(8.0))
                                .px(px(8.0))
                                // Every row carries the 2px left-bar slot (the
                                // current row colors it primary), so the current
                                // row's content never shifts against the others.
                                .border_l_2()
                                .border_color(if is_current {
                                    primary
                                } else {
                                    transparent_black()
                                })
                                .when(is_current, |el| el.bg(current_bg))
                                .hover(move |s| s.bg(current_bg))
                                .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                                    row_entity.update(cx, |view, cx| {
                                        view.switch_to_session(&name, cx);
                                    });
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(SharedString::from(row.name.clone())),
                                )
                                .child(
                                    div().text_xs().text_color(muted).child(SharedString::from(
                                        format!("{} windows", row.windows),
                                    )),
                                )
                                // Fixed attached-dot lane, so names and counts
                                // align whether or not a session is attached.
                                .child(h_flex().flex_none().w(px(10.0)).justify_center().children(
                                    row.attached.then(|| {
                                        div().size(px(6.0)).rounded_full().bg(attached_dot)
                                    }),
                                )),
                        );
                }

                let footer = match &prompt_input {
                    Some(input) => {
                        let cancel_entity = entity.clone();
                        h_flex()
                            .w_full()
                            .h(px(SESSION_SWITCHER_ROW_HEIGHT))
                            .items_center()
                            .px(px(8.0))
                            .border_t_1()
                            .border_color(border)
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
                            .w_full()
                            .h(px(SESSION_SWITCHER_ROW_HEIGHT))
                            .items_center()
                            .px(px(8.0))
                            .border_t_1()
                            .border_color(border)
                            .text_color(muted)
                            .cursor_pointer()
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

                list.child(footer)
            })
    }

    /// The per-window pane sidebar: a fixed-width column listing the active
    /// window's panes. A row click focuses its pane (`select-pane`), the row
    /// `x` closes it (`kill-pane`), and the header splits the active pane
    /// side-by-side (`|` -> `split-window -h`) or stacked (`-` ->
    /// `split-window -v`). Every control only emits a tmux command; the next
    /// snapshot redraws the result.
    fn render_pane_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let bg = cx.theme().tab_bar;
        let border = cx.theme().border;
        let active_bg = cx.theme().list_active;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;

        let active_pane = self.active_pane_id.as_deref();
        let rows: Vec<(String, String, bool)> = self
            .windows
            .iter()
            .find(|w| w.is_active)
            .map(|w| w.pane_ids.as_slice())
            .unwrap_or(&[])
            .iter()
            .map(|id| {
                let label = self
                    .panes
                    .get(id)
                    .and_then(|entry| entry.entity.read(cx).current_command().map(str::to_string))
                    .filter(|cmd| !cmd.is_empty())
                    .unwrap_or_else(|| id.clone());
                let is_active = active_pane == Some(id.as_str());
                (id.clone(), label, is_active)
            })
            .collect();

        // Visual "|" splits side-by-side (tmux `-h`); visual "-" stacks (tmux
        // `-v`) -- the naming is inverted vs. the divider orientation.
        let split_side = active_pane.map(|id| split_command(true, id));
        let split_stack = active_pane.map(|id| split_command(false, id));

        let header = h_flex()
            .flex_none()
            .w_full()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .child(self.sidebar_button("|", split_side, fg, muted, active_bg, cx))
            .child(self.sidebar_button("-", split_stack, fg, muted, active_bg, cx));

        let mut list = v_flex().w_full().flex_1().min_h_0();
        for (id, label, is_active) in rows {
            let select_id = id.clone();
            let row_bg = if is_active { active_bg } else { bg };
            let row_fg = if is_active { fg } else { muted };
            let row = h_flex()
                .w_full()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(4.0))
                .bg(row_bg)
                .text_color(row_fg)
                .hover(|s| s.bg(active_bg).text_color(fg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, _window, _cx| {
                        if let Err(e) = this
                            .tmux_command_tx
                            .try_send(select_pane_command(&select_id))
                        {
                            debug!(error = %e, "failed to send select-pane command");
                        }
                    }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label)),
                )
                .child(self.sidebar_button(
                    "x",
                    Some(kill_pane_command(&id)),
                    fg,
                    muted,
                    active_bg,
                    cx,
                ));
            list = list.child(row);
        }

        v_flex()
            .flex_none()
            .w(px(PANE_SIDEBAR_WIDTH))
            .h_full()
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .text_size(px(DEFAULT_FONT_SIZE))
            .font_family("JetBrainsMono Nerd Font Mono")
            .child(header)
            .child(list)
            .into_any_element()
    }

    /// A square glyph button for the pane sidebar. With a command it emits that
    /// command on click and stops propagation so a parent row does not also act;
    /// without one it renders dimmed and inert (no active pane to target).
    fn sidebar_button(
        &self,
        glyph: &'static str,
        command: Option<String>,
        fg: Hsla,
        muted: Hsla,
        hover_bg: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        let button = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(20.0))
            .rounded(px(4.0))
            .child(glyph);
        match command {
            Some(command) => button
                .text_color(muted)
                .hover(|s| s.bg(hover_bg).text_color(fg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                        if let Err(e) = this.tmux_command_tx.try_send(command.clone()) {
                            debug!(error = %e, command = %command, "failed to send sidebar command");
                        }
                        cx.stop_propagation();
                    }),
                ),
            None => button.text_color(muted.opacity(0.4)),
        }
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
        // Skip pane auto-focus while an inline rename or the switcher's
        // new-session prompt owns focus, so a snapshot arriving mid-edit does
        // not steal the keystroke stream from the input.
        if self.needs_focus && self.renaming_window.is_none() && self.new_session_prompt.is_none() {
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

        let (grid_size, pane_cwd, pane_command, prefix_pending) = self
            .active_pane_id
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .map(|entry| {
                let pane = entry.entity.read(cx);
                (
                    pane.grid_size(),
                    pane.working_directory().map(String::from),
                    pane.current_command().map(String::from),
                    pane.prefix_pending(),
                )
            })
            .unwrap_or((TermSize { cols: 0, rows: 0 }, None, None, false));

        let cwd = pane_cwd
            .or_else(|| self.working_directory.clone())
            .unwrap_or_default();
        let command = pane_command.unwrap_or_default();

        let size_label = format!("{}x{}", grid_size.cols, grid_size.rows);

        // Connection indicator: Catppuccin Mocha semantic colors (not in the
        // gpui-component theme tokens), driven by the SSH lifecycle channel.
        let (status_label, status_color) = match self.connection_status {
            ConnectionStatus::Connecting => ("connecting", rgb(0xf9e2af)),
            ConnectionStatus::Connected => ("connected", rgb(0xa6e3a1)),
            ConnectionStatus::Reconnecting => ("reconnecting", rgb(0xfab387)),
            ConnectionStatus::Disconnected => ("disconnected", rgb(0xf38ba8)),
        };

        let selected_index = self.windows.iter().position(|w| w.is_active).unwrap_or(0);
        // Tab affordances reuse the theme tokens; the close glyph idles muted and
        // reddens on hover, the new-window glyph idles muted and brightens.
        let close_idle = cx.theme().muted_foreground;
        let close_hover = cx.theme().danger;
        let new_idle = cx.theme().muted_foreground;
        let new_hover = cx.theme().foreground;
        let activity_count_color = cx.theme().muted_foreground;

        // Fold each window's panes to its `(dominant, active_count)` before the tab
        // loop: `window_activity` reads `self.panes` while the loop below borrows
        // `self.windows`, so pre-computing keeps the two shared borrows from
        // overlapping. Read live here (not cached) so an observed pane transition
        // reflects on the next render (`docs/spec-pane-activity-indicators.md`).
        let mut window_activities: Vec<(PaneActivity, usize)> =
            Vec::with_capacity(self.windows.len());
        for w in &self.windows {
            window_activities.push(self.window_activity(&w.id, cx));
        }

        // One Tab per window. Single click selects the window, double click opens
        // an inline rename input in place of the label; this dispatch lives on the
        // per-`Tab` `on_click` (not the bar) because it needs the click count to
        // tell the two apart. The per-tab "x" suffix and middle-click both kill
        // the window (own mouse-down + stop_propagation so they do not also
        // select); the editing tab shows the input instead and omits the "x".
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
                    let label = SharedString::from(format!("{}: {}", w.index, w.name));
                    let close_target = w.id.clone();
                    let middle_target = w.id.clone();
                    let close = div()
                        .id(("tab-close", ix))
                        .px(px(4.0))
                        .text_color(close_idle)
                        .hover(move |this| this.text_color(close_hover))
                        .child("x")
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
                    let mut tab = Tab::new().label(label).suffix(close).on_mouse_down(
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
                    );
                    // Activity indicator as a prefix (before the label, opposite the
                    // close "x" suffix): a compact dot for the window's dominant
                    // pane state, plus the busy-or-attention pane count when > 0. A
                    // free window shows neither, so an idle single-pane window stays
                    // clean (`docs/spec-pane-activity-indicators.md`).
                    if let Some(dot_color) = activity_dot_color(dominant) {
                        let mut indicator = h_flex()
                            .gap(px(4.0))
                            .items_center()
                            .child(div().size(px(8.0)).rounded_full().bg(dot_color));
                        if active_count > 0 {
                            indicator = indicator.child(
                                div()
                                    .text_xs()
                                    .text_color(activity_count_color)
                                    .child(SharedString::from(active_count.to_string())),
                            );
                        }
                        tab = tab.prefix(indicator);
                    }
                    tab
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
        // flex slot next to the sidebar, below the tab bar, above the statusbar —
        // never from the window viewport: with the editor split open, the
        // terminal panel only gets a slice of the window, and a viewport-derived
        // grid would overshoot and clip/mis-wrap every pane (#424). The canvas
        // overlays the pane area (absolute, zero layout impact) and its prepaint
        // sees the post-layout bounds each frame, so a dock-splitter drag
        // re-sends the client resize as the panel geometry changes.
        let entity = cx.entity().clone();
        let grid_observer = canvas(
            move |bounds: Bounds<Pixels>, _window: &mut Window, cx: &mut App| {
                entity.update(cx, |view: &mut Self, _cx| {
                    let grid = grid_size_for(bounds.size, cell_size);
                    if grid != view.client_grid_size {
                        view.client_grid_size = grid;
                        let _ = view.size_changed_tx.try_send(grid);
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let statusbar = h_flex()
            .id("statusbar")
            .justify_between()
            .w_full()
            .h(statusbar_height())
            .bg(cx.theme().tab_bar)
            .border_t_1()
            .border_color(cx.theme().border)
            // Statusbar stays fixed-size; font zoom only scales terminal content,
            // and the bar has a fixed height that larger text would overflow.
            .text_size(px(DEFAULT_FONT_SIZE))
            .text_color(cx.theme().muted_foreground)
            .font_family("JetBrainsMono Nerd Font Mono")
            .px(px(12.0))
            // Left slot: connection / session / window info (Phase 2d fields land here).
            .child(
                h_flex()
                    .gap(px(16.0))
                    .child(
                        h_flex()
                            .gap(px(6.0))
                            .child(div().size(px(8.0)).rounded_full().bg(status_color))
                            .child(SharedString::from(status_label)),
                    )
                    // Session label doubles as the session-switcher trigger
                    // (`docs/spec-session-switch.md`, interim statusbar
                    // placement until the phase-21 title bar).
                    .children(
                        (!self.session_name.is_empty()).then(|| self.render_session_switcher(cx)),
                    )
                    .child(self.ssh_label.clone())
                    .children((!cwd.is_empty()).then(|| SharedString::from(cwd.clone()))),
            )
            // Right slot: command / git status (Phase 2d fields land here).
            .child(
                h_flex()
                    .gap(px(16.0))
                    // Pending-prefix indicator (tmux key-table mirroring): shown
                    // while the focused pane is capturing the chord key after the
                    // configured prefix; clears on dispatch/cancel (Escape or any
                    // unbound chord key falls through the state machine back to
                    // idle — see `crate::prefix`).
                    .children(
                        prefix_pending.then(|| div().text_color(rgb(0xf9e2af)).child("PREFIX")),
                    )
                    // Explicit key-table refresh trigger (tmux key-table
                    // mirroring): re-queries `list-keys`/`show-options` on
                    // click — the manual escape hatch alongside the automatic
                    // attach and binding-mutating-dispatch triggers.
                    .child(
                        div()
                            .id("refresh-key-table")
                            .cursor_pointer()
                            .hover(|s| s.text_color(new_hover))
                            .child("refresh keys")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _event: &MouseDownEvent, _window, _cx| {
                                    let _ = this.key_table_request_tx.try_send(());
                                }),
                            ),
                    )
                    .children((!command.is_empty()).then(|| SharedString::from(command.clone())))
                    .child(SharedString::from(size_label)),
            );

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
            .child(tab_bar)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_pane_sidebar(cx))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(pane_area)
                            .child(grid_observer),
                    ),
            )
            .child(statusbar);

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
        activity_dot_color, aggregate_activity, grid_size_for, kill_pane_command, quote_tmux_name,
        resize_direction, select_pane_command, split_command, PaneActivity, SessionListItem,
        SessionView, TermSize, TerminalHandle, DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE,
    };
    use gpui::{px, rgb, size, App, AppContext as _, Entity, SharedString, TestAppContext};

    #[test]
    fn test_grid_size_for_exact_multiple_returns_full_grid() {
        assert_eq!(
            grid_size_for(size(px(800.0), px(600.0)), size(px(10.0), px(20.0))),
            TermSize { cols: 80, rows: 30 }
        );
    }

    #[test]
    fn test_grid_size_for_partial_cells_floors_to_whole_cells() {
        assert_eq!(
            grid_size_for(size(px(805.0), px(619.0)), size(px(10.0), px(20.0))),
            TermSize { cols: 80, rows: 30 }
        );
    }

    #[test]
    fn test_grid_size_for_collapsed_area_clamps_to_one_by_one() {
        assert_eq!(
            grid_size_for(size(px(0.0), px(0.0)), size(px(10.0), px(20.0))),
            TermSize { cols: 1, rows: 1 }
        );
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
    fn test_kill_pane_command_targets_pane() {
        assert_eq!(kill_pane_command("%7"), "kill-pane -t %7");
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
    fn test_activity_dot_color_free_has_no_dot() {
        assert_eq!(activity_dot_color(PaneActivity::Free), None);
    }

    #[test]
    fn test_activity_dot_color_busy_and_attention_are_distinct_palette_literals() {
        let busy = activity_dot_color(PaneActivity::Busy);
        let attention = activity_dot_color(PaneActivity::Attention);
        assert_eq!(busy, Some(rgb(0xa6e3a1).into()));
        assert_eq!(attention, Some(rgb(0xfab387).into()));
        assert_ne!(busy, attention);
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

    /// Opening the switcher (any entry point — statusbar trigger or palette
    /// command) marks it open and issues exactly one on-demand list refresh;
    /// re-opening while already open must not spam a second query
    /// (`docs/spec-session-switch.md`).
    #[gpui::test]
    fn test_open_session_switcher_marks_open_and_requests_one_list_refresh(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);

            session.update(cx, |view, cx| view.open_session_switcher(cx));

            assert!(session.read(cx).switcher_open, "switcher marked open");
            assert!(
                handle.session_list_request_rx.try_recv().is_ok(),
                "opening requests an on-demand list refresh"
            );

            session.update(cx, |view, cx| view.open_session_switcher(cx));
            assert!(
                handle.session_list_request_rx.try_recv().is_err(),
                "opening an already-open switcher sends no second refresh"
            );
        });
    }

    /// Selecting another session emits one switch request carrying the current
    /// client grid (so the fresh control child reflows to the live viewport)
    /// and closes the switcher; the indicator itself only updates on the fresh
    /// snapshot, never optimistically.
    #[gpui::test]
    fn test_switch_to_session_sends_the_request_and_closes_the_switcher(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);

            session.update(cx, |view, cx| {
                view.session_name = SharedString::from("rift");
                view.open_session_switcher(cx);
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
            assert!(!session.read(cx).switcher_open, "switcher closed");
            assert_eq!(
                session.read(cx).session_name.as_ref(),
                "rift",
                "indicator stays on the attached session until the fresh snapshot"
            );
        });
    }

    /// Selecting the already-attached session only closes the popover — no
    /// pointless re-attach crosses the seam.
    #[gpui::test]
    fn test_switch_to_session_attached_session_sends_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (session, handle) = session_and_handle(cx);

            session.update(cx, |view, cx| {
                view.session_name = SharedString::from("rift");
                view.open_session_switcher(cx);
                view.switch_to_session("rift", cx);
            });

            assert!(
                handle.session_switch_rx.try_recv().is_err(),
                "no switch request for the attached session"
            );
            assert!(!session.read(cx).switcher_open, "switcher still closes");
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
                name: "rift".into(),
                windows: 3,
                attached: true,
            }];
            let second = vec![
                SessionListItem {
                    name: "rift".into(),
                    windows: 3,
                    attached: true,
                },
                SessionListItem {
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

    /// The palette's "New Session..." entry opens the switcher with the footer
    /// prompt already active (and still triggers the on-open list refresh).
    #[gpui::test]
    fn test_open_new_session_prompt_opens_switcher_with_prompt_active(cx: &mut TestAppContext) {
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

        cx.update_window(window.into(), |_, window, cx| {
            session.update(cx, |view, cx| view.open_new_session_prompt(window, cx));

            assert!(session.read(cx).switcher_open, "switcher opened");
            assert!(
                session.read(cx).new_session_prompt.is_some(),
                "new-session prompt active"
            );
            assert!(
                handle.session_list_request_rx.try_recv().is_ok(),
                "opening still requests the list refresh"
            );

            session.update(cx, |view, cx| view.cancel_new_session_prompt(cx));
            assert!(
                session.read(cx).new_session_prompt.is_none(),
                "cancel restores the footer row"
            );
            assert!(
                session.read(cx).switcher_open,
                "cancelling the prompt keeps the switcher open"
            );
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
}
