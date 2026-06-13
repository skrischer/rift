//! Commitment-gate spike (issue #173): a real rust-analyzer round-trip over
//! `async-lsp`, end-to-end.
//!
//! This proves the toolchain the rest of the milestone commits to —
//! `async-lsp`'s `MainLoop` driving a child language server over tokio
//! child-process stdio (bridged to futures-io via `tokio-util`'s `compat`),
//! the `Router` notification path delivering `publishDiagnostics`, and
//! `lsp-types` round-tripping the wire format. It is intentionally minimal: a
//! single server, a single document, on-disk content fed once via `didOpen`.
//! The production registry, lifecycle, and worktree-driven document sync are
//! later issues under `docs/spec-daemon-lsp.md`; nothing here special-cases
//! rust-analyzer beyond it being the proving server named in the spec.

use std::ops::ControlFlow;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::LanguageServer;
use futures::channel::mpsc;
use futures::StreamExt;
use lsp_types::{
    ClientCapabilities, DidOpenTextDocumentParams, InitializeParams, InitializedParams,
    PublishDiagnosticsParams, TextDocumentItem, Url, WindowClientCapabilities, WorkspaceFolder,
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;
use tracing::info;

use crate::{LspError, Result};

/// How long to wait for the first `publishDiagnostics` after `didOpen` before
/// giving up. rust-analyzer must spawn, index the (tiny) fixture crate, and
/// publish — generous so a cold machine does not flake, bounded so a no-go
/// (server never publishes) fails fast instead of hanging.
const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(60);

/// Router state for the spike: a sender that forwards each
/// `publishDiagnostics` notification out of the main loop to the caller.
struct SpikeClient {
    diagnostics_tx: mpsc::UnboundedSender<PublishDiagnosticsParams>,
}

/// Event used to break the main loop once the round-trip is done.
struct Stop;

/// Run one rust-analyzer round-trip against `root_dir` and return the first
/// batch of diagnostics published for `relative_file`.
///
/// Spawns `rust-analyzer` (must be on `$PATH`) at `root_dir`, initializes the
/// session there, opens `relative_file` with its on-disk content, waits for the
/// server to publish diagnostics for it, then shuts the server down cleanly.
///
/// Returns the diagnostics rust-analyzer published for the file — non-empty
/// when the fixture contains a deliberate error, proving the signal travels
/// server -> `async-lsp` -> caller end-to-end.
pub async fn run_rust_analyzer_roundtrip(
    root_dir: &Path,
    relative_file: &Path,
) -> Result<Vec<lsp_types::Diagnostic>> {
    let root_uri = Url::from_file_path(root_dir)
        .map_err(|()| LspError::InvalidUri(root_dir.display().to_string()))?;
    let file_path = root_dir.join(relative_file);
    let file_uri = Url::from_file_path(&file_path)
        .map_err(|()| LspError::InvalidUri(file_path.display().to_string()))?;
    let text = tokio::fs::read_to_string(&file_path)
        .await
        .map_err(|source| LspError::ReadDocument {
            path: file_path.display().to_string(),
            source,
        })?;

    let (diagnostics_tx, mut diagnostics_rx) = mpsc::unbounded();

    let (mainloop, mut server) = async_lsp::MainLoop::new_client(|_server| {
        let mut router = Router::new(SpikeClient { diagnostics_tx });
        router
            .notification::<lsp_types::notification::PublishDiagnostics>(|this, params| {
                let _ = this.diagnostics_tx.unbounded_send(params);
                ControlFlow::Continue(())
            })
            // rust-analyzer chats progress and log messages; swallow them so
            // unhandled-notification warnings do not pollute the spike output.
            .notification::<lsp_types::notification::Progress>(|_, _| ControlFlow::Continue(()))
            .notification::<lsp_types::notification::ShowMessage>(|_, _| ControlFlow::Continue(()))
            .notification::<lsp_types::notification::LogMessage>(|_, _| ControlFlow::Continue(()))
            .event(|_, _: Stop| ControlFlow::Break(Ok(())));

        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .service(router)
    });

    let mut child = tokio::process::Command::new("rust-analyzer")
        .current_dir(root_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| LspError::Spawn {
            server: "rust-analyzer".to_string(),
            source,
        })?;
    // Both handles are `Some` because the pipes above were just requested; an
    // invariant of the spawn config, not a runtime condition.
    let stdout = child
        .stdout
        .take()
        .expect("child configured with piped stdout")
        .compat();
    let stdin = child
        .stdin
        .take()
        .expect("child configured with piped stdin")
        .compat_write();

    let mainloop_fut = tokio::spawn(async move { mainloop.run_buffered(stdout, stdin).await });

    server
        .initialize(InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "root".into(),
            }]),
            capabilities: ClientCapabilities {
                window: Some(WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..WindowClientCapabilities::default()
                }),
                ..ClientCapabilities::default()
            },
            ..InitializeParams::default()
        })
        .await?;
    server.initialized(InitializedParams {})?;

    server.did_open(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: file_uri.clone(),
            language_id: "rust".into(),
            version: 0,
            text,
        },
    })?;

    // Wait for the first publish that targets our file. rust-analyzer may emit
    // an empty set first (before indexing finishes), then the real one — keep
    // reading until a publish for this file actually carries diagnostics, or
    // the timeout fires.
    let diagnostics = wait_for_diagnostics(&mut diagnostics_rx, &file_uri).await?;
    info!(count = diagnostics.len(), "spike: received diagnostics");

    // Clean shutdown so rust-analyzer is not left orphaned (kill_on_drop is the
    // backstop, not the happy path).
    server.shutdown(()).await?;
    server.exit(())?;
    let _ = server.emit(Stop);
    let _ = mainloop_fut.await;

    Ok(diagnostics)
}

/// Read from the diagnostics stream until a non-empty publish for `file_uri`
/// arrives or the timeout elapses. An empty set for the file is treated as
/// "not yet" (rust-analyzer publishes a placeholder before indexing settles).
async fn wait_for_diagnostics(
    rx: &mut mpsc::UnboundedReceiver<PublishDiagnosticsParams>,
    file_uri: &Url,
) -> Result<Vec<lsp_types::Diagnostic>> {
    let deadline = tokio::time::sleep(DIAGNOSTICS_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            maybe = rx.next() => match maybe {
                Some(params) if &params.uri == file_uri && !params.diagnostics.is_empty() => {
                    return Ok(params.diagnostics);
                }
                Some(_) => continue,
                None => return Err(LspError::ServerClosed),
            },
            () = &mut deadline => {
                return Err(LspError::Timeout(DIAGNOSTICS_TIMEOUT));
            }
        }
    }
}
