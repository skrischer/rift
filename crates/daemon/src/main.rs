use rift_daemon::channels;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("rift-daemon starting");

    // No transport yet (own Phase 3 sub-spec); construct the dispatch loop and
    // its channels, then run until the inbound channel closes. With no sender
    // wired in, `run` returns immediately — the binary stays thin and the loop
    // lives in the library.
    let (daemon, handles) = channels(256, 256);
    drop(handles);
    daemon.run().await;

    Ok(())
}
