//! Commitment-gate spike (issue #173): a real rust-analyzer round-trip.
//!
//! `#[ignore]`d because it invokes a real `rust-analyzer` binary (must be on
//! `$PATH`) and indexes a fixture crate — too heavy and environment-dependent
//! for the unit gate, matching `async-lsp`'s own example convention. Run it
//! explicitly to exercise the spike:
//!
//! ```sh
//! cargo test -p rift-lsp --test rust_analyzer_spike -- --ignored --nocapture
//! ```

use std::path::Path;

use rift_lsp::spike::run_rust_analyzer_roundtrip;

#[tokio::test]
#[ignore = "invokes a real rust-analyzer; run with --ignored"]
async fn test_rust_analyzer_roundtrip_fixture_error_publishes_diagnostics() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust_project");
    let file = Path::new("src/lib.rs");

    let diagnostics = run_rust_analyzer_roundtrip(&root, file)
        .await
        .expect("rust-analyzer round-trip should publish diagnostics for the fixture error");

    assert!(
        !diagnostics.is_empty(),
        "fixture has a deliberate type error; rust-analyzer must report at least one diagnostic"
    );

    // The deliberate error is a `u32 = &str` mismatch; rust-analyzer reports it
    // as a type-mismatch diagnostic. Assert on the message substance, not an
    // exact string, so a rust-analyzer wording change does not break the spike.
    let messages: String = diagnostics
        .iter()
        .map(|d| d.message.to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        messages.contains("mismatch") || messages.contains("expected"),
        "expected a type-mismatch diagnostic, got: {messages}"
    );
}
