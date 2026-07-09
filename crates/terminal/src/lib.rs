mod colors;
pub mod error;
pub mod keyboard;
pub mod keytable;
pub mod layout;
pub mod pane_view;
pub mod prefix;
mod search;
mod session_view;
mod tmux_quote;

pub use keytable::{
    classify_command, keystroke_to_tmux_key, normalize_tmux_key, parse_list_keys, parse_options,
    Binding, DispatchDecision, KeyTable, PrefixOptions,
};
pub use pane_view::{PaneActivity, PaneView};
pub use session_view::{
    SessionSnapshot, SessionView, SessionViewEvent, StatusWindow, TerminalHandle, MAX_FONT_SIZE,
    MIN_FONT_SIZE,
};
pub use termy_terminal_ui::TerminalUiRenderMetricsSnapshot;
pub use tmux_quote::quote_tmux_arg;

use alacritty_terminal::grid::Dimensions;

/// GPUI key context set on the terminal pane's focusable div. The app binds
/// `tab`/`shift-tab` to `NoAction` in this context to shadow gpui-component's
/// `Root` focus-navigation and forward Tab to the PTY. Exported so the binding
/// and the `key_context(...)` call reference one string instead of two literals
/// that must stay in sync.
pub const TERMINAL_KEY_CONTEXT: &str = "Terminal";

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

/// The reply to a key-table refresh request: the raw `list-keys` and
/// `show-options` output (newline-joined text, as carried by
/// [`rift_protocol::DaemonMessage::KeyTableReply`]), routed to `SessionView` to
/// parse with [`keytable::parse_list_keys`]/[`keytable::parse_options`] and
/// rebuild the mirrored key-table lookup
/// (`docs/spec-tmux-keytable-mirroring.md`).
pub struct KeyTableQueryResult {
    pub list_keys: String,
    pub options: String,
}

/// One tmux session for the session-switcher picker, as carried by
/// `rift_protocol::DaemonMessage::SessionListReply` and mapped at the app seam
/// (`docs/spec-session-switch.md`). Every arrival replaces the whole list
/// (replace semantics, like the layout stream); which session THIS client is
/// attached to is not carried here — the layout snapshot's `session` string
/// owns that (the truthful-indicator contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListItem {
    /// tmux's `$<n>` session id (`rift_protocol::SessionEntry::id`) — the
    /// rename-stable target for rename/kill commands (`docs/spec-session-management.md`),
    /// unlike `name`, which changes on rename.
    pub id: u32,
    pub name: String,
    /// The session's window count (`#{session_windows}`).
    pub windows: u32,
    /// Whether at least one client is attached to this session.
    pub attached: bool,
}

/// A cockpit switch emitted by the session switcher: re-attach this client to
/// `session` (the daemon's attach-or-create `new-session -A -s <name>`, so a
/// fresh name creates the session). `size` is the client's current grid,
/// re-asserted by the bridge task strictly after the `Attach` so the fresh
/// control child reflows to the live viewport instead of the tmux default —
/// the render side cannot re-send it itself, its resize channel only fires on
/// a size *change*. Inert on the legacy tmux path (`RIFT_TERMINAL_LEGACY`):
/// the receiver drops there, so a switch request goes nowhere (the legacy
/// path is slated for removal, #285).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSwitchRequest {
    pub session: String,
    pub size: TermSize,
}

/// A session-order mutation emitted by the title-bar strip (#686,
/// `docs/spec-session-management.md`): a drag-to-reorder commit resequences
/// the whole visible session list; a chip rename additionally renames the
/// order-store's key so the reordered slot survives the rename (only an
/// external CLI rename re-slots — the store's own self-healing rule). Routed
/// to `rift-app`'s session-order store via [`TerminalHandle::session_order_rx`];
/// the store's mutation, persistence, and the resulting render-time re-sort
/// are all `rift-app`'s concern — `rift-terminal` only emits the mutation and
/// never depends on `rift-app` (crate boundary,
/// `docs/constitution.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOrderUpdate {
    /// Replace the stored order with this full sequence of session names —
    /// the user's new drag-committed order (a total order, not a subset).
    Reorder(Vec<String>),
    /// Rename the order-store's key for a session (`old` -> `new`) so its
    /// slot survives an in-UI rename.
    Rename { old: String, new: String },
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

/// The SSH/tmux session lifecycle state, surfaced by the statusbar connection
/// indicator (dot colors per the design contract: connected = success,
/// reconnecting = warning, not connected = muted). Driven by the SSH session
/// thread (not polled) — see `docs/spec-connection-robustness.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    /// Mid-session daemon-stream recovery while SSH itself is up (#475):
    /// renders the warning dot only, no banner.
    Reconnecting,
    /// The SSH-level reconnect loop (#476): SSH itself dropped and the engine
    /// retries forever under jittered capped backoff. Renders the warning dot
    /// plus the danger banner carrying this 1-based retry counter and the
    /// Cancel action.
    SshReconnecting {
        retry: u32,
    },
    /// A visible not-connected end state (orderly tmux exit, canceled
    /// reconnect, or a non-retryable auth/config failure) — never an app
    /// quit. The Connection screen (#477) will own this state once it lands.
    Disconnected,
}

impl ConnectionStatus {
    /// The status-dot label and theme color for this state — shared by
    /// `SessionView`'s own statusbar and the title bar's connection group
    /// (#511, `docs/spec-cockpit-chrome.md`), so the two render the identical
    /// mapping instead of drifting apart.
    pub fn status_dot(self, cx: &gpui::App) -> (&'static str, gpui::Hsla) {
        use gpui_component::ActiveTheme as _;

        match self {
            ConnectionStatus::Connecting => ("connecting", cx.theme().warning),
            ConnectionStatus::Connected => ("connected", cx.theme().success),
            ConnectionStatus::Reconnecting | ConnectionStatus::SshReconnecting { .. } => {
                ("reconnecting", cx.theme().warning)
            }
            ConnectionStatus::Disconnected => ("disconnected", cx.theme().muted_foreground),
        }
    }
}

/// Switches to the Nth window (1-based, by statusbar tab order) of the active
/// tmux session. Bound to `Alt+1..9` in the app and dispatched to [`SessionView`]
/// through the GPUI action system, so the chord is intercepted before the
/// keystroke reaches the PTY. `Alt+digit` (rather than `Ctrl+Shift+digit`) is
/// deliberate: GPUI normalizes shifted digits to their layout symbol and strips
/// the shift modifier, and on Linux/X11 the keyboard mapper is a no-op, so a
/// `ctrl-shift-1` binding cannot match there. An unshifted modifier+digit needs
/// no layout mapping and matches identically on Windows and Linux.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectWindow(pub usize);

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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use gpui_component::ActiveTheme as _;

    #[gpui::test]
    fn test_status_dot_connected_reads_success_label_and_color(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let (label, color) = ConnectionStatus::Connected.status_dot(cx);
            assert_eq!(label, "connected");
            assert_eq!(color, cx.theme().success);
        });
    }

    #[gpui::test]
    fn test_status_dot_connecting_reads_warning_label_and_color(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let (label, color) = ConnectionStatus::Connecting.status_dot(cx);
            assert_eq!(label, "connecting");
            assert_eq!(color, cx.theme().warning);
        });
    }

    #[gpui::test]
    fn test_status_dot_reconnecting_variants_read_warning_label_and_color(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);

            let (label, color) = ConnectionStatus::Reconnecting.status_dot(cx);
            assert_eq!(label, "reconnecting");
            assert_eq!(color, cx.theme().warning);

            let (label, color) = ConnectionStatus::SshReconnecting { retry: 3 }.status_dot(cx);
            assert_eq!(label, "reconnecting");
            assert_eq!(color, cx.theme().warning);
        });
    }

    #[gpui::test]
    fn test_status_dot_disconnected_reads_muted_label_and_color(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let (label, color) = ConnectionStatus::Disconnected.status_dot(cx);
            assert_eq!(label, "disconnected");
            assert_eq!(color, cx.theme().muted_foreground);
        });
    }
}
