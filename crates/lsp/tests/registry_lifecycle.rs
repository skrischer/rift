//! Registry lifecycle against a real rust-analyzer (issue #174).
//!
//! `#[ignore]`d like the spike: it spawns a real `rust-analyzer` (must be on
//! `$PATH`) at the fixture crate, so it is too heavy and environment-dependent
//! for the unit gate. Run it explicitly:
//!
//! ```sh
//! cargo test -p rift-lsp --test registry_lifecycle -- --ignored --nocapture
//! ```
//!
//! Proves the lazy lifecycle this issue adds, end-to-end: observing a matching
//! file starts the server at the worktree root, a second observation of another
//! matching file reuses it (no second instance), the started server is
//! addressable, and the server publishes diagnostics that reach the registry's
//! channel — then a clean shutdown drains it.

use std::path::Path;
use std::time::Duration;

use lsp_types::PublishDiagnosticsParams;
use rift_lsp::server::ServerId;
use rift_lsp::Registry;
use tokio::sync::mpsc;

#[tokio::test]
#[ignore = "invokes a real rust-analyzer; run with --ignored"]
async fn test_registry_lazy_start_reuse_and_diagnostics() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust_project");
    let (tx, mut rx) = mpsc::unbounded_channel::<(ServerId, PublishDiagnosticsParams)>();
    let mut registry = Registry::new(root, tx);

    // First observation of a matching file lazily starts rust-analyzer.
    let started = registry.observe(Path::new("src/lib.rs")).await;
    assert_eq!(
        started.len(),
        1,
        "one rust-analyzer should start for a .rs file"
    );
    assert_eq!(registry.len(), 1);
    let id = started[0];
    assert!(
        registry.server(id).is_some(),
        "the started server is addressable"
    );
    assert_eq!(registry.servers_for_language("rust"), &[id]);

    // A second observation of another matching file reuses the running server —
    // lazy-once-per-language, reused for the session.
    let reused = registry.observe(Path::new("src/other.rs")).await;
    assert_eq!(
        reused,
        vec![id],
        "a second .rs observation reuses the server"
    );
    assert_eq!(registry.len(), 1, "no second instance is started");

    // rust-analyzer indexes the fixture (a deliberate `u32 = &str` error) and
    // publishes diagnostics through the registry's channel, tagged with the id.
    let (from, params) = tokio::time::timeout(Duration::from_secs(60), recv_nonempty(&mut rx))
        .await
        .expect("rust-analyzer should publish diagnostics within the timeout");
    assert_eq!(
        from, id,
        "diagnostics are tagged with the originating server id"
    );
    assert!(!params.diagnostics.is_empty());

    registry.shutdown().await;
    assert!(registry.is_empty(), "shutdown clears the registry");
}

/// Drain the channel until a publish carrying actual diagnostics arrives —
/// rust-analyzer emits empty placeholder sets before indexing settles.
async fn recv_nonempty(
    rx: &mut mpsc::UnboundedReceiver<(ServerId, PublishDiagnosticsParams)>,
) -> (ServerId, PublishDiagnosticsParams) {
    loop {
        match rx.recv().await {
            Some((id, params)) if !params.diagnostics.is_empty() => return (id, params),
            Some(_) => continue,
            None => panic!("registry channel closed before diagnostics arrived"),
        }
    }
}
