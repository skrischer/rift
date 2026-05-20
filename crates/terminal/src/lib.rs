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
