// Copyright (C) 2024 rift contributors — licensed under GPL-3.0-or-later.
//
//! Stub-server round-trip tests for the navigation request path (issue #194).
//!
//! Uses `async-lsp`'s in-process `MainLoop::new_server` / `new_client` pair
//! wired via `tokio::io::duplex` to avoid spawning real language-server
//! binaries. The stub server answers with canned hover / definition /
//! references responses for a specific "known" position and returns empty
//! results for everything else.
//!
//! Tests verify:
//! - Correct rift-typed result for the known position.
//! - Empty result for an unknown position.
//! - Capability check: if the server does not advertise the capability the
//!   method returns empty / None immediately without sending any request.
//! - Offset-encoding translation: a UTF-16 server reports positions in CUs;
//!   the returned `NavLocation` must carry rift (UTF-8 char) offsets.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use async_lsp::router::Router;
use async_lsp::{LanguageServer, ServerSocket};
use futures::AsyncReadExt;
use lsp_types::{
    GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability, InitializedParams,
    Location, MarkedString, OneOf, Position as LspPosition, Range as LspRange, ServerCapabilities,
    TextDocumentIdentifier, Url,
};
use rift_lsp::nav::{
    has_definition, has_hover, has_references, lsp_range_to_rift, PositionEncoding,
};
use rift_protocol::Position;
use tokio_util::compat::TokioAsyncReadCompatExt;
use tower::ServiceBuilder;

const MEMORY_CHANNEL_SIZE: usize = 64 << 10; // 64 KiB

/// A unique counter for stub test roots so parallel tests do not collide.
static ROOT_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A temporary directory that removes itself on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rift-nav-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp root");
        let path = path.canonicalize().expect("canonicalize temp root");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ── Known position for the stub server ───────────────────────────────────────

/// The stub server returns a non-empty result for this position only.
const KNOWN_LSP_POS: LspPosition = LspPosition {
    line: 2,
    character: 5,
};

/// What the stub server returns as a definition target (absolute path).
fn stub_def_uri(root: &Path) -> Url {
    Url::from_file_path(root.join("src/target.rs")).unwrap()
}

/// The stub server's definition target range (ASCII line — CU == char).
const STUB_DEF_RANGE: LspRange = LspRange {
    start: LspPosition {
        line: 0,
        character: 0,
    },
    end: LspPosition {
        line: 0,
        character: 5,
    },
};

/// Stub hover text returned for the known position.
const STUB_HOVER_TEXT: &str = "stub hover: **fn foo**";

// ── Capability sets ───────────────────────────────────────────────────────────

/// Caps for a server that advertises hover + definition + references.
fn full_caps() -> ServerCapabilities {
    ServerCapabilities {
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    }
}

/// Caps for a server that advertises no navigation capabilities.
fn no_nav_caps() -> ServerCapabilities {
    ServerCapabilities::default()
}

// ── Stub server builder ───────────────────────────────────────────────────────

/// Spawn an in-process async-lsp stub server, run `f` with the `ServerSocket`
/// and `ServerCapabilities`, then shut down cleanly.
///
/// The stub answers initialize with `caps`, responds to hover/definition/
/// references only for `KNOWN_LSP_POS`, and shuts down on `shutdown + exit`.
async fn with_stub_server<F, Fut>(
    caps: ServerCapabilities,
    root: PathBuf,
    f: F,
) -> rift_lsp::Result<()>
where
    F: FnOnce(ServerSocket, ServerCapabilities) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = rift_lsp::Result<()>> + Send + 'static,
{
    let caps_clone = caps.clone();
    let caps_for_init = caps.clone();
    let root_for_def = root.clone();
    let root_for_refs = root.clone();

    // `_client_socket` must be kept alive until the server_task completes —
    // it is the sender of the server's internal event channel; dropping it
    // prematurely causes "Sender is alive" panics in run_buffered.
    let (server_main, _client_socket) = async_lsp::MainLoop::new_server(move |_client| {
        let mut router = Router::new(());

        router
            .request::<lsp_types::request::Initialize, _>(move |_, _params| {
                let caps = caps_for_init.clone();
                async move {
                    Ok(lsp_types::InitializeResult {
                        capabilities: caps,
                        server_info: None,
                    })
                }
            })
            .notification::<lsp_types::notification::Initialized>(|_, _: InitializedParams| {
                ControlFlow::Continue(())
            })
            .request::<lsp_types::request::Shutdown, _>(|_, _: ()| async { Ok(()) })
            .notification::<lsp_types::notification::Exit>(|_, _: ()| ControlFlow::Break(Ok(())))
            .request::<lsp_types::request::HoverRequest, _>(|_, params| async move {
                let pos = params.text_document_position_params.position;
                if pos.line == KNOWN_LSP_POS.line && pos.character == KNOWN_LSP_POS.character {
                    Ok(Some(Hover {
                        contents: HoverContents::Scalar(MarkedString::String(
                            STUB_HOVER_TEXT.to_owned(),
                        )),
                        range: None,
                    }))
                } else {
                    Ok(None)
                }
            })
            .request::<lsp_types::request::GotoDefinition, _>(move |_, params| {
                let def_uri = stub_def_uri(&root_for_def);
                async move {
                    let pos = params.text_document_position_params.position;
                    if pos.line == KNOWN_LSP_POS.line && pos.character == KNOWN_LSP_POS.character {
                        Ok(Some(GotoDefinitionResponse::Scalar(Location {
                            uri: def_uri,
                            range: STUB_DEF_RANGE,
                        })))
                    } else {
                        Ok(None)
                    }
                }
            })
            .request::<lsp_types::request::References, _>(move |_, params| {
                let root = root_for_refs.clone();
                async move {
                    let pos = params.text_document_position.position;
                    if pos.line == KNOWN_LSP_POS.line && pos.character == KNOWN_LSP_POS.character {
                        Ok(Some(vec![Location {
                            uri: stub_def_uri(&root),
                            range: STUB_DEF_RANGE,
                        }]))
                    } else {
                        Ok(Some(vec![]))
                    }
                }
            });

        ServiceBuilder::new().service(router)
    });

    let (client_main, mut server_socket) = async_lsp::MainLoop::new_client(|_server| {
        let mut router = Router::new(());
        router
            .notification::<lsp_types::notification::PublishDiagnostics>(|_, _| {
                ControlFlow::Continue(())
            })
            .notification::<lsp_types::notification::Progress>(|_, _| ControlFlow::Continue(()))
            .notification::<lsp_types::notification::LogMessage>(|_, _| ControlFlow::Continue(()))
            .notification::<lsp_types::notification::ShowMessage>(|_, _| ControlFlow::Continue(()));
        ServiceBuilder::new().service(router)
    });

    // Wire a loopback duplex channel between server and client main loops.
    let (server_stream, client_stream) = tokio::io::duplex(MEMORY_CHANNEL_SIZE);
    let (server_rx, server_tx) = server_stream.compat().split();
    let server_task = tokio::spawn(async move {
        server_main.run_buffered(server_rx, server_tx).await.ok();
    });
    let (client_rx, client_tx) = client_stream.compat().split();
    let client_task = tokio::spawn(async move {
        client_main.run_buffered(client_rx, client_tx).await.ok();
    });

    // LSP handshake.
    let root_uri = Url::from_file_path(&root).expect("test root is a valid file path");
    server_socket
        .initialize(lsp_types::InitializeParams {
            workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
                uri: root_uri,
                name: "test".into(),
            }]),
            capabilities: lsp_types::ClientCapabilities::default(),
            ..lsp_types::InitializeParams::default()
        })
        .await
        .unwrap();
    server_socket.initialized(InitializedParams {}).unwrap();

    // Run caller's assertions with a clone of the socket.
    let socket_clone = server_socket.clone();
    f(socket_clone, caps_clone).await?;

    // Graceful shutdown: shutdown + exit causes the server's mainloop to
    // return, which propagates EOF to the client's mainloop, both tasks exit.
    server_socket.shutdown(()).await.ok();
    server_socket.exit(()).ok();
    // Drop the remaining socket handle so no sender outlives the main loops.
    drop(server_socket);
    // Wait for both loops to finish (with a safety timeout to avoid hangs).
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server_task).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), client_task).await;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The known position in rift wire encoding (line 2, char 5 — ASCII line).
fn known_rift_pos() -> Position {
    Position {
        line: KNOWN_LSP_POS.line,
        character: KNOWN_LSP_POS.character,
    }
}

/// An unknown position — the stub server returns empty/None for it.
fn unknown_rift_pos() -> Position {
    Position {
        line: 99,
        character: 0,
    }
}

#[tokio::test]
async fn test_hover_known_position_returns_content() {
    let tmp = TempDir::new("hover-known");
    let root = tmp.path.clone();
    let encoding = PositionEncoding::Utf16;

    with_stub_server(full_caps(), root, move |mut socket, caps| {
        Box::pin(async move {
            assert!(has_hover(&caps));
            // ASCII text: char offset == UTF-16 CU.
            let text = "line 0\nline 1\nfn foo() {}\nline 3\n";
            let pos = known_rift_pos();
            let lsp_pos = rift_lsp::nav::rift_pos_to_lsp(pos, text, encoding);

            let params = lsp_types::HoverParams {
                text_document_position_params: lsp_types::TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::parse("file:///tmp/test.rs").unwrap(),
                    },
                    position: lsp_pos,
                },
                work_done_progress_params: Default::default(),
            };

            let result = socket.hover(params).await.expect("hover request succeeded");
            assert!(result.is_some(), "known position must return hover content");
            let content = rift_lsp::nav::hover_to_protocol(result.unwrap(), text, encoding);
            assert_eq!(content.markdown, STUB_HOVER_TEXT);

            Ok(())
        })
    })
    .await
    .expect("test passed");
}

#[tokio::test]
async fn test_hover_unknown_position_returns_none() {
    let tmp = TempDir::new("hover-unknown");
    let root = tmp.path.clone();
    let encoding = PositionEncoding::Utf16;

    with_stub_server(full_caps(), root, move |mut socket, caps| {
        Box::pin(async move {
            assert!(has_hover(&caps));
            let text = "line 0\n";
            let pos = unknown_rift_pos();
            let lsp_pos = rift_lsp::nav::rift_pos_to_lsp(pos, text, encoding);

            let params = lsp_types::HoverParams {
                text_document_position_params: lsp_types::TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::parse("file:///tmp/test.rs").unwrap(),
                    },
                    position: lsp_pos,
                },
                work_done_progress_params: Default::default(),
            };

            let result = socket.hover(params).await.expect("hover request succeeded");
            assert!(result.is_none(), "unknown position must return None");

            Ok(())
        })
    })
    .await
    .expect("test passed");
}

#[tokio::test]
async fn test_definition_known_position_returns_location() {
    let tmp = TempDir::new("def-known");
    let root = tmp.path.clone();
    // Create the target file so the disk-read fallback can produce a line preview.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/target.rs"), "fn bar() {}\n").unwrap();
    let encoding = PositionEncoding::Utf16;

    with_stub_server(full_caps(), root.clone(), move |mut socket, caps| {
        Box::pin(async move {
            assert!(has_definition(&caps));
            let text = "line 0\nline 1\nfn foo() {}\n";
            let pos = known_rift_pos();
            let lsp_pos = rift_lsp::nav::rift_pos_to_lsp(pos, text, encoding);

            let params = lsp_types::GotoDefinitionParams {
                text_document_position_params: lsp_types::TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::parse("file:///tmp/test.rs").unwrap(),
                    },
                    position: lsp_pos,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };

            let result = socket
                .definition(params)
                .await
                .expect("definition succeeded");
            assert!(result.is_some());
            let locs =
                rift_lsp::nav::definition_response_to_protocol(result.unwrap(), &root, encoding);
            assert_eq!(locs.len(), 1, "one definition target expected");
            let loc = &locs[0];
            assert!(
                !loc.out_of_root,
                "target inside root must not be out_of_root"
            );
            assert!(
                loc.path.ends_with("src/target.rs"),
                "path should be relative to root: {}",
                loc.path
            );
            // STUB_DEF_RANGE is on line 0, chars 0..5 (ASCII).
            assert_eq!(loc.range.start.line, 0);
            assert_eq!(loc.range.start.character, 0);
            assert_eq!(loc.range.end.character, 5);
            // Line preview: first line of "fn bar() {}\n" trimmed.
            assert_eq!(loc.line_preview.as_deref(), Some("fn bar() {}"));

            Ok(())
        })
    })
    .await
    .expect("test passed");
}

#[tokio::test]
async fn test_definition_unknown_position_returns_empty() {
    let tmp = TempDir::new("def-unknown");
    let root = tmp.path.clone();
    let encoding = PositionEncoding::Utf16;

    with_stub_server(full_caps(), root.clone(), move |mut socket, _caps| {
        Box::pin(async move {
            let text = "line 0\n";
            let pos = unknown_rift_pos();
            let lsp_pos = rift_lsp::nav::rift_pos_to_lsp(pos, text, encoding);

            let params = lsp_types::GotoDefinitionParams {
                text_document_position_params: lsp_types::TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::parse("file:///tmp/test.rs").unwrap(),
                    },
                    position: lsp_pos,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };

            let result = socket
                .definition(params)
                .await
                .expect("definition succeeded");
            let locs = match result {
                Some(resp) => rift_lsp::nav::definition_response_to_protocol(resp, &root, encoding),
                None => vec![],
            };
            assert!(
                locs.is_empty(),
                "unknown position must return empty locations"
            );

            Ok(())
        })
    })
    .await
    .expect("test passed");
}

#[tokio::test]
async fn test_references_known_position_returns_location() {
    let tmp = TempDir::new("refs-known");
    let root = tmp.path.clone();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/target.rs"), "fn bar() {}\n").unwrap();
    let encoding = PositionEncoding::Utf16;

    with_stub_server(full_caps(), root.clone(), move |mut socket, caps| {
        Box::pin(async move {
            assert!(has_references(&caps));
            let text = "line 0\nline 1\nfn foo() {}\n";
            let pos = known_rift_pos();
            let lsp_pos = rift_lsp::nav::rift_pos_to_lsp(pos, text, encoding);

            let params = lsp_types::ReferenceParams {
                text_document_position: lsp_types::TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::parse("file:///tmp/test.rs").unwrap(),
                    },
                    position: lsp_pos,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: lsp_types::ReferenceContext {
                    include_declaration: true,
                },
            };

            let result = socket
                .references(params)
                .await
                .expect("references succeeded");
            let locs = match result {
                Some(raw) => rift_lsp::nav::references_to_protocol(raw, &root, encoding),
                None => vec![],
            };
            assert_eq!(locs.len(), 1, "known position must return one reference");
            assert!(
                locs[0].path.ends_with("src/target.rs"),
                "path: {}",
                locs[0].path
            );

            Ok(())
        })
    })
    .await
    .expect("test passed");
}

#[tokio::test]
async fn test_references_unknown_position_returns_empty() {
    let tmp = TempDir::new("refs-unknown");
    let root = tmp.path.clone();
    let encoding = PositionEncoding::Utf16;

    with_stub_server(full_caps(), root.clone(), move |mut socket, _caps| {
        Box::pin(async move {
            let text = "line 0\n";
            let pos = unknown_rift_pos();
            let lsp_pos = rift_lsp::nav::rift_pos_to_lsp(pos, text, encoding);

            let params = lsp_types::ReferenceParams {
                text_document_position: lsp_types::TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::parse("file:///tmp/test.rs").unwrap(),
                    },
                    position: lsp_pos,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: lsp_types::ReferenceContext {
                    include_declaration: true,
                },
            };

            let result = socket
                .references(params)
                .await
                .expect("references succeeded");
            let locs = match result {
                Some(raw) => rift_lsp::nav::references_to_protocol(raw, &root, encoding),
                None => vec![],
            };
            assert!(locs.is_empty(), "unknown position must return empty refs");

            Ok(())
        })
    })
    .await
    .expect("test passed");
}

/// Capability check: a server with no nav caps must advertise nothing.
#[tokio::test]
async fn test_capability_check_no_nav_caps() {
    let caps = no_nav_caps();
    assert!(!has_hover(&caps), "no_nav_caps must not advertise hover");
    assert!(
        !has_definition(&caps),
        "no_nav_caps must not advertise definition"
    );
    assert!(
        !has_references(&caps),
        "no_nav_caps must not advertise references"
    );
}

/// Offset-encoding integration: a UTF-16 server reporting a range on a
/// multi-byte line (`ä`, CJK, astral-plane `𝄞`). `lsp_range_to_rift` must
/// translate CUs → UTF-8 char offsets correctly.
#[tokio::test]
async fn test_offset_encoding_multibyte_in_location_range() {
    // Line: "äX中Y𝄞Z!" — Z is at UTF-16 CU 6 but UTF-8 char index 5.
    let text = "äX中Y\u{1D11E}Z!";
    let encoding = PositionEncoding::Utf16;

    // Server reports Z..! as the range (UTF-16 CUs 6..7).
    let lsp_range = LspRange {
        start: LspPosition {
            line: 0,
            character: 6,
        },
        end: LspPosition {
            line: 0,
            character: 7,
        },
    };

    let rift_range = lsp_range_to_rift(lsp_range, text, encoding);

    // After translation: start=char 5 (Z), end=char 6 (!).
    assert_eq!(
        rift_range.start.character, 5,
        "UTF-16 CU 6 must translate to UTF-8 char 5 (Z)"
    );
    assert_eq!(
        rift_range.end.character, 6,
        "UTF-16 CU 7 must translate to UTF-8 char 6 (!)"
    );
}
