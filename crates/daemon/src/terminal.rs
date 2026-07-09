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
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use rift_protocol::{ClientMessage, DaemonMessage, PaneLayout, SessionEntry, WindowLayout};
use rift_tmux_core::{Client, CommandId, Event};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

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
/// the format is one line per pane with the window fields repeated. `window_index`
/// carries tmux's real `#{window_index}` (the tab number the client renders and
/// `select-window -t :N` targets) end-to-end instead of a synthesized array
/// position, so a closed window's gap (`renumber-windows` off, the default) shows
/// up client-side too (#495). `#{==:#{pane_current_command},#{b:default-shell}}`
/// is tmux's own comparison of the pane's foreground command against the
/// basename of its `default-shell` option, evaluated server-side into a `0`/`1`
/// — the client never carries a shell name list or process taxonomy
/// (agent-agnostic, #510). Fields are tab-separated because both
/// `pane_current_path` and `window_name` may contain spaces — with tabs each stays
/// a single field (a literal tab inside a quoted tmux argument is preserved, and
/// tmux octal-escapes it in the reply, which the control-mode [`Client`] decodes
/// back; tmux-reference pitfall 8). `window_name` is last so a name containing
/// tabs stays in the final field (see [`parse_layout_line`]). The `#{...}` formats
/// are single-quoted because the control parser treats an unquoted `#` as a
/// comment (tmux-reference pitfall 9).
const LAYOUT_QUERY: &str = "list-panes -s -F '#{window_id}\t#{window_index}\t#{window_active}\t#{pane_id}\t#{pane_active}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{pane_current_command}\t#{pane_current_path}\t#{==:#{pane_current_command},#{b:default-shell}}\t#{window_name}'";

/// One query that rebuilds the whole session list (`docs/spec-session-switch.md`),
/// one line per session on the server. Same conventions as [`LAYOUT_QUERY`]:
/// tab-separated fields with the free-form one (`session_name`) LAST, so a name
/// containing spaces or tabs stays intact in the final field (see
/// [`parse_session_line`]); the `#{...}` formats are single-quoted because the
/// control parser treats an unquoted `#` as a comment (tmux-reference pitfall 9).
/// `#{session_id}` (`$<n>`) is the rename-stable key; `#{session_attached}` is
/// the attached-client count, folded to a bool at the parse. `#{@root}` — the
/// durable per-session root stamp [`stamp_root_command`] writes at attach
/// (`docs/spec-session-root-picker.md`) — sits right before `session_name`,
/// same convention as [`ROOT_QUERY`]'s `session_path`: the field most likely
/// to contain arbitrary characters (the user-renamable session name) stays
/// last, so a picked filesystem path never displaces it.
const SESSION_LIST_QUERY: &str = "list-sessions -F '#{session_id}\t#{session_windows}\t#{session_attached}\t#{@root}\t#{session_name}'";

/// The `-F` format alone (no `list-sessions -F '...'` wrapper), for the
/// pre-attach one-off query ([`query_session_list_detached`], #757), which runs
/// `tmux` directly with each argument passed separately — so the format takes
/// no surrounding shell quotes. MUST stay identical to the format embedded in
/// [`SESSION_LIST_QUERY`] so both paths parse with [`parse_session_line`].
const SESSION_LIST_FORMAT: &str =
    "#{session_id}\t#{session_windows}\t#{session_attached}\t#{@root}\t#{session_name}";

/// One query that resolves the attached session's project root
/// (`docs/spec-per-session-project-root.md`): `@root` — the durable stamp
/// [`stamp_root_command`] writes at attach — and `#{session_path}` — the
/// session's working directory (Phase 34's `-c` default) — returned together
/// so [`resolve_session_root`] can prefer the former and fall back to the
/// latter for a session `@root` has never stamped (created outside rift).
/// `session_path` is LAST (same convention as [`LAYOUT_QUERY`]/
/// [`SESSION_LIST_QUERY`]): the field most likely to contain arbitrary
/// characters stays intact in the final position. Sent with no `-t`, like
/// [`reroot_command`]: over the just-attached control connection,
/// `display-message -p` without a target reports on the issuing client's own
/// current session, so the session name never has to be embedded here. The
/// `#{...}` format is single-quoted because the control parser treats an
/// unquoted `#` as a comment (tmux-reference pitfall 9).
const ROOT_QUERY: &str = "display-message -p '#{@root}\t#{session_path}'";

/// One resolved-root signal per `Attach`, sent once the [`ROOT_QUERY`] reply
/// lands (`docs/spec-per-session-project-root.md`, the Attach seam, #737).
/// Internal to the daemon, NOT a protocol message: `lib.rs`'s
/// `serve_connection` consumes it to drive the per-root `ContextMap`'s
/// acquire/release/re-subscribe sequence, in the defined resolve -> acquire
/// -> snapshot order. `None` when [`resolve_session_root`] itself resolves to
/// `None` (tmux reports an empty `session_path` too — should not happen for a
/// live session, but handled defensively rather than assumed). Sent with
/// `try_send`, not an awaited send: this is a best-effort, at-most-one-per-
/// attach signal, never worth blocking the tmux read loop over, and a
/// superseded value (a rapid re-attach) is fine to drop since the newer
/// attach's own resolution follows right behind. A full channel or a gone
/// receiver (e.g. a test that constructs `terminal_task` directly and never
/// drains it) is silently tolerated for the same reason.
pub(crate) type RootResolved = mpsc::Sender<Option<PathBuf>>;

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
/// `root` is the daemon's watched project root (`docs/spec-session-start-directory.md`):
/// when present, a freshly created session's default working directory is set to
/// it (`Attach::spawn`), so every later window and split inherits the project
/// root instead of `$HOME`. `None` in the rootless test call sites. `root_resolved`
/// carries the per-attach resolved session root back to `serve_connection` — see
/// [`RootResolved`].
pub(crate) async fn terminal_task(
    mut inbound: mpsc::Receiver<ClientMessage>,
    outbound: mpsc::Sender<DaemonMessage>,
    server_socket: Option<String>,
    root: Option<PathBuf>,
    root_resolved: RootResolved,
) {
    let mut attach: Option<Attach> = None;
    let mut buf = vec![0u8; TERM_READ_BUFFER];
    let mut resume_poll = tokio::time::interval(RESUME_POLL);
    resume_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // Only poll while attached; otherwise this branch never fires.
            result = read_attach_stdout(&mut attach, &mut buf), if attach.is_some() => match result {
                Ok(0) | Err(_) => terminal_down(&mut attach, &outbound, None).await,
                Ok(n) => {
                    let outcome = match attach.as_mut() {
                        Some(a) => a.process(&buf[..n], &outbound, &root_resolved).await,
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
                Some(ClientMessage::Attach { session, root: picked_root }) => {
                    // Re-attach: tear the current child down before opening anew.
                    // The create-with-root transport
                    // (`docs/spec-session-root-picker.md`): `Some(picked)`
                    // overrides the daemon's own configured `root` for this one
                    // attach — `new-session -c <picked>` and the `@root` stamp
                    // both target the picked root; `None` (every existing
                    // caller — reconnect / switch / pick-existing) preserves
                    // today's configured-root behavior unchanged. See
                    // `effective_attach_root`.
                    detach(&mut attach).await;
                    let picked_root = effective_attach_root(picked_root.as_deref(), root.as_deref());
                    attach = open_attach(
                        session,
                        server_socket.as_deref(),
                        picked_root.as_deref(),
                        &outbound,
                    )
                    .await;
                }
                Some(ClientMessage::QuerySessionList) if attach.is_none() => {
                    // The picker queries the session list BEFORE attaching
                    // (#757, `docs/spec-post-connect-picker.md`): there is no
                    // control-mode child yet, so answer with a one-off
                    // `tmux list-sessions` instead of dropping it in the arm
                    // below. A host with no server running replies with an
                    // empty list, so the picker reaches its zero-session
                    // create-only state rather than the client hanging on a
                    // reply that never arrives. Once attached, a
                    // `QuerySessionList` falls through to the control-mode path
                    // (`handle_client_message` -> `request_session_list`).
                    query_session_list_detached(server_socket.as_deref(), &outbound).await;
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

/// Read into `buf` from the attach's stdout. Pends forever when not attached,
/// so the outer `select!` can guard this branch with `if attach.is_some()` and
/// never race a missing child.
async fn read_attach_stdout(attach: &mut Option<Attach>, buf: &mut [u8]) -> std::io::Result<usize> {
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
    root: Option<&Path>,
    outbound: &mpsc::Sender<DaemonMessage>,
) -> Option<Attach> {
    match Attach::spawn(session.clone(), server_socket, root).await {
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

/// Answer a pre-attach [`ClientMessage::QuerySessionList`] (#757) with a one-off
/// `tmux list-sessions`, independent of the control-mode attach path
/// ([`Attach::send_command`]), which does not exist until a session is attached.
/// The picker issues this query before any `Attach`
/// (`docs/spec-post-connect-picker.md`), so a host with no server running yet
/// (fresh boot, or every session killed) must not hang the client: tmux exits
/// non-zero with "no server running on ..." and this maps that — like any
/// failure — to an EMPTY [`DaemonMessage::SessionListReply`], which drives the
/// picker's zero-session create-only state. `server_socket` selects the tmux
/// server (`-L`), matching [`spawn_args`].
async fn query_session_list_detached(
    server_socket: Option<&str>,
    outbound: &mpsc::Sender<DaemonMessage>,
) {
    let mut command = tokio::process::Command::new("tmux");
    if let Some(socket) = server_socket {
        command.args(["-L", socket]);
    }
    command.args(["list-sessions", "-F", SESSION_LIST_FORMAT]);
    let sessions = match command.output().await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            parse_session_list(&text.lines().map(str::to_owned).collect::<Vec<_>>())
        }
        // A non-zero exit is the "no server running" case (or a genuine tmux
        // error); the picker's zero-session edge treats both as an empty list.
        Ok(_) => Vec::new(),
        Err(err) => {
            warn!(%err, "one-off tmux list-sessions failed; replying with an empty session list");
            Vec::new()
        }
    };
    let _ = outbound
        .send(DaemonMessage::SessionListReply { sessions })
        .await;
}

/// Resolve the root to attach with for one [`ClientMessage::Attach`]
/// (`docs/spec-session-root-picker.md`, the create-with-root transport):
/// `picked` (the message's own `root` field, `Some` only when the client is
/// creating a session at a root chosen through the root picker) wins over
/// `configured` (the daemon's own startup-configured project root, threaded
/// into every attach today via [`terminal_task`]'s `root` parameter). `None`
/// when neither is set. A pure function so the precedence is unit-tested
/// without spawning tmux.
fn effective_attach_root(picked: Option<&str>, configured: Option<&Path>) -> Option<PathBuf> {
    match picked {
        Some(picked) => Some(PathBuf::from(picked)),
        None => configured.map(Path::to_path_buf),
    }
}

/// Build the argv (excluding the `tmux` program name itself) for the attach's
/// control-mode child: an optional `-L <server_socket>`, then the fixed
/// `-C new-session -A -s <session>`, then an optional `-c <root>`
/// (`docs/spec-session-start-directory.md`) so a freshly created session's
/// default working directory is the project root — inherited by every later
/// `new-window`/`split-window` that omits its own `-c` (tmux >=1.9). `-c` only
/// takes effect when `new-session -A` actually creates the session; attaching
/// an existing one leaves its default directory untouched (the re-root path is
/// separate). Extracted as a pure function so the argv shape is unit-tested
/// without spawning tmux; each entry is its own argv element, never a shell
/// string, so a root path cannot inject extra arguments.
fn spawn_args(session: &str, server_socket: Option<&str>, root: Option<&Path>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(socket) = server_socket {
        args.push("-L".to_owned());
        args.push(socket.to_owned());
    }
    args.push("-C".to_owned());
    args.push("new-session".to_owned());
    args.push("-A".to_owned());
    args.push("-s".to_owned());
    args.push(session.to_owned());
    if let Some(root) = root {
        args.push("-c".to_owned());
        args.push(root.to_string_lossy().into_owned());
    }
    args
}

/// Build the control-mode command that re-roots the just-attached session to
/// `root` (`docs/spec-session-start-directory.md`). `-c` on `new-session -A`
/// only takes effect when the session is *created*; a session that already
/// existed (created outside rift in `$HOME`, or persisted from before this
/// change) keeps its stale default directory unless re-rooted separately. In
/// Phase 34 the daemon has exactly one root, so sending this unconditionally on
/// every attach is idempotent: a no-op for a freshly created session (already
/// at `root`), a fix for a pre-existing one.
///
/// Validated against a real tmux 3.4 server (see the spec's decision log):
/// `attach-session -c <root>`, sent with **no `-t`**, over the control-mode
/// connection that is already attached to the target session, sets that
/// session's `session_path` (the default directory `new-window`/
/// `split-window` inherit) to `root` — omitting `-t` targets the issuing
/// client's own current session, so the session name never has to be embedded
/// (and quoted) in the command line at all. Re-sending it for a session
/// already at `root` is harmless: tmux applies the same value and the
/// resulting `%session-changed` for this attach's own (unchanged) session id
/// is a no-op (see `Event::SessionChanged`'s `switched` check).
///
/// Unlike `spawn_args`, this string is not process argv — it is parsed by
/// tmux's own control-mode command lexer (a shell-like grammar), so `root` is
/// quoted with [`quote_tmux_arg`]: an unquoted space would otherwise split it
/// into two tokens (confirmed against real tmux: unquoted, a rooted path
/// containing a space fails with tmux's `%error … too many arguments`).
///
/// Quoting alone cannot neutralize a `\n`/`\r` in `root`: [`Attach::send_command`]
/// frames each command as one control-mode *line*, terminated before tmux's
/// own lexer (and hence `quote_tmux_arg`'s `'...'` escaping) ever runs — a
/// literal newline in the path would split this into two control-mode
/// commands, the second one unquoted and attacker-controlled (POSIX paths may
/// legally contain `\n`; only `/` and NUL are forbidden). Returns `None` for
/// such a `root` instead of building an unsafe command; the caller skips the
/// re-root and logs rather than sending it (best-effort degrade, matching the
/// spec's "never abort the daemon" risk mitigation, narrowed here to "never
/// send a malformed command").
fn reroot_command(root: &Path) -> Option<String> {
    let root = root.to_string_lossy();
    if root.contains('\n') || root.contains('\r') {
        return None;
    }
    Some(format!("attach-session -c {}", quote_tmux_arg(&root)))
}

/// Build the control-mode command that stamps `root` as `session`'s durable,
/// session-scoped `@root` user option (`docs/spec-per-session-project-root.md`;
/// `@root` does not exist in the codebase before this — this is the
/// introduction). Sent right after `new-session -A` on the same
/// `new-session -A -s <session>` chokepoint [`spawn_args`] uses, alongside
/// [`reroot_command`], so a rift-attached session durably carries its
/// project root as tmux metadata — read back on a later attach via
/// [`ROOT_QUERY`] / [`resolve_session_root`]. Sent unconditionally on every
/// attach, mirroring [`reroot_command`]'s idempotence rationale: in the
/// current single-root daemon, resending the same value is a no-op for a
/// session already stamped, and durably couples one that is not yet.
///
/// Unlike [`reroot_command`] (which omits `-t` to avoid embedding the
/// session name), this command targets the session explicitly — `-t
/// <session>` — so both `session` and `root` are quoted with
/// [`quote_tmux_arg`] against tmux's control-mode command lexer, and both are
/// checked for an embedded `\n`/`\r`: a character that would split
/// [`Attach::send_command`]'s single control-mode line into two, the second
/// unquoted and attacker-controlled (the same line-framing hazard
/// [`reroot_command`] documents). Returns `None` rather than building an
/// unsafe command; the caller skips the stamp and logs.
fn stamp_root_command(session: &str, root: &Path) -> Option<String> {
    let root = root.to_string_lossy();
    if session.contains('\n')
        || session.contains('\r')
        || root.contains('\n')
        || root.contains('\r')
    {
        return None;
    }
    Some(format!(
        "set -t {} @root {}",
        quote_tmux_arg(session),
        quote_tmux_arg(&root)
    ))
}

/// Wrap `value` as a single literal tmux control-mode command-line argument:
/// wrapping in `'...'` and escaping an embedded `'` as `'\''` makes tmux's
/// *lexer* treat `value` as exactly one token regardless of in-line
/// whitespace or metacharacters (`"`, `;`, a leading `-`, `$`, `#`). This is a
/// within-line, lexer-level defense only — it says nothing about a `\n`/`\r`
/// in `value`, which breaks the control-mode *line framing* the lexer never
/// even sees (guarded separately by [`reroot_command`]'s `None` case; do not
/// rely on this function for that). Mirrors
/// `crates/terminal/src/tmux_quote.rs::quote_tmux_arg`; duplicated rather
/// than shared because the daemon and `terminal` crates stay independent
/// (`docs/constitution.md` crate-boundary rule) and this is the daemon's only
/// tmux command line that embeds a dynamic, unbounded value.
fn quote_tmux_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    /// The in-flight session-list query (at most one), correlated by
    /// [`CommandId`] — the same coalescing discipline as `layout_query`.
    session_list_query: Option<CommandId>,
    /// The in-flight [`ROOT_QUERY`] (at most one), correlated by
    /// [`CommandId`] — resolves this session's project root
    /// (`docs/spec-per-session-project-root.md`) once, right after attach;
    /// unlike `layout_query`/`session_list_query` it is not re-queried on
    /// churn (a session's root does not change without a re-attach).
    root_query: Option<CommandId>,
    /// Session churn arrived while a session-list query was in flight;
    /// re-query once it returns so a burst (`%sessions-changed` storms)
    /// collapses to bounded queries.
    session_list_dirty: bool,
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

impl Attach {
    async fn spawn(
        session: String,
        server_socket: Option<&str>,
        root: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let mut command = tokio::process::Command::new("tmux");
        command
            .args(spawn_args(&session, server_socket, root))
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
            session_list_query: None,
            session_list_dirty: false,
            root_query: None,
            paused: HashSet::new(),
            captures: HashMap::new(),
            key_table_query: None,
        };
        // Enable tmux's per-pane flow control for this attach (tmux→daemon leg).
        attach
            .send_command(&format!("refresh-client -f pause-after={PAUSE_AFTER_SECS}"))
            .await?;
        // Re-root a pre-existing session whose default directory predates this
        // attach (`docs/spec-session-start-directory.md`); a no-op when the
        // session was just created with `-c root` above. Best-effort: a reply
        // arrives as an unmatched CommandReply and is silently dropped, same
        // as any other ack (see `process`'s CommandReply arm). A root that
        // cannot be safely framed as one control-mode line (`\n`/`\r`) skips
        // the re-root rather than sending a malformed command — see
        // `reroot_command`.
        if let Some(root) = root {
            match reroot_command(root) {
                Some(cmd) => {
                    attach.send_command(&cmd).await?;
                }
                None => warn!(
                    root = %root.display(),
                    "skipping session re-root: root path contains a control character that cannot be safely sent over tmux control mode"
                ),
            }
            // Couple this session to its project root
            // (`docs/spec-per-session-project-root.md`): stamp durable,
            // session-scoped `@root` metadata alongside the re-root above —
            // see `stamp_root_command`.
            match stamp_root_command(&attach.session, root) {
                Some(cmd) => {
                    attach.send_command(&cmd).await?;
                }
                None => warn!(
                    session = %attach.session,
                    root = %root.display(),
                    "skipping @root stamp: session or root contains a control character that cannot be safely sent over tmux control mode"
                ),
            }
        }
        // The task loop already reads stdout (we are subscribed), so any change
        // after this query lands as a live LayoutUpdate — no gap; the snapshot is
        // the current state, updates replace wholesale, so no duplicate either.
        let id = attach.send_command(LAYOUT_QUERY).await?;
        attach.layout_query = Some(id);
        // Key-table mirror: queried unprompted on attach/reconnect (the spec's
        // "on attach/reconnect" refresh trigger), same as the layout query above.
        attach.request_key_table().await?;
        // Resolve this session's project root (`docs/spec-per-session-project-root.md`):
        // query `@root` + `session_path` together on this freshly-attached
        // child, same correlated round-trip discipline as the layout query
        // above. Later steps (#736/#737) consume the resolved value; this
        // step's `process` arm only resolves and logs it.
        let root_id = attach.send_command(ROOT_QUERY).await?;
        attach.root_query = Some(root_id);
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
            ClientMessage::QuerySessionList => {
                self.request_session_list().await;
            }
            // Empty input is a no-op; Attach is handled by the task; Hello never
            // reaches the terminal task. The buffer-channel requests
            // (`OpenFile`/`SaveFile`) and the live-buffer feed (`BufferChanged`/
            // `BufferClosed`, #189) are not terminal messages — they are routed to
            // the per-connection buffer service or the shared LSP loop, not here.
            // Navigation requests (hover/definition/references/document-symbol,
            // #193, #526) are handled by the shared dispatch loop and LSP worker
            // — never the terminal task.
            // `RequestDiff` (source-control diff, #335) is likewise not a
            // terminal message; its daemon-side handler lands in a follow-on
            // issue — until then it is silently dropped here too.
            // The source-control write ops (stage/unstage/discard/commit/
            // stage-hunk) are not terminal messages either: the file-level ops
            // are answered per connection by `git_write::reply` (#544) and hunk
            // staging is parked on the shared loop (#545) — neither reaches the
            // terminal task, so both are a defensive no-op here.
            // The file-operation requests (create/create-dir/rename/delete,
            // #673) are likewise not terminal messages; their `std::fs`-backed
            // handlers land in a follow-on issue — silently dropped here in
            // the meantime, same convention.
            // `QueryDirEntries` (the directory-browse channel, #766) is
            // likewise not a terminal message: it is answered per connection
            // by `browse::reply`, so it never reaches the terminal task.
            ClientMessage::Input { .. }
            | ClientMessage::Attach { .. }
            | ClientMessage::OpenFile { .. }
            | ClientMessage::SaveFile { .. }
            | ClientMessage::BufferChanged { .. }
            | ClientMessage::BufferClosed { .. }
            | ClientMessage::HoverRequest { .. }
            | ClientMessage::DefinitionRequest { .. }
            | ClientMessage::ReferencesRequest { .. }
            | ClientMessage::DocumentSymbolRequest { .. }
            | ClientMessage::RequestDiff { .. }
            | ClientMessage::StageFile { .. }
            | ClientMessage::UnstageFile { .. }
            | ClientMessage::StageHunk { .. }
            | ClientMessage::DiscardFile { .. }
            | ClientMessage::Commit { .. }
            | ClientMessage::CreateFile { .. }
            | ClientMessage::CreateDir { .. }
            | ClientMessage::RenamePath { .. }
            | ClientMessage::DeletePath { .. }
            | ClientMessage::QueryDirEntries { .. }
            | ClientMessage::Hello { .. } => {}
        }
        Ok(())
    }

    /// Feed one stdout chunk through the parser and route the resulting events.
    async fn process(
        &mut self,
        bytes: &[u8],
        outbound: &mpsc::Sender<DaemonMessage>,
        root_resolved: &RootResolved,
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
                    // Also sent when this client is switched to another
                    // session (an external `switch-client`): then — mirroring
                    // the SessionRenamed arm — re-query the layout so the
                    // session indicator AND the terminal content refresh now,
                    // not at the next unrelated structural change. The
                    // attach-time delivery (no previous id) adopts without
                    // re-querying: the spawn-time snapshot query is already
                    // in flight.
                    let switched = self.session_id.is_some_and(|id| id != session);
                    self.session_id = Some(session);
                    self.session = name;
                    if switched {
                        self.request_layout().await;
                    }
                }
                Event::SessionRenamed { session, name } => {
                    // tmux broadcasts %session-renamed for every session on
                    // the server, not just the attached one — so ANY rename
                    // changes the session list; refresh it. Only when the id
                    // matches this attach does the layout echo change too:
                    // adopt the new name and re-query so the client sees it
                    // now, not at the next unrelated structural change.
                    self.request_session_list().await;
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
                    } else if id.is_some() && id == self.session_list_query {
                        self.session_list_query = None;
                        if !error {
                            outbound
                                .send(DaemonMessage::SessionListReply {
                                    sessions: parse_session_list(&output),
                                })
                                .await
                                .map_err(|_| Closed)?;
                        }
                        if self.session_list_dirty {
                            self.session_list_dirty = false;
                            self.request_session_list().await;
                        }
                    } else if id.is_some() && id == self.root_query {
                        self.root_query = None;
                        if !error {
                            if let Some(line) = output.first() {
                                let mut fields = line.splitn(2, '\t');
                                let root_option = fields.next().unwrap_or("");
                                let session_path = fields.next().unwrap_or("");
                                let resolved = resolve_session_root(root_option, session_path);
                                debug!(
                                    session = %self.session,
                                    root = ?resolved,
                                    "resolved session root on attach"
                                );
                                // Best-effort (see `RootResolved`'s doc comment):
                                // `serve_connection` drives the per-root
                                // `ContextMap` re-root from this; never worth
                                // blocking the tmux read loop over.
                                let _ = root_resolved.try_send(resolved);
                            }
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
                // Session-list churn: a session was created or destroyed
                // (%sessions-changed) or another client switched sessions
                // (%client-session-changed, changing attached flags) — re-query
                // the list, coalesced, and push the fresh reply unprompted
                // (docs/spec-session-switch.md).
                Event::SessionsChanged | Event::ClientSessionChanged { .. } => {
                    self.request_session_list().await;
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

    /// Issue a session-list query, coalescing so at most one is in flight —
    /// the same trailing-edge re-issue discipline as [`Attach::request_layout`],
    /// so a burst of session churn collapses to bounded queries.
    async fn request_session_list(&mut self) {
        if self.session_list_query.is_some() {
            self.session_list_dirty = true;
            return;
        }
        match self.send_command(SESSION_LIST_QUERY).await {
            Ok(id) => self.session_list_query = Some(id),
            Err(err) => warn!(%err, "session-list query failed"),
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
                window_index: parsed.window_index,
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
    window_index: u32,
    window_active: bool,
    window_name: String,
    pane: PaneLayout,
}

/// Parse one tab-separated `@<win> <win_index> <win_active> %<pane>
/// <pane_active> <left> <top> <width> <height> <command> <path> <is_shell>
/// <name>` line (see [`LAYOUT_QUERY`] for why tabs); `splitn(13, '\t')` keeps a
/// window name containing tabs intact in the final field.
fn parse_layout_line(line: &str) -> Option<ParsedPaneLine> {
    let mut fields = line.splitn(13, '\t');
    let window_id = fields.next()?.strip_prefix('@')?.parse().ok()?;
    let window_index = fields.next()?.parse().ok()?;
    let window_active = fields.next()? == "1";
    let pane_id = fields.next()?.strip_prefix('%')?.parse().ok()?;
    let pane_active = fields.next()? == "1";
    let left = fields.next()?.parse().ok()?;
    let top = fields.next()?.parse().ok()?;
    let width = fields.next()?.parse().ok()?;
    let height = fields.next()?.parse().ok()?;
    let current_command = fields.next()?.to_owned();
    let current_path = fields.next()?.to_owned();
    let is_shell = fields.next()? == "1";
    let window_name = fields.next().unwrap_or("").to_owned();
    Some(ParsedPaneLine {
        window_id,
        window_index,
        window_active,
        window_name,
        pane: PaneLayout {
            pane_id,
            active: pane_active,
            left,
            top,
            width,
            height,
            current_path,
            current_command,
            is_shell,
        },
    })
}

/// Parse [`SESSION_LIST_QUERY`] reply lines into session entries, in server
/// order, skipping malformed lines (same tolerance as [`parse_layout`]).
fn parse_session_list(lines: &[String]) -> Vec<SessionEntry> {
    lines
        .iter()
        .filter_map(|line| parse_session_line(line))
        .collect()
}

/// Parse one tab-separated `$<id> <windows> <attached> <root> <name>` line
/// (see [`SESSION_LIST_QUERY`] for why tabs); `splitn(5, '\t')` keeps a
/// session name containing tabs intact in the final field. `attached` is
/// tmux's attached-client COUNT, folded to a bool. `root` is `#{@root}`
/// (`docs/spec-session-root-picker.md`) — empty for a session never stamped
/// by [`stamp_root_command`] — folded to `None`.
fn parse_session_line(line: &str) -> Option<SessionEntry> {
    let mut fields = line.splitn(5, '\t');
    let id = fields.next()?.strip_prefix('$')?.parse().ok()?;
    let windows = fields.next()?.parse().ok()?;
    let attached = fields.next()?.parse::<u32>().ok()? > 0;
    let root = fields.next()?;
    let root = if root.is_empty() {
        None
    } else {
        Some(root.to_owned())
    };
    let name = fields.next()?.to_owned();
    Some(SessionEntry {
        id,
        name,
        windows,
        attached,
        root,
    })
}

/// Resolve a session's project root from a [`ROOT_QUERY`] reply's two raw
/// fields (`docs/spec-per-session-project-root.md`): `root_option` is
/// `#{@root}` — the durable stamp [`stamp_root_command`] writes at attach —
/// and `session_path` is `#{session_path}` — the session's working
/// directory (Phase 34's `-c` default). Prefers `root_option` when
/// non-empty (the session has been stamped); falls back to `session_path`
/// for a session `@root` has never stamped (created outside rift, or
/// attached before this phase). `None` only when tmux itself reports an
/// empty `session_path` too (should not happen for a live session, but the
/// daemon degrades rather than assumes). Pure so it is unit-tested without
/// tmux; the caller ([`Attach::process`]'s `root_query` reply arm) passes
/// the two raw reply fields through untouched.
fn resolve_session_root(root_option: &str, session_path: &str) -> Option<PathBuf> {
    if !root_option.is_empty() {
        Some(PathBuf::from(root_option))
    } else if !session_path.is_empty() {
        Some(PathBuf::from(session_path))
    } else {
        None
    }
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
    fn test_effective_attach_root_picked_wins_over_configured() {
        let root = effective_attach_root(Some("/picked/root"), Some(Path::new("/configured")));
        assert_eq!(root, Some(PathBuf::from("/picked/root")));
    }

    #[test]
    fn test_effective_attach_root_none_picked_falls_back_to_configured() {
        let root = effective_attach_root(None, Some(Path::new("/configured")));
        assert_eq!(root, Some(PathBuf::from("/configured")));
    }

    #[test]
    fn test_effective_attach_root_neither_set_is_none() {
        assert_eq!(effective_attach_root(None, None), None);
    }

    #[test]
    fn test_spawn_args_with_root_appends_c_flag_after_new_session() {
        let args = spawn_args("rift", None, Some(Path::new("/home/dev/proj")));
        assert_eq!(
            args,
            vec![
                "-C",
                "new-session",
                "-A",
                "-s",
                "rift",
                "-c",
                "/home/dev/proj"
            ]
        );
    }

    #[test]
    fn test_spawn_args_without_root_omits_c_flag() {
        let args = spawn_args("rift", None, None);
        assert_eq!(args, vec!["-C", "new-session", "-A", "-s", "rift"]);
        assert!(!args.iter().any(|a| a == "-c"));
    }

    #[test]
    fn test_spawn_args_with_server_socket_prepends_l_flag() {
        let args = spawn_args("rift", Some("mysock"), Some(Path::new("/proj")));
        assert_eq!(
            args,
            vec![
                "-L",
                "mysock",
                "-C",
                "new-session",
                "-A",
                "-s",
                "rift",
                "-c",
                "/proj"
            ]
        );
    }

    #[test]
    fn test_reroot_command_wraps_root_in_attach_session_c_with_no_target() {
        let command = reroot_command(Path::new("/home/dev/proj")).expect("plain root is safe");
        assert_eq!(command, "attach-session -c '/home/dev/proj'");
        // No `-t <session>`: the command targets the issuing client's own
        // current session (validated against real tmux), so a session name
        // never needs to be embedded (and quoted) here.
        assert!(!command.contains("-t"));
    }

    #[test]
    fn test_reroot_command_quotes_a_root_containing_a_space() {
        // Unquoted, real tmux rejects this with `%error … too many
        // arguments` (validated) because its command lexer splits on
        // whitespace; the quoting keeps the whole path one argument.
        let command = reroot_command(Path::new("/tmp/rift reroot project"))
            .expect("a space is a lexer-level, not line-framing, character");
        assert_eq!(command, "attach-session -c '/tmp/rift reroot project'");
    }

    #[test]
    fn test_reroot_command_escapes_an_embedded_single_quote() {
        let command = reroot_command(Path::new("/tmp/rift's project"))
            .expect("an embedded quote is a lexer-level, not line-framing, character");
        assert_eq!(command, "attach-session -c '/tmp/rift'\\''s project'");
    }

    #[test]
    fn test_reroot_command_with_newline_in_root_returns_none() {
        // `Attach::send_command` frames each command as one control-mode
        // line, terminated before tmux's lexer (and quote_tmux_arg's
        // escaping) ever runs; a literal `\n` would split this into two
        // control-mode commands, the second unquoted and attacker-controlled.
        // A POSIX path may legally contain `\n` (only `/` and NUL are
        // forbidden), so this must degrade to "no re-root", never send the
        // malformed command.
        assert_eq!(reroot_command(Path::new("/tmp/a\nkill-server")), None);
        assert_eq!(reroot_command(Path::new("/tmp/a\rkill-server")), None);
    }

    #[test]
    fn test_stamp_root_command_sets_at_root_with_quoted_session_and_root() {
        let command = stamp_root_command("rift", Path::new("/home/dev/proj"))
            .expect("plain session and root are safe");
        assert_eq!(command, "set -t 'rift' @root '/home/dev/proj'");
    }

    #[test]
    fn test_stamp_root_command_quotes_a_root_containing_a_space() {
        // Unquoted, tmux's command lexer would split this into two
        // arguments (same lexer hazard `reroot_command` documents).
        let command = stamp_root_command("rift", Path::new("/tmp/rift stamp project"))
            .expect("a space is a lexer-level, not line-framing, character");
        assert_eq!(command, "set -t 'rift' @root '/tmp/rift stamp project'");
    }

    #[test]
    fn test_stamp_root_command_quotes_a_session_containing_a_space() {
        let command = stamp_root_command("my session", Path::new("/tmp/proj"))
            .expect("a space in the session name is a lexer-level character");
        assert_eq!(command, "set -t 'my session' @root '/tmp/proj'");
    }

    #[test]
    fn test_stamp_root_command_escapes_an_embedded_single_quote() {
        let command = stamp_root_command("rift", Path::new("/tmp/rift's project"))
            .expect("an embedded quote is a lexer-level, not line-framing, character");
        assert_eq!(command, "set -t 'rift' @root '/tmp/rift'\\''s project'");
    }

    #[test]
    fn test_stamp_root_command_with_newline_in_root_returns_none() {
        // Same line-framing hazard `reroot_command` guards: a literal `\n`
        // in `root` would split this into two control-mode commands, the
        // second unquoted and attacker-controlled.
        assert_eq!(
            stamp_root_command("rift", Path::new("/tmp/a\nkill-server")),
            None
        );
        assert_eq!(
            stamp_root_command("rift", Path::new("/tmp/a\rkill-server")),
            None
        );
    }

    #[test]
    fn test_stamp_root_command_with_newline_in_session_returns_none() {
        // Unlike `reroot_command`, this command embeds the session name, so
        // it must be checked for the same line-framing hazard.
        assert_eq!(
            stamp_root_command("rift\nkill-server", Path::new("/tmp/proj")),
            None
        );
        assert_eq!(
            stamp_root_command("rift\rkill-server", Path::new("/tmp/proj")),
            None
        );
    }

    #[test]
    fn test_quote_tmux_arg_plain_wraps_in_single_quotes() {
        assert_eq!(quote_tmux_arg("proj"), "'proj'");
    }

    #[test]
    fn test_quote_tmux_arg_with_single_quote_is_escaped() {
        assert_eq!(quote_tmux_arg("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_parse_layout_line_full_fields() {
        let parsed =
            parse_layout_line("@0\t3\t1\t%1\t1\t51\t0\t49\t30\tnvim\t/home/dev/proj\t0\tbash")
                .expect("parse");
        assert_eq!(parsed.window_id, 0);
        assert_eq!(
            parsed.window_index, 3,
            "window_index is tmux's real index, independent of window_id"
        );
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
                current_path: "/home/dev/proj".to_owned(),
                current_command: "nvim".to_owned(),
                is_shell: false,
            }
        );
    }

    #[test]
    fn test_parse_layout_line_spaced_path_and_window_name_are_preserved() {
        // Both the cwd and the window name may contain spaces; the tab
        // separator keeps each a single field.
        let parsed = parse_layout_line(
            "@2\t5\t0\t%5\t0\t0\t0\t80\t24\tbash\t/home/dev/my repo\t1\tmy project",
        )
        .expect("parse");
        assert_eq!(parsed.window_index, 5);
        assert_eq!(parsed.window_name, "my project");
        assert_eq!(parsed.pane.current_path, "/home/dev/my repo");
        assert_eq!(parsed.pane.current_command, "bash");
        assert!(!parsed.window_active);
        assert!(!parsed.pane.active);
    }

    #[test]
    fn test_parse_layout_line_is_shell_true_for_shell_idle_pane() {
        // tmux's own `#{==:...}` comparison reports `1` when the pane's
        // foreground command is its default shell.
        let parsed =
            parse_layout_line("@0\t0\t1\t%1\t1\t0\t0\t80\t24\tbash\t/tmp\t1\tbash").expect("parse");
        assert!(parsed.pane.is_shell);
    }

    #[test]
    fn test_parse_layout_line_is_shell_false_for_running_process() {
        // Any foreground command other than the default shell compares
        // unequal — no client-side command taxonomy involved (#510).
        let parsed = parse_layout_line("@0\t0\t1\t%1\t1\t0\t0\t80\t24\tcargo\t/tmp\t0\tbash")
            .expect("parse");
        assert!(!parsed.pane.is_shell);
    }

    #[test]
    fn test_parse_layout_line_empty_meta_fields_parse_as_empty() {
        // tmux emits an empty field (two adjacent tabs) when a format
        // variable has no value; that must parse, not skip the pane.
        let parsed =
            parse_layout_line("@0\t0\t1\t%1\t1\t0\t0\t80\t24\t\t\t0\tbash").expect("parse");
        assert_eq!(parsed.pane.current_command, "");
        assert_eq!(parsed.pane.current_path, "");
        assert!(!parsed.pane.is_shell);
        assert_eq!(parsed.window_name, "bash");
    }

    #[test]
    fn test_parse_layout_line_malformed_returns_none() {
        assert_eq!(
            parse_layout_line("0\t0\t1\t%1\t1\t0\t0\t80\t24\tbash\t/tmp\tbash").map(|_| ()),
            None
        ); // window id no @
        assert_eq!(
            parse_layout_line("@0\tabc\t1\t%1\t1\t0\t0\t80\t24\tbash\t/tmp\tbash").map(|_| ()),
            None
        ); // window index non-numeric (#495)
        assert_eq!(
            parse_layout_line("@0\t0\t1\t1\t0\t0\t80\t24\tbash\t/tmp\tbash").map(|_| ()),
            None
        ); // pane id no %
        assert_eq!(
            parse_layout_line("@0\t0\t1\t%1\t1\t0\t0\t80").map(|_| ()),
            None
        ); // too few fields
        assert_eq!(
            parse_layout_line("@0 1 %1 1 0 0 80 24 bash").map(|_| ()),
            None
        ); // pre-#442 space-separated line
    }

    #[test]
    fn test_parse_layout_groups_panes_by_window_in_order() {
        let lines = vec![
            "@0\t0\t1\t%0\t0\t0\t0\t50\t30\tbash\t/tmp\t1\teditor".to_owned(),
            "@0\t0\t1\t%1\t1\t51\t0\t49\t30\tbash\t/tmp\t1\teditor".to_owned(),
            // window_index 2, not 1: simulates the gap tmux leaves after
            // closing window 1 (`renumber-windows` off, the default) — the
            // real index must survive, not collapse to array position (#495).
            "@1\t2\t0\t%2\t1\t0\t0\t100\t30\tbash\t/tmp\t1\tlogs".to_owned(),
        ];
        let windows = parse_layout(&lines);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].window_id, 0);
        assert_eq!(windows[0].window_index, 0);
        assert_eq!(windows[0].name, "editor");
        assert!(windows[0].active);
        assert_eq!(windows[0].panes.len(), 2);
        assert_eq!(windows[0].panes[0].pane_id, 0);
        assert_eq!(windows[0].panes[1].pane_id, 1);
        assert!(windows[0].panes[1].active);
        assert_eq!(windows[1].window_id, 1);
        assert_eq!(
            windows[1].window_index, 2,
            "the real tmux window index must survive even when non-contiguous \
             with the previous window's index"
        );
        assert_eq!(windows[1].name, "logs");
        assert!(!windows[1].active);
        assert_eq!(windows[1].panes.len(), 1);
    }

    #[test]
    fn test_parse_layout_skips_malformed_lines() {
        let lines = vec![
            "garbage line".to_owned(),
            "@0\t0\t1\t%0\t1\t0\t0\t80\t24\tbash\t/tmp\t1\tbash".to_owned(),
        ];
        let windows = parse_layout(&lines);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].panes.len(), 1);
    }

    #[test]
    fn test_parse_session_line_full_fields() {
        let parsed = parse_session_line("$3\t2\t1\t\trift").expect("parse");
        assert_eq!(
            parsed,
            SessionEntry {
                id: 3,
                name: "rift".to_owned(),
                windows: 2,
                attached: true,
                root: None,
            }
        );
    }

    #[test]
    fn test_parse_session_line_name_with_spaces_and_tabs_preserved() {
        // The name is the LAST field precisely so spaces and even tabs inside
        // it survive the split (the spec's malformed-name risk).
        let parsed = parse_session_line("$0\t1\t0\t\tmy project\twith tab").expect("parse");
        assert_eq!(parsed.name, "my project\twith tab");
        assert!(!parsed.attached);
    }

    #[test]
    fn test_parse_session_line_attached_count_folds_to_bool() {
        // `#{session_attached}` is a client COUNT: several attached clients
        // still mean "attached", zero means not.
        assert!(
            parse_session_line("$0\t1\t2\t\trift")
                .expect("parse")
                .attached
        );
        assert!(
            !parse_session_line("$0\t1\t0\t\trift")
                .expect("parse")
                .attached
        );
    }

    #[test]
    fn test_parse_session_line_root_set_yields_some() {
        let parsed = parse_session_line("$0\t1\t1\t/home/dev/proj\trift").expect("parse");
        assert_eq!(parsed.root, Some("/home/dev/proj".to_owned()));
    }

    #[test]
    fn test_parse_session_line_root_empty_yields_none() {
        let parsed = parse_session_line("$0\t1\t1\t\trift").expect("parse");
        assert_eq!(parsed.root, None);
    }

    #[test]
    fn test_parse_session_line_malformed_returns_none() {
        assert_eq!(parse_session_line("0\t1\t1\t\trift").map(|_| ()), None); // id no $
        assert_eq!(parse_session_line("$0\t1\t1").map(|_| ()), None); // too few fields
        assert_eq!(parse_session_line("$0\tmany\t1\t\trift").map(|_| ()), None); // windows not numeric
        assert_eq!(parse_session_line("$0\t1\tyes\t\trift").map(|_| ()), None); // attached not numeric
        assert_eq!(parse_session_line("$0 1 1 rift").map(|_| ()), None); // space-separated
        assert_eq!(parse_session_line("").map(|_| ()), None);
    }

    #[test]
    fn test_parse_session_list_skips_malformed_lines() {
        let lines = vec![
            "garbage".to_owned(),
            "$0\t1\t1\t\trift".to_owned(),
            "$5\t3\t0\t/home/dev/scratch\tscratch".to_owned(),
        ];
        let sessions = parse_session_list(&lines);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, 0);
        assert_eq!(sessions[1].name, "scratch");
        assert!(!sessions[1].attached);
        assert_eq!(sessions[1].root, Some("/home/dev/scratch".to_owned()));
    }

    #[test]
    fn test_resolve_session_root_prefers_non_empty_root_option() {
        let resolved = resolve_session_root("/home/dev/proj", "/home/dev/other");
        assert_eq!(resolved, Some(PathBuf::from("/home/dev/proj")));
    }

    #[test]
    fn test_resolve_session_root_falls_back_to_session_path_when_root_option_empty() {
        let resolved = resolve_session_root("", "/home/dev/other");
        assert_eq!(resolved, Some(PathBuf::from("/home/dev/other")));
    }

    #[test]
    fn test_resolve_session_root_returns_none_when_both_empty() {
        assert_eq!(resolve_session_root("", ""), None);
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
        spawn_task_with_root(server, outbound_cap, None)
    }

    /// Like [`spawn_task`] but with the daemon's configured project root
    /// (`docs/spec-per-session-project-root.md`), for fixtures that exercise
    /// the `@root` stamp / re-root (`Attach::spawn`'s `root` parameter).
    fn spawn_task_with_root(
        server: &TmuxServer,
        outbound_cap: usize,
        root: Option<PathBuf>,
    ) -> (
        mpsc::Sender<ClientMessage>,
        mpsc::Receiver<DaemonMessage>,
        tokio::task::JoinHandle<()>,
    ) {
        let (in_tx, in_rx) = mpsc::channel(64);
        let (out_tx, out_rx) = mpsc::channel(outbound_cap);
        // The resolved-root receiver is not observed by these fixtures; #737's
        // `test_attach_sends_resolved_root_on_the_internal_channel` constructs
        // `terminal_task` directly to observe it instead.
        let (root_resolved_tx, _root_resolved_rx) = mpsc::channel(4);
        let socket = server.name.clone();
        let handle = tokio::spawn(terminal_task(
            in_rx,
            out_tx,
            Some(socket),
            root,
            root_resolved_tx,
        ));
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
                root: None,
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
        // The pane metadata added in #442: a fresh pane runs the shell in the
        // directory the session was created from, so both fields are live.
        let pane = &windows[0].panes[0];
        assert!(
            pane.current_path.starts_with('/'),
            "pane cwd must be an absolute path: {:?}",
            pane.current_path
        );
        assert!(
            !pane.current_command.is_empty(),
            "pane current command must name the running shell"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_attach_with_root_stamps_at_root_readable_via_display_message() {
        // Validates `stamp_root_command` against a real tmux server
        // (`docs/spec-per-session-project-root.md` acceptance): after attach,
        // `@root` is independently queryable — via a plain `tmux
        // display-message`, outside the daemon's own control-mode child —
        // and carries the daemon's configured root.
        let server = TmuxServer::new("rootstamp");
        let root = std::env::temp_dir().join("rift-root-coupling-test");
        let (in_tx, mut out_rx, task) = spawn_task_with_root(&server, 256, Some(root.clone()));

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
            })
            .await
            .expect("send attach");

        recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutSnapshot { .. } => Some(()),
            _ => None,
        })
        .await
        .expect("layout snapshot after attach");

        let output = std::process::Command::new("tmux")
            .args([
                "-L",
                &server.name,
                "display-message",
                "-p",
                "-t",
                "rift",
                "#{@root}",
            ])
            .output()
            .expect("query @root via tmux display-message");
        assert!(output.status.success(), "tmux display-message failed");
        let stamped = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        assert_eq!(stamped, root.to_string_lossy());

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_attach_sends_resolved_root_on_the_internal_channel() {
        // #737 (the Attach seam): once the `ROOT_QUERY` reply lands,
        // `Attach::process` reports the resolved root on the internal
        // `RootResolved` channel — the signal `serve_connection` (`lib.rs`)
        // consumes to drive the per-root `ContextMap` re-root. Bypasses
        // `spawn_task_with_root` (which discards this channel) to observe it
        // directly.
        let server = TmuxServer::new("resolvedroot");
        let root = std::env::temp_dir().join("rift-resolved-root-test");
        let (in_tx, in_rx) = mpsc::channel(64);
        let (out_tx, mut out_rx) = mpsc::channel(256);
        let (root_resolved_tx, mut root_resolved_rx) = mpsc::channel(4);
        let task = tokio::spawn(terminal_task(
            in_rx,
            out_tx,
            Some(server.name.clone()),
            Some(root.clone()),
            root_resolved_tx,
        ));

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
            })
            .await
            .expect("send attach");

        recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutSnapshot { .. } => Some(()),
            _ => None,
        })
        .await
        .expect("layout snapshot after attach");

        let resolved = tokio::time::timeout(Duration::from_secs(10), root_resolved_rx.recv())
            .await
            .expect("resolved-root channel fires within the timeout")
            .expect("channel stays open while the terminal task is alive");
        assert_eq!(resolved, Some(root));

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
                    root: None,
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
                root: None,
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
                root: None,
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
                root: None,
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
                root: None,
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

    #[tokio::test]
    async fn test_layout_update_after_split() {
        let server = TmuxServer::new("split");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
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
                root: None,
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
                root: None,
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
    async fn test_layout_update_after_kill_window_keeps_real_tmux_indices() {
        // `renumber-windows` is off by default, so killing a non-last window
        // leaves a gap in tmux's own index numbering. The layout must surface
        // that real gap, not a synthesized array-position stand-in that would
        // silently close it (#495).
        let server = TmuxServer::new("killwin");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::LayoutSnapshot { .. }).then_some(())
        })
        .await
        .expect("snapshot");

        // Two more windows.
        for _ in 0..2 {
            in_tx
                .send(ClientMessage::TmuxCommand {
                    cmd: "new-window".to_owned(),
                })
                .await
                .expect("new-window");
        }
        let indices_before = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                if windows.len() != 3 {
                    return None;
                }
                let mut indices: Vec<u32> = windows.iter().map(|w| w.window_index).collect();
                indices.sort_unstable();
                Some(indices)
            }
            _ => None,
        })
        .await
        .expect("3-window layout update");

        // Kill the middle window by its real tmux index (not an assumed
        // array position — the whole point under test).
        let middle = indices_before[1];
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: format!("kill-window -t :{middle}"),
            })
            .await
            .expect("kill-window");

        let indices_after = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { windows, .. } => {
                if windows.len() != 2 {
                    return None;
                }
                let mut indices: Vec<u32> = windows.iter().map(|w| w.window_index).collect();
                indices.sort_unstable();
                Some(indices)
            }
            _ => None,
        })
        .await;
        assert_eq!(
            indices_after,
            Some(vec![indices_before[0], indices_before[2]]),
            "surviving windows must keep their real (now-gapped) indices, not renumber to be \
             contiguous"
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
                root: None,
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
                root: None,
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
                root: None,
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

    // --- session list + external switch-client (docs/spec-session-switch.md) ---

    #[tokio::test]
    async fn test_query_session_list_returns_attached_session() {
        let server = TmuxServer::new("sesslist-query");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
            })
            .await
            .expect("attach");
        recv_until(&mut out_rx, 10, |m| {
            matches!(m, DaemonMessage::LayoutSnapshot { .. }).then_some(())
        })
        .await
        .expect("snapshot");

        in_tx
            .send(ClientMessage::QuerySessionList)
            .await
            .expect("query session list");
        let sessions = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::SessionListReply { sessions } => Some(sessions.clone()),
            _ => None,
        })
        .await
        .expect("session-list reply");
        let rift = sessions
            .iter()
            .find(|s| s.name == "rift")
            .expect("the attached session must be listed");
        assert!(rift.attached, "this attach counts as an attached client");
        assert!(rift.windows >= 1, "a live session has at least one window");

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_query_session_list_pre_attach_no_server_replies_empty() {
        // #757: the picker queries the list BEFORE any Attach. With no tmux
        // server running (fresh boot, or every session killed), the daemon must
        // answer with an EMPTY SessionListReply so the picker reaches its
        // create-only state, not drop the query and hang the client.
        let server = TmuxServer::new("presess-empty");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::QuerySessionList)
            .await
            .expect("query session list");
        let sessions = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::SessionListReply { sessions } => Some(sessions.clone()),
            _ => None,
        })
        .await
        .expect("session-list reply");
        assert!(
            sessions.is_empty(),
            "no tmux server running must reply with an empty list, got {sessions:?}"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_query_session_list_pre_attach_lists_existing_sessions() {
        // #757: with a server that already has sessions, the pre-attach picker
        // query lists them WITHOUT the client ever attaching through the task.
        let server = TmuxServer::new("presess-list");
        let status = std::process::Command::new("tmux")
            .args(["-L", &server.name, "new-session", "-d", "-s", "rift757pre"])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run tmux new-session");
        assert!(status.success(), "seed a session out-of-band");

        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);
        in_tx
            .send(ClientMessage::QuerySessionList)
            .await
            .expect("query session list");
        let sessions = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::SessionListReply { sessions } => Some(sessions.clone()),
            _ => None,
        })
        .await
        .expect("session-list reply");
        assert!(
            sessions.iter().any(|s| s.name == "rift757pre"),
            "the pre-existing session must be listed pre-attach, got {sessions:?}"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_session_create_and_kill_push_session_list_unprompted() {
        // Session churn (%sessions-changed) must push a fresh SessionListReply
        // with NO client request — the live-list contract.
        let server = TmuxServer::new("sesslist-churn");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
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
                cmd: "new-session -d -s rift465other".to_owned(),
            })
            .await
            .expect("new-session");
        let created = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::SessionListReply { sessions } => sessions
                .iter()
                .any(|s| s.name == "rift465other")
                .then_some(()),
            _ => None,
        })
        .await;
        assert!(
            created.is_some(),
            "creating a session must push a SessionListReply listing it, unprompted"
        );

        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "kill-session -t rift465other".to_owned(),
            })
            .await
            .expect("kill-session");
        let dropped = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::SessionListReply { sessions } => sessions
                .iter()
                .all(|s| s.name != "rift465other")
                .then_some(()),
            _ => None,
        })
        .await;
        assert!(
            dropped.is_some(),
            "killing a session must push a SessionListReply without it, unprompted"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_session_rename_pushes_session_list_unprompted() {
        // %session-renamed is broadcast for ANY session on the server; the
        // list must refresh even when the renamed session is not this attach's.
        let server = TmuxServer::new("sesslist-rename");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
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
                cmd: "new-session -d -s rift465ren".to_owned(),
            })
            .await
            .expect("new-session");
        recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::SessionListReply { sessions } => sessions
                .iter()
                .any(|s| s.name == "rift465ren")
                .then_some(()),
            _ => None,
        })
        .await
        .expect("create push");

        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "rename-session -t rift465ren rift465renamed".to_owned(),
            })
            .await
            .expect("rename-session");
        let renamed = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::SessionListReply { sessions } => {
                (sessions.iter().any(|s| s.name == "rift465renamed")
                    && sessions.iter().all(|s| s.name != "rift465ren"))
                .then_some(())
            }
            _ => None,
        })
        .await;
        assert!(
            renamed.is_some(),
            "renaming a foreign session must push a SessionListReply with the new name, unprompted"
        );

        drop(in_tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_external_switch_client_refreshes_layout() {
        // An external `switch-client` (no ClientMessage::Attach involved) sends
        // only %session-changed — the daemon must re-query so both the session
        // echo and the terminal content refresh immediately instead of staying
        // stale until the next structural event (spec finding B1).
        let server = TmuxServer::new("switchclient");
        let (in_tx, mut out_rx, task) = spawn_task(&server, 256);

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
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
                cmd: "new-session -d -s rift465b".to_owned(),
            })
            .await
            .expect("new-session");
        in_tx
            .send(ClientMessage::TmuxCommand {
                cmd: "switch-client -t rift465b".to_owned(),
            })
            .await
            .expect("switch-client");

        let windows = recv_until(&mut out_rx, 10, |m| match m {
            DaemonMessage::LayoutUpdate { session, windows } if session == "rift465b" => {
                Some(windows.clone())
            }
            _ => None,
        })
        .await;
        let windows = windows.expect(
            "an external switch-client must surface a LayoutUpdate carrying the target session",
        );
        assert!(
            !windows.is_empty(),
            "the refreshed layout must carry the target session's windows"
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
                    root: None,
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
                root: None,
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
                root: None,
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
                root: None,
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
