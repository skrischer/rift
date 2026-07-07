//! Daemon-side LSP wiring (issues #177, #195).
//!
//! Bridges the three crates the diagnostics path spans, all off the dispatch
//! loop:
//!
//! - **explorer → lsp**: [`document_changes`] maps an explorer `Change` batch
//!   onto the [`rift_lsp::DocumentChange`] vocabulary the disk-backed document
//!   sync consumes. The explorer snapshot includes gitignored paths (#309), so
//!   [`document_changes`] filters `entry.ignored` itself — an ignored path never
//!   drives a server, even though it now reaches this boundary.
//! - **lsp → servers**: the [`LspWorker`] owns the [`Registry`] and the
//!   [`DocumentSync`]. On each change it ensures the matching servers are
//!   running, then dispatches the sync action to exactly those servers through a
//!   [`ServerSink`].
//! - **servers → protocol**: each server's `publishDiagnostics` flows out of the
//!   registry's channel; the worker translates `lsp_types` diagnostics into
//!   rift's own protocol types (the daemon does the translation — `protocol`
//!   stays free of `lsp-types`) and hands them to the dispatch loop, keyed by
//!   worktree-relative path and server id for full-set-per-`(file, server)`
//!   replace. A crashed server's replacement publishes under a fresh id, so
//!   the worker clears the dead id's sets when the registry prunes it (#427).
//! - **navigation requests** (#195, #482): [`NavRequest`] carries hover /
//!   definition / references requests from a connection to the worker, each
//!   tagged with the requesting connection's private `reply` channel. The worker
//!   finds the first capable server via the registry, spawns a task that issues
//!   the typed LSP request, and sends the rift-typed [`DaemonMessage`] response
//!   straight back on that connection's `reply` channel — never onto the shared
//!   bus, so with two clients attached one client's answer cannot reach the
//!   other (#482). The worker is stateless for nav; drop-stale discipline is
//!   enforced per connection at the connection, keyed by [`NavRequestId`].
//!
//! The dispatch loop never blocks on server I/O: spawning, initialization, and
//! stdio all live on the registry's own tasks; the loop only forwards change
//! batches and folds the resulting diagnostics.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rift_explorer::Change;
use rift_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, NumberOrString, PublishDiagnosticsParams, ServerCapabilities,
};
use rift_lsp::nav::PositionEncoding;
use rift_lsp::{
    DocumentChange, DocumentSelector, DocumentSink, DocumentSync, Registry, ServerId,
    ServerLifecycle,
};
use rift_protocol::{
    DaemonMessage, Diagnostic, DiagnosticSeverity, LspServerState, NavRequestId, Position, Range,
};
use tokio::sync::mpsc;
use tracing::warn;

/// One server's full current diagnostic set for one file, ready to fold into the
/// daemon `State` and broadcast. `path` is worktree-relative (the protocol key
/// space); `server` is the daemon-assigned server id as a string. An empty
/// `items` clears that server's set for the file (LSP full-set replace).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostics {
    pub path: String,
    pub server: String,
    pub items: Vec<Diagnostic>,
}

/// One language server's lifecycle transition (issue #520), ready to fold
/// into the daemon `State` and broadcast as a `DaemonMessage::LspStatus`.
/// `server` is the stable server name (e.g. `"rust-analyzer"`), not a
/// per-spawn id — mirroring [`LspDiagnostics`] structurally, but keyed
/// differently (name-scoped health vs id-scoped diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspStatusEvent {
    pub server: String,
    pub state: LspServerState,
}

/// A live-buffer event from the editor (#189): the disk→buffer source-of-truth
/// shift. Carried from the dispatch loop to the off-loop [`LspWorker`], which
/// applies it to its [`DocumentSync`] so the open buffer drives `didChange`
/// instead of disk. `path` is worktree-relative (the protocol key space).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferEvent {
    /// The editor's buffer for `path` changed: feed `content` to the server(s) as
    /// a `didChange` and mark the path live (disk modifications for it are then
    /// suppressed).
    Changed { path: String, content: String },
    /// The editor closed the buffer for `path`: drop the override and revert to
    /// the disk-backed baseline.
    Closed { path: String },
}

/// A navigation request forwarded from a connection to the [`LspWorker`]
/// (issues #195, #482). Carries the protocol fields needed to issue the typed
/// LSP request off the dispatch loop, plus `reply` — the requesting connection's
/// private response channel, so the answer returns to that socket alone and is
/// never broadcast to other attached clients.
///
/// The worker spawns a task per request so slow server I/O never blocks document
/// sync or diagnostics. `id` is the drop-stale correlation key, applied per
/// connection at the connection: a response whose id a newer request has
/// superseded is dropped before it reaches the socket.
#[derive(Debug)]
pub enum NavRequest {
    Hover {
        id: NavRequestId,
        path: String,
        position: Position,
        reply: mpsc::Sender<DaemonMessage>,
    },
    Definition {
        id: NavRequestId,
        path: String,
        position: Position,
        reply: mpsc::Sender<DaemonMessage>,
    },
    References {
        id: NavRequestId,
        path: String,
        position: Position,
        reply: mpsc::Sender<DaemonMessage>,
    },
}

/// Map an explorer change batch onto the [`DocumentChange`] stream document sync
/// consumes. `Added` / `Changed` carry a full entry; only non-ignored file
/// entries drive a document — a directory carries no text, and an ignored file
/// (#309: now present in the snapshot) must never drive a server — so both are
/// dropped here. `Removed` carries no entry, so its kind and ignored status are
/// unknown — it is always forwarded, and document sync no-ops it when no
/// document was open for the path (e.g. a removed directory, or a removed
/// ignored file that was never opened).
pub fn document_changes(batch: &[Change]) -> Vec<DocumentChange> {
    let mut changes = Vec::new();
    for change in batch {
        match change {
            Change::Added { path, entry }
                if entry.kind == rift_explorer::EntryKind::File && !entry.ignored =>
            {
                changes.push(DocumentChange::Created { path: path.clone() });
            }
            Change::Changed { path, entry }
                if entry.kind == rift_explorer::EntryKind::File && !entry.ignored =>
            {
                changes.push(DocumentChange::Modified { path: path.clone() });
            }
            // A directory add/change, or an ignored file, carries no document.
            Change::Added { .. } | Change::Changed { .. } => {}
            Change::Removed { path } => {
                changes.push(DocumentChange::Removed { path: path.clone() });
            }
        }
    }
    changes
}

/// A [`DocumentSink`] that forwards each `didOpen` / `didChange` / `didClose`
/// notification to a fixed set of servers — the ones the registry matched for
/// the document's path.
///
/// The notification methods are non-blocking enqueues (they push onto the server
/// socket's internal queue, no I/O), so forwarding to several servers stays
/// synchronous and never blocks the worker. The sink borrows the registry for
/// the call and looks each server up by id, so a server that exited between
/// `observe` and the dispatch is simply skipped.
struct ServerSink<'a> {
    registry: &'a Registry,
    ids: Vec<ServerId>,
}

impl<'a> ServerSink<'a> {
    fn for_servers(registry: &'a Registry, ids: Vec<ServerId>) -> Self {
        Self { registry, ids }
    }

    /// Run `notify` against every still-live server in the set, logging — never
    /// propagating — a per-server failure so one dead server cannot stop the
    /// others (multi-server aggregation must survive one server dying; the
    /// registry restarts it lazily on the next matching change).
    fn fan_out(&self, action: &str, notify: impl Fn(&rift_lsp::Server) -> rift_lsp::Result<()>) {
        for id in &self.ids {
            if let Some(server) = self.registry.server(*id) {
                if let Err(error) = notify(server) {
                    warn!(action, %error, "notification to a language server failed");
                }
            }
        }
    }
}

impl DocumentSink for ServerSink<'_> {
    fn did_open(&mut self, params: DidOpenTextDocumentParams) -> rift_lsp::Result<()> {
        self.fan_out("didOpen", |server| server.did_open(params.clone()));
        Ok(())
    }

    fn did_change(&mut self, params: DidChangeTextDocumentParams) -> rift_lsp::Result<()> {
        self.fan_out("didChange", |server| server.did_change(params.clone()));
        Ok(())
    }

    fn did_save(&mut self, params: DidSaveTextDocumentParams) -> rift_lsp::Result<()> {
        self.fan_out("didSave", |server| server.did_save(params.clone()));
        Ok(())
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) -> rift_lsp::Result<()> {
        self.fan_out("didClose", |server| server.did_close(params.clone()));
        Ok(())
    }
}

/// The off-loop LSP worker: owns the server [`Registry`] and the document
/// [`DocumentSync`], turns document changes into server notifications, and
/// streams translated diagnostics back to the dispatch loop.
///
/// Single-task ownership keeps the registry and sync `!Sync`-free of shared
/// locks — the daemon's "state flows through channels" discipline — and means
/// the dispatch loop never touches a server. The registry publishes diagnostics
/// on its own channel; the worker `select!`s over that and the inbound document
/// changes so a slow server publish never stalls document sync and vice versa.
///
/// Navigation requests (#195) arrive on `nav_requests`: the worker looks up the
/// first capable server in the registry, spawns a task to issue the typed LSP
/// request off the loop, and sends the translated [`DaemonMessage`] response
/// straight back on the request's own `reply` channel — the requesting
/// connection's private inbox (#482). The worker holds no nav state: routing to
/// the right socket and drop-stale correlation both live at the connection.
pub struct LspWorker {
    root: PathBuf,
    registry: Registry,
    sync: DocumentSync,
    doc_changes: mpsc::Receiver<Vec<DocumentChange>>,
    /// Live-buffer events from the editor (#189), driving the disk→buffer shift.
    buffer_events: mpsc::Receiver<BufferEvent>,
    diagnostics_out: mpsc::Sender<LspDiagnostics>,
    /// Lifecycle transitions (#520), drained from the registry after every
    /// `observe` call alongside `clear_pruned`.
    lifecycle_out: mpsc::Sender<LspStatusEvent>,
    /// The registry's diagnostics channel, drained by `run`.
    server_diagnostics: mpsc::UnboundedReceiver<(ServerId, PublishDiagnosticsParams)>,
    /// Navigation requests from the connections (#195, #482), each carrying the
    /// requesting connection's `reply` channel for the response.
    nav_requests: mpsc::Receiver<NavRequest>,
    /// Worktree-relative paths with a live (non-empty) diagnostic set per
    /// server instance, mirrored from the publishes forwarded downstream. When
    /// the registry prunes a dead instance these are the paths that need a
    /// clearing empty update — the replacement publishes under a fresh id, so
    /// nothing else would ever drop the dead id's sets (#427).
    diagnostic_paths: HashMap<ServerId, HashSet<String>>,
}

impl LspWorker {
    /// Wire a worker for `root` using `selector`'s language → server table.
    /// `doc_changes` carries disk-driven change batches from the dispatch loop;
    /// `buffer_events` carries the editor's live-buffer feed (#189);
    /// `diagnostics_out` carries translated diagnostics back to it;
    /// `lifecycle_out` carries lifecycle transitions (#520).
    /// `nav_requests` carries navigation requests (#195, #482), each with the
    /// requesting connection's `reply` channel for the response.
    pub fn new(
        root: PathBuf,
        selector: DocumentSelector,
        doc_changes: mpsc::Receiver<Vec<DocumentChange>>,
        buffer_events: mpsc::Receiver<BufferEvent>,
        diagnostics_out: mpsc::Sender<LspDiagnostics>,
        lifecycle_out: mpsc::Sender<LspStatusEvent>,
        nav_requests: mpsc::Receiver<NavRequest>,
    ) -> Self {
        let (server_tx, server_diagnostics) = mpsc::unbounded_channel();
        let registry = Registry::with_selector(selector, root.clone(), server_tx);
        let sync = DocumentSync::new(root.clone());
        Self {
            root,
            registry,
            sync,
            doc_changes,
            buffer_events,
            diagnostics_out,
            lifecycle_out,
            server_diagnostics,
            nav_requests,
            diagnostic_paths: HashMap::new(),
        }
    }

    /// Drive the worker until a channel into it closes (the dispatch loop went
    /// away) — then shut every server down cleanly.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                batch = self.doc_changes.recv() => match batch {
                    Some(batch) => self.apply_changes(batch).await,
                    // The dispatch loop dropped the sender; nothing more to sync.
                    None => break,
                },
                event = self.buffer_events.recv() => match event {
                    Some(event) => self.apply_buffer_event(event).await,
                    // The buffer-event sender dropped (dispatch loop gone).
                    // Awaiting a pending future here would park the whole
                    // select loop forever (#497) — the arm handler never
                    // returns, so no other channel is ever polled again.
                    // Every inbound sender lives on the dispatch loop, so a
                    // closed channel means the loop is gone: stop and let the
                    // shutdown below clean up the servers.
                    None => break,
                },
                published = self.server_diagnostics.recv() => match published {
                    Some((id, params)) => self.publish(id, params),
                    // Every server's sender dropped and the registry is gone;
                    // the registry itself holds one, so this only fires after
                    // `shutdown`. Treat it as a stop.
                    None => break,
                },
                req = self.nav_requests.recv() => match req {
                    Some(req) => self.dispatch_nav(req).await,
                    // Same as `buffer_events`: every connection dropped its
                    // `nav_requests` clone (and the dispatch loop's keeper) —
                    // the daemon is going away, so stop instead of parking the
                    // select loop (#497).
                    None => break,
                },
            }
        }
        self.registry.shutdown().await;
    }

    /// Apply one live-buffer event (#189): feed the editor's buffer to the
    /// matching server(s) as a `didChange`, or revert a closed buffer to disk.
    ///
    /// Mirrors [`apply_changes`](Self::apply_changes): ensure the matching servers
    /// are running, then drive [`DocumentSync`] against a [`ServerSink`] over the
    /// disjoint `registry` / `sync` fields. When no server matches (unknown
    /// language, or all unavailable) the sink is empty — sync still tracks the
    /// path's live/closed state, but there is nothing to notify.
    async fn apply_buffer_event(&mut self, event: BufferEvent) {
        let path = match &event {
            BufferEvent::Changed { path, .. } | BufferEvent::Closed { path } => PathBuf::from(path),
        };
        let ids = self.registry.observe(&path).await;
        self.clear_pruned();
        self.forward_lifecycle();
        // The sink borrows `registry` immutably while `sync` is borrowed mutably —
        // distinct fields, so the split borrow holds (as in `apply_changes`).
        let mut sink = ServerSink::for_servers(&self.registry, ids);
        let result = match event {
            BufferEvent::Changed { content, .. } => {
                self.sync.apply_buffer_change(&path, content, &mut sink)
            }
            BufferEvent::Closed { .. } => self.sync.apply_buffer_close(&path, &mut sink),
        };
        if let Err(error) = result {
            // A read failure on close (file gone) or a URI error is not fatal:
            // log and move on, exactly as the disk path does.
            warn!(path = %path.display(), %error, "live-buffer sync failed");
        }
    }

    /// Sync one change batch: ensure the matching servers are running, then
    /// dispatch each change's `didOpen` / `didChange` / `didClose` to exactly
    /// those servers.
    async fn apply_changes(&mut self, batch: Vec<DocumentChange>) {
        for change in batch {
            let ids = self.registry.observe(change.path()).await;
            // The observe may have pruned a dead instance: clear its stale
            // sets before this change's own publishes land.
            self.clear_pruned();
            self.forward_lifecycle();
            // A change matching no server (unknown language, or every server for
            // it unavailable) drives no document — there is nothing to feed.
            if ids.is_empty() {
                continue;
            }
            // The sink borrows the registry immutably while `sync` is borrowed
            // mutably — distinct fields, so the split borrow holds.
            let mut sink = ServerSink::for_servers(&self.registry, ids);
            if let Err(error) = self.sync.apply(&change, &mut sink) {
                // A read failure (file removed mid-flight) or a URI error is not
                // fatal: log and move on, the next change re-syncs from disk.
                warn!(path = %change.path().display(), %error, "document sync failed");
            }
        }
    }

    /// Translate one server's `publishDiagnostics` into an [`LspDiagnostics`]
    /// keyed by worktree-relative path and server id, and forward it to the
    /// dispatch loop. A publish for a URI outside the watched root, or one that
    /// is not a `file://` path, is dropped — there is no relative key for it.
    fn publish(&mut self, id: ServerId, params: PublishDiagnosticsParams) {
        // A publish that raced the server's death is dropped: forwarding it
        // could re-record diagnostics under a dead id after `clear_pruned`
        // already ran, leaving a stale set nothing would ever clear (#427).
        if self.registry.server(id).is_none() {
            return;
        }
        let Some(path) = self.relative_path(&params) else {
            return;
        };
        let items: Vec<Diagnostic> = params
            .diagnostics
            .iter()
            .map(translate_diagnostic)
            .collect();
        if items.is_empty() {
            if let Some(paths) = self.diagnostic_paths.get_mut(&id) {
                paths.remove(&path);
                if paths.is_empty() {
                    self.diagnostic_paths.remove(&id);
                }
            }
        } else {
            self.diagnostic_paths
                .entry(id)
                .or_default()
                .insert(path.clone());
        }
        let diagnostics = LspDiagnostics {
            path,
            server: id.0.to_string(),
            items,
        };
        // A closed receiver means the dispatch loop is gone; the worker's own
        // channels will close next and `run` will stop, so dropping here is fine.
        let _ = self.diagnostics_out.try_send(diagnostics);
    }

    /// Push a clearing (empty) update for every path a just-pruned (dead)
    /// server instance still had live diagnostics on (#427).
    ///
    /// Called after each `Registry::observe`: a restart assigns the
    /// replacement a fresh [`ServerId`], so the dead id's `(path, server)`
    /// sets downstream would otherwise persist forever — stale errors in the
    /// problems view and status counts.
    fn clear_pruned(&mut self) {
        for id in self.registry.take_pruned() {
            let Some(paths) = self.diagnostic_paths.remove(&id) else {
                continue;
            };
            for path in paths {
                let cleared = LspDiagnostics {
                    path,
                    server: id.0.to_string(),
                    items: Vec::new(),
                };
                // Same closed-receiver rationale as `publish`.
                let _ = self.diagnostics_out.try_send(cleared);
            }
        }
    }

    /// Forward every lifecycle transition the registry has recorded since the
    /// last call (issue #520), translated to the wire state. Called after
    /// each `Registry::observe`, alongside `clear_pruned`.
    fn forward_lifecycle(&mut self) {
        for (server, state) in self.registry.take_lifecycle_events() {
            let event = LspStatusEvent {
                server: server.to_string(),
                state: translate_lifecycle(state),
            };
            // Same closed-receiver rationale as `publish`.
            let _ = self.lifecycle_out.try_send(event);
        }
    }

    /// The publish URI as a path relative to the watched root, or `None` when the
    /// URI is not a file under the root.
    ///
    /// A dropped publish (non-`file://` URI, or a file outside the root) is logged
    /// with the offending URI so a server publishing for an out-of-root header
    /// does not vanish without a trace — the drop itself is correct (there is no
    /// relative key for such a URI), only the silence was the bug.
    fn relative_path(&self, params: &PublishDiagnosticsParams) -> Option<String> {
        let Ok(absolute) = params.uri.to_file_path() else {
            warn!(uri = %params.uri, "dropped diagnostics publish for non-file:// URI");
            return None;
        };
        let Ok(relative) = absolute.strip_prefix(&self.root) else {
            warn!(uri = %params.uri, "dropped diagnostics publish for out-of-root URI");
            return None;
        };
        Some(relative.to_string_lossy().into_owned())
    }

    // ── Navigation request path (#195) ───────────────────────────────────────

    /// Dispatch a navigation request off the loop.
    ///
    /// Looks up the first capable server in the registry (synchronously — the
    /// registry is owned by this task), clones its socket and capabilities
    /// (the only handles that are `Send + 'static`), then spawns a task that
    /// issues the typed LSP request and sends the translated [`DaemonMessage`]
    /// straight back on the request's `reply` channel — the requesting
    /// connection's private inbox, so no other client sees it (#482). Drop-stale
    /// is the connection's concern, not the worker's.
    ///
    /// When no server is capable (not started, indexing, or wrong language) the
    /// request is a silent no-op — "no result" is the correct answer, not an
    /// error, matching the capability-check behaviour in [`NavRequester`]. The
    /// connection recorded this request's id as its latest on send, so a later
    /// request still supersedes it even though this one produced no response.
    async fn dispatch_nav(&mut self, req: NavRequest) {
        let root = self.root.clone();

        match req {
            NavRequest::Hover {
                id,
                path,
                position,
                reply,
            } => {
                let Some(requester) = self.find_capable_server(&path, NavCap::Hover) else {
                    return;
                };
                let abs_path = root.join(&path);
                tokio::spawn(async move {
                    let abs_path2 = abs_path.clone();
                    let text = tokio::task::spawn_blocking(move || {
                        rift_lsp::nav::read_text_from_disk(&abs_path2).unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default();
                    match requester.hover(&abs_path, position, &text).await {
                        Ok(content) => {
                            let _ = reply
                                .send(DaemonMessage::HoverResponse { id, content })
                                .await;
                        }
                        Err(err) => {
                            warn!(%err, "hover request failed");
                        }
                    }
                });
            }
            NavRequest::Definition {
                id,
                path,
                position,
                reply,
            } => {
                let Some(requester) = self.find_capable_server(&path, NavCap::Definition) else {
                    return;
                };
                let abs_path = root.join(&path);
                tokio::spawn(async move {
                    let abs_path2 = abs_path.clone();
                    let text = tokio::task::spawn_blocking(move || {
                        rift_lsp::nav::read_text_from_disk(&abs_path2).unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default();
                    match requester.definition(&abs_path, position, &text).await {
                        Ok(targets) => {
                            let _ = reply
                                .send(DaemonMessage::DefinitionResponse { id, targets })
                                .await;
                        }
                        Err(err) => {
                            warn!(%err, "definition request failed");
                        }
                    }
                });
            }
            NavRequest::References {
                id,
                path,
                position,
                reply,
            } => {
                let Some(requester) = self.find_capable_server(&path, NavCap::References) else {
                    return;
                };
                let abs_path = root.join(&path);
                tokio::spawn(async move {
                    let abs_path2 = abs_path.clone();
                    let text = tokio::task::spawn_blocking(move || {
                        rift_lsp::nav::read_text_from_disk(&abs_path2).unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default();
                    match requester.references(&abs_path, position, &text).await {
                        Ok(locations) => {
                            let _ = reply
                                .send(DaemonMessage::ReferencesResponse { id, locations })
                                .await;
                        }
                        Err(err) => {
                            warn!(%err, "references request failed");
                        }
                    }
                });
            }
        }
    }

    /// Look up the first live server in the registry that serves `rel_path`
    /// (by the selector's extension matching) and advertises `cap`. Returns an
    /// [`OwnedNavRequester`] — cloned socket, capabilities, and encoding — that
    /// a spawned task can use without borrowing the registry. Returns `None`
    /// when no capable server is available (silent no-op, not an error).
    fn find_capable_server(
        &self,
        rel_path: &str,
        cap: NavCap,
    ) -> Option<rift_lsp::OwnedNavRequester> {
        let abs_path = self.root.join(rel_path);
        let result = self
            .registry
            .first_capable_for_path(&abs_path, |caps| cap.check(caps));
        result.map(|(socket, caps)| {
            let encoding = PositionEncoding::from_capabilities(&caps);
            rift_lsp::OwnedNavRequester::new(socket, caps, encoding, self.root.clone())
        })
    }
}

// ── Navigation helpers ────────────────────────────────────────────────────────

/// Which LSP navigation capability a request requires.
#[derive(Clone, Copy)]
enum NavCap {
    Hover,
    Definition,
    References,
}

impl NavCap {
    fn check(self, caps: &ServerCapabilities) -> bool {
        use rift_lsp::nav::{has_definition, has_hover, has_references};
        match self {
            NavCap::Hover => has_hover(caps),
            NavCap::Definition => has_definition(caps),
            NavCap::References => has_references(caps),
        }
    }
}

/// Translate the registry's lifecycle enum into the wire state (1:1) — kept
/// as a translation, not a shared type, so `rift-lsp`'s lifecycle bookkeeping
/// stays independent of the protocol's exact wire shape.
fn translate_lifecycle(state: ServerLifecycle) -> LspServerState {
    match state {
        ServerLifecycle::Starting => LspServerState::Starting,
        ServerLifecycle::Running => LspServerState::Running,
        ServerLifecycle::Crashed => LspServerState::Crashed,
    }
}

/// Translate one `lsp_types` diagnostic into rift's own protocol diagnostic.
///
/// The daemon owns this translation so `crates/protocol` stays free of
/// `lsp-types`. A diagnostic with no severity defaults to `Error` (LSP leaves it
/// client-defined; rift surfaces an un-annotated problem at full weight); the
/// `code`, whether numeric or string in LSP, is rendered to a string.
fn translate_diagnostic(diagnostic: &rift_lsp::lsp_types::Diagnostic) -> Diagnostic {
    Diagnostic {
        range: translate_range(diagnostic.range),
        severity: translate_severity(diagnostic.severity),
        message: diagnostic.message.clone(),
        source: diagnostic.source.clone(),
        code: diagnostic.code.as_ref().map(translate_code),
    }
}

fn translate_range(range: rift_lsp::lsp_types::Range) -> Range {
    Range {
        start: translate_position(range.start),
        end: translate_position(range.end),
    }
}

fn translate_position(position: rift_lsp::lsp_types::Position) -> Position {
    Position {
        line: position.line,
        character: position.character,
    }
}

/// Map LSP's severity onto rift's four-variant enum, defaulting an omitted
/// severity to `Error`.
fn translate_severity(
    severity: Option<rift_lsp::lsp_types::DiagnosticSeverity>,
) -> DiagnosticSeverity {
    use rift_lsp::lsp_types::DiagnosticSeverity as Lsp;
    match severity {
        Some(Lsp::WARNING) => DiagnosticSeverity::Warning,
        Some(Lsp::INFORMATION) => DiagnosticSeverity::Information,
        Some(Lsp::HINT) => DiagnosticSeverity::Hint,
        // ERROR and any unknown/omitted severity surface at full weight.
        _ => DiagnosticSeverity::Error,
    }
}

/// Render an LSP diagnostic code (numeric or string) to a string.
fn translate_code(code: &NumberOrString) -> String {
    match code {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_explorer::{Entry, EntryKind};
    use rift_lsp::lsp_types::{
        DiagnosticSeverity as LspSeverity, Position as LspPosition, Range as LspRange,
    };
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn file_entry() -> Entry {
        Entry {
            kind: EntryKind::File,
            ignored: false,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    fn dir_entry() -> Entry {
        Entry {
            kind: EntryKind::Dir,
            ignored: false,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    fn ignored_file_entry() -> Entry {
        Entry {
            kind: EntryKind::File,
            ignored: true,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_document_changes_maps_file_adds_and_changes_drops_dirs() {
        let batch = vec![
            Change::Added {
                path: PathBuf::from("src/main.rs"),
                entry: file_entry(),
            },
            Change::Changed {
                path: PathBuf::from("src/lib.rs"),
                entry: file_entry(),
            },
            Change::Added {
                path: PathBuf::from("src"),
                entry: dir_entry(),
            },
            Change::Removed {
                path: PathBuf::from("old.rs"),
            },
        ];
        let changes = document_changes(&batch);
        assert_eq!(
            changes,
            vec![
                DocumentChange::Created {
                    path: PathBuf::from("src/main.rs"),
                },
                DocumentChange::Modified {
                    path: PathBuf::from("src/lib.rs"),
                },
                DocumentChange::Removed {
                    path: PathBuf::from("old.rs"),
                },
            ]
        );
    }

    /// Invariant guard (#309): an ignored file's `Added`/`Changed` must never
    /// drive a document change, now that the explorer snapshot includes
    /// gitignored entries. A `Removed` for an ignored path still forwards (its
    /// kind and ignored status are unknown at removal time) — document sync
    /// no-ops it as a harmless `didClose` against a never-opened document.
    #[test]
    fn test_document_changes_filters_ignored_files_but_still_forwards_removed() {
        let batch = vec![
            Change::Added {
                path: PathBuf::from("dist/bundle.js"),
                entry: ignored_file_entry(),
            },
            Change::Changed {
                path: PathBuf::from("dist/other.js"),
                entry: ignored_file_entry(),
            },
            Change::Added {
                path: PathBuf::from("src/main.rs"),
                entry: file_entry(),
            },
            Change::Removed {
                path: PathBuf::from("dist/gone.js"),
            },
        ];
        let changes = document_changes(&batch);
        assert_eq!(
            changes,
            vec![
                DocumentChange::Created {
                    path: PathBuf::from("src/main.rs"),
                },
                DocumentChange::Removed {
                    path: PathBuf::from("dist/gone.js"),
                },
            ]
        );
    }

    #[test]
    fn test_translate_severity_maps_each_and_defaults_to_error() {
        assert_eq!(
            translate_severity(Some(LspSeverity::ERROR)),
            DiagnosticSeverity::Error
        );
        assert_eq!(
            translate_severity(Some(LspSeverity::WARNING)),
            DiagnosticSeverity::Warning
        );
        assert_eq!(
            translate_severity(Some(LspSeverity::INFORMATION)),
            DiagnosticSeverity::Information
        );
        assert_eq!(
            translate_severity(Some(LspSeverity::HINT)),
            DiagnosticSeverity::Hint
        );
        assert_eq!(translate_severity(None), DiagnosticSeverity::Error);
    }

    #[test]
    fn test_translate_code_renders_number_and_string() {
        assert_eq!(translate_code(&NumberOrString::Number(42)), "42");
        assert_eq!(
            translate_code(&NumberOrString::String("E0308".into())),
            "E0308"
        );
    }

    #[test]
    fn test_translate_diagnostic_carries_range_message_source_and_code() {
        let lsp = rift_lsp::lsp_types::Diagnostic {
            range: LspRange {
                start: LspPosition {
                    line: 1,
                    character: 4,
                },
                end: LspPosition {
                    line: 1,
                    character: 9,
                },
            },
            severity: Some(LspSeverity::ERROR),
            code: Some(NumberOrString::String("E0308".into())),
            source: Some("rustc".into()),
            message: "mismatched types".into(),
            ..Default::default()
        };
        let translated = translate_diagnostic(&lsp);
        assert_eq!(translated.range.start.line, 1);
        assert_eq!(translated.range.start.character, 4);
        assert_eq!(translated.range.end.character, 9);
        assert_eq!(translated.severity, DiagnosticSeverity::Error);
        assert_eq!(translated.message, "mismatched types");
        assert_eq!(translated.source.as_deref(), Some("rustc"));
        assert_eq!(translated.code.as_deref(), Some("E0308"));
    }

    #[test]
    fn test_translate_lifecycle_maps_each_state() {
        assert_eq!(
            translate_lifecycle(ServerLifecycle::Starting),
            LspServerState::Starting
        );
        assert_eq!(
            translate_lifecycle(ServerLifecycle::Running),
            LspServerState::Running
        );
        assert_eq!(
            translate_lifecycle(ServerLifecycle::Crashed),
            LspServerState::Crashed
        );
    }

    /// A table whose binary cannot exist on `$PATH`, so `observe` always
    /// takes the failed-start path deterministically (issue #520) — no real
    /// server process is ever spawned.
    const MISSING_BINARY: &[rift_lsp::ServerSpec] = &[rift_lsp::ServerSpec {
        language: "rust",
        binary: "rift-nonexistent-lsp-status-test",
        args: &[],
        extensions: &["rs"],
    }];

    #[tokio::test]
    async fn test_apply_changes_forwards_starting_then_crashed_for_missing_binary() {
        let (_doc_tx, doc_rx) = mpsc::channel(8);
        let (_buffer_tx, buffer_rx) = mpsc::channel(8);
        let (diag_tx, _diag_rx) = mpsc::channel(8);
        let (status_tx, mut status_rx) = mpsc::channel(8);
        let (_nav_req_tx, nav_req_rx) = mpsc::channel(8);
        // An absolute root: `Url::from_file_path` rejects a relative one.
        let root = std::env::current_dir().expect("cwd is readable in tests");
        let mut worker = LspWorker::new(
            root,
            DocumentSelector::with_table(MISSING_BINARY),
            doc_rx,
            buffer_rx,
            diag_tx,
            status_tx,
            nav_req_rx,
        );

        worker
            .apply_changes(vec![DocumentChange::Created {
                path: PathBuf::from("main.rs"),
            }])
            .await;

        let binary = MISSING_BINARY[0].binary;
        assert_eq!(
            status_rx.try_recv(),
            Ok(LspStatusEvent {
                server: binary.to_string(),
                state: LspServerState::Starting,
            })
        );
        assert_eq!(
            status_rx.try_recv(),
            Ok(LspStatusEvent {
                server: binary.to_string(),
                state: LspServerState::Crashed,
            })
        );
        assert!(status_rx.try_recv().is_err(), "no further events");
    }

    // Navigation drop-stale routing (per connection) is exercised in the
    // `crates/daemon/src/lib.rs` tests, where the connection-side gate and the
    // two-connection reply routing live (#482).

    // ── Worker shutdown on channel close (#497) ──────────────────────────────

    /// Every channel handle the dispatch loop would hold against a worker,
    /// so a test can drop exactly one sender and keep the rest alive.
    struct WorkerChannels {
        doc_tx: mpsc::Sender<Vec<DocumentChange>>,
        buffer_tx: mpsc::Sender<BufferEvent>,
        diag_rx: mpsc::Receiver<LspDiagnostics>,
        status_rx: mpsc::Receiver<LspStatusEvent>,
        nav_req_tx: mpsc::Sender<NavRequest>,
    }

    /// A worker over an empty server table (no external process is ever
    /// spawned) plus the dispatch-loop side of all its channels.
    fn worker_with_channels() -> (LspWorker, WorkerChannels) {
        let (doc_tx, doc_rx) = mpsc::channel(8);
        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        let (diag_tx, diag_rx) = mpsc::channel(8);
        let (status_tx, status_rx) = mpsc::channel(8);
        let (nav_req_tx, nav_req_rx) = mpsc::channel(8);
        let worker = LspWorker::new(
            PathBuf::from("."),
            DocumentSelector::with_table(&[]),
            doc_rx,
            buffer_rx,
            diag_tx,
            status_tx,
            nav_req_rx,
        );
        let channels = WorkerChannels {
            doc_tx,
            buffer_tx,
            diag_rx,
            status_rx,
            nav_req_tx,
        };
        (worker, channels)
    }

    /// Await `run()` under a timeout, keeping the still-open handles alive for
    /// the duration so only the channel the test closed is closed. A timeout
    /// means the worker parked instead of shutting down.
    async fn assert_run_terminates<Open>(worker: LspWorker, still_open: Open, closed: &str) {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), worker.run()).await;
        assert!(
            result.is_ok(),
            "run() must terminate when {closed} closes, not park the worker"
        );
        drop(still_open);
    }

    #[tokio::test]
    async fn test_run_doc_changes_closed_terminates() {
        let (worker, channels) = worker_with_channels();
        let WorkerChannels {
            doc_tx,
            buffer_tx,
            diag_rx,
            status_rx,
            nav_req_tx,
        } = channels;
        drop(doc_tx);
        assert_run_terminates(
            worker,
            (buffer_tx, diag_rx, status_rx, nav_req_tx),
            "doc_changes",
        )
        .await;
    }

    /// Regression (#497): a closed buffer-event channel used to await
    /// `std::future::pending()` inside the select arm, freezing `run()` even
    /// though every other channel was still open.
    #[tokio::test]
    async fn test_run_buffer_events_closed_terminates() {
        let (worker, channels) = worker_with_channels();
        let WorkerChannels {
            doc_tx,
            buffer_tx,
            diag_rx,
            status_rx,
            nav_req_tx,
        } = channels;
        drop(buffer_tx);
        assert_run_terminates(
            worker,
            (doc_tx, diag_rx, status_rx, nav_req_tx),
            "buffer_events",
        )
        .await;
    }

    /// Regression (#497): same park as `buffer_events`, on the nav path.
    #[tokio::test]
    async fn test_run_nav_requests_closed_terminates() {
        let (worker, channels) = worker_with_channels();
        let WorkerChannels {
            doc_tx,
            buffer_tx,
            diag_rx,
            status_rx,
            nav_req_tx,
        } = channels;
        drop(nav_req_tx);
        assert_run_terminates(
            worker,
            (doc_tx, buffer_tx, diag_rx, status_rx),
            "nav_requests",
        )
        .await;
    }
}
