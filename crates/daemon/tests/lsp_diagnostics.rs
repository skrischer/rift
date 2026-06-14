//! Diagnostics integration test (issue #177), driven by a stub LSP server.
//!
//! Exercises the full daemon wiring end-to-end against a deterministic server
//! (`src/bin/stub_lsp_server.rs`) instead of a real, environment-dependent one:
//! a write to a matching file flows through the explorer watcher → document sync
//! → the stub → `publishDiagnostics` → translation → the daemon's broadcast
//! channel. The stub publishes one diagnostic for any document whose text
//! contains its marker, and an empty (clearing) set otherwise, so the test
//! controls the diagnostics purely through file content.
//!
//! Three properties, one per spec Verification bullet:
//! 1. Introducing an error yields a `Diagnostics` update carrying it; fixing the
//!    file yields a follow-up clearing it (empty set) — the model converges.
//! 2. Two servers on the same language aggregate: both servers' diagnostics
//!    appear for the file, keyed by server id; one server clearing its set
//!    leaves the other's intact.
//! 3. A write to an ignored path (`target/…`) drives no server and emits no
//!    `Diagnostics`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rift_daemon::{channels, DiagnosticKey, Handles};
use rift_lsp::selector::ServerSpec;
use rift_lsp::DocumentSelector;
use rift_protocol::{ClientMessage, DaemonMessage, DiagnosticSeverity};
use tokio::sync::broadcast;

/// The marker that makes the (single / type-checker) stub publish a diagnostic.
/// A file without it clears that stub's set.
const ERROR_MARKER: &str = "LSP_STUB_ERROR";

/// A second, independent marker the linter stub reacts to. Having two markers
/// lets the aggregation test clear one server's set (drop its marker) while the
/// other's set stays (its marker remains) — the "one clears, the other intact"
/// case.
const LINT_MARKER: &str = "LSP_STUB_LINT";

/// Single-server table: one stub bound to the rust extension, reporting a
/// recognizable message.
const ONE_SERVER: &[ServerSpec] = &[ServerSpec {
    language: "rust",
    binary: "stub_lsp_server",
    args: &["--marker", ERROR_MARKER, "--message", "type-checker error"],
    extensions: &["rs"],
}];

/// Two distinct stub binaries on the same language — the aggregation case. Each
/// reacts to its own marker and reports a distinguishable message, so the test
/// can aggregate both and then clear exactly one.
const TWO_SERVERS: &[ServerSpec] = &[
    ServerSpec {
        language: "rust",
        binary: "stub_lsp_server",
        args: &["--marker", ERROR_MARKER, "--message", "type-checker error"],
        extensions: &["rs"],
    },
    ServerSpec {
        language: "rust",
        binary: "stub_lsp_server_two",
        args: &["--marker", LINT_MARKER, "--message", "linter error"],
        extensions: &["rs"],
    },
];

/// A self-cleaning temp directory, mirroring the daemon/explorer test helpers so
/// this stays self-contained without a `tempfile` dev-dependency.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rift-lsp-it-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp root");
        // Canonicalize so the URIs document sync builds match what the watcher
        // and the stub report (the temp dir may be a symlink on some setups).
        let path = path.canonicalize().expect("canonicalize temp root");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(&path, contents).expect("write file");
}

/// Wait until the initial worktree scan has landed in `State`, so a subsequent
/// write is observed as a watcher change (not folded into the initial scan,
/// which does not drive document sync) — the watcher must be armed first.
async fn wait_for_scan(handles: &Handles) {
    let mut state = handles.state.clone();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if state.borrow_and_update().worktree.is_some() {
                return;
            }
            state.changed().await.expect("state sender alive");
        }
    })
    .await
    .expect("worktree scan lands within the timeout");
}

/// Put the stub binary on `$PATH` under the two names the tables reference, so
/// the registry's `Command::new(binary)` resolves them. Returns the temp dir
/// holding the copies (kept alive for the test). `set_var` mutates the test
/// process env once; the copies' unique names avoid clashing with anything else.
fn stage_stub_on_path() -> TempDir {
    let bin = TempDir::new("bin");
    let source = Path::new(env!("CARGO_BIN_EXE_stub_lsp_server"));
    for name in ["stub_lsp_server", "stub_lsp_server_two"] {
        let dest = bin.path.join(name);
        std::fs::copy(source, &dest).expect("copy stub binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&dest).expect("stat copy").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&dest, perms).expect("chmod copy");
        }
    }
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.path.clone()];
    paths.extend(std::env::split_paths(&existing));
    let joined = std::env::join_paths(paths).expect("join PATH");
    // SAFETY: the test process is single-purpose; the registry's child spawns
    // inherit this PATH so they can resolve the staged stubs.
    std::env::set_var("PATH", joined);
    bin
}

/// Receive `Diagnostics` events until `pick` yields, with a generous ceiling for
/// the change to cross the watcher, document sync, the stub, and translation.
async fn recv_diagnostics_until<T>(
    events: &mut broadcast::Receiver<DaemonMessage>,
    mut pick: impl FnMut(String, String, Vec<rift_protocol::Diagnostic>) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Ok(DaemonMessage::Diagnostics {
                    path,
                    server,
                    items,
                }) => {
                    if let Some(found) = pick(path, server, items) {
                        return found;
                    }
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => panic!("event bus closed early"),
            }
        }
    })
    .await
    .expect("expected a Diagnostics event within the timeout")
}

#[tokio::test]
async fn test_error_introduced_then_fixed_converges() {
    let _bin = stage_stub_on_path();
    let tmp = TempDir::new("converge");

    let (mut daemon, handles) = channels(256, 16);
    daemon.watch_worktree(tmp.path.clone());
    daemon.watch_lsp(tmp.path.clone(), DocumentSelector::with_table(ONE_SERVER));
    let mut events = handles.subscribe();
    let loop_handle = tokio::spawn(daemon.run());
    wait_for_scan(&handles).await;

    // Introduce an error: a new .rs file whose text carries the marker. The
    // watcher surfaces it, document sync opens it on the stub, and the stub
    // publishes one diagnostic for it.
    write_file(
        &tmp.path,
        "app.rs",
        &format!("fn main() {{}} // {ERROR_MARKER}"),
    );
    let items = recv_diagnostics_until(&mut events, |path, server, items| {
        (path == "app.rs" && server == "0" && !items.is_empty()).then_some(items)
    })
    .await;
    assert_eq!(items.len(), 1, "the marker yields exactly one diagnostic");
    assert_eq!(items[0].severity, DiagnosticSeverity::Error);
    assert_eq!(items[0].message, "type-checker error");
    assert_eq!(items[0].source.as_deref(), Some("stub"));

    // Fix it: rewrite the file without the marker. The stub re-publishes an
    // empty set, clearing its diagnostics for the file — the model converges.
    write_file(&tmp.path, "app.rs", "fn main() {}");
    let cleared = recv_diagnostics_until(&mut events, |path, server, items| {
        (path == "app.rs" && server == "0" && items.is_empty()).then_some(())
    })
    .await;
    let () = cleared;

    drop(handles);
    drop(events);
    loop_handle.await.expect("dispatch loop joins");
}

#[tokio::test]
async fn test_two_servers_aggregate_and_clear_independently() {
    let _bin = stage_stub_on_path();
    let tmp = TempDir::new("aggregate");

    let (mut daemon, handles) = channels(256, 16);
    daemon.watch_worktree(tmp.path.clone());
    daemon.watch_lsp(tmp.path.clone(), DocumentSelector::with_table(TWO_SERVERS));
    let mut events = handles.subscribe();
    let loop_handle = tokio::spawn(daemon.run());
    wait_for_scan(&handles).await;

    // The file carries BOTH markers, so each stub publishes for it: the daemon's
    // diagnostics aggregate under two distinct `(path, server)` keys.
    write_file(
        &tmp.path,
        "lib.rs",
        &format!("pub fn f() {{}} // {ERROR_MARKER} {LINT_MARKER}"),
    );

    let mut messages: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if messages.contains_key("0") && messages.contains_key("1") {
                return;
            }
            if let Ok(DaemonMessage::Diagnostics {
                path,
                server,
                items,
            }) = events.recv().await
            {
                if path == "lib.rs" && !items.is_empty() {
                    messages.insert(server, items[0].message.clone());
                }
            }
        }
    })
    .await
    .expect("both servers publish for the file");
    assert_eq!(
        messages.get("0").map(String::as_str),
        Some("type-checker error")
    );
    assert_eq!(messages.get("1").map(String::as_str), Some("linter error"));

    // Clear exactly one server's set: drop only the linter marker (keep the
    // type-checker one). The linter stub re-publishes an empty set; the
    // type-checker stub re-publishes its non-empty set unchanged. The daemon
    // keys both by server id, so the linter clear must NOT drop the
    // type-checker's diagnostic.
    write_file(
        &tmp.path,
        "lib.rs",
        &format!("pub fn f() {{}} // {ERROR_MARKER}"),
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(DaemonMessage::Diagnostics {
                path,
                server,
                items,
            }) = events.recv().await
            {
                if path == "lib.rs" && server == "1" && items.is_empty() {
                    return;
                }
            }
        }
    })
    .await
    .expect("the linter server clears its own set");

    // The surviving (type-checker) set is still held: the daemon's State carries
    // server "0"'s diagnostic for the file, and no entry for the cleared server
    // "1" — the per-server keying held; one server's clear left the other intact.
    let state = handles.state.borrow().clone();
    let zero = state.diagnostics.get(&DiagnosticKey {
        path: "lib.rs".to_string(),
        server: "0".to_string(),
    });
    assert_eq!(
        zero.map(|items| items[0].message.as_str()),
        Some("type-checker error"),
        "the type-checker's set survives the linter's clear"
    );
    assert!(
        !state.diagnostics.contains_key(&DiagnosticKey {
            path: "lib.rs".to_string(),
            server: "1".to_string(),
        }),
        "the linter's cleared set is removed, not clobbering the type-checker's"
    );

    drop(handles);
    drop(events);
    loop_handle.await.expect("dispatch loop joins");
}

#[tokio::test]
async fn test_write_to_ignored_path_emits_no_diagnostics() {
    let _bin = stage_stub_on_path();
    let tmp = TempDir::new("ignored");
    // A tracked file establishes the worktree and proves the pipeline is live;
    // the ignored write must produce nothing despite carrying the marker.
    write_file(&tmp.path, ".gitignore", "target/\n");

    let (mut daemon, handles) = channels(256, 16);
    daemon.watch_worktree(tmp.path.clone());
    daemon.watch_lsp(tmp.path.clone(), DocumentSelector::with_table(ONE_SERVER));
    let mut events = handles.subscribe();
    let loop_handle = tokio::spawn(daemon.run());
    wait_for_scan(&handles).await;

    // Write a .rs file inside target/ carrying the marker. The explorer snapshot
    // excludes target/, so the change never reaches document sync and no server
    // is started for it — no Diagnostics must ever be emitted.
    write_file(
        &tmp.path,
        "target/generated.rs",
        &format!("fn x() {{}} // {ERROR_MARKER}"),
    );

    // Then write a non-ignored file carrying the marker: it MUST yield a
    // diagnostic. Receiving that one without ever seeing one for the ignored
    // path proves the ignored write was silent (the non-ignored write is the
    // ordering barrier — anything the ignored write would have produced precedes
    // it through the same single pipeline).
    write_file(
        &tmp.path,
        "real.rs",
        &format!("fn y() {{}} // {ERROR_MARKER}"),
    );

    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Ok(DaemonMessage::Diagnostics { path, items, .. }) => {
                    assert_ne!(
                        path, "target/generated.rs",
                        "an ignored path must never produce diagnostics"
                    );
                    if path == "real.rs" && !items.is_empty() {
                        return;
                    }
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => panic!("event bus closed early"),
            }
        }
    })
    .await
    .expect("the non-ignored file yields a diagnostic");

    drop(handles);
    drop(events);
    loop_handle.await.expect("dispatch loop joins");
}

#[tokio::test]
async fn test_live_buffer_surfaces_unsaved_error_without_a_disk_write() {
    // The cut-C acceptance (#189): an UNSAVED edit's error surfaces without a save
    // first. The on-disk file never carries the marker; only the live buffer
    // (`BufferChanged`) does — yet the stub publishes a diagnostic for it, proving
    // the buffer, not disk, is the LSP's source of truth. Fixing the buffer clears
    // it; closing the buffer reverts to disk (also clean) coherently.
    let _bin = stage_stub_on_path();
    let tmp = TempDir::new("live-buffer");
    // The disk file is clean — no marker. If the LSP read disk, no diagnostic.
    write_file(&tmp.path, "buf.rs", "fn main() {}");

    let (mut daemon, handles) = channels(256, 16);
    daemon.watch_worktree(tmp.path.clone());
    daemon.watch_lsp(tmp.path.clone(), DocumentSelector::with_table(ONE_SERVER));
    let mut events = handles.subscribe();
    let loop_handle = tokio::spawn(daemon.run());
    wait_for_scan(&handles).await;

    // The editor feeds the open buffer's content with the marker — an unsaved edit
    // (nothing is written to disk). The buffer becomes the LSP source of truth, so
    // the stub publishes a diagnostic against it.
    handles
        .inbound
        .send(ClientMessage::BufferChanged {
            path: "buf.rs".to_string(),
            content: format!("fn main() {{ }} // {ERROR_MARKER}"),
        })
        .await
        .expect("send BufferChanged");

    let items = recv_diagnostics_until(&mut events, |path, server, items| {
        (path == "buf.rs" && server == "0" && !items.is_empty()).then_some(items)
    })
    .await;
    assert_eq!(
        items.len(),
        1,
        "the unsaved buffer's marker yields a diagnostic"
    );
    assert_eq!(items[0].severity, DiagnosticSeverity::Error);

    // The on-disk file is still clean — the feed wrote nothing to disk.
    assert_eq!(
        std::fs::read_to_string(tmp.path.join("buf.rs")).unwrap(),
        "fn main() {}",
        "the live-buffer feed must not touch disk"
    );

    // Fixing the buffer (drop the marker) clears the diagnostic without a save.
    handles
        .inbound
        .send(ClientMessage::BufferChanged {
            path: "buf.rs".to_string(),
            content: "fn main() {}".to_string(),
        })
        .await
        .expect("send fixing BufferChanged");
    recv_diagnostics_until(&mut events, |path, server, items| {
        (path == "buf.rs" && server == "0" && items.is_empty()).then_some(())
    })
    .await;

    // Closing the buffer reverts to the disk-backed baseline (still clean), so the
    // diagnostic stays cleared — the disk→buffer override is released coherently.
    handles
        .inbound
        .send(ClientMessage::BufferClosed {
            path: "buf.rs".to_string(),
        })
        .await
        .expect("send BufferClosed");

    drop(handles);
    drop(events);
    loop_handle.await.expect("dispatch loop joins");
}
