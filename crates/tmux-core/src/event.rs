use crate::client::CommandId;

/// A parsed tmux control-mode message.
///
/// Notifications map one-to-one to tmux's `%`-prefixed lines. The command
/// output framed by the `%begin`/`%end`/`%error` guards is collapsed into a
/// single [`Event::CommandReply`], correlated back to the originating
/// [`Client::send_command`](crate::Client::send_command) through its
/// [`CommandId`]. Any `%`-notification outside the modeled set is preserved
/// verbatim as [`Event::Other`] so the stream never desyncs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `%output %<pane> <data>` — pane bytes with octal escapes decoded. The
    /// bytes are opaque (agent-agnostic): VTE interpretation stays client-side.
    Output { pane: u32, data: Vec<u8> },
    /// `%layout-change @<window> <layout> [<visible_layout>] [<flags>]`. The
    /// layout strings are kept raw; geometry parsing is out of scope here.
    LayoutChange {
        window: u32,
        layout: String,
        visible_layout: Option<String>,
        flags: Option<String>,
    },
    /// `%window-add @<window>`.
    WindowAdd { window: u32 },
    /// `%window-close @<window>` — also tmux's `%unlinked-window-close`, which
    /// carries the same payload and the same meaning to a client.
    WindowClose { window: u32 },
    /// `%session-changed $<session> <name>`.
    SessionChanged { session: u32, name: String },
    /// `%pane-mode-changed %<pane>` — the pane entered or left copy mode.
    PaneModeChanged { pane: u32 },
    /// A completed command block (`%begin` … `%end`/`%error`). `id` is the
    /// correlated [`CommandId`] for a reply to one of this client's commands,
    /// or `None` for a block tmux issues itself (e.g. the one on attach).
    /// `error` is `true` when the block closed with `%error`. `output` holds
    /// the block's response lines in order.
    CommandReply {
        id: Option<CommandId>,
        error: bool,
        output: Vec<String>,
    },
    /// `%exit [<reason>]` — the tmux server is gone.
    Exit { reason: Option<String> },
    /// Any other `%`-notification, kept verbatim (`name` includes the leading
    /// `%`; `args` is the remainder) so an unmodeled message never desyncs the
    /// stream.
    Other { name: String, args: String },
}
