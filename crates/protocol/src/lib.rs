use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Input { pane_id: u32, data: String },
    ResizePane { pane_id: u32, cols: u16, rows: u16 },
    TmuxCommand { cmd: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    PaneOutput {
        pane_id: u32,
        cells: Vec<u8>,
    },
    StateUpdate {
        sessions: Vec<String>,
    },
    FileEvent {
        kind: FileEventKind,
        path: String,
        git_status: Option<String>,
    },
    FileSync {
        path: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEventKind {
    Create,
    Modify,
    Delete,
    Rename,
}
