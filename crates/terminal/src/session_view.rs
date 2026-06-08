use std::collections::HashMap;

use gpui::*;
use gpui_component::tab::TabBar;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use termy_terminal_ui::TmuxSnapshot;
use tracing::debug;

use crate::layout::{self, LayoutNode};
use crate::pane_view::{measure_cell_size, statusbar_height, PaneView};
use crate::{
    CaptureRequest, CaptureResult, ConnectionStatus, PaneInput, PaneOutput, SelectWindow,
    SubscriptionUpdate, TermSize,
};

const DEFAULT_FONT_SIZE: f32 = 14.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 40.0;
const FONT_SIZE_STEP: f32 = 1.0;

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
}

struct PaneEntry {
    entity: Entity<PaneView>,
    pty_tx: flume::Sender<Vec<u8>>,
}

struct WindowState {
    id: String,
    name: String,
    index: i32,
    is_active: bool,
    pane_ids: Vec<String>,
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
    focus_handle: FocusHandle,
    needs_focus: bool,
    window_grid_size: TermSize,
    ssh_label: SharedString,
    session_name: SharedString,
    working_directory: Option<String>,
    connection_status: ConnectionStatus,
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
            focus_handle: cx.focus_handle(),
            needs_focus: true,
            window_grid_size: TermSize { cols: 80, rows: 24 },
            ssh_label,
            session_name: SharedString::default(),
            working_directory: None,
            connection_status: ConnectionStatus::Connecting,
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

            if window.is_active {
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

                    if let Some(buffered) = self.early_output_buffer.remove(&pane_id) {
                        for bytes in buffered {
                            let _ = pty_tx.send(bytes);
                        }
                    }

                    debug!(pane_id = %pane_id, "created pane");
                    self.panes.insert(pane_id, PaneEntry { entity, pty_tx });
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

        let active_pane = self.active_pane_id.clone();
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
                let is_active = active_pane.as_deref() == Some(id.as_str());
                (id.clone(), label, is_active)
            })
            .collect();

        // Visual "|" splits side-by-side (tmux `-h`); visual "-" stacks (tmux
        // `-v`) -- the naming is inverted vs. the divider orientation.
        let split_side = active_pane
            .as_ref()
            .map(|id| format!("split-window -h -t {}", id));
        let split_stack = active_pane
            .as_ref()
            .map(|id| format!("split-window -v -t {}", id));

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
                            .try_send(format!("select-pane -t {}", select_id))
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
                    Some(format!("kill-pane -t {}", id)),
                    fg,
                    muted,
                    active_bg,
                    cx,
                ));
            list = list.child(row);
        }

        v_flex()
            .flex_none()
            .w(px(160.0))
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
        if self.needs_focus {
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
        let total_cols = (viewport.width / cell_size.width).floor() as usize;
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

        let (grid_size, pane_cwd, pane_command) = self
            .active_pane_id
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .map(|entry| {
                let pane = entry.entity.read(cx);
                (
                    pane.grid_size(),
                    pane.working_directory().map(String::from),
                    pane.current_command().map(String::from),
                )
            })
            .unwrap_or((TermSize { cols: 0, rows: 0 }, None, None));

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
        let window_ids: Vec<String> = self.windows.iter().map(|w| w.id.clone()).collect();
        let tab_labels: Vec<SharedString> = self
            .windows
            .iter()
            .map(|w| SharedString::from(format!("{}: {}", w.index, w.name)))
            .collect();

        let tab_bar = TabBar::new("tab-bar")
            .selected_index(selected_index)
            .children(tab_labels)
            .on_click(cx.listener(move |this, index: &usize, _, _| {
                if let Some(id) = window_ids.get(*index) {
                    if let Err(e) = this
                        .tmux_command_tx
                        .try_send(format!("select-window -t {}", id))
                    {
                        debug!(error = %e, "failed to send window switch command");
                    }
                }
            }));

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
            // tab-bar click handler.
            .on_action(cx.listener(|this, action: &SelectWindow, _window, _cx| {
                if let Some(win) = this.windows.get(action.0.saturating_sub(1)) {
                    if let Err(e) = this
                        .tmux_command_tx
                        .try_send(format!("select-window -t {}", win.id))
                    {
                        debug!(error = %e, "failed to send window switch command");
                    }
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
    use super::resize_direction;

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
}
