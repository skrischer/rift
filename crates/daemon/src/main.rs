use std::path::Path;

use anyhow::Context;
use rift_daemon::{connect_relay, ping, serve, serve_uds};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Stdout is reserved for protocol frames in stdio mode; nothing else may
    // write to it, or a stray banner would be decoded as a frame length prefix
    // and stall the client decoder. All logging goes to stderr.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // Long-lived reattachable mode: bind a Unix socket and serve connections
        // until the process is signalled. The SSH host launches this detached so
        // it survives connection drops (issue #62).
        Some("--serve-uds") => {
            let path = args.next().context("--serve-uds requires a socket path")?;
            let root =
                std::env::current_dir().context("cannot resolve the daemon launch directory")?;
            eprintln!(
                "rift-daemon listening on {path}, worktree {}",
                root.display()
            );
            serve_uds(Path::new(&path), Some(root)).await
        }
        // Relay mode: connect the process's stdio to a running daemon's socket.
        // The SSH host wires its channel to this so the channel reaches the
        // persistent daemon without interpreting the protocol.
        Some("--connect") => {
            let path = args.next().context("--connect requires a socket path")?;
            connect_relay(Path::new(&path)).await
        }
        // Throwaway VTE-location spike mode (issue #201): serve the protocol on
        // a Unix socket while forwarding one tmux session's raw %output bytes.
        // Delete with the real terminal-streaming implementation (#202-#205).
        Some("--spike-serve-uds") => {
            let path = args
                .next()
                .context("--spike-serve-uds requires a socket path")?;
            let session = args
                .next()
                .context("--spike-serve-uds requires a tmux session name")?;
            eprintln!("rift-daemon spike listening on {path} (tmux session {session})");
            rift_daemon::spike::serve_spike(Path::new(&path), &session).await
        }
        // Probe mode: exit 0 if a daemon is listening on the socket, 1 otherwise.
        // The SSH host keys its reattach-vs-spawn decision on this status; no
        // output is emitted either way.
        Some("--ping") => {
            let path = args.next().context("--ping requires a socket path")?;
            std::process::exit(if ping(Path::new(&path)).await { 0 } else { 1 });
        }
        Some(other) => anyhow::bail!("unknown argument: {other}"),
        // Default stdio mode: speak the protocol over stdin/stdout for a single
        // session. `serve` returns when stdin reaches EOF.
        None => {
            let root =
                std::env::current_dir().context("cannot resolve the daemon launch directory")?;
            eprintln!("rift-daemon starting, worktree {}", root.display());
            serve(tokio::io::stdin(), tokio::io::stdout(), Some(root)).await
        }
    }
}
