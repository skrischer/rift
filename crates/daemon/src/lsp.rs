//! Daemon-side LSP wiring (issue #177).
//!
//! Bridges the three crates the diagnostics path spans, all off the dispatch
//! loop:
//!
//! - **explorer → lsp**: [`document_changes`] maps an explorer `Change` batch
//!   onto the [`rift_lsp::DocumentChange`] vocabulary the disk-backed document
//!   sync consumes. The explorer snapshot already excludes ignored paths
//!   (`target/`, `.git/`, `.gitignore`d), so an ignored path never reaches here
//!   and never drives a server.
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
//!
//! The dispatch loop never blocks on server I/O: spawning, initialization, and
//! stdio all live on the registry's own tasks; the loop only forwards change
//! batches and folds the resulting diagnostics.

use std::path::PathBuf;

use rift_explorer::Change;
use rift_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    NumberOrString, PublishDiagnosticsParams,
};
use rift_lsp::{DocumentChange, DocumentSelector, DocumentSink, DocumentSync, Registry, ServerId};
use rift_protocol::{Diagnostic, DiagnosticSeverity, Position, Range};
use tokio::sync::mpsc;

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

/// Map an explorer change batch onto the [`DocumentChange`] stream document sync
/// consumes. `Added` / `Changed` carry a full entry; only file entries drive a
/// document (a directory carries no text), so directory changes are dropped
/// here. `Removed` carries no entry, so its kind is unknown — it is always
/// forwarded, and document sync no-ops it when no document was open for the path
/// (e.g. a removed directory).
pub fn document_changes(batch: &[Change]) -> Vec<DocumentChange> {
    let mut changes = Vec::new();
    for change in batch {
        match change {
            Change::Added { path, entry } if entry.kind == rift_explorer::EntryKind::File => {
                changes.push(DocumentChange::Created { path: path.clone() });
            }
            Change::Changed { path, entry } if entry.kind == rift_explorer::EntryKind::File => {
                changes.push(DocumentChange::Modified { path: path.clone() });
            }
            // A directory add/change carries no document.
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
                    eprintln!("rift-daemon: {action} to a language server failed: {error}");
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
pub struct LspWorker {
    root: PathBuf,
    registry: Registry,
    sync: DocumentSync,
    doc_changes: mpsc::Receiver<Vec<DocumentChange>>,
    diagnostics_out: mpsc::Sender<LspDiagnostics>,
    /// The registry's diagnostics channel, drained by `run`.
    server_diagnostics: mpsc::UnboundedReceiver<(ServerId, PublishDiagnosticsParams)>,
}

impl LspWorker {
    /// Wire a worker for `root` using `selector`'s language → server table.
    /// `doc_changes` carries change batches from the dispatch loop;
    /// `diagnostics_out` carries translated diagnostics back to it.
    pub fn new(
        root: PathBuf,
        selector: DocumentSelector,
        doc_changes: mpsc::Receiver<Vec<DocumentChange>>,
        diagnostics_out: mpsc::Sender<LspDiagnostics>,
    ) -> Self {
        let (server_tx, server_diagnostics) = mpsc::unbounded_channel();
        let registry = Registry::with_selector(selector, root.clone(), server_tx);
        let sync = DocumentSync::new(root.clone());
        Self {
            root,
            registry,
            sync,
            doc_changes,
            diagnostics_out,
            server_diagnostics,
        }
    }

    /// Drive the worker until either channel into it closes (the dispatch loop
    /// went away) — then shut every server down cleanly.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                batch = self.doc_changes.recv() => match batch {
                    Some(batch) => self.apply_changes(batch).await,
                    // The dispatch loop dropped the sender; nothing more to sync.
                    None => break,
                },
                published = self.server_diagnostics.recv() => match published {
                    Some((id, params)) => self.publish(id, params),
                    // Every server's sender dropped and the registry is gone;
                    // the registry itself holds one, so this only fires after
                    // `shutdown`. Treat it as a stop.
                    None => break,
                },
            }
        }
        self.registry.shutdown().await;
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
                eprintln!(
                    "rift-daemon: document sync failed for {}: {error}",
                    change.path().display()
                );
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
    fn relative_path(&self, params: &PublishDiagnosticsParams) -> Option<String> {
        let absolute = params.uri.to_file_path().ok()?;
        let relative = absolute.strip_prefix(&self.root).ok()?;
        Some(relative.to_string_lossy().into_owned())
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
}
