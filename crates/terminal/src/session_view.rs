use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{h_flex, v_flex, ActiveTheme, Sizable};
use termy_terminal_ui::TmuxSnapshot;
use tracing::debug;

use crate::keytable::{self, KeyTable, PrefixOptions};
use crate::layout::{self, LayoutNode};
use crate::pane_view::{measure_cell_size, statusbar_height, PaneActivity, PaneView};
use crate::{
    CaptureRequest, CaptureResult, ConnectionStatus, KeyTableQueryResult, PaneInput, PaneOutput,
    SelectWindow, SubscriptionUpdate, TermSize,
};

const DEFAULT_FONT_SIZE: f32 = 14.0;
/// Lower bound of the whole-client font size, shared by the `Ctrl+=`/`Ctrl+-`
/// zoom path and the settings surface's font-scale field (#366).
pub const MIN_FONT_SIZE: f32 = 8.0;
/// Upper bound of the whole-client font size (see [`MIN_FONT_SIZE`]).
pub const MAX_FONT_SIZE: f32 = 40.0;
const FONT_SIZE_STEP: f32 = 1.0;
/// Width of the always-visible pane sidebar. Shared between the sidebar render
/// and the tmux client-width compute so the reported column count never
/// includes the space the sidebar occupies.
const PANE_SIDEBAR_WIDTH: f32 = 160.0;
/// Recurring re-render cadence that ages the per-pane output-recency fallback
/// from busy back to free for the window-tab aggregate. Only the recency
/// fallback needs it; OSC-133 and bell transitions stay event-driven (they
/// re-render via the per-pane observation) (`docs/spec-pane-activity-indicators.md`).
const ACTIVITY_IDLE_TICK: Duration = Duration::from_millis(1000);

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
    window_grid_size: TermSize,
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
            window_grid_size: TermSize { cols: 80, rows: 24 },
            ssh_label,
            session_name: SharedString::default(),
            working_directory: None,
            connection_status: ConnectionStatus::Connecting,
            key_table: Arc::new(KeyTable::default()),
            prefix_options: PrefixOptions::default(),
            key_table_request_tx,
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
                            // The active window must never surface attention:
                            // acknowledge every one of its panes on every snapshot
                            // (not only on the is_active edge — a continuously
                            // active window has no edge), so tab clicks, Alt+1..9,
                            // and %output-confirmed selects clear it uniformly
                            // (`docs/spec-pane-activity-indicators.md`).
                            if is_active_window {
                                pv.acknowledge_attention();
                            }
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
    /// surfaces attention: its panes are acknowledged on every snapshot, and a
    /// bell arriving between snapshots is read as the pane's underlying busy/free
    /// (`docs/spec-pane-activity-indicators.md`).
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
        // Skip pane auto-focus while an inline rename owns focus, so a snapshot
        // arriving mid-rename does not steal the keystroke stream from the input.
        if self.needs_focus && self.renaming_window.is_none() {
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

        let tab_bar_h = statusbar_height();

        let viewport = window.viewport_size();
        // The pane sidebar occupies a fixed slice of the viewport width, so the
        // panes only get what remains; reporting the full width to tmux would
        // clip every pane's right edge.
        let total_cols =
            ((viewport.width - px(PANE_SIDEBAR_WIDTH)) / cell_size.width).floor() as usize;
        let total_rows = ((viewport.height - statusbar_height() - tab_bar_h) / cell_size.height)
            .floor() as usize;
        let window_size = TermSize {
            cols: total_cols.max(1),
            rows: total_rows.max(1),
        };
        if window_size != self.window_grid_size {
            self.window_grid_size = window_size;
            let _ = self.size_changed_tx.try_send(window_size);
        }

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
                    } else if let Err(e) = this
                        .tmux_command_tx
                        .try_send(format!("select-window -t {}", window_id))
                    {
                        debug!(error = %e, "failed to send window switch command");
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
                    .children((!self.session_name.is_empty()).then(|| self.session_name.clone()))
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
            .on_action(cx.listener(|this, action: &SelectWindow, _window, _cx| {
                if let Some(win) = this.windows.get(action.0.saturating_sub(1)) {
                    if let Err(e) = this
                        .tmux_command_tx
                        .try_send(format!("select-window -t {}", win.id))
                    {
                        debug!(error = %e, "failed to send window switch command");
                    }
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
                    .child(div().flex().flex_1().min_w_0().h_full().child(pane_area)),
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
        activity_dot_color, aggregate_activity, kill_pane_command, quote_tmux_name,
        resize_direction, select_pane_command, split_command, PaneActivity, SessionView,
        DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE,
    };
    use gpui::{px, rgb, AppContext as _, TestAppContext};

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
