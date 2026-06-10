use std::path::Path;

use anyhow::Context;
use rift_daemon::{connect_relay, serve, serve_uds};

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
            eprintln!("rift-daemon listening on {path}");
            serve_uds(Path::new(&path)).await
        }
        // Relay mode: connect the process's stdio to a running daemon's socket.
        // The SSH host wires its channel to this so the channel reaches the
        // persistent daemon without interpreting the protocol.
        Some("--connect") => {
            let path = args.next().context("--connect requires a socket path")?;
            connect_relay(Path::new(&path)).await
        }
        Some(other) => anyhow::bail!("unknown argument: {other}"),
        // Default stdio mode: speak the protocol over stdin/stdout for a single
        // session. `serve` returns when stdin reaches EOF.
        None => {
            eprintln!("rift-daemon starting");
            serve(tokio::io::stdin(), tokio::io::stdout()).await
        }
    }
}
