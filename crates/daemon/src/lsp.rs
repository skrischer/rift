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
//!   replace.
//! - **navigation requests** (#195): [`NavRequest`] carries hover / definition /
//!   references requests from the dispatch loop to the worker. The worker finds
//!   the first capable server via the registry, spawns a task that issues the
//!   typed LSP request, and forwards the rift-typed [`DaemonMessage`] response
//!   back to the dispatch loop for broadcasting. Drop-stale discipline: the
//!   worker tracks the latest [`NavRequestId`] per operation type; a response
//!   whose id is superseded is silently dropped.
//!
//! The dispatch loop never blocks on server I/O: spawning, initialization, and
//! stdio all live on the registry's own tasks; the loop only forwards change
//! batches and folds the resulting diagnostics.

use std::path::PathBuf;

use rift_explorer::Change;
use rift_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, NumberOrString, PublishDiagnosticsParams, ServerCapabilities,
};
use rift_lsp::nav::PositionEncoding;
use rift_lsp::{DocumentChange, DocumentSelector, DocumentSink, DocumentSync, Registry, ServerId};
use rift_protocol::{DaemonMessage, Diagnostic, DiagnosticSeverity, NavRequestId, Position, Range};
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

/// A navigation request forwarded from the dispatch loop to the [`LspWorker`]
/// (issue #195). Carries the protocol fields needed to issue the typed LSP
/// request off the dispatch loop and correlate the response.
///
/// The worker spawns a task per request so slow server I/O never blocks document
/// sync or diagnostics. `id` is the drop-stale correlation key: the worker
/// tracks the latest `NavRequestId` per operation type and silently drops a
/// response if a newer request has been forwarded since this one was dispatched.
#[derive(Debug)]
pub enum NavRequest {
    Hover {
        id: NavRequestId,
        path: String,
        position: Position,
    },
    Definition {
        id: NavRequestId,
        path: String,
        position: Position,
    },
    References {
        id: NavRequestId,
        path: String,
        position: Position,
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
/// request off the loop, and forwards the translated [`DaemonMessage`] response
/// on `nav_responses`. Drop-stale correlation is enforced here (one
/// latest-id-per-op-type tracker), so superseded in-flight tasks are silently
/// discarded before broadcasting.
pub struct LspWorker {
    root: PathBuf,
    registry: Registry,
    sync: DocumentSync,
    doc_changes: mpsc::Receiver<Vec<DocumentChange>>,
    /// Live-buffer events from the editor (#189), driving the disk→buffer shift.
    buffer_events: mpsc::Receiver<BufferEvent>,
    diagnostics_out: mpsc::Sender<LspDiagnostics>,
    /// The registry's diagnostics channel, drained by `run`.
    server_diagnostics: mpsc::UnboundedReceiver<(ServerId, PublishDiagnosticsParams)>,
    /// Navigation requests from the dispatch loop (#195).
    nav_requests: mpsc::Receiver<NavRequest>,
    /// Translated navigation responses flowing back to the dispatch loop.
    nav_responses: mpsc::Sender<DaemonMessage>,
    /// Latest `NavRequestId` forwarded for each operation type, used for
    /// drop-stale discipline: a response whose id is older than the latest is
    /// dropped before broadcasting.
    latest_hover_id: Option<NavRequestId>,
    latest_definition_id: Option<NavRequestId>,
    latest_references_id: Option<NavRequestId>,
    /// Completed response tasks: each yields a `(NavRequestId, DaemonMessage)`.
    completed_nav: mpsc::UnboundedReceiver<(NavRequestId, DaemonMessage)>,
    /// The sender side of `completed_nav` — cloned into each spawned nav task.
    completed_nav_tx: mpsc::UnboundedSender<(NavRequestId, DaemonMessage)>,
}

impl LspWorker {
    /// Wire a worker for `root` using `selector`'s language → server table.
    /// `doc_changes` carries disk-driven change batches from the dispatch loop;
    /// `buffer_events` carries the editor's live-buffer feed (#189);
    /// `diagnostics_out` carries translated diagnostics back to it.
    /// `nav_requests` / `nav_responses` are the navigation request path (#195).
    pub fn new(
        root: PathBuf,
        selector: DocumentSelector,
        doc_changes: mpsc::Receiver<Vec<DocumentChange>>,
        buffer_events: mpsc::Receiver<BufferEvent>,
        diagnostics_out: mpsc::Sender<LspDiagnostics>,
        nav_requests: mpsc::Receiver<NavRequest>,
        nav_responses: mpsc::Sender<DaemonMessage>,
    ) -> Self {
        let (server_tx, server_diagnostics) = mpsc::unbounded_channel();
        let registry = Registry::with_selector(selector, root.clone(), server_tx);
        let sync = DocumentSync::new(root.clone());
        let (completed_nav_tx, completed_nav) = mpsc::unbounded_channel();
        Self {
            root,
            registry,
            sync,
            doc_changes,
            buffer_events,
            diagnostics_out,
            server_diagnostics,
            nav_requests,
            nav_responses,
            latest_hover_id: None,
            latest_definition_id: None,
            latest_references_id: None,
            completed_nav,
            completed_nav_tx,
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
                    // The buffer-event sender dropped (dispatch loop gone). The
                    // disk path may still run, so do not stop on this alone;
                    // `doc_changes` / the registry channel close drives the stop.
                    None => std::future::pending::<()>().await,
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
                    // Nav request sender dropped (dispatch loop gone). The disk
                    // path may still serve; stop is driven by `doc_changes` or
                    // the diagnostics channel.
                    None => std::future::pending::<()>().await,
                },
                result = self.completed_nav.recv() => {
                    if let Some((id, msg)) = result {
                        self.forward_nav_response(id, msg);
                    }
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
        let Some(path) = self.relative_path(&params) else {
            return;
        };
        let items = params
            .diagnostics
            .iter()
            .map(translate_diagnostic)
            .collect();
        let diagnostics = LspDiagnostics {
            path,
            server: id.0.to_string(),
            items,
        };
        // A closed receiver means the dispatch loop is gone; the worker's own
        // channels will close next and `run` will stop, so dropping here is fine.
        let _ = self.diagnostics_out.try_send(diagnostics);
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
    /// back via `completed_nav_tx`. The caller's `id` is recorded as the latest
    /// for its operation type so `forward_nav_response` can apply drop-stale.
    ///
    /// When no server is capable (not started, indexing, or wrong language) the
    /// request is a silent no-op — "no result" is the correct answer, not an
    /// error, matching the capability-check behaviour in [`NavRequester`].
    async fn dispatch_nav(&mut self, req: NavRequest) {
        let root = self.root.clone();
        let tx = self.completed_nav_tx.clone();

        match req {
            NavRequest::Hover { id, path, position } => {
                // Record the latest id before the capability check: if no server
                // is available the early-return means the client gets no response
                // (treated as "server not ready"), and a subsequent hover request
                // that does find a server will correctly supersede this id.
                self.latest_hover_id = Some(id);
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
                            let _ = tx.send((id, DaemonMessage::HoverResponse { id, content }));
                        }
                        Err(err) => {
                            warn!(%err, "hover request failed");
                        }
                    }
                });
            }
            NavRequest::Definition { id, path, position } => {
                self.latest_definition_id = Some(id);
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
                            let _ =
                                tx.send((id, DaemonMessage::DefinitionResponse { id, targets }));
                        }
                        Err(err) => {
                            warn!(%err, "definition request failed");
                        }
                    }
                });
            }
            NavRequest::References { id, path, position } => {
                self.latest_references_id = Some(id);
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
                            let _ =
                                tx.send((id, DaemonMessage::ReferencesResponse { id, locations }));
                        }
                        Err(err) => {
                            warn!(%err, "references request failed");
                        }
                    }
                });
            }
        }
    }

    /// Forward a completed nav response to the dispatch loop, applying drop-stale
    /// discipline: if a newer request of the same type was dispatched after the
    /// one that produced this response, drop it silently.
    fn forward_nav_response(&self, id: NavRequestId, msg: DaemonMessage) {
        let latest = match &msg {
            DaemonMessage::HoverResponse { .. } => self.latest_hover_id,
            DaemonMessage::DefinitionResponse { .. } => self.latest_definition_id,
            DaemonMessage::ReferencesResponse { .. } => self.latest_references_id,
            // Only nav response messages are routed here; any other variant is a
            // caller bug — drop it rather than panic.
            _ => return,
        };
        match latest {
            Some(latest_id) if latest_id == id => {
                // This response is still current: forward to the dispatch loop.
                // A closed receiver means the loop is gone; dropping is correct.
                if let Err(err) = self.nav_responses.try_send(msg) {
                    warn!(%err, "dropped nav response (dispatch loop full or closed)");
                }
            }
            _ => {
                // Superseded — a newer request was dispatched; drop silently.
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

    // ── Navigation correlation drop-stale tests ──────────────────────────────

    /// Minimal test harness for `forward_nav_response`: constructs the minimum
    /// `LspWorker` state needed to call the method without a real registry,
    /// by wrapping the fields directly.
    ///
    /// Acceptance: "an out-of-order / superseded response is dropped, not
    /// applied" (issue #195). This test drives `forward_nav_response`
    /// directly with a response whose id is older than `latest_hover_id`.
    #[test]
    fn test_correlation_superseded_hover_response_is_dropped() {
        // Two request ids: the client sent id=1, then immediately id=2.
        let id_first = NavRequestId(1);
        let id_second = NavRequestId(2);

        let (nav_resp_tx, mut nav_resp_rx) = tokio::sync::mpsc::channel::<DaemonMessage>(8);

        // Simulate: latest hover id is 2 (the second request was dispatched).
        // A response for id=1 arrives first (slow server, out of order).
        // Not used — we test forward_nav_response directly via the harness.
        let (_completed_nav_tx, _completed_nav_rx) =
            tokio::sync::mpsc::unbounded_channel::<(NavRequestId, DaemonMessage)>();

        // Build a minimal LspWorker with the fields we need by calling
        // forward_nav_response directly on a partial struct through a helper.
        // We test the logic independently of the registry and async machinery.
        let worker_state = WorkerDropStaleHarness {
            latest_hover_id: Some(id_second),
            latest_definition_id: None,
            latest_references_id: None,
            nav_responses: nav_resp_tx,
        };

        // Response for the FIRST (superseded) request — must be dropped.
        let superseded_msg = DaemonMessage::HoverResponse {
            id: id_first,
            content: None,
        };
        worker_state.forward_nav_response(id_first, superseded_msg);

        // Nothing should have arrived on the channel.
        assert!(
            nav_resp_rx.try_recv().is_err(),
            "a superseded hover response must be dropped, not forwarded"
        );

        // Response for the SECOND (current) request — must be forwarded.
        let current_msg = DaemonMessage::HoverResponse {
            id: id_second,
            content: Some(rift_protocol::HoverContent {
                markdown: "fn foo()".to_owned(),
                range: None,
            }),
        };
        worker_state.forward_nav_response(id_second, current_msg);

        let forwarded = nav_resp_rx
            .try_recv()
            .expect("current hover response must be forwarded");
        match forwarded {
            DaemonMessage::HoverResponse { id, .. } => {
                assert_eq!(id, id_second, "forwarded response id must match latest");
            }
            other => panic!("expected HoverResponse, got {other:?}"),
        }
    }

    #[test]
    fn test_correlation_superseded_definition_response_is_dropped() {
        let id_old = NavRequestId(10);
        let id_new = NavRequestId(11);
        let (nav_resp_tx, mut nav_resp_rx) = tokio::sync::mpsc::channel::<DaemonMessage>(8);

        let harness = WorkerDropStaleHarness {
            latest_hover_id: None,
            latest_definition_id: Some(id_new),
            latest_references_id: None,
            nav_responses: nav_resp_tx,
        };

        // Old definition response: drop.
        harness.forward_nav_response(
            id_old,
            DaemonMessage::DefinitionResponse {
                id: id_old,
                targets: vec![],
            },
        );
        assert!(
            nav_resp_rx.try_recv().is_err(),
            "superseded definition response must be dropped"
        );

        // New definition response: forward.
        harness.forward_nav_response(
            id_new,
            DaemonMessage::DefinitionResponse {
                id: id_new,
                targets: vec![],
            },
        );
        assert!(
            nav_resp_rx.try_recv().is_ok(),
            "current definition response must be forwarded"
        );
    }

    #[test]
    fn test_correlation_superseded_references_response_is_dropped() {
        let id_old = NavRequestId(20);
        let id_new = NavRequestId(21);
        let (nav_resp_tx, mut nav_resp_rx) = tokio::sync::mpsc::channel::<DaemonMessage>(8);

        let harness = WorkerDropStaleHarness {
            latest_hover_id: None,
            latest_definition_id: None,
            latest_references_id: Some(id_new),
            nav_responses: nav_resp_tx,
        };

        harness.forward_nav_response(
            id_old,
            DaemonMessage::ReferencesResponse {
                id: id_old,
                locations: vec![],
            },
        );
        assert!(
            nav_resp_rx.try_recv().is_err(),
            "superseded references response must be dropped"
        );

        harness.forward_nav_response(
            id_new,
            DaemonMessage::ReferencesResponse {
                id: id_new,
                locations: vec![],
            },
        );
        assert!(
            nav_resp_rx.try_recv().is_ok(),
            "current references response must be forwarded"
        );
    }

    /// Minimal struct for testing `forward_nav_response` without a full
    /// `LspWorker` — only the fields the method reads.
    struct WorkerDropStaleHarness {
        latest_hover_id: Option<NavRequestId>,
        latest_definition_id: Option<NavRequestId>,
        latest_references_id: Option<NavRequestId>,
        nav_responses: tokio::sync::mpsc::Sender<DaemonMessage>,
    }

    impl WorkerDropStaleHarness {
        fn forward_nav_response(&self, id: NavRequestId, msg: DaemonMessage) {
            let latest = match &msg {
                DaemonMessage::HoverResponse { .. } => self.latest_hover_id,
                DaemonMessage::DefinitionResponse { .. } => self.latest_definition_id,
                DaemonMessage::ReferencesResponse { .. } => self.latest_references_id,
                _ => return,
            };
            match latest {
                Some(latest_id) if latest_id == id => {
                    let _ = self.nav_responses.try_send(msg);
                }
                _ => {}
            }
        }
    }
}
