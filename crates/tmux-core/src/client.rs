use std::collections::VecDeque;

use crate::event::Event;
use crate::parser;

/// Connection lifecycle of a control-mode client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    /// No control-mode message parsed yet.
    #[default]
    Connecting,
    /// The control stream is live (at least one message parsed).
    Ready,
    /// tmux sent `%exit`; the session is gone. Terminal — never reverts.
    Exited,
}

/// Correlation handle returned by [`Client::send_command`], matched back to the
/// resulting [`Event::CommandReply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandId(pub u64);

/// A command ready to write to tmux's control-mode stdin, paired with the
/// [`CommandId`] that will tag its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: CommandId,
    pub bytes: Vec<u8>,
}

/// In-flight `%begin` … `%end`/`%error` block.
#[derive(Debug)]
struct Block {
    number: u64,
    /// `%begin` flags non-zero: this block answers a command we sent.
    client_issued: bool,
    output: Vec<String>,
}

/// rift's tmux control-mode client: a byte-oriented parser for the notification
/// stream and command guards, with FIFO response correlation and connection
/// state. Pure (no I/O) — feed it the bytes tmux writes, write
/// [`Command::bytes`] back to tmux; the daemon owns the transport.
///
/// Command replies correlate to [`send_command`](Client::send_command) calls by
/// order: tmux processes a control client's commands in order and echoes their
/// `%begin`/`%end` blocks in the same order, so the oldest unanswered command
/// owns the next client-issued reply. Blocks tmux issues itself (e.g. on
/// attach) carry zero `%begin` flags and never consume a pending command.
#[derive(Debug, Default)]
pub struct Client {
    line_buf: Vec<u8>,
    block: Option<Block>,
    pending: VecDeque<CommandId>,
    next_command_id: u64,
    state: ConnectionState,
}

impl Client {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Queue a control-mode command. Returns the newline-terminated bytes to
    /// write to tmux stdin and the [`CommandId`] that will tag the matching
    /// [`Event::CommandReply`]. The command line is sent verbatim; callers must
    /// quote tmux format arguments — `'…#{pane_id}…'` — because the control
    /// parser treats an unquoted `#` as a comment (`docs/tmux-reference.md`).
    pub fn send_command(&mut self, command: &str) -> Command {
        let id = CommandId(self.next_command_id);
        self.next_command_id += 1;
        self.pending.push_back(id);
        let mut bytes = Vec::with_capacity(command.len() + 1);
        bytes.extend_from_slice(command.as_bytes());
        bytes.push(b'\n');
        Command { id, bytes }
    }

    /// Feed bytes read from tmux's control-mode stdout. Returns every complete
    /// message parsed; a trailing partial line is buffered for the next call,
    /// so callers may split reads anywhere (mid-line, mid-escape).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        self.line_buf.extend_from_slice(bytes);
        let mut consumed = 0;
        while let Some(rel) = self.line_buf[consumed..].iter().position(|&b| b == b'\n') {
            let end = consumed + rel;
            // Own the line so `self` is free to mutate while processing it.
            let mut line = self.line_buf[consumed..end].to_vec();
            consumed = end + 1;
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut events);
        }
        if consumed > 0 {
            self.line_buf.drain(..consumed);
        }
        self.advance_state(&events);
        events
    }

    fn process_line(&mut self, line: &[u8], events: &mut Vec<Event>) {
        if self.block.is_some() {
            self.process_block_line(line, events);
            return;
        }
        if let Some(rest) = line.strip_prefix(b"%begin ") {
            if let Some((number, flags)) = parser::parse_guard(rest) {
                self.block = Some(Block {
                    number,
                    client_issued: flags != 0,
                    output: Vec::new(),
                });
            }
            return;
        }
        // A closing guard with no open block is malformed; drop it rather than
        // mis-parsing it as a notification.
        if line.starts_with(b"%end ") || line.starts_with(b"%error ") {
            return;
        }
        if let Some(event) = parser::parse_notification(line) {
            events.push(event);
        }
    }

    fn process_block_line(&mut self, line: &[u8], events: &mut Vec<Event>) {
        let block_number = self.block.as_ref().map(|b| b.number);
        // The block closes on the first %end/%error whose command number
        // matches the opening %begin; requiring the match keeps command output
        // that happens to start with %end/%error from closing the block early.
        let closed_with_error = if let Some(rest) = line.strip_prefix(b"%end ") {
            parser::parse_guard(rest)
                .filter(|&(n, _)| Some(n) == block_number)
                .map(|_| false)
        } else if let Some(rest) = line.strip_prefix(b"%error ") {
            parser::parse_guard(rest)
                .filter(|&(n, _)| Some(n) == block_number)
                .map(|_| true)
        } else {
            None
        };

        match closed_with_error {
            Some(error) => {
                let block = self
                    .block
                    .take()
                    .expect("block present inside block handler");
                let id = if block.client_issued {
                    self.pending.pop_front()
                } else {
                    None
                };
                events.push(Event::CommandReply {
                    id,
                    error,
                    output: block.output,
                });
            }
            None => {
                if let Some(block) = self.block.as_mut() {
                    block
                        .output
                        .push(String::from_utf8_lossy(line).into_owned());
                }
            }
        }
    }

    fn advance_state(&mut self, events: &[Event]) {
        for event in events {
            match event {
                Event::Exit { .. } => self.state = ConnectionState::Exited,
                _ if self.state == ConnectionState::Connecting => {
                    self.state = ConnectionState::Ready;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_starts_connecting() {
        assert_eq!(Client::new().state(), ConnectionState::Connecting);
    }

    #[test]
    fn test_send_command_assigns_sequential_ids_and_newline_terminates() {
        let mut client = Client::new();
        let first = client.send_command("list-panes");
        let second = client.send_command("kill-server");
        assert_eq!(first.id, CommandId(0));
        assert_eq!(second.id, CommandId(1));
        assert_eq!(first.bytes, b"list-panes\n".to_vec());
        assert_eq!(second.bytes, b"kill-server\n".to_vec());
    }

    #[test]
    fn test_feed_parses_a_single_notification() {
        let mut client = Client::new();
        let events = client.feed(b"%output %0 hi\n");
        assert_eq!(
            events,
            vec![Event::Output {
                pane: 0,
                data: b"hi".to_vec(),
            }]
        );
        assert_eq!(client.state(), ConnectionState::Ready);
    }

    #[test]
    fn test_feed_buffers_partial_line_across_calls() {
        let mut client = Client::new();
        assert_eq!(client.feed(b"%out"), Vec::new());
        assert_eq!(client.feed(b"put %2 ab"), Vec::new());
        assert_eq!(
            client.feed(b"c\n"),
            vec![Event::Output {
                pane: 2,
                data: b"abc".to_vec(),
            }]
        );
    }

    #[test]
    fn test_feed_correlates_client_command_reply() {
        let mut client = Client::new();
        let cmd = client.send_command("list-panes");
        let events = client.feed(b"%begin 100 7 1\n0: [80x24] %0 (active)\n%end 100 7 1\n");
        assert_eq!(
            events,
            vec![Event::CommandReply {
                id: Some(cmd.id),
                error: false,
                output: vec!["0: [80x24] %0 (active)".to_owned()],
            }]
        );
    }

    #[test]
    fn test_feed_error_block_reports_error() {
        let mut client = Client::new();
        let cmd = client.send_command("bogus");
        let events =
            client.feed(b"%begin 100 8 1\nparse error: unknown command: bogus\n%error 100 8 1\n");
        assert_eq!(
            events,
            vec![Event::CommandReply {
                id: Some(cmd.id),
                error: true,
                output: vec!["parse error: unknown command: bogus".to_owned()],
            }]
        );
    }

    #[test]
    fn test_feed_server_internal_block_has_no_command_id() {
        // The attach-time block carries zero %begin flags and must not consume
        // a pending command.
        let mut client = Client::new();
        let cmd = client.send_command("list-panes");
        let events = client.feed(b"%begin 1 5 0\n%end 1 5 0\n");
        assert_eq!(
            events,
            vec![Event::CommandReply {
                id: None,
                error: false,
                output: Vec::new(),
            }]
        );
        // The pending command is still unanswered: its reply correlates next.
        let next = client.feed(b"%begin 2 6 1\n%end 2 6 1\n");
        assert_eq!(
            next,
            vec![Event::CommandReply {
                id: Some(cmd.id),
                error: false,
                output: Vec::new(),
            }]
        );
    }

    #[test]
    fn test_feed_block_output_containing_percent_lines_is_not_closed_early() {
        let mut client = Client::new();
        let cmd = client.send_command("show-something");
        // An output line that itself starts with %end but with a different
        // command number must be treated as data, not as the closing guard.
        let events = client.feed(b"%begin 100 9 1\n%end 1 1 0\nreal data\n%end 100 9 1\n");
        assert_eq!(
            events,
            vec![Event::CommandReply {
                id: Some(cmd.id),
                error: false,
                output: vec!["%end 1 1 0".to_owned(), "real data".to_owned()],
            }]
        );
    }

    #[test]
    fn test_feed_exit_sets_terminal_state() {
        let mut client = Client::new();
        let events = client.feed(b"%window-add @0\n%exit\n");
        assert_eq!(
            events,
            vec![Event::WindowAdd { window: 0 }, Event::Exit { reason: None },]
        );
        assert_eq!(client.state(), ConnectionState::Exited);
    }

    #[test]
    fn test_feed_carriage_return_line_endings_are_trimmed() {
        let mut client = Client::new();
        let events = client.feed(b"%window-add @1\r\n");
        assert_eq!(events, vec![Event::WindowAdd { window: 1 }]);
    }

    #[test]
    fn test_feed_split_multibyte_output_reassembles_by_concatenation() {
        // Each %output decodes independently; concatenating the payloads of the
        // two notifications recovers the 'ä' (0xC3 0xA4) split across them.
        let mut client = Client::new();
        let events = client.feed(
            &[
                b"%output %0 ".as_slice(),
                &[0xc3],
                b"\n%output %0 ".as_slice(),
                &[0xa4],
                b"\n".as_slice(),
            ]
            .concat(),
        );
        let payload: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                Event::Output { pane: 0, data } => Some(data.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(payload, vec![0xc3, 0xa4]);
    }
}
