use std::collections::HashMap;

use gpui::*;
use gpui_component::tab::TabBar;
use gpui_component::{h_flex, ActiveTheme};
use termy_terminal_ui::TmuxSnapshot;
use tracing::debug;

use crate::layout::{self, LayoutNode};
use crate::pane_view::{measure_cell_size, statusbar_height, PaneView};
use crate::{PaneInput, PaneOutput, TermSize};

pub struct TerminalHandle {
    pub pane_output_tx: flume::Sender<PaneOutput>,
    pub input_rx: flume::Receiver<PaneInput>,
    pub size_changed_rx: flume::Receiver<TermSize>,
    pub snapshot_tx: flume::Sender<TmuxSnapshot>,
    pub tmux_command_rx: flume::Receiver<String>,
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
    #[allow(dead_code)]
    pane_ids: Vec<String>,
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
    focus_handle: FocusHandle,
    needs_focus: bool,
    window_grid_size: TermSize,
    ssh_label: SharedString,
    working_directory: Option<String>,
}

impl SessionView {
    pub fn new(cx: &mut Context<Self>) -> (Self, TerminalHandle) {
        let (pane_output_tx, pane_output_rx) = flume::unbounded::<PaneOutput>();
        let (input_tx, input_rx) = flume::unbounded::<PaneInput>();
        let (size_changed_tx, size_changed_rx) = flume::unbounded();
        let (snapshot_tx, snapshot_rx) = flume::unbounded::<TmuxSnapshot>();
        let (tmux_command_tx, tmux_command_rx) = flume::unbounded::<String>();

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
            focus_handle: cx.focus_handle(),
            needs_focus: true,
            window_grid_size: TermSize { cols: 80, rows: 24 },
            ssh_label,
            working_directory: None,
        };

        let handle = TerminalHandle {
            pane_output_tx,
            input_rx,
            size_changed_rx,
            snapshot_tx,
            tmux_command_rx,
        };

        (view, handle)
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
                            if !pane_state.current_path.is_empty() {
                                pv.set_working_directory(pane_state.current_path.clone());
                            }
                            cx.notify();
                        });
                    }
                } else {
                    let (pty_tx, pty_rx) = flume::unbounded::<Vec<u8>>();
                    let input_tx = self.input_tx.clone();
                    let size_changed_tx = self.size_changed_tx.clone();
                    let pane_id = pane_state.id.clone();

                    let entity = cx.new(|pane_cx| {
                        let mut pv = PaneView::new(pane_cx, pty_rx, input_tx, size_changed_tx);
                        pv.set_pane_id(pane_id.clone());
                        pv.set_tmux_size(pane_state.width, pane_state.height);
                        if !pane_state.current_path.is_empty() {
                            pv.set_working_directory(pane_state.current_path.clone());
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

    fn render_layout(&self, node: &LayoutNode, border_color: Hsla) -> AnyElement {
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
                let mut container = div().flex().size_full();
                container = if *horizontal {
                    container.flex_row()
                } else {
                    container.flex_col()
                };
                let last = children.len().saturating_sub(1);
                for (i, (proportion, child)) in children.iter().enumerate() {
                    let inner = self.render_layout(child, border_color);
                    let mut wrapper = div()
                        .flex_1()
                        .flex_basis(relative(*proportion))
                        .size_full()
                        .child(inner);
                    if i < last {
                        wrapper = if *horizontal {
                            wrapper.border_r_1().border_color(border_color)
                        } else {
                            wrapper.border_b_1().border_color(border_color)
                        };
                    }
                    container = container.child(wrapper);
                }
                container.into_any_element()
            }
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

        let font_size = px(14.0);
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

        let (grid_size, pane_cwd) = self
            .active_pane_id
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .map(|entry| {
                let pane = entry.entity.read(cx);
                (pane.grid_size(), pane.working_directory().map(String::from))
            })
            .unwrap_or((TermSize { cols: 0, rows: 0 }, None));

        let cwd = pane_cwd
            .or_else(|| self.working_directory.clone())
            .unwrap_or_default();

        let size_label = format!("{}x{}", grid_size.cols, grid_size.rows);

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

        let pane_area = if let Some(ref layout) = self.layout {
            self.render_layout(layout, cx.theme().border)
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
            .text_size(font_size)
            .text_color(cx.theme().muted_foreground)
            .font_family("JetBrainsMono Nerd Font Mono")
            .px(px(12.0))
            // Left slot: connection / session / window info (Phase 2d fields land here).
            .child(
                h_flex()
                    .gap(px(16.0))
                    .child(self.ssh_label.clone())
                    .children((!cwd.is_empty()).then(|| SharedString::from(cwd.clone()))),
            )
            // Right slot: command / git status (Phase 2d fields land here).
            .child(h_flex().child(SharedString::from(size_label)));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .child(tab_bar)
            .child(pane_area)
            .child(statusbar)
    }
}
