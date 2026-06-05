mod colors;
pub mod error;
pub mod keyboard;
pub mod layout;
pub mod pane_view;
mod session_view;

pub use pane_view::PaneView;
pub use session_view::{SessionView, TerminalHandle};
pub use termy_terminal_ui::TerminalUiRenderMetricsSnapshot;

use alacritty_terminal::grid::Dimensions;

pub struct PaneOutput {
    pub pane_id: String,
    pub bytes: Vec<u8>,
}

pub struct PaneInput {
    pub pane_id: String,
    pub bytes: Vec<u8>,
}

/// A request for a bounded `capture-pane` range, issued by a pane when the user
/// scrolls past the top of the live `Term`'s own (post-attach) scrollback. The
/// SSH thread answers it via [`TmuxClient::capture_pane_range`] and returns the
/// payload as a [`CaptureResult`]. `start_row`/`end_row` are tmux line addresses
/// (`-` for the extreme, negative for history); `join_wraps` is `-J`.
pub struct CaptureRequest {
    pub pane_id: String,
    pub start_row: String,
    pub end_row: String,
    pub join_wraps: bool,
}

/// The payload of a [`CaptureRequest`], routed back to the originating pane.
/// `bytes` is empty on capture error/timeout so the pane can clear its in-flight
/// flag and allow a retry without wedging scrolling.
pub struct CaptureResult {
    pub pane_id: String,
    pub bytes: Vec<u8>,
}

/// A tmux format-subscription update (`%subscription-changed`). `name` is the
/// subscription registered via [`termy_terminal_ui::TmuxClient::subscribe`];
/// `pane` is `-` for window- or session-scoped subscriptions.
pub struct SubscriptionUpdate {
    pub name: String,
    pub session: String,
    pub window: String,
    pub pane: String,
    pub value: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}
