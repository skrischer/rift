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
use rift_protocol::{ClientMessage, DaemonMessage, DiagnosticSeverity, LspServerState};
use tokio::sync::broadcast;

/// The marker that makes the (single / type-checker) stub publish a diagnostic.
/// A file without it clears that stub's set.
const ERROR_MARKER: &str = "LSP_STUB_ERROR";

/// A second, independent marker the linter stub reacts to. Having two markers
/// lets the aggregation test clear one server's set (drop its marker) while the
/// other's set stays (its marker remains) — the "one clears, the other intact"
/// case.
const LINT_MARKER: &str = "LSP_STUB_LINT";

/// The marker that makes the crashing stub exit abruptly (no publish, no
/// shutdown handshake) — the crash+restart test's kill switch (#427).
const CRASH_MARKER: &str = "LSP_STUB_CRASH";

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

/// Single-server table whose stub additionally dies on the crash marker — the
/// crash+restart case (#427).
const CRASHING_SERVER: &[ServerSpec] = &[ServerSpec {
    language: "rust",
    binary: "stub_lsp_server",
    args: &[
        "--marker",
        ERROR_MARKER,
        "--message",
        "type-checker error",
        "--crash-marker",
        CRASH_MARKER,
    ],
    extensions: &["rs"],
}];

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
/// the registry's `Command::new(binary)` resolves them. Runs its env mutation
/// **exactly once** for the whole test process, guarded by a `OnceLock`: the
/// single `set_var("PATH")` completes before any test spawns a stub child, so the
/// process env is never mutated concurrently with a child spawn. That race — the
/// parallel tests each rewriting `PATH` (line by line via `set_var`) while other
/// tests' daemons were spawning stub children that read `environ` — intermittently
/// left a spawn unable to resolve the stub, starving the pipeline until the tests'
/// timeouts and flaking CI (issue #363). Every test calls this at its start; only
/// the first performs the copy + `set_var`, the rest observe the completed state.
///
/// The staging dir lives for the whole process (a static is never dropped); it is
/// two small binary copies under the OS temp dir, reclaimed by the temp reaper —
/// deliberately not self-cleaning, since it must outlive every parallel test.
fn stage_stub_on_path() {
    static STAGED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    STAGED.get_or_init(|| {
        let bin = std::env::temp_dir().join(format!("rift-lsp-it-stub-bin-{}", std::process::id()));
        std::fs::create_dir_all(&bin).expect("create stub bin dir");
        let source = Path::new(env!("CARGO_BIN_EXE_stub_lsp_server"));
        for name in ["stub_lsp_server", "stub_lsp_server_two"] {
            let dest = bin.join(name);
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
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&existing));
        let joined = std::env::join_paths(paths).expect("join PATH");
        // The registry's child spawns inherit this PATH so they can resolve the
        // staged stubs. This is the only env mutation, and it runs once.
        std::env::set_var("PATH", joined);
    });
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

/// Repeatedly rewrite `relative` under `root` with `content(revision)` while
/// waiting for a `Diagnostics` event `pick` accepts. Document sync reads file
/// content at processing time, so two quick writes can collapse into one
/// observed content — driving fresh changes until the expected event lands
/// keeps the crash+restart test deterministic without sleeping on internal
/// timings.
async fn write_until_diagnostics<T>(
    root: &Path,
    relative: &str,
    events: &mut broadcast::Receiver<DaemonMessage>,
    content: impl Fn(u32) -> String,
    mut pick: impl FnMut(String, String, Vec<rift_protocol::Diagnostic>) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut revision = 0u32;
        loop {
            revision += 1;
            write_file(root, relative, &content(revision));
            let slice = tokio::time::sleep(Duration::from_secs(2));
            tokio::pin!(slice);
            loop {
                tokio::select! {
                    _ = &mut slice => break,
                    msg = events.recv() => match msg {
                        Ok(DaemonMessage::Diagnostics { path, server, items }) => {
                            if let Some(found) = pick(path, server, items) {
                                return found;
                            }
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => {
                            panic!("event bus closed early")
                        }
                    },
                }
            }
        }
    })
    .await
    .expect("expected a Diagnostics event within the timeout")
}

/// Receive `LspStatus` events until `pick` yields (issue #520), the
/// lifecycle-health analogue of `recv_diagnostics_until`.
async fn recv_lsp_status_until<T>(
    events: &mut broadcast::Receiver<DaemonMessage>,
    mut pick: impl FnMut(String, LspServerState) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Ok(DaemonMessage::LspStatus { server, state }) => {
                    if let Some(found) = pick(server, state) {
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
    .expect("expected an LspStatus event within the timeout")
}

/// Repeatedly rewrite `relative` under `root` with `content(revision)` while
/// waiting for an `LspStatus` event `pick` accepts — the lifecycle-health
/// analogue of `write_until_diagnostics`, needed for the same reason: a quick
/// write can collapse before the server processes it.
async fn write_until_lsp_status<T>(
    root: &Path,
    relative: &str,
    events: &mut broadcast::Receiver<DaemonMessage>,
    content: impl Fn(u32) -> String,
    mut pick: impl FnMut(String, LspServerState) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut revision = 0u32;
        loop {
            revision += 1;
            write_file(root, relative, &content(revision));
            let slice = tokio::time::sleep(Duration::from_secs(2));
            tokio::pin!(slice);
            loop {
                tokio::select! {
                    _ = &mut slice => break,
                    msg = events.recv() => match msg {
                        Ok(DaemonMessage::LspStatus { server, state }) => {
                            if let Some(found) = pick(server, state) {
                                return found;
                            }
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => {
                            panic!("event bus closed early")
                        }
                    },
                }
            }
        }
    })
    .await
    .expect("expected an LspStatus event within the timeout")
}

#[tokio::test]
async fn test_error_introduced_then_fixed_converges() {
    stage_stub_on_path();
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
async fn test_server_crash_and_restart_clears_previous_instance_diagnostics() {
    // Regression for #427: diagnostics are keyed by per-spawn server id, and a
    // crashed server's replacement publishes under a fresh id — nothing ever
    // overwrote the dead id's sets, so stale errors persisted forever. The LSP
    // worker must push clearing empty updates for the dead instance's paths
    // when the registry prunes it.
    stage_stub_on_path();
    let tmp = TempDir::new("crash-restart");

    let (mut daemon, handles) = channels(256, 16);
    daemon.watch_worktree(tmp.path.clone());
    daemon.watch_lsp(
        tmp.path.clone(),
        DocumentSelector::with_table(CRASHING_SERVER),
    );
    let mut events = handles.subscribe();
    let loop_handle = tokio::spawn(daemon.run());
    wait_for_scan(&handles).await;

    // Introduce an error under the first instance (server id "0").
    write_file(
        &tmp.path,
        "app.rs",
        &format!("fn main() {{}} // {ERROR_MARKER}"),
    );
    recv_diagnostics_until(&mut events, |path, server, items| {
        (path == "app.rs" && server == "0" && !items.is_empty()).then_some(())
    })
    .await;

    // Crash the server and drive changes until the daemon observes the death:
    // the next matching change prunes the dead instance, restarts the binary
    // under a fresh id, and must push a clearing empty set for the old id's
    // paths. Every nudge write carries the crash marker, so whichever content
    // an instance ends up reading it never publishes — the only possible
    // source of an empty "0" set is the prune-time clear.
    write_until_diagnostics(
        &tmp.path,
        "app.rs",
        &mut events,
        |revision| format!("fn main() {{}} // {CRASH_MARKER} rev {revision}"),
        |path, server, items| (path == "app.rs" && server == "0" && items.is_empty()).then_some(()),
    )
    .await;

    // The stale set is gone from the daemon's State, not just cleared on the
    // bus — a (re)attaching client must not have it replayed either.
    assert!(
        !handles
            .state
            .borrow()
            .diagnostics
            .contains_key(&DiagnosticKey {
                path: "app.rs".to_string(),
                server: "0".to_string(),
            }),
        "the dead instance's diagnostics must not survive in State"
    );

    // The replacement serves the file under its own id: reintroducing the
    // error (without the crash marker) yields a diagnostic keyed by a fresh id.
    let server = write_until_diagnostics(
        &tmp.path,
        "app.rs",
        &mut events,
        |revision| format!("fn main() {{}} // {ERROR_MARKER} rev {revision}"),
        |path, server, items| (path == "app.rs" && !items.is_empty()).then_some(server),
    )
    .await;
    assert_ne!(
        server, "0",
        "the restarted server publishes under a fresh id"
    );

    drop(handles);
    drop(events);
    loop_handle.await.expect("dispatch loop joins");
}

#[tokio::test]
async fn test_lsp_status_crash_then_restart_flips_health_dot() {
    // Acceptance (issue #520): killing the language server yields `crashed`
    // on the next observe; a restart yields `running` — no app restart.
    // `LspStatus` is keyed by the server's stable *name* (the stub binary),
    // unlike `Diagnostics`, which the sibling crash+restart test keys by the
    // per-spawn id.
    stage_stub_on_path();
    let tmp = TempDir::new("status-crash-restart");

    let (mut daemon, handles) = channels(256, 16);
    daemon.watch_worktree(tmp.path.clone());
    daemon.watch_lsp(
        tmp.path.clone(),
        DocumentSelector::with_table(CRASHING_SERVER),
    );
    let mut events = handles.subscribe();
    let loop_handle = tokio::spawn(daemon.run());
    wait_for_scan(&handles).await;

    // First start: `starting` then `running` for the stub's stable name.
    write_file(
        &tmp.path,
        "app.rs",
        &format!("fn main() {{}} // {ERROR_MARKER}"),
    );
    recv_lsp_status_until(&mut events, |server, state| {
        (server == "stub_lsp_server" && state == LspServerState::Running).then_some(())
    })
    .await;
    assert_eq!(
        handles
            .state
            .borrow()
            .lsp_status
            .get("stub_lsp_server")
            .copied(),
        Some(LspServerState::Running)
    );

    // Crash it, then drive changes (still crash-marked) until the daemon
    // observes the death and flips the name-keyed health to `crashed`.
    write_until_lsp_status(
        &tmp.path,
        "app.rs",
        &mut events,
        |revision| format!("fn main() {{}} // {CRASH_MARKER} rev {revision}"),
        |server, state| {
            (server == "stub_lsp_server" && state == LspServerState::Crashed).then_some(())
        },
    )
    .await;
    assert_eq!(
        handles
            .state
            .borrow()
            .lsp_status
            .get("stub_lsp_server")
            .copied(),
        Some(LspServerState::Crashed)
    );

    // A later change without the crash marker restarts it: health flips back
    // to `running` — no app restart needed.
    write_until_lsp_status(
        &tmp.path,
        "app.rs",
        &mut events,
        |revision| format!("fn main() {{}} // {ERROR_MARKER} rev {revision}"),
        |server, state| {
            (server == "stub_lsp_server" && state == LspServerState::Running).then_some(())
        },
    )
    .await;
    assert_eq!(
        handles
            .state
            .borrow()
            .lsp_status
            .get("stub_lsp_server")
            .copied(),
        Some(LspServerState::Running)
    );

    drop(handles);
    drop(events);
    loop_handle.await.expect("dispatch loop joins");
}

#[tokio::test]
async fn test_two_servers_aggregate_and_clear_independently() {
    stage_stub_on_path();
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
    stage_stub_on_path();
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

/// A stub that publishes under the *canonical* document path — modelling a real
/// server (rust-analyzer) that canonicalizes paths internally. Used by the #308
/// regression test to surface the daemon's root-canonicalization requirement
/// without a real server.
#[cfg(unix)]
const CANONICALIZING_SERVER: &[ServerSpec] = &[ServerSpec {
    language: "rust",
    binary: "stub_lsp_server",
    args: &[
        "--marker",
        ERROR_MARKER,
        "--message",
        "type-checker error",
        "--canonicalize-uri",
    ],
    extensions: &["rs"],
}];

#[cfg(unix)]
static SYMLINK_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[tokio::test]
#[cfg(unix)]
async fn test_diagnostics_keyed_under_canonical_relative_path_for_a_symlinked_root() {
    // Regression for #308: the LSP worker must key diagnostics in the same path
    // space as the worktree snapshot — relative to the *canonical* root. The
    // worktree scan canonicalizes its root (so entry paths, and thus the editor's
    // open path, are canonical-relative), but `watch_lsp` was handed the raw root.
    // When the raw root differs from its canonical form (here: a symlink) AND the
    // server publishes under the canonical path (as rust-analyzer does — modelled
    // by the `--canonicalize-uri` stub), the worker's `strip_prefix(symlink_root)`
    // on the canonical publish URI failed, dropping the diagnostic entirely. No
    // `Diagnostics` ever reached the client, so no inline marker rendered. The
    // other tests pass an already-canonical temp root, hiding this — so this one
    // deliberately drives a NON-canonical (symlinked) root against a canonicalizing
    // server. The earlier #189 stub test echoed the opened URI verbatim, which is
    // exactly why the bug slipped through.
    //
    // With the fix, `watch_lsp` canonicalizes its root, so the strip succeeds and
    // the diagnostic keys under the plain relative path (`app.rs`) — the same key
    // the editor's canonical-relative open path looks up by.
    stage_stub_on_path();
    let real = TempDir::new("symlink-real");
    // A sibling symlink pointing at the real (canonical) root. Passing the symlink
    // as the daemon root makes the raw root differ from its canonical form.
    let link = std::env::temp_dir().join(format!(
        "rift-lsp-it-symlink-{}-{}",
        std::process::id(),
        SYMLINK_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&real.path, &link).expect("create root symlink");
    struct LinkGuard(PathBuf);
    impl Drop for LinkGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _link_guard = LinkGuard(link.clone());
    // Sanity: the symlink path really differs from its canonical target, so the
    // test is exercising the mismatch and not a no-op.
    assert_ne!(
        link.canonicalize().expect("canonicalize symlink"),
        link,
        "the symlink root must differ from its canonical form"
    );

    let (mut daemon, handles) = channels(256, 16);
    daemon.watch_worktree(link.clone());
    daemon.watch_lsp(
        link.clone(),
        DocumentSelector::with_table(CANONICALIZING_SERVER),
    );
    let mut events = handles.subscribe();
    let loop_handle = tokio::spawn(daemon.run());
    wait_for_scan(&handles).await;

    // Write through the real path; the watcher observes it under the canonical root.
    write_file(
        &real.path,
        "app.rs",
        &format!("fn main() {{}} // {ERROR_MARKER}"),
    );
    // The key must be the plain canonical-relative path — the same key the editor's
    // open path (also canonical-relative) looks up by. Before the fix this timed
    // out: the canonical publish URI failed to strip the symlink root, so the
    // diagnostic was dropped at the daemon and never broadcast.
    let items = recv_diagnostics_until(&mut events, |path, server, items| {
        (path == "app.rs" && server == "0" && !items.is_empty()).then_some(items)
    })
    .await;
    assert_eq!(items.len(), 1, "the marker yields exactly one diagnostic");

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
    stage_stub_on_path();
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
