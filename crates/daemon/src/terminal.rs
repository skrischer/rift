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
use tracing::{error, warn};

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
            // Only poll while attached; otherwise this branch never fires. Stdout
            // reads and the status-line timer tick both need a mutable borrow of
            // `attach`, so they share one branch (`read_stdout_or_status_tick`)
            // rather than two separate `select!` arms, which the borrow checker
            // would reject as two live `&mut attach` borrows at once.
            event = read_stdout_or_status_tick(&mut attach, &mut buf), if attach.is_some() => match event {
                StdoutEvent::Read(Ok(0)) | StdoutEvent::Read(Err(_)) => terminal_down(&mut attach, &outbound, None).await,
                StdoutEvent::Read(Ok(n)) => {
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
                StdoutEvent::StatusInterval => {
                    if let Some(a) = attach.as_mut() {
                        if let Err(err) = a.request_status_line().await {
                            warn!(%err, "status-line refresh failed");
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

/// The outcome of [`read_stdout_or_status_tick`]: either a stdout read
/// completed, or the status-line mirror's `status-interval` timer ticked.
enum StdoutEvent {
    Read(std::io::Result<usize>),
    StatusInterval,
}

/// Read into `buf` from the attach's stdout, racing the attach's
/// `status-interval` re-fetch timer (if one is running) — combined into one
/// function so the outer `select!` needs only a single mutable borrow of
/// `attach` (`stdout` and `status_timer` are disjoint fields of the same
/// `Attach`, so borrowing both here is fine). Pends forever when not attached;
/// when the timer is disabled (`status-interval 0`) only the stdout read is
/// awaited — no busy poll.
async fn read_stdout_or_status_tick(attach: &mut Option<Attach>, buf: &mut [u8]) -> StdoutEvent {
    match attach {
        Some(a) => {
            let stdout = &mut a.stdout;
            match a.status_timer.as_mut() {
                Some(timer) => tokio::select! {
                    result = stdout.read(buf) => StdoutEvent::Read(result),
                    _ = timer.tick() => StdoutEvent::StatusInterval,
                },
                None => StdoutEvent::Read(stdout.read(buf).await),
            }
        }
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
            error!(%session, %err, "tmux attach failed");
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
    /// This attach's tmux session id, learned from the `%session-changed`
    /// tmux sends on attach. tmux broadcasts `%session-renamed` for ANY
    /// session on the server to every control client, so a rename is only
    /// adopted when its id matches this one; `None` until attach delivers it.
    session_id: Option<u32>,
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
    /// The in-flight `list-keys` + `show-options` query pair (at most one),
    /// for the tmux key-table mirror (`docs/spec-tmux-keytable-mirroring.md`).
    /// A newer request simply replaces this — a reply whose id no longer
    /// matches is dropped, which is fine since a fresher query is already
    /// in flight.
    key_table_query: Option<KeyTableQuery>,
    /// The in-flight `show-options -A` + two `display-message -p` query
    /// triple (at most one), for the tmux status-line mirror
    /// (`docs/spec-tmux-statusline-mirroring.md`). Same replace-on-newer-request
    /// convention as `key_table_query`.
    status_line_query: Option<StatusLineQuery>,
    /// The `status-interval` re-fetch timer, rebuilt whenever a fresh
    /// [`DaemonMessage::StatusLineReply`] reports a changed interval; `None`
    /// while the value is `0` (`status-interval 0` disables tmux's own draw
    /// timer, so the mirror's re-fetch timer is disabled too — no busy poll).
    status_timer: Option<tokio::time::Interval>,
    /// The `status-interval` value [`Attach::status_timer`] was last built
    /// from, so an unchanged value (the common case — most refreshes are not
    /// interval edits) skips rebuilding the timer.
    last_status_interval_secs: Option<u64>,
}

/// One in-flight `list-keys` + `show-options` round trip: both commands are
/// queued together, and each reply is stashed here until both have arrived, so
/// [`DaemonMessage::KeyTableReply`] always carries a coherent pair rather than
/// racing two separate messages.
struct KeyTableQuery {
    list_keys_id: CommandId,
    show_options_id: CommandId,
    list_keys_output: Option<Vec<String>>,
    show_options_output: Option<Vec<String>>,
}

/// One in-flight `show-options -A` + two `display-message -p` round trip: all
/// three commands are queued together, and each reply is stashed here until
/// all three have arrived, so [`DaemonMessage::StatusLineReply`] always
/// carries a coherent triple rather than racing separate messages.
struct StatusLineQuery {
    options_id: CommandId,
    status_left_id: CommandId,
    status_right_id: CommandId,
    options_output: Option<Vec<String>>,
    status_left_output: Option<Vec<String>>,
    status_right_output: Option<Vec<String>>,
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
            session_id: None,
            child,
            stdin,
            stdout,
            client: Client::new(),
            snapshot_sent: false,
            layout_query: None,
            layout_dirty: false,
            paused: HashSet::new(),
            captures: HashMap::new(),
            key_table_query: None,
            status_line_query: None,
            status_timer: None,
            last_status_interval_secs: None,
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
        // Key-table mirror: queried unprompted on attach/reconnect (the spec's
        // "on attach/reconnect" refresh trigger), same as the layout query above.
        attach.request_key_table().await?;
        // Status-line mirror: same unprompted attach/reconnect trigger; the
        // reply also seeds `status_timer` for the ongoing `status-interval`
        // cadence.
        attach.request_status_line().await?;
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
            ClientMessage::QueryKeyTable => {
                self.request_key_table().await?;
            }
            ClientMessage::QueryStatusLine => {
                self.request_status_line().await?;
            }
            // Empty input is a no-op; Attach is handled by the task; Hello never
            // reaches the terminal task. The buffer-channel requests
            // (`OpenFile`/`SaveFile`) and the live-buffer feed (`BufferChanged`/
            // `BufferClosed`, #189) are not terminal messages — they are routed to
            // the per-connection buffer service or the shared LSP loop, not here.
            // Navigation requests (hover/definition/references, #193) are handled
            // by the shared dispatch loop and LSP worker — never the terminal task.
            // `RequestDiff` (source-control diff, #335) is likewise not a
            // terminal message; its daemon-side handler lands in a follow-on
            // issue — until then it is silently dropped here too.
            ClientMessage::Input { .. }
            | ClientMessage::Attach { .. }
            | ClientMessage::OpenFile { .. }
            | ClientMessage::SaveFile { .. }
            | ClientMessage::BufferChanged { .. }
            | ClientMessage::BufferClosed { .. }
            | ClientMessage::HoverRequest { .. }
            | ClientMessage::DefinitionRequest { .. }
            | ClientMessage::ReferencesRequest { .. }
            | ClientMessage::RequestDiff { .. }
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
                // Structural, focus, and rename changes all re-query the layout:
                // the `list-panes` query carries window_active/pane_active and
                // window_name, so a `select-window` (tab), `select-pane`, or
                // `rename-window` (explicit or the shell's automatic-rename) is
                // reflected to the client as a LayoutUpdate with refreshed flags
                // and names. tmux signals all of these out-of-band
                // (`%session-window-changed`, `%window-pane-changed`,
                // `%window-renamed`) with no geometry, so they would otherwise
                // leave the UI stale.
                Event::LayoutChange { .. }
                | Event::WindowAdd { .. }
                | Event::WindowClose { .. }
                | Event::WindowRenamed { .. }
                | Event::SessionWindowChanged { .. }
                | Event::WindowPaneChanged { .. } => {
                    self.request_layout().await;
                }
                Event::SessionChanged { session, name } => {
                    // Sent on attach with this client's own session id and
                    // name; remember the id so foreign `%session-renamed`
                    // broadcasts can be told apart from this attach's own.
                    self.session_id = Some(session);
                    self.session = name;
                }
                Event::SessionRenamed { session, name } => {
                    // tmux broadcasts %session-renamed for every session on
                    // the server, not just the attached one. Only when the id
                    // matches this attach does the layout echo change: adopt
                    // the new name and re-query so the client sees it now, not
                    // at the next unrelated structural change.
                    if self.session_id == Some(session) {
                        self.session = name;
                        self.request_layout().await;
                    }
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
                    } else if self.key_table_reply_id_matches(id) {
                        if let Some(message) = self.apply_key_table_reply(id, error, output) {
                            outbound.send(message).await.map_err(|_| Closed)?;
                        }
                    } else if self.status_line_reply_id_matches(id) {
                        if let Some(message) = self.apply_status_line_reply(id, error, output) {
                            outbound.send(message).await.map_err(|_| Closed)?;
                        }
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
                Event::Other { .. } | Event::PaneModeChanged { .. } => {}
            }
        }
        Ok(Flow::Continue)
    }

    /// Feed one `CommandReply` into the in-flight key-table query, if it
    /// matches. Stashes the reply's output (empty on error — a partial table
    /// beats a failed one, same convention as the client-side parser) and
    /// returns the combined [`DaemonMessage::KeyTableReply`] once both the
    /// `list-keys` and `show-options` legs have arrived; `None` while the pair
    /// is still incomplete or `id` matches neither.
    fn apply_key_table_reply(
        &mut self,
        id: Option<CommandId>,
        error: bool,
        output: Vec<String>,
    ) -> Option<DaemonMessage> {
        let query = self.key_table_query.as_mut()?;
        let id = id?;
        if id == query.list_keys_id {
            query.list_keys_output = Some(if error { Vec::new() } else { output });
        } else if id == query.show_options_id {
            query.show_options_output = Some(if error { Vec::new() } else { output });
        } else {
            return None;
        }
        let query = self.key_table_query.as_ref()?;
        let (list_keys, options) = (
            query.list_keys_output.as_ref()?,
            query.show_options_output.as_ref()?,
        );
        let message = DaemonMessage::KeyTableReply {
            list_keys: list_keys.join("\n"),
            options: options.join("\n"),
        };
        self.key_table_query = None;
        Some(message)
    }

    /// Issue the `list-keys` + `show-options -A` query pair for the tmux
    /// key-table mirror. `-A` resolves options inherited from the global scope
    /// (a `.tmux.conf` `set -g prefix C-a` shows up here, unlike plain
    /// `show-options`, which lists only session-level overrides) — the same
    /// scoping the status-line query uses. A request while one is already in
    /// flight simply replaces the tracked pair; the superseded reply (if it
    /// ever arrives) will not match either tracked id and is silently dropped —
    /// acceptable since a fresher query is already running and its reply
    /// supersedes it anyway.
    async fn request_key_table(&mut self) -> std::io::Result<()> {
        let list_keys_id = self.send_command("list-keys").await?;
        let show_options_id = self.send_command("show-options -A").await?;
        self.key_table_query = Some(KeyTableQuery {
            list_keys_id,
            show_options_id,
            list_keys_output: None,
            show_options_output: None,
        });
        Ok(())
    }

    /// Whether `id` correlates to a leg of the in-flight key-table query.
    /// Checked before consuming a `CommandReply`'s owned `output`, so the
    /// dispatch in `process` can choose the right consumer without cloning.
    fn key_table_reply_id_matches(&self, id: Option<CommandId>) -> bool {
        let Some(id) = id else { return false };
        self.key_table_query
            .as_ref()
            .is_some_and(|query| query.list_keys_id == id || query.show_options_id == id)
    }

    /// Whether `id` correlates to a leg of the in-flight status-line query.
    /// Same purpose as [`Attach::key_table_reply_id_matches`].
    fn status_line_reply_id_matches(&self, id: Option<CommandId>) -> bool {
        let Some(id) = id else { return false };
        self.status_line_query.as_ref().is_some_and(|query| {
            query.options_id == id || query.status_left_id == id || query.status_right_id == id
        })
    }

    /// Feed one `CommandReply` into the in-flight status-line query, if it
    /// matches. Stashes the reply's output (empty on error — a partial result
    /// beats a failed one, same convention as [`Attach::apply_key_table_reply`])
    /// and returns the combined [`DaemonMessage::StatusLineReply`] once all
    /// three legs (`show-options -A`, and the two `display-message -p`
    /// expansions) have arrived; `None` while the triple is still incomplete
    /// or `id` matches neither. Also reschedules the `status-interval` timer
    /// off the freshly discovered options.
    fn apply_status_line_reply(
        &mut self,
        id: Option<CommandId>,
        error: bool,
        output: Vec<String>,
    ) -> Option<DaemonMessage> {
        let query = self.status_line_query.as_mut()?;
        let id = id?;
        if id == query.options_id {
            query.options_output = Some(if error { Vec::new() } else { output });
        } else if id == query.status_left_id {
            query.status_left_output = Some(if error { Vec::new() } else { output });
        } else if id == query.status_right_id {
            query.status_right_output = Some(if error { Vec::new() } else { output });
        } else {
            return None;
        }
        let complete = query.options_output.is_some()
            && query.status_left_output.is_some()
            && query.status_right_output.is_some();
        if !complete {
            return None;
        }
        // Move the completed triple out (ending the borrow above) before
        // touching `self` again to reschedule the timer.
        let query = self.status_line_query.take()?;
        let options = query.options_output.unwrap_or_default();
        let status_left = query.status_left_output.unwrap_or_default();
        let status_right = query.status_right_output.unwrap_or_default();
        self.reschedule_status_timer(extract_status_interval(&options));
        Some(DaemonMessage::StatusLineReply {
            options: options.join("\n"),
            status_left: status_left.join("\n"),
            status_right: status_right.join("\n"),
        })
    }

    /// Issue the `show-options -A` + two `display-message -p '#{T:...}'`
    /// query triple for the tmux status-line mirror
    /// (`docs/spec-tmux-statusline-mirroring.md`). `-A` resolves options
    /// inherited from the global scope (a `.tmux.conf` `set -g status-style
    /// ...` shows up here, unlike plain `show-options`, which lists only
    /// session-level overrides) — "session-resolved" per the spec outcome,
    /// same scoping as the key-table query. The `#{T:...}` fetches expand
    /// **by option name only**: the option's own text is never read here and
    /// spliced into a command line — the interpolation hazard the spec
    /// forbids. A request while one is already in flight simply replaces the
    /// tracked triple, mirroring [`Attach::request_key_table`]'s coalescing.
    async fn request_status_line(&mut self) -> std::io::Result<()> {
        let options_id = self.send_command("show-options -A").await?;
        let status_left_id = self
            .send_command("display-message -p '#{T:status-left}'")
            .await?;
        let status_right_id = self
            .send_command("display-message -p '#{T:status-right}'")
            .await?;
        self.status_line_query = Some(StatusLineQuery {
            options_id,
            status_left_id,
            status_right_id,
            options_output: None,
            status_left_output: None,
            status_right_output: None,
        });
        Ok(())
    }

    /// Rebuild the `status-interval` re-fetch timer if the discovered
    /// interval changed. `None` (the reply's `show-options -A` output had no
    /// parseable `status-interval` line) leaves the current timer untouched —
    /// a malformed reply must not kill a working timer; `Some(0)` disables it
    /// entirely (tmux's own "no interval redraw" semantics, spec outcome).
    fn reschedule_status_timer(&mut self, interval_secs: Option<u64>) {
        let Some(secs) = interval_secs else {
            return;
        };
        if self.last_status_interval_secs == Some(secs) {
            return;
        }
        self.last_status_interval_secs = Some(secs);
        self.status_timer = (secs > 0).then(|| {
            let period = Duration::from_secs(secs);
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            timer
        });
    }

    /// Issue a layout query, coalescing so at most one is in flight.
    async fn request_layout(&mut self) {
        if self.layout_query.is_some() {
            self.layout_dirty = true;
            return;
        }
        match self.send_command(LAYOUT_QUERY).await {
            Ok(id) => self.layout_query = Some(id),
            Err(err) => warn!(%err, "layout query failed"),
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
                warn!(pane, %err, "resume pane failed");
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

/// Pull `status-interval`'s resolved value out of a `show-options -A` reply,
/// for the daemon's own re-fetch timer. A plain integer, never tmux-quoted,
/// so a whitespace split suffices — the full `status-*` option set is parsed
/// client-side (`rift_terminal::statusline::parse_status_options`); the
/// daemon extracts only the one field it needs to drive scheduling. A
/// trailing `*` (an `-A` value inherited from a higher scope) is stripped
/// before matching the option name.
fn extract_status_interval(lines: &[String]) -> Option<u64> {
    lines.iter().find_map(|line| {
        let mut tokens = line.split_whitespace();
        let name = tokens.next()?.trim_end_matches('*');
        if name != "status-interval" {
            return None;
        }
        tokens.next()?.parse().ok()
    })
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

    /// Set a global tmux option directly via the `tmux` CLI (outside the
    /// control-mode attach under test), for fixtures that need a non-default
    /// global value (a `status-*` option, the prefix) in place before the
    /// daemon's own attach-or-create runs. Unlike
    /// `new-session`/`attach-session`, plain `set-option` never starts a
    /// fresh server on an unused socket — it errors with "no such file or
    /// directory" against a socket nothing has bound yet — so this first
    /// spins up a detached `rift` session (the name every such test attaches
    /// to) to give the server something to run against; `new-session -A` in
    /// `Attach::spawn` then attaches to that same session instead of
    /// creating a second one.
    fn set_global_option(server: &TmuxServer, name: &str, value: &str) {
        let _ = std::process::Command::new("tmux")
            .args(["-L", &server.name, "new-session", "-d", "-s", "rift"])
            .stderr(std::process::Stdio::null())
            .status();
        let status = std::process::Command::new("tmux")
            .args(["-L", &server.name, "set-option", "-g", name, value])
            .status()
            .expect("run tmux set-option");
        assert!(status.success(), "tmux set-option -g {name} {value} failed");
    }

    /// Whether a `show-options -A` reply's `options` text reports option
    /// `name` as `expected`, tolerating the trailing `*` `-A` adds to a name
    /// whose value is inherited from a higher scope rather than set at this
    /// exact level.
    fn reports_option(options: &str, name: &str, expected: &str) -> bool {
        options.lines().any(|line| {
            let mut tokens = line.split_whitespace();
            let token = tokens.next().unwrap_or("").trim_end_matches('*');
            token == name && tokens.next() == Some(expected)
        })
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
    async fn test_attach_issues_key_table_reply_unprompted() {
        let server = TmuxServer::new("keytable-attach");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");

        // No client request needed: the attach itself issues `list-keys` +
        // `show-options -A`, mirroring the unprompted layout query.
        let list_keys = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::KeyTableReply { list_keys, .. } => Some(list_keys.clone()),
            _ => None,
        })
        .await
        .expect("key-table reply after attach");
        // `list-keys` always reports tmux's compiled-in default bindings, so
        // this is non-empty regardless of the test environment's config.
        assert!(!list_keys.is_empty());

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_query_key_table_reissues_reply() {
        let server = TmuxServer::new("keytable-query");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::KeyTableReply { .. }).then_some(())
        })
        .await
        .expect("attach-time key-table reply");

        in_tx
            .send(ClientMessage::QueryKeyTable)
            .await
            .expect("query key table");
        let second = recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::KeyTableReply { .. }).then_some(())
        })
        .await;
        assert!(
            second.is_some(),
            "an explicit QueryKeyTable must produce another KeyTableReply"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_key_table_options_global_only_prefix_is_resolved() {
        // The key-table `show-options -A` must resolve a prefix set only at
        // the *global* scope (a `.tmux.conf` `set -g prefix C-a`) — plain
        // `show-options` lists only session-level overrides and would miss
        // it, leaving rift on the C-b default while tmux uses C-a (#439).
        let server = TmuxServer::new("keytable-resolved");
        set_global_option(&server, "prefix", "C-a");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        let options = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::KeyTableReply { options, .. } => Some(options.clone()),
            _ => None,
        })
        .await
        .expect("key-table reply after attach");
        assert!(
            reports_option(&options, "prefix", "C-a"),
            "a global-only prefix override must resolve: {options}"
        );

        drop(in_tx);
        let _ = task.await;
    }

    // --- status-line mirror (#219) ---

    #[tokio::test]
    async fn test_attach_issues_status_line_reply_unprompted() {
        let server = TmuxServer::new("statusline-attach");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");

        // No client request needed: the attach itself issues the
        // `show-options -A` + display-message triple, mirroring the
        // unprompted key-table query.
        let options = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::StatusLineReply { options, .. } => Some(options.clone()),
            _ => None,
        })
        .await
        .expect("status-line reply after attach");
        assert!(
            options.contains("status-interval"),
            "show-options -A must report status-interval: {options}"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_query_status_line_reissues_reply() {
        let server = TmuxServer::new("statusline-query");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::StatusLineReply { .. }).then_some(())
        })
        .await
        .expect("attach-time status-line reply");

        in_tx
            .send(ClientMessage::QueryStatusLine)
            .await
            .expect("query status line");
        let second = recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::StatusLineReply { .. }).then_some(())
        })
        .await;
        assert!(
            second.is_some(),
            "an explicit QueryStatusLine must produce another StatusLineReply"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_status_line_options_are_session_resolved_across_global_overrides() {
        // `show-options -A` must resolve a value set only at the *global*
        // scope (a `.tmux.conf` `set -g ...`), not just session-level
        // overrides — the session-resolved discovery the spec requires.
        let server = TmuxServer::new("statusline-resolved");
        set_global_option(&server, "status-style", "bg=colour234,fg=colour253");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        let options = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::StatusLineReply { options, .. } => Some(options.clone()),
            _ => None,
        })
        .await
        .expect("status-line reply");
        assert!(
            options.contains("bg=colour234,fg=colour253"),
            "a global-only status-style override must resolve: {options}"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_status_line_expansion_validates_variable_conditional_client_var_and_shell_segment(
    ) {
        // The first-issue expansion-fidelity validation the spec pins: a
        // variable (`#S`, the session name), a `#{?...}` conditional, a
        // client-scoped variable (`#{client_width}`, resolved against rift's
        // own control-mode client with no explicit `-c` targeting), and a
        // `#()` shell segment (expected empty under one-shot expansion).
        let server = TmuxServer::new("statusline-expand");
        set_global_option(
            &server,
            "status-left",
            "#{?#{==:#{host_short},nonexistenthost},yes,no}-#S-#{client_width}-#(echo shellout)",
        );
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        let status_left = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::StatusLineReply { status_left, .. } => Some(status_left.clone()),
            _ => None,
        })
        .await
        .expect("status-line reply");

        // Expected shape: "<conditional>-<session name>-<client width>-"
        // (the trailing `-` is what's left of the #() segment, which renders
        // empty under one-shot expansion).
        let parts: Vec<&str> = status_left.split('-').collect();
        assert_eq!(
            parts.as_slice(),
            ["no", "rift", parts.get(2).copied().unwrap_or_default(), ""],
            "unexpected expansion shape: {status_left}"
        );
        assert!(
            !parts[2].is_empty() && parts[2].chars().all(|c| c.is_ascii_digit()),
            "client_width must expand to a plain number: {status_left}"
        );
        assert!(
            !status_left.contains("shellout"),
            "a #() shell segment must never run under one-shot expansion: {status_left}"
        );
        assert!(
            !status_left.contains("#("),
            "the shell segment placeholder must not leak into the expanded text: {status_left}"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_status_line_expansion_applies_strftime() {
        // `#{T:...}` expands the option's format *and* runs the result
        // through strftime, so a literal `%Y` in status-right must become a
        // real (digits-only) year, not pass through unresolved.
        let server = TmuxServer::new("statusline-strftime");
        set_global_option(&server, "status-right", "%Y");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        let status_right = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::StatusLineReply { status_right, .. } => Some(status_right.clone()),
            _ => None,
        })
        .await
        .expect("status-line reply");

        assert!(
            !status_right.contains('%'),
            "strftime must consume the %-code: {status_right}"
        );
        assert_eq!(
            status_right.len(),
            4,
            "a %Y expansion is a 4-digit year: {status_right}"
        );
        assert!(
            status_right.chars().all(|c| c.is_ascii_digit()),
            "a %Y expansion must be all digits: {status_right}"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_status_interval_timer_refetches_without_client_request() {
        // The daemon's own `status-interval` cadence: a fast interval must
        // produce a *second* StatusLineReply with no client message at all.
        let server = TmuxServer::new("statusline-timer");
        set_global_option(&server, "status-interval", "1");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::StatusLineReply { .. }).then_some(())
        })
        .await
        .expect("attach-time status-line reply");

        let timer_driven = recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::StatusLineReply { .. }).then_some(())
        })
        .await;
        assert!(
            timer_driven.is_some(),
            "status-interval 1 must drive an unprompted re-fetch"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_status_interval_zero_disables_timer() {
        // Prove the timer both works (fast interval) and stops (interval 0)
        // in the same test, so a "no timer ever ran" false pass can't hide
        // behind a naive negative wait.
        let server = TmuxServer::new("statusline-notimer");
        set_global_option(&server, "status-interval", "1");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::StatusLineReply { .. }).then_some(())
        })
        .await
        .expect("attach-time status-line reply");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::StatusLineReply { .. }).then_some(())
        })
        .await
        .expect("the fast timer must fire at least once before being disabled");

        // Disable the timer (the change-trigger path: dispatch the option
        // change, then explicitly refresh — what the client-side
        // `mutates_status_options` check drives in practice) and drain the
        // reply that carries the new `status-interval 0`.
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "set-option -g status-interval 0".to_owned(),
            })
            .await
            .expect("dispatch status-interval 0");
        in_tx
            .send(ClientMessage::QueryStatusLine)
            .await
            .expect("query status line");
        let disabled = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::StatusLineReply { options, .. }
                if reports_option(options, "status-interval", "0") =>
            {
                Some(())
            }
            _ => None,
        })
        .await;
        assert!(
            disabled.is_some(),
            "expected a reply reporting status-interval 0"
        );

        // No further reply may arrive on its own — the timer is gone.
        let stray = recv_until(&mut out_rx, 3, |m| {
            matches!(m, DaemonMessage::StatusLineReply { .. }).then_some(())
        })
        .await;
        assert!(
            stray.is_none(),
            "status-interval 0 must disable the re-fetch timer (no busy poll)"
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
    async fn test_layout_update_after_select_pane() {
        let server = TmuxServer::new("selpane");
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

        // Two panes so focus can move; record the active pane after the split (the
        // new pane becomes active).
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "split-window -h".to_owned(),
            })
            .await
            .expect("split");
        let (active_after_split, pane_ids) = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                let panes: Vec<u32> = windows
                    .iter()
                    .flat_map(|w| w.panes.iter())
                    .map(|p| p.pane_id)
                    .collect();
                if panes.len() < 2 {
                    return None;
                }
                let active = windows
                    .iter()
                    .flat_map(|w| w.panes.iter())
                    .find(|p| p.active)
                    .map(|p| p.pane_id)?;
                Some((active, panes))
            }
            _ => None,
        })
        .await
        .expect("2-pane layout update with an active pane");

        // Select a different pane. tmux emits %window-pane-changed (no geometry),
        // which must re-query the layout and surface a LayoutUpdate whose active
        // flag has moved to the selected pane — the focus regression this guards.
        let target = *pane_ids
            .iter()
            .find(|&&p| p != active_after_split)
            .expect("a non-active pane");
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: format!("select-pane -t %{target}"),
            })
            .await
            .expect("select-pane");

        let moved = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                let active = windows
                    .iter()
                    .flat_map(|w| w.panes.iter())
                    .find(|p| p.active)
                    .map(|p| p.pane_id)?;
                (active == target).then_some(active)
            }
            _ => None,
        })
        .await;
        assert!(
            moved.is_some(),
            "select-pane must emit a LayoutUpdate with the active flag on the selected pane"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_layout_update_after_select_window() {
        let server = TmuxServer::new("selwin");
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

        // A second window so the active window can move; the new window becomes
        // active.
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "new-window".to_owned(),
            })
            .await
            .expect("new-window");
        let (active_after_new, window_ids) = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                if windows.len() < 2 {
                    return None;
                }
                let ids: Vec<u32> = windows.iter().map(|w| w.window_id).collect();
                let active = windows.iter().find(|w| w.active).map(|w| w.window_id)?;
                Some((active, ids))
            }
            _ => None,
        })
        .await
        .expect("2-window layout update with an active window");

        // Switch back to the other window. tmux emits %session-window-changed,
        // which must re-query and surface a LayoutUpdate with the active flag on
        // the selected window (the tab-switch half of the focus regression).
        let target = *window_ids
            .iter()
            .find(|&&w| w != active_after_new)
            .expect("a non-active window");
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: format!("select-window -t @{target}"),
            })
            .await
            .expect("select-window");

        let moved = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                let active = windows.iter().find(|w| w.active).map(|w| w.window_id)?;
                (active == target).then_some(active)
            }
            _ => None,
        })
        .await;
        assert!(
            moved.is_some(),
            "select-window must emit a LayoutUpdate with the active flag on the selected window"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_layout_update_after_rename_window() {
        // `rename-window` emits only %window-renamed — no structural event — so
        // the daemon must re-query the layout for the new name to reach the
        // client (the stale-tab-label regression this guards).
        let server = TmuxServer::new("renamewin");
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
                cmd: "rename-window rift429renamed".to_owned(),
            })
            .await
            .expect("rename-window");

        let renamed = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => windows
                .iter()
                .any(|w| w.name == "rift429renamed")
                .then_some(()),
            _ => None,
        })
        .await;
        assert!(
            renamed.is_some(),
            "rename-window must surface a LayoutUpdate carrying the new window name"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_layout_update_after_rename_session() {
        // `rename-session` emits only %session-renamed; the daemon adopts the
        // new name so the layout echo reports it without waiting for an
        // unrelated structural change.
        let server = TmuxServer::new("renamesess");
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
                cmd: "rename-session rift429sess".to_owned(),
            })
            .await
            .expect("rename-session");

        let renamed = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { session, .. } if session == "rift429sess" => Some(()),
            _ => None,
        })
        .await;
        assert!(
            renamed.is_some(),
            "rename-session must surface a LayoutUpdate echoing the new session name"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_layout_update_after_rename_other_session_keeps_own_name() {
        // tmux broadcasts %session-renamed for ANY session on the server to
        // every control client, not just clients attached to the renamed
        // session. Renaming a *different* session on the same server must not
        // change this attach's session echo (the cross-session name-poisoning
        // regression this guards).
        let server = TmuxServer::new("renameothersess");
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

        // A second session on the same server, renamed while this attach
        // watches — the %session-renamed broadcast reaches this client too.
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "new-session -d -s rift429other".to_owned(),
            })
            .await
            .expect("new-session");
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "rename-session -t rift429other rift429othernew".to_owned(),
            })
            .await
            .expect("rename-session");

        // Force a layout echo after the broadcast: the control stream is
        // ordered, so the %window-renamed (and the LayoutUpdate it triggers)
        // arrives after the foreign %session-renamed has been processed.
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "rename-window rift429after".to_owned(),
            })
            .await
            .expect("rename-window");

        let session = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { session, windows }
                if windows.iter().any(|w| w.name == "rift429after") =>
            {
                Some(session.clone())
            }
            _ => None,
        })
        .await;
        assert_eq!(
            session.as_deref(),
            Some("rift"),
            "renaming another session must not change this attach's session echo"
        );

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
