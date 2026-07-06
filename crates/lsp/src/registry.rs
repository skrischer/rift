//! The lazy, per-language server registry.
//!
//! The [`Registry`] is the lifecycle owner the spec mandates
//! (`docs/spec-daemon-lsp.md`, prior decision "lazy, per-language server
//! lifecycle … multi-server-per-language via a `Registry`"; `prior-art.md`
//! pattern #8). It maps a language to the running servers for it
//! (`HashMap<LanguageId, Vec<ServerId>>`, the Helix `Registry` shape) and, on
//! observing a changed file, ensures every server its [`DocumentSelector`]
//! matches is running — started lazily at the worktree root on first sight and
//! reused for the session.
//!
//! Three policies live here, each from a spec risk row:
//! - **Missing binary**: a server whose binary is not on `$PATH` is logged
//!   *once* per binary and skipped; it never errors the daemon.
//! - **Multi-server-per-language**: two specs targeting the same language both
//!   start and get distinct [`ServerId`]s, addressable independently.
//! - **Supervision**: a server that has exited is restarted lazily on the next
//!   matching change, throttled by per-binary exponential backoff; an instance
//!   must outlive a short liveness window to clear its backoff, so one that
//!   survives `initialize` but then crash-loops at runtime stays throttled
//!   (issue #273). The daemon never panics on a server crash.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_lsp::ServerSocket;
use lsp_types::PublishDiagnosticsParams;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::selector::{DocumentSelector, LanguageId, ServerName, ServerSpec};
use crate::server::{Server, ServerId};
use crate::{LspError, Result};

/// First retry delay after a server exit. Doubles on each consecutive failed
/// restart up to [`MAX_BACKOFF`]; the backoff clears only once a restarted
/// instance outlives [`LIVENESS_WINDOW`], not the moment it initializes.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Ceiling for the exponential restart backoff — a server that keeps crashing is
/// retried at most this often, so a restart-storm cannot busy-spawn.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How long a (re)started server must stay up before its restart backoff is
/// considered earned-clear. An instance that exits within this window of its
/// spawn is treated as crash-looping — its backoff escalates rather than
/// resets — so a server that survives `initialize` but then dies at runtime is
/// still throttled (issue #273). An instance that outlives the window has
/// proven stable and clears the backoff.
const LIVENESS_WINDOW: Duration = Duration::from_secs(5);

/// Per-binary restart-backoff bookkeeping. Keyed by [`ServerName`] so a crash of
/// one server does not throttle an unrelated one.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    /// Earliest [`Instant`] a restart of this binary may be attempted.
    next_attempt: Instant,
    /// The delay applied after the next failure, doubling each time.
    delay: Duration,
}

/// The lazy, per-language server registry.
///
/// Holds the running servers, the language → ids index, and the restart-backoff
/// state. Not `Clone`: it owns the live [`Server`]s. Diagnostics every server
/// publishes flow out through the channel passed to [`Registry::new`], tagged
/// with the [`ServerId`] for `(file, server)` aggregation downstream.
pub struct Registry {
    selector: DocumentSelector,
    root_dir: PathBuf,
    next_id: u64,
    /// The server store: every live (or recently-exited, pending-prune) server.
    servers: HashMap<ServerId, Server>,
    /// The multi-server-per-language index — the Helix `Registry` shape.
    by_language: HashMap<LanguageId, Vec<ServerId>>,
    /// Binaries already logged as missing, so the warning fires once per binary.
    missing_logged: HashSet<ServerName>,
    /// Per-binary restart throttle.
    backoff: HashMap<ServerName, Backoff>,
    /// Ids pruned from the store since the last [`Registry::take_pruned`] —
    /// dead instances whose diagnostics a downstream consumer may still hold.
    pruned: Vec<ServerId>,
    /// Cloned into each spawned server's router so its diagnostics reach the
    /// daemon consumer.
    diagnostics_tx: mpsc::UnboundedSender<(ServerId, PublishDiagnosticsParams)>,
}

impl Registry {
    /// A registry rooted at `root_dir` (the watched worktree root) using the
    /// built-in language → server table. Diagnostics from every server it
    /// starts are forwarded on `diagnostics_tx`, tagged with the originating
    /// [`ServerId`].
    pub fn new(
        root_dir: impl Into<PathBuf>,
        diagnostics_tx: mpsc::UnboundedSender<(ServerId, PublishDiagnosticsParams)>,
    ) -> Self {
        Self::with_selector(DocumentSelector::builtin(), root_dir, diagnostics_tx)
    }

    /// A registry with an explicit selector — used by tests to drive stub
    /// servers from a custom table without touching the built-in defaults.
    pub fn with_selector(
        selector: DocumentSelector,
        root_dir: impl Into<PathBuf>,
        diagnostics_tx: mpsc::UnboundedSender<(ServerId, PublishDiagnosticsParams)>,
    ) -> Self {
        Self {
            selector,
            root_dir: root_dir.into(),
            next_id: 0,
            servers: HashMap::new(),
            by_language: HashMap::new(),
            missing_logged: HashSet::new(),
            backoff: HashMap::new(),
            pruned: Vec::new(),
            diagnostics_tx,
        }
    }

    /// The ids of the servers currently registered for `language`, in start
    /// order. Empty when none has started.
    pub fn servers_for_language(&self, language: LanguageId) -> &[ServerId] {
        self.by_language
            .get(language)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// A live server by id, if it exists and has not exited.
    pub fn server(&self, id: ServerId) -> Option<&Server> {
        self.servers.get(&id).filter(|s| s.is_alive())
    }

    /// Find the first live server in the registry that serves `path` (by the
    /// selector's extension matching) and passes the `check` predicate on its
    /// `ServerCapabilities`. Returns a cloned [`ServerSocket`] and
    /// [`ServerCapabilities`] that a spawned task can use independently.
    ///
    /// Used by the navigation dispatch layer (issue #195) to route a request to
    /// the first capable server without blocking on server I/O: the socket and
    /// capabilities are cloned synchronously before the task is spawned.
    pub fn first_capable_for_path(
        &self,
        path: &Path,
        check: impl Fn(&lsp_types::ServerCapabilities) -> bool,
    ) -> Option<(ServerSocket, lsp_types::ServerCapabilities)> {
        for spec in self.selector.matching(path) {
            for &id in self
                .by_language
                .get(spec.language)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                if let Some(server) = self.servers.get(&id).filter(|s| s.is_alive()) {
                    let caps = server.capabilities().clone();
                    if check(&caps) {
                        return Some((server.socket(), caps));
                    }
                }
            }
        }
        None
    }

    /// The number of servers in the store, alive or pending-prune. Primarily for
    /// tests and diagnostics.
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// Whether any server is registered.
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// Ensure every server matching `path` is running, starting or restarting as
    /// needed, and return the ids of the servers now serving it.
    ///
    /// This is the lazy lifecycle entry point: called on each observed worktree
    /// change. For every [`ServerSpec`] the selector matches:
    /// - a live server is reused (returned as-is);
    /// - no server (or an exited one) triggers a lazy (re)start at the worktree
    ///   root, subject to the per-binary backoff for restarts;
    /// - a missing binary or a spawn/init failure is logged and skipped — never
    ///   fatal, so one broken server cannot stop the others on the same path.
    pub async fn observe(&mut self, path: &Path) -> Vec<ServerId> {
        self.prune_dead();

        let specs: Vec<&'static ServerSpec> = self.selector.matching(path).collect();
        let mut ids = Vec::with_capacity(specs.len());
        for spec in specs {
            if let Some(id) = self.ensure_started(spec).await {
                ids.push(id);
            }
        }
        ids
    }

    /// Reuse the live server for `spec` if there is one, otherwise (re)start it.
    /// Returns the serving id, or `None` when the server is unavailable
    /// (missing binary, backoff window, or a failed (re)start) — the skip path.
    async fn ensure_started(&mut self, spec: &'static ServerSpec) -> Option<ServerId> {
        if let Some(id) = self.live_server_for(spec) {
            return Some(id);
        }

        // A dead server for this spec is being restarted: honor the backoff
        // window and advance it, so a crash-looping server cannot busy-spawn.
        let restarting = self.backoff.contains_key(spec.binary);
        if restarting && !self.backoff_elapsed(spec.binary) {
            return None;
        }

        match self.start(spec).await {
            Ok(id) => {
                // The backoff is NOT cleared here: a successful `initialize` is
                // not proof of stability. A server that starts but then
                // crash-loops at runtime must stay throttled, so the backoff
                // clears only once the instance outlives the liveness window —
                // decided in `note_exit` when it is pruned (issue #273).
                Some(id)
            }
            Err(LspError::Spawn { .. }) => {
                self.log_missing_once(spec.binary);
                self.bump_backoff(spec.binary);
                None
            }
            Err(error) => {
                warn!(
                    server = spec.binary,
                    language = spec.language,
                    %error,
                    "failed to start language server; will retry on the next matching change"
                );
                self.bump_backoff(spec.binary);
                None
            }
        }
    }

    /// The id of a still-alive server matching `spec` (same language and
    /// binary), if one is registered. Reuse keys on the binary, not just the
    /// language, so two servers of the same language are each reused
    /// independently.
    fn live_server_for(&self, spec: &ServerSpec) -> Option<ServerId> {
        self.by_language
            .get(spec.language)?
            .iter()
            .copied()
            .find(|id| {
                self.servers
                    .get(id)
                    .is_some_and(|s| s.name() == spec.binary && s.is_alive())
            })
    }

    /// Spawn and register a fresh server for `spec`, assigning the next id.
    async fn start(&mut self, spec: &ServerSpec) -> Result<ServerId> {
        let id = ServerId(self.next_id);
        let server = Server::spawn(id, spec, &self.root_dir, self.diagnostics_tx.clone()).await?;
        self.next_id += 1;
        self.by_language.entry(spec.language).or_default().push(id);
        self.servers.insert(id, server);
        info!(
            server = spec.binary,
            language = spec.language,
            id = id.0,
            root = %self.root_dir.display(),
            "started language server"
        );
        Ok(id)
    }

    /// Drop servers whose main loop has ended, removing them from both the store
    /// and the language index so the next matching change restarts them. Each
    /// exit updates the per-binary backoff through [`Registry::note_exit`],
    /// escalating it for a crash-looping instance and clearing it for one that
    /// stayed up.
    fn prune_dead(&mut self) {
        let now = Instant::now();
        let dead: Vec<ServerId> = self
            .servers
            .iter()
            .filter(|(_, s)| !s.is_alive())
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            if let Some(server) = self.servers.remove(&id) {
                if let Some(ids) = self.by_language.get_mut(server.language()) {
                    ids.retain(|other| *other != id);
                }
                self.pruned.push(id);
                let alive = now.saturating_duration_since(server.started_at());
                self.note_exit(server.name(), alive);
                info!(
                    server = server.name(),
                    language = server.language(),
                    id = id.0,
                    alive_secs = alive.as_secs_f64(),
                    "language server exited; pruned, will restart on the next matching change"
                );
            }
        }
    }

    /// Fold a just-pruned instance's lifetime into the per-binary backoff. An
    /// instance that outlived [`LIVENESS_WINDOW`] proved stable, so its backoff
    /// clears and the next restart is immediate. One that died within the window
    /// is crash-looping, so its backoff escalates via [`Registry::bump_backoff`]
    /// — this is what keeps a server that survives `initialize` but then dies at
    /// runtime throttled, rather than restarted unbounded (issue #273).
    fn note_exit(&mut self, binary: ServerName, alive: Duration) {
        if alive >= LIVENESS_WINDOW {
            self.backoff.remove(binary);
        } else {
            self.bump_backoff(binary);
        }
    }

    /// Drain the ids of servers pruned since the last call.
    ///
    /// A pruned instance's diagnostics stay keyed by its [`ServerId`]
    /// downstream, and a restarted replacement publishes under a fresh id —
    /// nothing would ever overwrite the dead id's sets. The consumer drains
    /// this after each [`Registry::observe`] and pushes clearing (empty)
    /// updates for the dead ids' paths (issue #427).
    pub fn take_pruned(&mut self) -> Vec<ServerId> {
        std::mem::take(&mut self.pruned)
    }

    /// Log a missing binary at most once per binary name.
    fn log_missing_once(&mut self, binary: ServerName) {
        if self.missing_logged.insert(binary) {
            warn!(
                server = binary,
                "language server binary not found on $PATH; skipping its language"
            );
        }
    }

    /// Whether `binary`'s backoff window has elapsed (so a restart may proceed).
    /// An unknown binary has no window — its first start is never throttled.
    fn backoff_elapsed(&self, binary: ServerName) -> bool {
        match self.backoff.get(binary) {
            Some(b) => Instant::now() >= b.next_attempt,
            None => true,
        }
    }

    /// Advance `binary`'s backoff after a failed (re)start: schedule the next
    /// attempt and double the delay, capped at [`MAX_BACKOFF`].
    fn bump_backoff(&mut self, binary: ServerName) {
        let entry = self.backoff.entry(binary).or_insert(Backoff {
            next_attempt: Instant::now(),
            delay: INITIAL_BACKOFF,
        });
        entry.next_attempt = Instant::now() + entry.delay;
        entry.delay = (entry.delay * 2).min(MAX_BACKOFF);
    }

    /// Shut every server down cleanly. Best-effort — a server that ignores the
    /// request is killed on drop, so a shutdown error is logged, not propagated.
    pub async fn shutdown(&mut self) {
        for (id, server) in &mut self.servers {
            if let Err(error) = server.shutdown().await {
                warn!(
                    server = server.name(),
                    id = id.0,
                    %error,
                    "language server shutdown failed; killing on drop"
                );
            }
        }
        self.servers.clear();
        self.by_language.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::ServerSpec;

    /// A table whose binary cannot exist on `$PATH`, so every start fails the
    /// missing-binary path deterministically.
    const MISSING: &[ServerSpec] = &[ServerSpec {
        language: "rust",
        binary: "rift-nonexistent-server-xyz",
        args: &[],
        extensions: &["rs"],
    }];

    /// Two distinct binaries on the same language — the multi-server case. Both
    /// are missing on `$PATH`, so this exercises the *index* and the
    /// log-once/backoff bookkeeping without needing real servers installed.
    const TWO_SAME_LANGUAGE: &[ServerSpec] = &[
        ServerSpec {
            language: "rust",
            binary: "rift-nonexistent-type-checker",
            args: &[],
            extensions: &["rs"],
        },
        ServerSpec {
            language: "rust",
            binary: "rift-nonexistent-linter",
            args: &[],
            extensions: &["rs"],
        },
    ];

    fn registry(table: &'static [ServerSpec]) -> Registry {
        let (tx, _rx) = mpsc::unbounded_channel();
        // An absolute root: `Url::from_file_path` (run before the spawn) rejects
        // a relative path, so a relative root would short-circuit on
        // `InvalidUri` before the missing-binary path is ever reached.
        let root = std::env::current_dir().expect("cwd is readable in tests");
        Registry::with_selector(DocumentSelector::with_table(table), root, tx)
    }

    #[tokio::test]
    async fn test_observe_unmatched_path_starts_no_server() {
        let mut reg = registry(MISSING);
        let ids = reg.observe(Path::new("README.md")).await;
        assert!(ids.is_empty());
        assert!(reg.is_empty());
    }

    #[tokio::test]
    async fn test_missing_binary_is_skipped_and_never_fatal() {
        let mut reg = registry(MISSING);
        let ids = reg.observe(Path::new("main.rs")).await;
        assert!(ids.is_empty(), "a missing binary yields no server id");
        assert!(
            reg.is_empty(),
            "no server is registered for a missing binary"
        );
    }

    #[tokio::test]
    async fn test_missing_binary_logged_once() {
        let mut reg = registry(MISSING);
        reg.observe(Path::new("a.rs")).await;
        assert_eq!(reg.missing_logged.len(), 1);
        // A second observation must not re-log: the set already holds the binary.
        reg.observe(Path::new("b.rs")).await;
        assert_eq!(reg.missing_logged.len(), 1);
    }

    #[tokio::test]
    async fn test_failed_start_sets_backoff_window() {
        let mut reg = registry(MISSING);
        reg.observe(Path::new("a.rs")).await;
        // After the first failure a backoff window exists and has not elapsed,
        // so an immediate re-observe is throttled (no spawn attempt).
        let binary = MISSING[0].binary;
        assert!(reg.backoff.contains_key(binary));
        assert!(!reg.backoff_elapsed(binary));
    }

    #[tokio::test]
    async fn test_two_servers_same_language_tracked_independently() {
        let mut reg = registry(TWO_SAME_LANGUAGE);
        reg.observe(Path::new("lib.rs")).await;
        // Both binaries failed to spawn, but both were attempted and both are
        // logged-missing and backed-off independently — the multi-server index
        // and per-binary bookkeeping addressing each separately.
        assert_eq!(reg.missing_logged.len(), 2);
        assert_eq!(reg.backoff.len(), 2);
    }

    #[tokio::test]
    async fn test_take_pruned_without_dead_servers_returns_empty() {
        let mut reg = registry(MISSING);
        assert!(reg.take_pruned().is_empty());
        reg.observe(Path::new("a.rs")).await;
        // A failed start is not a prune — only a dead *registered* server is;
        // the real prune path is covered end-to-end by the daemon's stub-server
        // crash+restart integration test.
        assert!(reg.take_pruned().is_empty());
    }

    #[tokio::test]
    async fn test_backoff_window_blocks_immediate_restart() {
        let mut reg = registry(MISSING);
        reg.observe(Path::new("a.rs")).await;
        let binary = MISSING[0].binary;
        let first = reg.backoff[binary].next_attempt;
        // A re-observe inside the window must not advance the schedule (the
        // restart is skipped before any spawn attempt).
        reg.observe(Path::new("a.rs")).await;
        assert_eq!(reg.backoff[binary].next_attempt, first);
    }

    #[tokio::test]
    async fn test_exit_within_liveness_window_escalates_backoff() {
        let mut reg = registry(MISSING);
        let binary = MISSING[0].binary;
        // A server that dies well within the liveness window is crash-looping:
        // the exit must arm a throttle even though its `initialize` succeeded.
        reg.note_exit(binary, Duration::from_millis(50));
        assert!(
            reg.backoff.contains_key(binary),
            "a crash-loop exit arms the backoff"
        );
        assert!(
            !reg.backoff_elapsed(binary),
            "the throttle window blocks an immediate restart"
        );
        let first_delay = reg.backoff[binary].delay;
        // A second fast exit escalates the delay — exponential, not flat.
        reg.note_exit(binary, Duration::from_millis(50));
        assert!(
            reg.backoff[binary].delay > first_delay,
            "consecutive crash-loop exits escalate the backoff"
        );
    }

    #[tokio::test]
    async fn test_exit_after_liveness_window_clears_backoff() {
        let mut reg = registry(MISSING);
        let binary = MISSING[0].binary;
        // Seed an escalated throttle as if the binary had crash-looped before.
        reg.bump_backoff(binary);
        assert!(reg.backoff.contains_key(binary));
        // An instance that outlived the window proved stable: its exit clears
        // the backoff so the next restart is not throttled.
        reg.note_exit(binary, LIVENESS_WINDOW);
        assert!(
            !reg.backoff.contains_key(binary),
            "a server that stayed up clears its backoff"
        );
    }
}
