use gpui::*;
use termy_terminal_ui::TmuxSnapshot;

use crate::colors;
use crate::pane_view::{statusbar_height, PaneView};
use crate::{PaneInput, PaneOutput, TermSize};

pub struct TerminalHandle {
    pub pane_output_tx: flume::Sender<PaneOutput>,
    pub input_rx: flume::Receiver<PaneInput>,
    pub size_changed_rx: flume::Receiver<TermSize>,
    pub snapshot_tx: flume::Sender<TmuxSnapshot>,
}

pub struct SessionView {
    pane: Entity<PaneView>,
    active_pane_id: Option<String>,
    ssh_label: SharedString,
    working_directory: Option<String>,
}

impl SessionView {
    pub fn new(cx: &mut Context<Self>) -> (Self, TerminalHandle) {
        let (pane_output_tx, pane_output_rx) = flume::unbounded::<PaneOutput>();
        let (pty_tx, pty_rx) = flume::unbounded::<Vec<u8>>();
        let (input_tx, input_rx) = flume::unbounded::<PaneInput>();
        let (size_changed_tx, size_changed_rx) = flume::unbounded();
        let (snapshot_tx, snapshot_rx) = flume::unbounded::<TmuxSnapshot>();

        let pane = cx.new(|pane_cx| PaneView::new(pane_cx, pty_rx, input_tx, size_changed_tx));

        {
            cx.spawn(async move |_this, _cx| loop {
                let Ok(output) = pane_output_rx.recv_async().await else {
                    break;
                };
                if pty_tx.send(output.bytes).is_err() {
                    break;
                }
            })
            .detach();
        }

        {
            let pane = pane.clone();
            cx.spawn(async move |this, cx| loop {
                let Ok(snapshot) = snapshot_rx.recv_async().await else {
                    break;
                };
                let active_pane = snapshot
                    .windows
                    .iter()
                    .find(|w| w.is_active)
                    .and_then(|w| w.panes.iter().find(|p| p.is_active));
                let cwd = active_pane.map(|p| p.current_path.clone());
                let active_pane_id = snapshot
                    .windows
                    .iter()
                    .find(|w| w.is_active)
                    .and_then(|w| w.active_pane_id.clone());
                let result = cx.update(|cx| {
                    if let Some(ref id) = active_pane_id {
                        pane.update(cx, |pane_view, cx| {
                            pane_view.set_pane_id(id.clone());
                            if let Some(ref path) = cwd {
                                if !path.is_empty() {
                                    pane_view.set_working_directory(path.clone());
                                }
                            }
                            cx.notify();
                        });
                    }
                    this.update(cx, |view, _cx| {
                        view.active_pane_id = active_pane_id;
                        if let Some(path) = cwd {
                            if !path.is_empty() {
                                view.working_directory = Some(path);
                            }
                        }
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
            pane,
            active_pane_id: None,
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
}

impl Focusable for SessionView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.pane.read(cx).focus_handle(cx)
    }
}

impl Render for SessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = px(14.0);

        let pane = self.pane.read(cx);
        let grid_size = pane.grid_size();
        let cwd = self
            .working_directory
            .clone()
            .or_else(|| pane.working_directory().map(String::from))
            .unwrap_or_default();

        let size_label = format!("{}x{}", grid_size.cols, grid_size.rows);
        let bg_hsla = Hsla::from(colors::BACKGROUND);
        let statusbar_bg = Hsla::from(colors::SURFACE0);
        let statusbar_border = Hsla::from(colors::SURFACE1);
        let statusbar_fg = Hsla::from(colors::SUBTEXT0);

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
            .child(self.pane.clone())
            .child(statusbar)
    }
}
