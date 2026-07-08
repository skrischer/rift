//! Daemon-side LSP client for rift.
//!
//! Manages language-server child processes on the remote host and turns their
//! pushed diagnostics into rift's own protocol types. `gpui`-free and
//! musl-clean so it can be linked into the statically built daemon
//! (`docs/spec-daemon-lsp.md`).
//!
//! This crate currently carries the commitment-gate spike (issue #173) that
//! proves a real rust-analyzer round-trip over `async-lsp`. The production
//! registry, lifecycle, and worktree-driven document sync land in later issues
//! under the same spec.
//!
//! `lsp_types` is re-exported so the daemon translates the server's diagnostics
//! against the exact version this crate speaks across the wire — `protocol`
//! itself stays free of `lsp-types` (spec: the daemon does the translation).

mod error;

pub mod document;
pub mod nav;
pub mod registry;
pub mod selector;
pub mod server;
pub mod spike;

/// Re-exported so the daemon's navigation dispatch layer can hold an owned,
/// `Send + 'static` socket handle without taking a direct dependency on
/// `async-lsp` (the version is pinned here by `rift-lsp`'s own Cargo.toml).
pub use async_lsp::ServerSocket;
pub use document::{DocumentAction, DocumentChange, DocumentSink, DocumentSync};
pub use error::LspError;
pub use nav::{NavRequester, OwnedNavRequester, PositionEncoding};
pub use registry::{Registry, ServerLifecycle};
pub use selector::{DocumentSelector, ServerSpec};
pub use server::{Server, ServerId};

/// `lsp-types` re-exported at the version this crate's `async-lsp` speaks, so
/// consumers translating diagnostics never pin a mismatching copy.
pub use lsp_types;

/// Result alias for the LSP client layer.
pub type Result<T> = std::result::Result<T, LspError>;
