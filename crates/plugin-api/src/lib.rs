pub use rift_protocol::DaemonMessage;

#[derive(Debug, Clone)]
pub struct PaneStatus {
    pub pane_id: u32,
    pub state: PaneState,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneState {
    Active,
    Idle,
    WaitingForInput,
    Error,
}

pub trait PanePlugin: Send + Sync {
    fn name(&self) -> &str;
    fn handles_process(&self, process_name: &str) -> bool;
    fn process_output(&mut self, pane_id: u32, data: &[u8]) -> Option<PaneStatus>;
}
