//! Throwaway measurement harness for the VTE-location spike (issue #201) —
//! DELETE with the real terminal-streaming implementation (issues #202–#205).
//!
//! Runs the same workload over both terminal transports and feeds the received
//! bytes into a client-side `alacritty_terminal::Term`, so the numbers include
//! the client-side VTE cost the spike is judging:
//!
//! - **direct**: `tmux -CC` over an SSH PTY channel (today's production path)
//! - **daemon**: `tmux -C` child over pipes -> spike daemon -> UDS `--connect`
//!   relay -> SSH exec channel -> `DaemonClient` (the target topology)
//!
//! Both paths use the same minimal `%output` line parser, so the comparison
//! isolates the transport, not parser implementations. Each path runs on its
//! own throwaway tmux server (`-L <label>`), never touching the user's tmux.
//!
//! Run from the workspace root (the daemon path needs the musl spike build):
//!
//! ```text
//! cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl
//! cargo run --release -p rift-ssh --example vte_location_spike
//! ```
//!
//! Env: `RIFT_SSH_HOST`/`PORT`/`USER`/`KEY` (app defaults), `RIFT_DAEMON_BINARY`
//! (default `target/x86_64-unknown-linux-musl/release/rift-daemon`),
//! `RIFT_SPIKE_FLOOD_LINES` (default 500000). Flow control (`pause-after`) is
//! activated on neither path, so the flood numbers are raw pipeline throughput.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use anyhow::{bail, Context, Result};
use rift_protocol::{ClientMessage, DaemonMessage, PROTOCOL_VERSION};
use rift_ssh::{DaemonClient, PtyWriter, SshConnection};
use tokio::sync::mpsc;

const LATENCY_ITERATIONS: usize = 50;
const LATENCY_CHARS: &[u8] = b"qwertyuiopzxcvbn";
/// Echoed as `RIFT_PROOF_$((6*7))` / `RIFT_FLOOD_$((40+2))` so the marker only
/// exists in the pane's *output* — the echoed command line shows the literal
/// arithmetic and cannot match.
const PROOF_MARKER: &str = "RIFT_PROOF_42";
const FLOOD_MARKER: &str = "RIFT_FLOOD_42";

#[tokio::main]
async fn main() -> Result<()> {
    let host = std::env::var("RIFT_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("RIFT_SSH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(22);
    let user = std::env::var("RIFT_SSH_USER").unwrap_or_else(|_| "developer".into());
    let key = std::env::var("RIFT_SSH_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home/developer".into());
            PathBuf::from(home).join(".ssh").join("id_rsa")
        });
    let flood_lines: u64 = std::env::var("RIFT_SPIKE_FLOOD_LINES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(500_000);
    let pid = std::process::id();

    println!("vte_location_spike: host={host}:{port} user={user} flood_lines={flood_lines}");

    let mut conn = SshConnection::connect(&host, port, &user, &key)
        .await
        .context("SSH connection failed")?;

    // Path 1: direct -CC over an SSH PTY (production path).
    let label = format!("rift-spike-direct-{pid}");
    let (writer, mut rx) = start_direct(&mut conn, &label).await?;
    let sender = Sender::Direct(writer);
    let direct = measure(
        "direct: tmux -CC over SSH PTY",
        &sender,
        &mut rx,
        flood_lines,
    )
    .await?;
    sender.command("kill-server").await?;

    // Path 2: tmux -C pipes -> spike daemon -> UDS relay -> SSH exec channel.
    let session = format!("rift-spike-daemon-{pid}");
    let (client, mut rx, cleanup) = start_daemon_path(&mut conn, &session).await?;
    let sender = Sender::Daemon(client);
    let daemon = measure(
        "daemon: tmux -C pipes -> spike daemon -> UDS relay -> SSH exec",
        &sender,
        &mut rx,
        flood_lines,
    )
    .await?;
    sender.command("kill-server").await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = conn
        .exec_capture(&format!(
            "rm -f {} {} {}",
            cleanup.bin, cleanup.sock, cleanup.log
        ))
        .await;

    print_stats(&direct);
    print_stats(&daemon);
    Ok(())
}

/// Decoded events both transports reduce to.
enum Msg {
    Output(u32, Vec<u8>),
    PaneId(u32),
}

/// Input/command emission over either transport.
enum Sender {
    Direct(PtyWriter),
    Daemon(Arc<DaemonClient>),
}

impl Sender {
    async fn input(&self, pane: u32, data: &str) -> Result<()> {
        match self {
            // Mirror the production direct path: keystrokes go through
            // `send-keys -H` on the control channel.
            Sender::Direct(writer) => {
                let line = format!("send-keys -t %{pane} -H {}\n", hex_args(data.as_bytes()));
                writer.write(line.as_bytes()).await?;
            }
            Sender::Daemon(client) => {
                client
                    .send(ClientMessage::Input {
                        pane_id: pane,
                        data: data.to_owned(),
                    })
                    .await?;
            }
        }
        Ok(())
    }

    async fn command(&self, cmd: &str) -> Result<()> {
        match self {
            Sender::Direct(writer) => writer.write(format!("{cmd}\n").as_bytes()).await?,
            Sender::Daemon(client) => {
                client
                    .send(ClientMessage::TmuxCommand {
                        cmd: cmd.to_owned(),
                    })
                    .await?;
            }
        }
        Ok(())
    }
}

/// Start the direct path: `tmux -CC` on its own server over an SSH PTY, with a
/// reader task reducing the control-mode stream to [`Msg`]s.
async fn start_direct(
    conn: &mut SshConnection,
    label: &str,
) -> Result<(PtyWriter, mpsc::UnboundedReceiver<Msg>)> {
    let pty = conn
        .open_pty_exec(
            80,
            24,
            &format!("tmux -L {label} -CC new-session -A -s {label}"),
        )
        .await
        .context("failed to start direct tmux -CC path")?;
    let writer = pty.clone_writer();
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut buf: Vec<u8> = Vec::new();
        while let Ok(chunk) = pty.read().await {
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = buf.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let line = String::from_utf8_lossy(&line);
                if let Some(msg) = parse_line(&line) {
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
            }
        }
    });

    // Let tmux take the tty into raw mode before the first command, so the
    // command bytes are not mangled by the PTY's initial canonical mode.
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok((writer, rx))
}

struct DaemonPathRemote {
    bin: String,
    sock: String,
    log: String,
}

/// Start the daemon path: upload the musl spike binary, launch it detached
/// (`--spike-serve-uds`), attach through the `--connect` relay, handshake, and
/// reduce `DaemonMessage`s to [`Msg`]s.
async fn start_daemon_path(
    conn: &mut SshConnection,
    session: &str,
) -> Result<(
    Arc<DaemonClient>,
    mpsc::UnboundedReceiver<Msg>,
    DaemonPathRemote,
)> {
    let local_bin = std::env::var("RIFT_DAEMON_BINARY")
        .unwrap_or_else(|_| "target/x86_64-unknown-linux-musl/release/rift-daemon".into());
    let bytes = tokio::fs::read(&local_bin)
        .await
        .with_context(|| format!("failed to read local daemon binary {local_bin}"))?;

    let remote = DaemonPathRemote {
        bin: format!("/tmp/{session}-daemon"),
        sock: format!("/tmp/{session}.sock"),
        log: format!("/tmp/{session}.log"),
    };
    conn.upload_executable(&bytes, &remote.bin).await?;

    // Same detached-launch shape as production (`crates/ssh/src/launch.rs`),
    // pointed at the spike subcommand. Paths are /tmp-fixed, no quoting needed.
    let launch = format!(
        "setsid sh -c 'exec {bin} --spike-serve-uds {sock} {session} </dev/null >> {log} 2>&1' & \
         i=0; while [ $i -lt 50 ]; do [ -S {sock} ] && {{ printf RIFT_READY; exit 0; }}; \
         i=$((i+1)); sleep 0.1; done; printf RIFT_TIMEOUT",
        bin = remote.bin,
        sock = remote.sock,
        log = remote.log,
    );
    let out = conn.exec_capture(&launch).await?;
    if out.trim() != "RIFT_READY" {
        bail!("spike daemon launch failed: {out:?}");
    }

    let channel = conn
        .open_daemon_channel(&format!("{} --connect {}", remote.bin, remote.sock))
        .await?;
    let client = Arc::new(DaemonClient::new(channel));

    client
        .send(ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        })
        .await?;
    match client.recv().await {
        Some(DaemonMessage::Welcome { .. }) => {}
        other => bail!("unexpected spike daemon handshake reply: {other:?}"),
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let reader = client.clone();
    tokio::spawn(async move {
        while let Some(msg) = reader.recv().await {
            let mapped = match msg {
                DaemonMessage::PaneOutput { pane_id, bytes } => Msg::Output(pane_id, bytes),
                DaemonMessage::StateUpdate { sessions } => {
                    match sessions
                        .first()
                        .and_then(|s| s.strip_prefix('%'))
                        .and_then(|s| s.parse().ok())
                    {
                        Some(id) => Msg::PaneId(id),
                        None => continue,
                    }
                }
                _ => continue,
            };
            if tx.send(mapped).is_err() {
                return;
            }
        }
    });

    Ok((client, rx, remote))
}

struct PathStats {
    name: &'static str,
    render_proof: bool,
    /// Sorted send->echo round trips, milliseconds.
    latencies_ms: Vec<f64>,
    flood_bytes: u64,
    flood_secs: f64,
}

/// Run the measurement sequence over an established path: resolve the pane id,
/// prove client-side rendering, time keystroke echo round trips, then time a
/// flooding pane. All received bytes are fed into a client-side `Term`.
async fn measure(
    name: &'static str,
    sender: &Sender,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    flood_lines: u64,
) -> Result<PathStats> {
    // Pane id: ask tmux, the RIFT_PANE response line comes back as Msg::PaneId.
    // The argument must be quoted: an unquoted `#` starts a comment in tmux's
    // command parser and `{` opens a brace block -> "parse error".
    sender
        .command("display-message -p 'RIFT_PANE:#{pane_id}'")
        .await?;
    let pane = loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Some(Msg::PaneId(id))) => break id,
            Ok(Some(Msg::Output(..))) => continue,
            Ok(None) => bail!("{name}: stream closed before pane id arrived"),
            Err(_) => bail!("{name}: timed out waiting for pane id"),
        }
    };

    let mut term = Term::new(
        Config::default(),
        &SpikeSize {
            columns: 80,
            lines: 24,
        },
        VoidListener,
    );
    let mut parser: Processor = Processor::new();

    // Let the shell prompt settle: drain until 500ms of quiet.
    drain_quiet(rx, pane, &mut term, &mut parser, Duration::from_millis(500)).await;

    // Render proof: the marker only exists in the pane's output (see above),
    // and it must show up in the client-side Term grid.
    sender.input(pane, "echo RIFT_PROOF_$((6*7))\r").await?;
    let proof_deadline = Instant::now() + Duration::from_secs(5);
    let mut render_proof = false;
    while Instant::now() < proof_deadline {
        match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
            Ok(Some(Msg::Output(p, bytes))) if p == pane => {
                parser.advance(&mut term, &bytes);
                if grid_contains(&term, PROOF_MARKER) {
                    render_proof = true;
                    break;
                }
            }
            Ok(Some(_)) => continue,
            Ok(None) => bail!("{name}: stream closed during render proof"),
            Err(_) => continue,
        }
    }
    drain_quiet(rx, pane, &mut term, &mut parser, Duration::from_millis(300)).await;

    // Interactive latency: keystroke -> shell echo round trip.
    let mut latencies_ms = Vec::with_capacity(LATENCY_ITERATIONS);
    for i in 0..LATENCY_ITERATIONS {
        let c = LATENCY_CHARS[i % LATENCY_CHARS.len()];
        let needle = c;
        let started = Instant::now();
        sender.input(pane, std::str::from_utf8(&[c])?).await?;
        loop {
            match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                Ok(Some(Msg::Output(p, bytes))) if p == pane => {
                    parser.advance(&mut term, &bytes);
                    if bytes.contains(&needle) {
                        latencies_ms.push(started.elapsed().as_secs_f64() * 1000.0);
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => bail!("{name}: stream closed during latency loop"),
                Err(_) => bail!("{name}: keystroke echo timed out (iteration {i})"),
            }
        }
    }
    // Ctrl-U: clear the accumulated junk command line.
    sender.input(pane, "\u{15}").await?;
    drain_quiet(rx, pane, &mut term, &mut parser, Duration::from_millis(300)).await;
    latencies_ms.sort_by(|a, b| a.total_cmp(b));

    // Flooding pane: raw end-to-end throughput including client-side VTE cost.
    sender
        .input(
            pane,
            &format!("seq 1 {flood_lines}; echo RIFT_FLOOD_$((40+2))\r"),
        )
        .await?;
    let marker = FLOOD_MARKER.as_bytes();
    let mut window: Vec<u8> = Vec::new();
    let mut flood_bytes = 0u64;
    let mut first_chunk: Option<Instant> = None;
    let flood_secs = loop {
        match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
            Ok(Some(Msg::Output(p, bytes))) if p == pane => {
                let now = Instant::now();
                let started = *first_chunk.get_or_insert(now);
                flood_bytes += bytes.len() as u64;
                parser.advance(&mut term, &bytes);
                window.extend_from_slice(&bytes);
                if window.windows(marker.len()).any(|w| w == marker) {
                    break now.duration_since(started).as_secs_f64();
                }
                let keep = (marker.len() - 1).min(window.len());
                window.drain(..window.len() - keep);
            }
            Ok(Some(_)) => continue,
            Ok(None) => bail!("{name}: stream closed during flood"),
            Err(_) => bail!("{name}: flood marker never arrived (output lost?)"),
        }
    };

    Ok(PathStats {
        name,
        render_proof,
        latencies_ms,
        flood_bytes,
        flood_secs,
    })
}

/// Drain (and feed into the Term) everything arriving for `pane` until the
/// stream has been quiet for `quiet`.
async fn drain_quiet(
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    pane: u32,
    term: &mut Term<VoidListener>,
    parser: &mut Processor,
    quiet: Duration,
) {
    while let Ok(Some(msg)) = tokio::time::timeout(quiet, rx.recv()).await {
        if let Msg::Output(p, bytes) = msg {
            if p == pane {
                parser.advance(term, &bytes);
            }
        }
    }
}

fn print_stats(stats: &PathStats) {
    println!("=== {} ===", stats.name);
    println!(
        "render proof (marker visible in client-side Term grid): {}",
        if stats.render_proof { "OK" } else { "FAILED" }
    );
    let n = stats.latencies_ms.len();
    if n > 0 {
        let median = stats.latencies_ms[n / 2];
        let p95 = stats.latencies_ms[(n * 95 / 100).min(n - 1)];
        let max = stats.latencies_ms[n - 1];
        println!("latency  n={n} median={median:.2}ms p95={p95:.2}ms max={max:.2}ms");
    }
    let mb = stats.flood_bytes as f64 / (1024.0 * 1024.0);
    println!(
        "flood    {:.2} MiB in {:.2}s = {:.2} MiB/s (decoded payload, from first chunk)",
        mb,
        stats.flood_secs,
        mb / stats.flood_secs.max(f64::EPSILON)
    );
}

/// Reduce one control-mode / harness line to a [`Msg`]. Same shape as the
/// spike daemon's parser, so both transports are measured behind identical
/// client-side parsing.
fn parse_line(line: &str) -> Option<Msg> {
    if let Some(rest) = line.strip_prefix("%output %") {
        let (id, payload) = rest.split_once(' ')?;
        return Some(Msg::Output(id.parse().ok()?, decode_octal_escapes(payload)));
    }
    if let Some(pane) = line.strip_prefix("RIFT_PANE:%") {
        return Some(Msg::PaneId(pane.trim().parse().ok()?));
    }
    None
}

/// Decode tmux's octal escaping — duplicated from the spike daemon module on
/// purpose: this example is throwaway and must not grow a crate dependency on
/// rift-daemon for two helper functions.
fn decode_octal_escapes(escaped: &str) -> Vec<u8> {
    let bytes = escaped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && (b'0'..=b'3').contains(&bytes[i + 1])
            && (b'0'..=b'7').contains(&bytes[i + 2])
            && (b'0'..=b'7').contains(&bytes[i + 3])
        {
            let value =
                (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(value);
            i += 4;
        } else if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            out.push(b'\\');
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn hex_args(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

struct SpikeSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for SpikeSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

fn grid_contains(term: &Term<VoidListener>, needle: &str) -> bool {
    let grid = term.grid();
    (0..grid.screen_lines()).any(|l| {
        let row: String = (0..grid.columns())
            .map(|c| grid[Line(l as i32)][Column(c)].c)
            .collect();
        row.contains(needle)
    })
}
