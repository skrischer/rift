//! Throwaway VTE-location spike wiring (issue #201) — DELETE with the real
//! terminal-streaming implementation (issues #202–#205).
//!
//! Spawns a `tmux -C` child over plain pipes (single `-C`: no tty required —
//! verified; `-CC` fails `tcgetattr` on a pipe, see `docs/tmux-reference.md`),
//! forwards raw decoded `%output` bytes to connected clients as
//! `DaemonMessage::PaneOutput` frames, and routes `ClientMessage::Input` /
//! `TmuxCommand` back to the tmux stdin. Deliberately minimal: no flow
//! control, no command-response correlation, the placeholder protocol
//! messages are reused as-is (`PaneOutput.cells` carries raw bytes; the real
//! message set is issue #203). The pane id is announced to clients through a
//! `StateUpdate` whose single entry is the `%<id>` string.

use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use rift_protocol::{ClientMessage, DaemonMessage, PROTOCOL_VERSION};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc};

/// Broadcast backlog for the spike. Sized so a multi-megabyte flood (the
/// throughput measurement) never trips the `Lagged` drop path in
/// `serve_connection` and silently corrupts the measurement — the real
/// implementation owns bounded flow control instead (spec constraint).
const SPIKE_EVENT_CAPACITY: usize = 65_536;
const SPIKE_INBOUND_CAPACITY: usize = 256;

/// Run the spike daemon: serve the rift protocol on `socket_path` while
/// forwarding one tmux session's `%output` bytes, until the tmux child exits
/// (e.g. via a client-sent `kill-server`).
pub async fn serve_spike(socket_path: &Path, session: &str) -> anyhow::Result<()> {
    if socket_path.exists() {
        anyhow::bail!(
            "spike socket {} already exists; use a fresh per-run path",
            socket_path.display()
        );
    }
    let listener = UnixListener::bind(socket_path)?;

    // `-L <session>` puts the spike on its own tmux server socket, so a
    // client-sent `kill-server` can never reach the user's real tmux server.
    let mut child = tokio::process::Command::new("tmux")
        .args(["-L", session, "-C", "new-session", "-A", "-s", session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to spawn tmux -C child over pipes")?;
    let mut tmux_in = child.stdin.take().context("tmux child has no stdin")?;
    let tmux_out = child.stdout.take().context("tmux child has no stdout")?;

    let (events_tx, _keep_bus_open) = broadcast::channel(SPIKE_EVENT_CAPACITY);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(SPIKE_INBOUND_CAPACITY);

    // Inbound client messages -> tmux stdin (single writer, so client input
    // and the spike's own commands serialize on one queue).
    let events_for_inbound = events_tx.clone();
    let input_task = tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let line = match msg {
                ClientMessage::Hello { version: _ } => {
                    let _ = events_for_inbound.send(DaemonMessage::Welcome {
                        version: PROTOCOL_VERSION,
                    });
                    continue;
                }
                ClientMessage::Input { pane_id, data } => {
                    format!("send-keys -t %{pane_id} -H {}\n", hex_args(data.as_bytes()))
                }
                ClientMessage::TmuxCommand { cmd } => format!("{cmd}\n"),
                ClientMessage::ResizePane { .. } => continue,
            };
            if tmux_in.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    // Accept loop: each client connection reuses the shared frame-serving
    // plumbing against this spike's bus and inbound queue. Clients trigger the
    // pane-id announcement themselves (a `display-message -p RIFT_PANE:...`
    // TmuxCommand) so the resulting `StateUpdate` lands on a live subscription.
    let accept_inbound = inbound_tx;
    let accept_events = events_tx.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let (reader, writer) = stream.into_split();
                    let inbound = accept_inbound.clone();
                    let events = accept_events.subscribe();
                    tokio::spawn(async move {
                        if let Err(e) =
                            crate::serve_connection(reader, writer, inbound, events).await
                        {
                            eprintln!("rift-daemon spike connection ended with error: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("rift-daemon spike accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    });

    // Main loop: tmux control-mode stdout -> protocol events. Line-based is
    // safe because tmux octal-escapes control bytes, so payloads never carry
    // a raw newline.
    let mut lines = BufReader::new(tmux_out).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some((pane_id, bytes)) = parse_output_line(&line) {
            let _ = events_tx.send(DaemonMessage::PaneOutput {
                pane_id,
                cells: bytes,
            });
        } else if let Some(pane) = line.strip_prefix("RIFT_PANE:") {
            let _ = events_tx.send(DaemonMessage::StateUpdate {
                sessions: vec![pane.to_owned()],
            });
        } else if line.starts_with("%exit") {
            break;
        }
    }

    // tmux is gone (or sent %exit): stop serving and clean up the socket so
    // the spike leaves nothing behind.
    accept_task.abort();
    input_task.abort();
    let _ = child.wait().await;
    let _ = tokio::fs::remove_file(socket_path).await;
    Ok(())
}

/// Parse a `%output %<pane_id> <escaped>` notification line into the pane id
/// and the decoded payload bytes. Returns `None` for anything else, including
/// malformed `%output` lines.
fn parse_output_line(line: &str) -> Option<(u32, Vec<u8>)> {
    let rest = line.strip_prefix("%output %")?;
    let (id, payload) = rest.split_once(' ')?;
    let pane_id = id.parse().ok()?;
    Some((pane_id, decode_octal_escapes(payload)))
}

/// Decode tmux's octal escaping (OpenBSD vis, `VIS_OCTAL`): `\ooo` becomes the
/// byte it names, `\\` becomes a backslash, everything else passes through —
/// including malformed escapes, which are kept literally rather than dropped.
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

/// Render bytes as the space-separated hex arguments `send-keys -H` expects.
fn hex_args(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_output_line_plain_payload_returns_pane_and_bytes() {
        assert_eq!(
            parse_output_line("%output %3 hello"),
            Some((3, b"hello".to_vec()))
        );
    }

    #[test]
    fn test_parse_output_line_escaped_payload_decodes_control_bytes() {
        assert_eq!(
            parse_output_line("%output %0 ls\\015\\012"),
            Some((0, b"ls\r\n".to_vec()))
        );
    }

    #[test]
    fn test_parse_output_line_empty_payload_returns_empty_bytes() {
        assert_eq!(parse_output_line("%output %12 "), Some((12, Vec::new())));
    }

    #[test]
    fn test_parse_output_line_malformed_inputs_return_none() {
        for malformed in [
            "%begin 123 1 0",
            "%output 3 missing-percent",
            "%output %x not-a-number",
            "%output %3",
            "plain text",
            "",
        ] {
            assert_eq!(parse_output_line(malformed), None, "input: {malformed:?}");
        }
    }

    #[test]
    fn test_decode_octal_escapes_decodes_full_byte_range() {
        assert_eq!(decode_octal_escapes("\\000"), vec![0u8]);
        assert_eq!(decode_octal_escapes("\\033[1m"), b"\x1b[1m".to_vec());
        assert_eq!(decode_octal_escapes("\\377"), vec![0xffu8]);
    }

    #[test]
    fn test_decode_octal_escapes_double_backslash_is_one_backslash() {
        assert_eq!(decode_octal_escapes("a\\\\b"), b"a\\b".to_vec());
    }

    #[test]
    fn test_decode_octal_escapes_passes_utf8_through() {
        assert_eq!(decode_octal_escapes("käse"), "käse".as_bytes().to_vec());
    }

    #[test]
    fn test_decode_octal_escapes_malformed_escapes_kept_literally() {
        // Trailing backslash, too-short octal, out-of-range first digit.
        assert_eq!(decode_octal_escapes("ab\\"), b"ab\\".to_vec());
        assert_eq!(decode_octal_escapes("\\01"), b"\\01".to_vec());
        assert_eq!(decode_octal_escapes("\\777"), b"\\777".to_vec());
    }

    #[test]
    fn test_hex_args_encodes_space_separated_lowercase_hex() {
        assert_eq!(hex_args(b"ab\r"), "61 62 0d");
        assert_eq!(hex_args(b""), "");
    }
}
