//! Per-client tmux control-mode attach.
//!
//! Each connected rift client gets its own `tmux -C` child (attach-or-create for
//! the requested session), driven by rift's own control-mode client
//! ([`rift_tmux_core::Client`]). Notifications are routed onto that connection's
//! own outbound stream — `%output` → [`DaemonMessage::PaneOutput`], structural
//! changes → [`DaemonMessage::LayoutSnapshot`] (once, on attach) /
//! [`DaemonMessage::LayoutUpdate`] — and the reverse path (input, resize, raw
//! commands) is translated back into tmux commands. Killing the child detaches
//! only this client; the tmux server and session persist for other clients and
//! for reconnect. tmux server exit surfaces as [`DaemonMessage::TerminalExit`],
//! never a daemon crash.
//!
//! Flow control runs on both legs: tmux's per-pane `pause-after` bounds the
//! tmux→daemon buffer (a flooding pane is paused, others keep streaming), and a
//! bounded daemon→client channel backpressures the read loop; paused panes are
//! resumed as soon as that channel has room, so the loop never deadlocks.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use rift_protocol::{ClientMessage, DaemonMessage, PaneLayout, WindowLayout};
use rift_tmux_core::{Client, CommandId, Event};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;

/// Seconds of buffered pane output tmux tolerates before pausing that pane
/// (`refresh-client -f pause-after=N`). Small, so a flood is bounded tightly.
const PAUSE_AFTER_SECS: u32 = 1;

/// Read-chunk size for the control-mode stdout. The [`Client`] reassembles
/// lines regardless of chunk boundaries, so this only trades syscalls for memory.
const TERM_READ_BUFFER: usize = 16 * 1024;

/// How long to wait for a detached child to exit before force-killing it.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// How often to retry resuming paused panes. A paused pane emits no output, so
/// the resume cannot be driven off the output path — this poll closes the loop
/// once the outbound channel drains, with imperceptible latency.
const RESUME_POLL: Duration = Duration::from_millis(100);

/// One query that rebuilds the entire session layout. `-s` covers every window;
/// the format is one line per pane with the window fields repeated. `window_name`
/// is last so a name containing spaces stays in the final field (see
/// [`parse_layout_line`]). The `#{...}` formats are single-quoted because the
/// control parser treats an unquoted `#` as a comment (tmux-reference pitfall 9).
const LAYOUT_QUERY: &str = "list-panes -s -F '#{window_id} #{window_active} #{pane_id} #{pane_active} #{pane_left} #{pane_top} #{pane_width} #{pane_height} #{window_name}'";

/// Signals that the connection's outbound channel closed — the client is gone.
struct Closed;

/// Outcome of feeding one stdout chunk: keep going, or the tmux server exited.
enum Flow {
    Continue,
    Exited(Option<String>),
}

/// Drive one connection's terminal path until its inbound channel closes.
///
/// `inbound` carries the connection's terminal `ClientMessage`s (attach, input,
/// resize, raw command); `outbound` is the connection's bounded event channel,
/// drained by the socket writer. `server_socket` selects the tmux server: `None`
/// uses the default server (production); tests pass a unique `-L` name to isolate.
pub(crate) async fn terminal_task(
    mut inbound: mpsc::Receiver<ClientMessage>,
    outbound: mpsc::Sender<DaemonMessage>,
    server_socket: Option<String>,
) {
    let mut attach: Option<Attach> = None;
    let mut buf = vec![0u8; TERM_READ_BUFFER];
    let mut resume_poll = tokio::time::interval(RESUME_POLL);
    resume_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // Only read stdout while attached; otherwise this branch never fires.
            result = read_stdout(&mut attach, &mut buf), if attach.is_some() => match result {
                Ok(0) | Err(_) => terminal_down(&mut attach, &outbound, None).await,
                Ok(n) => {
                    let outcome = match attach.as_mut() {
                        Some(a) => a.process(&buf[..n], &outbound).await,
                        None => continue,
                    };
                    match outcome {
                        Ok(Flow::Continue) => {}
                        Ok(Flow::Exited(reason)) => terminal_down(&mut attach, &outbound, reason).await,
                        Err(Closed) => {
                            detach(&mut attach).await;
                            break;
                        }
                    }
                }
            },
            msg = inbound.recv() => match msg {
                Some(ClientMessage::Attach { session }) => {
                    // Re-attach: tear the current child down before opening anew.
                    detach(&mut attach).await;
                    attach = open_attach(session, server_socket.as_deref(), &outbound).await;
                }
                Some(other) => {
                    if let Some(a) = attach.as_mut() {
                        if a.handle_client_message(other).await.is_err() {
                            terminal_down(&mut attach, &outbound, None).await;
                        }
                    }
                    // Input before an attach has no target; drop it.
                }
                None => {
                    // The connection closed: detach this client's child (the tmux
                    // session persists), then end the task.
                    detach(&mut attach).await;
                    break;
                }
            },
            _ = resume_poll.tick() => {
                if let Some(a) = attach.as_mut() {
                    // Resume at most as many panes as the outbound channel has
                    // free slots, so paused panes don't all un-pause into one
                    // slot and immediately re-flood.
                    let room = outbound.capacity();
                    if room > 0 && !a.paused.is_empty() {
                        a.resume_paused(room).await;
                    }
                }
            }
        }
    }
}

/// Read into `buf` from the attach's stdout, or pend forever when not attached
/// (the `select!` guard keeps this branch off, but pending is safe regardless).
async fn read_stdout(attach: &mut Option<Attach>, buf: &mut [u8]) -> std::io::Result<usize> {
    match attach {
        Some(a) => a.stdout.read(buf).await,
        None => std::future::pending().await,
    }
}

/// Detach the current child (if any) without surfacing a path-down event — used
/// on re-attach and on a clean connection close.
async fn detach(attach: &mut Option<Attach>) {
    if let Some(a) = attach.take() {
        a.shutdown().await;
    }
}

/// Detach the current child and surface the terminal-path-down event to the
/// client. The daemon stays up — only this attach ended.
async fn terminal_down(
    attach: &mut Option<Attach>,
    outbound: &mpsc::Sender<DaemonMessage>,
    reason: Option<String>,
) {
    if let Some(a) = attach.take() {
        let session = a.session.clone();
        a.shutdown().await;
        let _ = outbound
            .send(DaemonMessage::TerminalExit { session, reason })
            .await;
    }
}

/// Spawn the attach, or surface a path-down if the spawn fails (tmux missing,
/// etc.) so the client learns the terminal is unavailable rather than hanging.
async fn open_attach(
    session: String,
    server_socket: Option<&str>,
    outbound: &mpsc::Sender<DaemonMessage>,
) -> Option<Attach> {
    match Attach::spawn(session.clone(), server_socket).await {
        Ok(attach) => Some(attach),
        Err(err) => {
            eprintln!("rift-daemon: tmux attach for session {session} failed: {err}");
            let _ = outbound
                .send(DaemonMessage::TerminalExit {
                    session,
                    reason: Some(err.to_string()),
                })
                .await;
            None
        }
    }
}

/// One client's live tmux control-mode attach.
struct Attach {
    session: String,
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    client: Client,
    /// Set once the attach-time [`DaemonMessage::LayoutSnapshot`] has been sent;
    /// every later layout query result is a [`DaemonMessage::LayoutUpdate`].
    snapshot_sent: bool,
    /// The in-flight layout query (at most one), correlated by [`CommandId`].
    layout_query: Option<CommandId>,
    /// A structural change arrived while a layout query was in flight; re-query
    /// once it returns so a burst of changes collapses to bounded queries.
    layout_dirty: bool,
    /// Panes tmux has paused (flow control); resumed once the outbound channel
    /// has room.
    paused: HashSet<u32>,
    /// In-flight `capture-pane` queries, mapping each command's [`CommandId`] to
    /// the pane it captures, so the reply is forwarded as a
    /// [`DaemonMessage::PaneCapture`] for that pane. Several may be in flight at
    /// once (one per pane); each is removed when its reply arrives.
    captures: HashMap<CommandId, u32>,
}

impl Attach {
    async fn spawn(session: String, server_socket: Option<&str>) -> anyhow::Result<Self> {
        let mut command = tokio::process::Command::new("tmux");
        if let Some(socket) = server_socket {
            command.args(["-L", socket]);
        }
        command
            .args(["-C", "new-session", "-A", "-s", &session])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Reap the control child if the Attach is dropped without shutdown —
            // a safety net; the normal path detaches gracefully.
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("spawn tmux -C control-mode child")?;
        let stdin = child.stdin.take().context("tmux child stdin missing")?;
        let stdout = child.stdout.take().context("tmux child stdout missing")?;

        let mut attach = Attach {
            session,
            child,
            stdin,
            stdout,
            client: Client::new(),
            snapshot_sent: false,
            layout_query: None,
            layout_dirty: false,
            paused: HashSet::new(),
            captures: HashMap::new(),
        };
        // Enable tmux's per-pane flow control for this attach (tmux→daemon leg).
        attach
            .send_command(&format!("refresh-client -f pause-after={PAUSE_AFTER_SECS}"))
            .await?;
        // The task loop already reads stdout (we are subscribed), so any change
        // after this query lands as a live LayoutUpdate — no gap; the snapshot is
        // the current state, updates replace wholesale, so no duplicate either.
        let id = attach.send_command(LAYOUT_QUERY).await?;
        attach.layout_query = Some(id);
        Ok(attach)
    }

    /// Emit a control-mode command and return its correlation id. All commands
    /// go through the [`Client`] so reply guards stay matched to the FIFO queue.
    async fn send_command(&mut self, command: &str) -> std::io::Result<CommandId> {
        let queued = self.client.send_command(command);
        self.stdin.write_all(&queued.bytes).await?;
        self.stdin.flush().await?;
        Ok(queued.id)
    }

    /// Translate one reverse-path client message into a tmux command.
    async fn handle_client_message(&mut self, msg: ClientMessage) -> std::io::Result<()> {
        match msg {
            ClientMessage::Input { pane_id, data } if !data.is_empty() => {
                // Forward opaque bytes to the pane (agent-agnostic): hex via
                // `send-keys -H`, so control bytes survive the command line.
                let line = format!("send-keys -t %{pane_id} -H {}", hex_bytes(data.as_bytes()));
                self.send_command(&line).await?;
            }
            ClientMessage::ResizePane { cols, rows, .. } => {
                // The client's viewport resized: set this control client's size so
                // tmux reflows (refresh-client -C; tmux-reference). A control
                // client has one viewport, so pane_id is not needed here.
                self.send_command(&format!("refresh-client -C {cols}x{rows}"))
                    .await?;
            }
            ClientMessage::TmuxCommand { cmd } => {
                self.send_command(&cmd).await?;
            }
            ClientMessage::CapturePane {
                pane_id,
                start,
                end,
                join,
            } => {
                // Bounded scrollback capture. `-e` preserves ANSI (color parity
                // with the live stream); `-J` rejoins soft-wrapped rows. The
                // reply's output is forwarded as a PaneCapture for this pane,
                // correlated by the returned CommandId. `start`/`end` are tmux
                // `-S`/`-E` line addresses (may begin with `-`), passed as their
                // own tokens; the pane target is `%<id>`.
                let join_flag = if join { " -J" } else { "" };
                let command =
                    format!("capture-pane -p -e{join_flag} -S {start} -E {end} -t %{pane_id}");
                let id = self.send_command(&command).await?;
                self.captures.insert(id, pane_id);
            }
            // Empty input is a no-op; Attach is handled by the task; Hello never
            // reaches the terminal task. The buffer-channel requests
            // (`OpenFile`/`SaveFile`) and the live-buffer feed (`BufferChanged`/
            // `BufferClosed`, #189) are not terminal messages — they are routed to
            // the per-connection buffer service or the shared LSP loop, not here.
            ClientMessage::Input { .. }
            | ClientMessage::Attach { .. }
            | ClientMessage::OpenFile { .. }
            | ClientMessage::SaveFile { .. }
            | ClientMessage::BufferChanged { .. }
            | ClientMessage::BufferClosed { .. }
            | ClientMessage::Hello { .. } => {}
        }
        Ok(())
    }

    /// Feed one stdout chunk through the parser and route the resulting events.
    async fn process(
        &mut self,
        bytes: &[u8],
        outbound: &mpsc::Sender<DaemonMessage>,
    ) -> Result<Flow, Closed> {
        for event in self.client.feed(bytes) {
            match event {
                Event::Output { pane, data } => {
                    outbound
                        .send(DaemonMessage::PaneOutput {
                            pane_id: pane,
                            bytes: data,
                        })
                        .await
                        .map_err(|_| Closed)?;
                }
                Event::LayoutChange { .. }
                | Event::WindowAdd { .. }
                | Event::WindowClose { .. } => {
                    self.request_layout().await;
                }
                Event::CommandReply { id, error, output } => {
                    if id.is_some() && id == self.layout_query {
                        self.layout_query = None;
                        if !error {
                            let windows = parse_layout(&output);
                            let message = if self.snapshot_sent {
                                DaemonMessage::LayoutUpdate {
                                    session: self.session.clone(),
                                    windows,
                                }
                            } else {
                                self.snapshot_sent = true;
                                DaemonMessage::LayoutSnapshot {
                                    session: self.session.clone(),
                                    windows,
                                }
                            };
                            outbound.send(message).await.map_err(|_| Closed)?;
                        }
                        if self.layout_dirty {
                            self.layout_dirty = false;
                            self.request_layout().await;
                        }
                    } else if let Some(pane) = id.and_then(|id| self.captures.remove(&id)) {
                        // A `capture-pane` reply: forward the captured bytes (empty
                        // on a capture error, so the client clears its in-flight
                        // flag and can retry). The output lines are already
                        // tmux-decoded by the Client's command-block decode.
                        let bytes = if error {
                            Vec::new()
                        } else {
                            join_capture(&output)
                        };
                        outbound
                            .send(DaemonMessage::PaneCapture {
                                pane_id: pane,
                                bytes,
                            })
                            .await
                            .map_err(|_| Closed)?;
                    }
                    // Other replies are acks for input/resize/raw commands — the
                    // Client already consumed their guards; nothing to forward.
                }
                Event::Exit { reason } => return Ok(Flow::Exited(reason)),
                Event::Other { name, args } if name == "%pause" => {
                    if let Some(pane) = parse_pane_arg(&args) {
                        self.paused.insert(pane);
                    }
                }
                Event::Other { name, args } if name == "%continue" => {
                    if let Some(pane) = parse_pane_arg(&args) {
                        self.paused.remove(&pane);
                    }
                }
                Event::Other { .. }
                | Event::SessionChanged { .. }
                | Event::PaneModeChanged { .. } => {}
            }
        }
        Ok(Flow::Continue)
    }

    /// Issue a layout query, coalescing so at most one is in flight.
    async fn request_layout(&mut self) {
        if self.layout_query.is_some() {
            self.layout_dirty = true;
            return;
        }
        match self.send_command(LAYOUT_QUERY).await {
            Ok(id) => self.layout_query = Some(id),
            Err(err) => eprintln!("rift-daemon: layout query failed: {err}"),
        }
    }

    /// Resume up to `limit` paused panes (tmux flow control). Bounding to the
    /// outbound channel's free slots avoids a thundering herd: with many panes
    /// paused and little room, only as many as can be absorbed un-pause now; the
    /// rest wait for the next poll once more room frees.
    async fn resume_paused(&mut self, limit: usize) {
        let to_resume: Vec<u32> = self.paused.iter().copied().take(limit).collect();
        for pane in to_resume {
            self.paused.remove(&pane);
            if let Err(err) = self
                .send_command(&format!("refresh-client -A '%{pane}:continue'"))
                .await
            {
                eprintln!("rift-daemon: resume pane %{pane} failed: {err}");
            }
        }
    }

    /// Detach this control client: ask tmux to detach (graceful — an abrupt kill
    /// can crash tmux, tmux-reference pitfall 5), close stdin, then reap within a
    /// grace window, force-killing only if it overruns. The server and session
    /// persist; this kills the client, not the server.
    async fn shutdown(mut self) {
        let _ = self.stdin.write_all(b"detach-client\n").await;
        let _ = self.stdin.flush().await;
        drop(self.stdin);
        match tokio::time::timeout(SHUTDOWN_GRACE, self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
            }
        }
    }
}

/// Render bytes as the space-separated lowercase hex `send-keys -H` consumes.
fn hex_bytes(data: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(data.len().saturating_mul(3));
    for (i, byte) in data.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Join a `capture-pane` reply's lines into the byte payload the client feeds to
/// its scrollback `Term`: one line per pane row, newline-separated (matching the
/// 1:1 row mapping the client's capture parser expects). The lines are already
/// tmux-decoded by the [`Client`]'s command-block decode.
fn join_capture(output: &[String]) -> Vec<u8> {
    output.join("\n").into_bytes()
}

/// Pull the `%<pane>` id out of a `%pause`/`%continue` notification argument.
fn parse_pane_arg(args: &str) -> Option<u32> {
    args.split_whitespace()
        .next()?
        .strip_prefix('%')?
        .parse()
        .ok()
}

/// Group [`LAYOUT_QUERY`] reply lines into per-window layouts, preserving the
/// order windows first appear and the order of panes within each.
fn parse_layout(lines: &[String]) -> Vec<WindowLayout> {
    let mut windows: Vec<WindowLayout> = Vec::new();
    for line in lines {
        let Some(parsed) = parse_layout_line(line) else {
            continue;
        };
        match windows.iter_mut().find(|w| w.window_id == parsed.window_id) {
            Some(window) => window.panes.push(parsed.pane),
            None => windows.push(WindowLayout {
                window_id: parsed.window_id,
                name: parsed.window_name,
                active: parsed.window_active,
                panes: vec![parsed.pane],
            }),
        }
    }
    windows
}

/// One parsed [`LAYOUT_QUERY`] line: the pane plus its window's identity.
struct ParsedPaneLine {
    window_id: u32,
    window_active: bool,
    window_name: String,
    pane: PaneLayout,
}

/// Parse one `@<win> <win_active> %<pane> <pane_active> <left> <top> <width>
/// <height> <name>` line; `splitn(9, ' ')` keeps a spaced window name intact.
fn parse_layout_line(line: &str) -> Option<ParsedPaneLine> {
    let mut fields = line.splitn(9, ' ');
    let window_id = fields.next()?.strip_prefix('@')?.parse().ok()?;
    let window_active = fields.next()? == "1";
    let pane_id = fields.next()?.strip_prefix('%')?.parse().ok()?;
    let pane_active = fields.next()? == "1";
    let left = fields.next()?.parse().ok()?;
    let top = fields.next()?.parse().ok()?;
    let width = fields.next()?.parse().ok()?;
    let height = fields.next()?.parse().ok()?;
    let window_name = fields.next().unwrap_or("").to_owned();
    Some(ParsedPaneLine {
        window_id,
        window_active,
        window_name,
        pane: PaneLayout {
            pane_id,
            active: pane_active,
            left,
            top,
            width,
            height,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_bytes_space_separated_lowercase() {
        assert_eq!(hex_bytes(b"ab\r"), "61 62 0d");
        assert_eq!(hex_bytes(&[0x1b, 0x00, 0xff]), "1b 00 ff");
        assert_eq!(hex_bytes(b""), "");
    }

    #[test]
    fn test_parse_pane_arg_extracts_pane_id() {
        assert_eq!(parse_pane_arg("%3"), Some(3));
        assert_eq!(parse_pane_arg("%12 extra"), Some(12));
        assert_eq!(parse_pane_arg("@3"), None);
        assert_eq!(parse_pane_arg(""), None);
    }

    #[test]
    fn test_parse_layout_line_full_fields() {
        let parsed = parse_layout_line("@0 1 %1 1 51 0 49 30 bash").expect("parse");
        assert_eq!(parsed.window_id, 0);
        assert!(parsed.window_active);
        assert_eq!(parsed.window_name, "bash");
        assert_eq!(
            parsed.pane,
            PaneLayout {
                pane_id: 1,
                active: true,
                left: 51,
                top: 0,
                width: 49,
                height: 30,
            }
        );
    }

    #[test]
    fn test_parse_layout_line_window_name_with_spaces_is_preserved() {
        let parsed = parse_layout_line("@2 0 %5 0 0 0 80 24 my project").expect("parse");
        assert_eq!(parsed.window_name, "my project");
        assert!(!parsed.window_active);
        assert!(!parsed.pane.active);
    }

    #[test]
    fn test_parse_layout_line_malformed_returns_none() {
        assert_eq!(
            parse_layout_line("0 1 %1 1 0 0 80 24 bash").map(|_| ()),
            None
        ); // window id no @
        assert_eq!(
            parse_layout_line("@0 1 1 1 0 0 80 24 bash").map(|_| ()),
            None
        ); // pane id no %
        assert_eq!(parse_layout_line("@0 1 %1 1 0 0 80").map(|_| ()), None); // too few fields
    }

    #[test]
    fn test_parse_layout_groups_panes_by_window_in_order() {
        let lines = vec![
            "@0 1 %0 0 0 0 50 30 editor".to_owned(),
            "@0 1 %1 1 51 0 49 30 editor".to_owned(),
            "@1 0 %2 1 0 0 100 30 logs".to_owned(),
        ];
        let windows = parse_layout(&lines);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].window_id, 0);
        assert_eq!(windows[0].name, "editor");
        assert!(windows[0].active);
        assert_eq!(windows[0].panes.len(), 2);
        assert_eq!(windows[0].panes[0].pane_id, 0);
        assert_eq!(windows[0].panes[1].pane_id, 1);
        assert!(windows[0].panes[1].active);
        assert_eq!(windows[1].window_id, 1);
        assert_eq!(windows[1].name, "logs");
        assert!(!windows[1].active);
        assert_eq!(windows[1].panes.len(), 1);
    }

    #[test]
    fn test_parse_layout_skips_malformed_lines() {
        let lines = vec![
            "garbage line".to_owned(),
            "@0 1 %0 1 0 0 80 24 bash".to_owned(),
        ];
        let windows = parse_layout(&lines);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].panes.len(), 1);
    }

    // --- real-tmux integration (#204) ---
    //
    // Each test drives `terminal_task` against an isolated tmux server (a unique
    // `-L` socket) so it never touches the developer's tmux, and tears that
    // server down at the end. Real tmux over real pipes — the constitution
    // prefers real fixtures to mocks.

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// A unique `-L` server name per test, plus teardown that kills that server.
    struct TmuxServer {
        name: String,
    }

    impl TmuxServer {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            TmuxServer {
                name: format!("rift204-{tag}-{}-{n}", std::process::id()),
            }
        }
    }

    impl Drop for TmuxServer {
        fn drop(&mut self) {
            // Best-effort: kill the isolated server so no stray tmux lingers.
            let _ = std::process::Command::new("tmux")
                .args(["-L", &self.name, "kill-server"])
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    /// Spawn `terminal_task` wired to `server`, returning the inbound sender and
    /// outbound receiver. `outbound_cap` bounds the daemon→client leg.
    fn spawn_task(
        server: &TmuxServer,
        outbound_cap: usize,
    ) -> (
        mpsc::Sender<ClientMessage>,
        mpsc::Receiver<DaemonMessage>,
        tokio::task::JoinHandle<()>,
    ) {
        let (in_tx, in_rx) = mpsc::channel(64);
        let (out_tx, out_rx) = mpsc::channel(outbound_cap);
        let socket = server.name.clone();
        let handle = tokio::spawn(terminal_task(in_rx, out_tx, Some(socket)));
        (in_tx, out_rx, handle)
    }

    /// Receive daemon messages until `pick` yields or the timeout elapses.
    async fn recv_until<T>(
        rx: &mut mpsc::Receiver<DaemonMessage>,
        secs: u64,
        mut pick: impl FnMut(&DaemonMessage) -> Option<T>,
    ) -> Option<T> {
        tokio::time::timeout(Duration::from_secs(secs), async {
            while let Some(msg) = rx.recv().await {
                if let Some(found) = pick(&msg) {
                    return Some(found);
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    #[tokio::test]
    async fn test_attach_streams_layout_snapshot_and_pane_output() {
        let server = TmuxServer::new("snap");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("send attach");

        // Collect until BOTH the attach snapshot and the pane's initial draw have
        // arrived (the draw can land before or after the snapshot, so don't
        // discard one waiting for the other).
        let mut snapshot_windows = None;
        let mut got_output = false;
        let both = recv_until(&mut out_rx, 10, |m| {
            match m {
                DaemonMessage::LayoutSnapshot { session, windows } if session == "rift" => {
                    snapshot_windows = Some(windows.clone());
                }
                DaemonMessage::PaneOutput { bytes, .. } if !bytes.is_empty() => got_output = true,
                _ => {}
            }
            (snapshot_windows.is_some() && got_output).then_some(())
        })
        .await;
        assert!(
            both.is_some(),
            "expected both a layout snapshot and pane output after attach"
        );
        let windows = snapshot_windows.expect("snapshot present");
        assert!(!windows.is_empty(), "snapshot has a window");
        assert!(!windows[0].panes.is_empty(), "window has a pane");

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_input_round_trips_to_pane() {
        let server = TmuxServer::new("input");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        let pane_id = {
            in_tx
                .send(ClientMessage::Attach {
                    session: "rift".to_owned(),
                })
                .await
                .expect("attach");
            let windows = recv_until(&mut out_rx, 10, |m| match m {
                DaemonMessage::LayoutSnapshot { windows, .. } => Some(windows.clone()),
                _ => None,
            })
            .await
            .expect("snapshot");
            windows[0].panes[0].pane_id
        };

        // Type a command that echoes a unique marker; expect it back as bytes.
        in_tx
            .send(ClientMessage::Input {
                pane_id,
                data: "printf RIFTMARKER204\n".to_owned(),
            })
            .await
            .expect("input");

        let mut seen = Vec::new();
        let found = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::PaneOutput { bytes, .. } => {
                seen.extend_from_slice(bytes);
                // Strip nothing — the marker bytes appear contiguously in the echo.
                if seen
                    .windows(b"RIFTMARKER204".len())
                    .any(|w| w == b"RIFTMARKER204")
                {
                    Some(())
                } else {
                    None
                }
            }
            _ => None,
        })
        .await;
        assert!(
            found.is_some(),
            "typed marker did not round-trip to the pane"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_capture_pane_returns_scrollback() {
        let server = TmuxServer::new("capture");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        let pane_id = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutSnapshot { windows, .. } => Some(windows[0].panes[0].pane_id),
            _ => None,
        })
        .await
        .expect("snapshot");

        // Print a unique marker so it lands in the pane's captured content.
        in_tx
            .send(ClientMessage::Input {
                pane_id,
                data: "printf 'RIFTCAPTURE777\\n'\n".to_owned(),
            })
            .await
            .expect("input");
        // Wait until the marker has been echoed live, so a capture now sees it.
        let mut live = Vec::new();
        recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::PaneOutput { bytes, .. } => {
                live.extend_from_slice(bytes);
                live.windows(b"RIFTCAPTURE777".len())
                    .any(|w| w == b"RIFTCAPTURE777")
                    .then_some(())
            }
            _ => None,
        })
        .await
        .expect("marker echoed live");

        // Capture the whole pane (history + visible) and assert the marker is in
        // the reply, correlated to this pane.
        in_tx
            .send(ClientMessage::CapturePane {
                pane_id,
                start: "-".to_owned(),
                end: "-".to_owned(),
                join: false,
            })
            .await
            .expect("capture");

        let captured = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::PaneCapture { pane_id: p, bytes } if *p == pane_id => {
                Some(bytes.clone())
            }
            _ => None,
        })
        .await
        .expect("pane capture reply");
        assert!(
            captured
                .windows(b"RIFTCAPTURE777".len())
                .any(|w| w == b"RIFTCAPTURE777"),
            "captured scrollback must contain the printed marker"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_layout_update_after_split() {
        let server = TmuxServer::new("split");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::LayoutSnapshot { .. }).then_some(())
        })
        .await
        .expect("snapshot");

        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "split-window -h".to_owned(),
            })
            .await
            .expect("split");

        let panes = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                let panes: usize = windows.iter().map(|w| w.panes.len()).sum();
                (panes >= 2).then_some(panes)
            }
            _ => None,
        })
        .await;
        assert!(panes.is_some(), "split must produce a 2-pane layout update");

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_disconnect_leaves_session_alive() {
        let server = TmuxServer::new("persist");

        // First client: attach, create a uniquely-named window, then disconnect.
        {
            let (in_tx, mut out_rx, task) = spawn_task(&server, 256);
            in_tx
                .send(ClientMessage::Attach {
                    session: "rift".to_owned(),
                })
                .await
                .expect("attach 1");
            recv_until(&mut out_rx, 10, |m| {
                matches!(m, DaemonMessage::LayoutSnapshot { .. }).then_some(())
            })
            .await
            .expect("snapshot 1");
            in_tx
                .send(ClientMessage::TmuxCommand {
                    cmd: "new-window -n persisted".to_owned(),
                })
                .await
                .expect("new-window");
            // Wait until the layout reflects the new window before disconnecting.
            recv_until(&mut out_rx, 10, |m| match m {
                DaemonMessage::LayoutUpdate { windows, .. } => {
                    windows.iter().any(|w| w.name == "persisted").then_some(())
                }
                _ => None,
            })
            .await
            .expect("layout shows new window");
            // Disconnect: dropping the inbound sender detaches this client's child.
            drop(in_tx);
            let _ = task.await;
        }

        // Second client: re-attach to the same session on the same server. The
        // window the first client made must still be there — the session
        // persisted across the per-client teardown.
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);
        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach 2");
        let persisted = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutSnapshot { windows, .. } => {
                windows.iter().any(|w| w.name == "persisted").then_some(())
            }
            _ => None,
        })
        .await;
        assert!(
            persisted.is_some(),
            "the session and its window must survive the first client's disconnect"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_server_exit_surfaces_terminal_exit() {
        let server = TmuxServer::new("exit");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::LayoutSnapshot { .. }).then_some(())
        })
        .await
        .expect("snapshot");

        // Kill the (isolated) tmux server: its exit must surface as a
        // terminal-path-down event, and the task must keep running (no panic).
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "kill-server".to_owned(),
            })
            .await
            .expect("kill-server");

        let exited = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::TerminalExit { session, .. } if session == "rift" => Some(()),
            _ => None,
        })
        .await;
        assert!(exited.is_some(), "server exit must surface as TerminalExit");
        assert!(
            !task.is_finished(),
            "the task survives a terminal path-down"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_flooding_pane_keeps_other_panes_streaming() {
        // A small outbound channel makes the bound observable: a flooding pane
        // cannot grow it (tmux pause-after backpressures), and a second pane's
        // marker still gets through — no starvation, bounded buffers.
        let server = TmuxServer::new("flood");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 8);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        // Resize the client larger so the split has room.
        in_tx
            .send(ClientMessage::ResizePane {
                pane_id: 0,
                cols: 120,
                rows: 40,
            })
            .await
            .expect("resize");
        let first_pane = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutSnapshot { windows, .. } => Some(windows[0].panes[0].pane_id),
            _ => None,
        })
        .await
        .expect("snapshot");

        // Split, then learn the second pane's id from the layout update.
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "split-window -h".to_owned(),
            })
            .await
            .expect("split");
        let second_pane = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                let ids: Vec<u32> = windows
                    .iter()
                    .flat_map(|w| w.panes.iter().map(|p| p.pane_id))
                    .collect();
                ids.iter().copied().find(|&id| id != first_pane)
            }
            _ => None,
        })
        .await
        .expect("second pane id");

        // Flood the first pane; the daemon must keep the second pane streaming.
        in_tx
            .send(ClientMessage::Input {
                pane_id: first_pane,
                data: "yes RIFTFLOOD\n".to_owned(),
            })
            .await
            .expect("flood input");
        in_tx
            .send(ClientMessage::Input {
                pane_id: second_pane,
                data: "printf RIFTQUIET204\n".to_owned(),
            })
            .await
            .expect("quiet input");

        // The quiet pane's marker must arrive despite the flood — drain the
        // (bounded) channel until it shows up.
        let mut quiet_bytes = Vec::new();
        let found = recv_until(&mut out_rx, 20, |m| match m {
            DaemonMessage::PaneOutput { pane_id, bytes } if *pane_id == second_pane => {
                quiet_bytes.extend_from_slice(bytes);
                quiet_bytes
                    .windows(b"RIFTQUIET204".len())
                    .any(|w| w == b"RIFTQUIET204")
                    .then_some(())
            }
            _ => None,
        })
        .await;
        assert!(
            found.is_some(),
            "the quiet pane's marker must stream through despite the flooding pane"
        );

        drop(in_tx);
        // Stop the flood promptly; the server drop kills it regardless.
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
}
