use rift_daemon::serve;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("rift-daemon starting");

    // The daemon speaks the `rift-protocol` framing over stdio: the SSH host
    // wires the daemon's stdin/stdout to a `russh` channel, so reading the
    // protocol from stdio keeps the binary transport-agnostic and musl-clean
    // (no russh in the daemon). `serve` returns when stdin reaches EOF.
    serve(tokio::io::stdin(), tokio::io::stdout()).await
}
