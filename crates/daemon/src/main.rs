use rift_daemon::serve;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `serve` owns stdout for the protocol frame stream; nothing else may write
    // to it, or a stray banner would be decoded as a frame length prefix and
    // stall the client decoder. Log to stderr instead.
    eprintln!("rift-daemon starting");

    // The daemon speaks the `rift-protocol` framing over stdio: the SSH host
    // wires the daemon's stdin/stdout to a `russh` channel, so reading the
    // protocol from stdio keeps the binary transport-agnostic and musl-clean
    // (no russh in the daemon). `serve` returns when stdin reaches EOF.
    serve(tokio::io::stdin(), tokio::io::stdout()).await
}
