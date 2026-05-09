use std::collections::HashMap;

use gpui::*;
use termy_terminal_ui::TmuxSnapshot;
use tracing::debug;

use crate::colors;
use crate::pane_view::{statusbar_height, PaneView};
use crate::{PaneInput, PaneOutput, TermSize};

pub struct TerminalHandle {
    pub pane_output_tx: flume::Sender<PaneOutput>,
    pub input_rx: flume::Receiver<PaneInput>,
    pub size_changed_rx: flume::Receiver<TermSize>,
    pub snapshot_tx: flume::Sender<TmuxSnapshot>,
}

struct PaneEntry {
    entity: Entity<PaneView>,
    pty_tx: flume::Sender<Vec<u8>>,
}

#[allow(dead_code)]
struct WindowState {
    id: String,
    name: String,
    index: i32,
    is_active: bool,
    pane_ids: Vec<String>,
}

pub struct SessionView {
    panes: HashMap<String, PaneEntry>,
    early_output_buffer: HashMap<String, Vec<Vec<u8>>>,
    windows: Vec<WindowState>,
    active_pane_id: Option<String>,
    input_tx: flume::Sender<PaneInput>,
    size_changed_tx: flume::Sender<TermSize>,
    focus_handle: FocusHandle,
    needs_focus: bool,
    ssh_label: SharedString,
    working_directory: Option<String>,
}

impl SessionView {
    pub fn new(cx: &mut Context<Self>) -> (Self, TerminalHandle) {
        let (pane_output_tx, pane_output_rx) = flume::unbounded::<PaneOutput>();
        let (input_tx, input_rx) = flume::unbounded::<PaneInput>();
        let (size_changed_tx, size_changed_rx) = flume::unbounded();
        let (snapshot_tx, snapshot_rx) = flume::unbounded::<TmuxSnapshot>();

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
            active_pane_id: None,
            input_tx: input_tx.clone(),
            size_changed_tx: size_changed_tx.clone(),
            focus_handle: cx.focus_handle(),
            needs_focus: true,
            ssh_label,
            working_directory: None,
        };

        let handle = TerminalHandle {
            pane_output_tx,
            input_rx,
            size_changed_rx,
            snapshot_tx,
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
        self.active_pane_id = active_pane_id;
        if let Some(cwd) = active_cwd {
            self.working_directory = Some(cwd);
        }

        cx.notify();
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

        let font_size = px(14.0);

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
        let bg_hsla = Hsla::from(colors::BACKGROUND);
        let statusbar_bg = Hsla::from(colors::SURFACE0);
        let statusbar_border = Hsla::from(colors::SURFACE1);
        let statusbar_fg = Hsla::from(colors::SUBTEXT0);

        let active_window = self.windows.iter().find(|w| w.is_active);
        let pane_entities: Vec<Entity<PaneView>> = active_window
            .map(|w| {
                w.pane_ids
                    .iter()
                    .filter_map(|id| self.panes.get(id).map(|e| e.entity.clone()))
                    .collect()
            })
            .unwrap_or_else(|| self.panes.values().map(|e| e.entity.clone()).collect());

        let pane_area = div().flex().flex_col().flex_grow().children(pane_entities);

        let statusbar = div()
            .id("statusbar")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .h(statusbar_height())
            .bg(statusbar_bg)
            .border_t_1()
            .border_color(statusbar_border)
            .text_size(font_size)
            .text_color(statusbar_fg)
            .font_family("JetBrainsMono Nerd Font Mono")
            .px(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(16.0))
                    .child(self.ssh_label.clone())
                    .children((!cwd.is_empty()).then(|| SharedString::from(cwd.clone()))),
            )
            .child(div().child(SharedString::from(size_label)));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(bg_hsla)
            .child(pane_area)
            .child(statusbar)
    }
}
