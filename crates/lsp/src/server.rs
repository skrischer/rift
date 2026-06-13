//! A single running language-server child process and its `async-lsp` main
//! loop.
//!
//! A [`Server`] owns one spawned server binary: the child process, the
//! `async-lsp` [`MainLoop`](async_lsp::MainLoop) driving its JSON-RPC stdio, and
//! the [`ServerSocket`] used to send it notifications. It is started lazily by
//! the [`Registry`](crate::Registry) at the worktree root and reused for the
//! session. The transport is exactly the spike's (`spike.rs`): a tokio child
//! bridged into `async-lsp`'s futures-io `run_buffered` via `tokio-util`'s
//! `compat`, keeping the crate on tokio alone.
//!
//! Supervision lives here: the main loop runs on its own task, and a liveness
//! [`watch`](tokio::sync::watch) flag flips to `false` the moment that task
//! ends (server exit, transport error, or clean shutdown). The registry reads
//! that flag to know a server died and to restart it lazily on the next
//! matching change. A server exit never propagates as a panic — it is a logged
//! state transition (`docs/spec-daemon-lsp.md`, the supervision risk row).

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{LanguageServer, ServerSocket};
use lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializedParams, PublishDiagnosticsParams, Url,
    WindowClientCapabilities, WorkspaceFolder,
};
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;
use tracing::{info, warn};

use crate::selector::{LanguageId, ServerName, ServerSpec};
use crate::{LspError, Result};

/// A monotonic identifier for a running server instance, unique within a
/// [`Registry`](crate::Registry). Two servers of the same language get distinct
/// ids, so the registry — and later the diagnostics protocol — can address each
/// independently (the multi-server-per-language case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerId(pub u64);

/// Router state for a running server: forwards every `publishDiagnostics` out
/// of the main loop, tagged with the originating [`ServerId`] so a downstream
/// consumer can key full-set replacement by `(file, server)` (the aggregation
/// the spec mandates). Other server chatter (progress, log, window messages) is
/// swallowed so it does not pollute the daemon log.
struct ServerClient {
    id: ServerId,
    diagnostics_tx: mpsc::UnboundedSender<(ServerId, PublishDiagnosticsParams)>,
}

/// One running language-server instance.
///
/// Cloneable handle parts (`socket`) let the registry drive the server; the
/// supervisor task owns the child and reports liveness through `alive`.
#[derive(Debug)]
pub struct Server {
    id: ServerId,
    language: LanguageId,
    name: ServerName,
    socket: ServerSocket,
    /// `true` while the main loop runs; flipped to `false` by the supervisor the
    /// moment the server exits. The registry reads this to decide a restart.
    alive: watch::Receiver<bool>,
}

impl Server {
    /// The id assigned at spawn.
    pub fn id(&self) -> ServerId {
        self.id
    }

    /// The language this server diagnoses.
    pub fn language(&self) -> LanguageId {
        self.language
    }

    /// The server binary name.
    pub fn name(&self) -> ServerName {
        self.name
    }

    /// Whether the main loop is still running. `false` means the server exited
    /// and the registry should restart it on the next matching change.
    pub fn is_alive(&self) -> bool {
        *self.alive.borrow()
    }

    /// A handle to send notifications to this server (e.g. `did_open`,
    /// `did_change`). Prefer the typed [`Server::did_open`] /
    /// [`Server::did_change`] / [`Server::did_close`] wrappers — they keep
    /// `async_lsp` an internal detail of this crate so consumers (the daemon's
    /// document sink) need not name the transport type.
    pub fn socket(&self) -> ServerSocket {
        self.socket.clone()
    }

    /// Send a `textDocument/didOpen` notification to this server.
    ///
    /// Non-blocking: the notification is enqueued on the server socket's
    /// internal channel, not written synchronously. An error means the server's
    /// main loop has ended (it exited); the caller logs and the registry
    /// restarts it lazily on the next matching change.
    pub fn did_open(&self, params: DidOpenTextDocumentParams) -> Result<()> {
        self.socket.clone().did_open(params)?;
        Ok(())
    }

    /// Send a `textDocument/didChange` notification to this server. See
    /// [`Server::did_open`] for the non-blocking / error semantics.
    pub fn did_change(&self, params: DidChangeTextDocumentParams) -> Result<()> {
        self.socket.clone().did_change(params)?;
        Ok(())
    }

    /// Send a `textDocument/didClose` notification to this server. See
    /// [`Server::did_open`] for the non-blocking / error semantics.
    pub fn did_close(&self, params: DidCloseTextDocumentParams) -> Result<()> {
        self.socket.clone().did_close(params)?;
        Ok(())
    }

    /// Spawn `spec`'s binary at `root_dir`, initialize the LSP session there,
    /// and start supervising it.
    ///
    /// The binary is resolved on `$PATH` by the OS (`Command::new`); a missing
    /// binary surfaces as [`LspError::Spawn`], which the registry maps to its
    /// log-once-and-skip policy rather than a fatal error. On success the server
    /// is initialized (`initialize` → `initialized`) and ready to receive
    /// document notifications; the main loop runs detached on its own task and
    /// flips the liveness flag when it ends.
    pub async fn spawn(
        id: ServerId,
        spec: &ServerSpec,
        root_dir: &Path,
        diagnostics_tx: mpsc::UnboundedSender<(ServerId, PublishDiagnosticsParams)>,
    ) -> Result<Self> {
        let root_uri = Url::from_file_path(root_dir)
            .map_err(|()| LspError::InvalidUri(root_dir.display().to_string()))?;

        let (mainloop, socket) = async_lsp::MainLoop::new_client(|_server| {
            let mut router = Router::new(ServerClient { id, diagnostics_tx });
            router
                .notification::<lsp_types::notification::PublishDiagnostics>(|this, params| {
                    // A closed receiver only means the consumer is gone; that is
                    // not the server's failure, so drop the notification rather
                    // than tearing the loop down.
                    let _ = this.diagnostics_tx.send((this.id, params));
                    ControlFlow::Continue(())
                })
                .notification::<lsp_types::notification::Progress>(|_, _| ControlFlow::Continue(()))
                .notification::<lsp_types::notification::ShowMessage>(|_, _| {
                    ControlFlow::Continue(())
                })
                .notification::<lsp_types::notification::LogMessage>(|_, _| {
                    ControlFlow::Continue(())
                });

            ServiceBuilder::new()
                .layer(TracingLayer::default())
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(router)
        });

        let mut child = tokio::process::Command::new(spec.binary)
            .args(spec.args)
            .current_dir(root_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| LspError::Spawn {
                server: spec.binary.to_string(),
                source,
            })?;
        // Both handles are `Some` because the pipes above were just requested —
        // an invariant of the spawn config, not a runtime condition.
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

        let (alive_tx, alive_rx) = watch::channel(true);
        let name = spec.binary;
        let language = spec.language;
        let root: PathBuf = root_dir.to_path_buf();

        // Supervisor task: own the child, drive the main loop to completion,
        // then mark the server dead. The child is killed on drop, so a transport
        // error or a dropped registry never leaks a process.
        tokio::spawn(async move {
            let _child = child;
            match mainloop.run_buffered(stdout, stdin).await {
                Ok(()) => info!(
                    server = name,
                    language,
                    root = %root.display(),
                    "language server exited"
                ),
                Err(error) => warn!(
                    server = name,
                    language,
                    root = %root.display(),
                    %error,
                    "language server main loop ended with error"
                ),
            }
            // A closed receiver just means the registry was dropped; nothing to
            // restart, so the send result is intentionally ignored.
            let _ = alive_tx.send(false);
        });

        let mut server = Self {
            id,
            language,
            name,
            socket,
            alive: alive_rx,
        };

        server.initialize(root_uri).await?;
        Ok(server)
    }

    /// Run the LSP handshake (`initialize` → `initialized`) at the worktree
    /// root. Capabilities mirror the spike's minimal set — enough for servers
    /// that gate work-done progress, nothing the v1 diagnostics path does not
    /// use.
    async fn initialize(&mut self, root_uri: Url) -> Result<()> {
        self.socket
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
        self.socket.initialized(InitializedParams {})?;
        info!(
            server = self.name,
            language = self.language,
            "language server initialized"
        );
        Ok(())
    }

    /// Ask the server to shut down cleanly (`shutdown` → `exit`). Best-effort:
    /// `kill_on_drop` on the child is the backstop if the server ignores the
    /// request, so errors here are not fatal — the registry logs and drops.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.socket.shutdown(()).await?;
        self.socket.exit(())?;
        Ok(())
    }
}
