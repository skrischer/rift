//! Disk-backed document sync (issue #175).
//!
//! Turns worktree change events into LSP `didOpen` / `didChange` / `didClose`
//! actions, reading each document's full text from disk so diagnostics reflect
//! the on-disk state. Sync is full-text (`TextDocumentSyncKind::Full`): rift
//! owns no editor buffers and the daemon only ever has the whole new file from
//! disk, so there are no incremental deltas to send (spec: disk-backed
//! full-text document model).
//!
//! v1 `didOpen` breadth is the *observed / changed* file set: the first time a
//! matching file is seen as created or modified it is opened; later
//! modifications drive `didChange`; removal drives `didClose`. There is no
//! eager whole-tree open (spec prior decision).
//!
//! This module is deliberately self-contained: it consumes a minimal
//! [`DocumentChange`] stream and emits actions through a small [`DocumentSink`]
//! abstraction, so it is unit-testable without a real language server. Mapping
//! the explorer's worktree change stream onto [`DocumentChange`] and wiring a
//! concrete server registry as the sink is the daemon's job (issue #177); this
//! module knows nothing of either.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, Url,
    VersionedTextDocumentIdentifier,
};

use crate::{LspError, Result};

/// A worktree change relevant to document sync, keyed by a path relative to the
/// sync root.
///
/// This is the explorer's file-change vocabulary (`Added` / `Changed` /
/// `Removed`) reduced to the file events document sync acts on — directory
/// entries carry no document. The daemon maps `explorer::Change` onto this so
/// `crates/lsp` need not depend on `crates/explorer`. Because the explorer
/// stream already excludes ignored paths (`target/`, `.git/`, `.gitignore`d
/// paths are absent from its snapshot), a change inside an ignored path never
/// reaches this type, so no document is ever opened for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentChange {
    /// A file was created — its first observation in the session.
    Created { path: PathBuf },
    /// A file's content changed on disk.
    Modified { path: PathBuf },
    /// A file was removed from the worktree.
    Removed { path: PathBuf },
}

impl DocumentChange {
    /// The worktree-relative path this change concerns.
    pub fn path(&self) -> &Path {
        match self {
            Self::Created { path } | Self::Modified { path } | Self::Removed { path } => path,
        }
    }
}

/// A single sync action [`DocumentSync`] decides for a change, addressed by the
/// document's `file://` URI.
///
/// Surfaced for inspection / testing; [`DocumentSync::apply`] dispatches these
/// to a [`DocumentSink`] directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentAction {
    /// First observation of a matching file: open it with its full disk text.
    Open {
        uri: Url,
        language_id: String,
        version: i32,
        text: String,
    },
    /// A modification to an already-open document: full-text replace.
    Change {
        uri: Url,
        version: i32,
        text: String,
    },
    /// Removal of an open document.
    Close { uri: Url },
}

/// The minimal abstraction document sync emits through.
///
/// A real implementation forwards each call as the matching LSP notification to
/// the language server(s) registered for the document; tests use a recording
/// stub. Kept intentionally tiny so this module is unit-testable without a
/// server, and so the concrete registry wiring stays the daemon's concern
/// (issue #177).
pub trait DocumentSink {
    /// Open a document on the relevant server(s) (`textDocument/didOpen`).
    fn did_open(&mut self, params: DidOpenTextDocumentParams) -> Result<()>;
    /// Push a full-text replacement (`textDocument/didChange`).
    fn did_change(&mut self, params: DidChangeTextDocumentParams) -> Result<()>;
    /// Close a document (`textDocument/didClose`).
    fn did_close(&mut self, params: DidCloseTextDocumentParams) -> Result<()>;
}

/// State of a document this sync has opened: its URI and the last version sent.
struct OpenDocument {
    uri: Url,
    version: i32,
}

/// Drives disk-backed `didOpen` / `didChange` / `didClose` from a worktree
/// change stream against a [`DocumentSink`].
///
/// Tracks which files are currently open so a `Created` / `Modified` change
/// becomes `didOpen` on first observation and `didChange` afterwards, and
/// `didClose` only fires for a document that was actually open. Paths are
/// resolved against `root`; only files with a recognized language
/// ([`language_id_for`]) ever open a document.
pub struct DocumentSync {
    root: PathBuf,
    open: HashMap<PathBuf, OpenDocument>,
}

impl DocumentSync {
    /// Create a sync rooted at the watched worktree root. `root` is the same
    /// directory the explorer snapshot was scanned from; change paths are
    /// resolved relative to it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            open: HashMap::new(),
        }
    }

    /// Whether the document at `relative` is currently open.
    pub fn is_open(&self, relative: &Path) -> bool {
        self.open.contains_key(relative)
    }

    /// Number of currently-open documents.
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Decide and dispatch the sync action for one worktree change, reading the
    /// file's full text from disk when an open or change is warranted.
    ///
    /// - `Created` / `Modified` for a recognized-language file: `didOpen` on
    ///   first observation, `didChange` (full text) once already open.
    /// - `Removed` for a currently-open file: `didClose`.
    /// - A file whose language is unrecognized, or a removal of a file that was
    ///   never opened, is a no-op (`Ok(None)`).
    ///
    /// Returns the action taken so callers (and tests) can observe it. An
    /// unreadable file (e.g. removed between the change event and the read)
    /// yields [`LspError::ReadDocument`]; the document is left in its prior
    /// open/closed state so a later change can retry.
    pub fn apply<S: DocumentSink>(
        &mut self,
        change: &DocumentChange,
        sink: &mut S,
    ) -> Result<Option<DocumentAction>> {
        match change {
            DocumentChange::Created { path } | DocumentChange::Modified { path } => {
                self.apply_upsert(path, sink)
            }
            DocumentChange::Removed { path } => self.apply_remove(path, sink),
        }
    }

    /// Handle a create / modify: open on first observation, else full-text change.
    fn apply_upsert<S: DocumentSink>(
        &mut self,
        relative: &Path,
        sink: &mut S,
    ) -> Result<Option<DocumentAction>> {
        let Some(language_id) = language_id_for(relative) else {
            return Ok(None);
        };
        let absolute = self.root.join(relative);
        let uri = Url::from_file_path(&absolute)
            .map_err(|()| LspError::InvalidUri(absolute.display().to_string()))?;
        let text = std::fs::read_to_string(&absolute).map_err(|source| LspError::ReadDocument {
            path: absolute.display().to_string(),
            source,
        })?;

        match self.open.get_mut(relative) {
            Some(doc) => {
                doc.version += 1;
                let version = doc.version;
                sink.did_change(DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier {
                        uri: uri.clone(),
                        version,
                    },
                    content_changes: vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: text.clone(),
                    }],
                })?;
                Ok(Some(DocumentAction::Change { uri, version, text }))
            }
            None => {
                let version = 0;
                sink.did_open(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: uri.clone(),
                        language_id: language_id.to_owned(),
                        version,
                        text: text.clone(),
                    },
                })?;
                self.open.insert(
                    relative.to_path_buf(),
                    OpenDocument {
                        uri: uri.clone(),
                        version,
                    },
                );
                Ok(Some(DocumentAction::Open {
                    uri,
                    language_id: language_id.to_owned(),
                    version,
                    text,
                }))
            }
        }
    }

    /// Handle a removal: close an open document, ignore a never-opened one.
    fn apply_remove<S: DocumentSink>(
        &mut self,
        relative: &Path,
        sink: &mut S,
    ) -> Result<Option<DocumentAction>> {
        let Some(doc) = self.open.remove(relative) else {
            return Ok(None);
        };
        sink.did_close(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: doc.uri.clone(),
            },
        })?;
        Ok(Some(DocumentAction::Close { uri: doc.uri }))
    }
}

/// Resolve a file's LSP `language_id` from its extension, or `None` for an
/// extension with no known language server mapping (no document is opened).
///
/// A data table, not code: adding a language is a single arm. The set is the
/// languages whose servers rift's spec names plus the common companions a
/// single project tends to carry; an unrecognized extension never drives a
/// server. Language ids follow the LSP specification's identifier list.
pub fn language_id_for(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    let language_id = match extension {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "json" => "json",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        _ => return None,
    };
    Some(language_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A self-cleaning temporary directory, mirroring the explorer tests' helper
    /// so these stay self-contained without a `tempfile` dev-dependency.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("rift-docsync-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            // Canonicalize so the URIs the sync builds match the temp root the
            // OS reports (macOS / some setups symlink the temp dir).
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

    /// A recording [`DocumentSink`] that captures every notification it receives,
    /// standing in for a real language server in unit tests.
    #[derive(Default)]
    struct RecordingSink {
        opened: Vec<(Url, String, String)>,
        changed: Vec<(Url, i32, String)>,
        closed: Vec<Url>,
    }

    impl DocumentSink for RecordingSink {
        fn did_open(&mut self, params: DidOpenTextDocumentParams) -> Result<()> {
            let doc = params.text_document;
            self.opened.push((doc.uri, doc.language_id, doc.text));
            Ok(())
        }

        fn did_change(&mut self, params: DidChangeTextDocumentParams) -> Result<()> {
            let text = params
                .content_changes
                .into_iter()
                .next()
                .map(|c| c.text)
                .unwrap_or_default();
            self.changed
                .push((params.text_document.uri, params.text_document.version, text));
            Ok(())
        }

        fn did_close(&mut self, params: DidCloseTextDocumentParams) -> Result<()> {
            self.closed.push(params.text_document.uri);
            Ok(())
        }
    }

    #[test]
    fn test_created_recognized_file_opens_with_full_disk_text() {
        let tmp = TempDir::new("open");
        write_file(&tmp.path, "src/main.rs", "fn main() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let action = sync
            .apply(
                &DocumentChange::Created {
                    path: PathBuf::from("src/main.rs"),
                },
                &mut sink,
            )
            .expect("apply create");

        assert!(matches!(action, Some(DocumentAction::Open { .. })));
        assert_eq!(sink.opened.len(), 1);
        let (uri, language_id, text) = &sink.opened[0];
        assert_eq!(language_id, "rust");
        assert_eq!(text, "fn main() {}");
        assert_eq!(uri.to_file_path().unwrap(), tmp.path.join("src/main.rs"));
        assert!(sync.is_open(Path::new("src/main.rs")));
    }

    #[test]
    fn test_modify_after_open_drives_full_text_change_not_reopen() {
        let tmp = TempDir::new("change");
        write_file(&tmp.path, "lib.rs", "v1");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        sync.apply(
            &DocumentChange::Created {
                path: PathBuf::from("lib.rs"),
            },
            &mut sink,
        )
        .expect("open");

        // The agent rewrites the file; document sync re-reads disk in full.
        write_file(&tmp.path, "lib.rs", "v2 changed");
        let action = sync
            .apply(
                &DocumentChange::Modified {
                    path: PathBuf::from("lib.rs"),
                },
                &mut sink,
            )
            .expect("change");

        assert!(matches!(action, Some(DocumentAction::Change { .. })));
        assert_eq!(sink.opened.len(), 1, "must not re-open an already-open doc");
        assert_eq!(sink.changed.len(), 1);
        let (_, version, text) = &sink.changed[0];
        assert_eq!(text, "v2 changed", "full-text re-read from disk");
        assert_eq!(*version, 1, "version increments past the didOpen version 0");
    }

    #[test]
    fn test_first_observation_via_modified_opens_the_document() {
        // The explorer may surface a file's first observation as a Changed delta
        // (e.g. an mtime bump on an existing-but-never-synced file); document
        // sync must still open it rather than skipping straight to didChange.
        let tmp = TempDir::new("modify-first");
        write_file(&tmp.path, "main.rs", "fn main() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let action = sync
            .apply(
                &DocumentChange::Modified {
                    path: PathBuf::from("main.rs"),
                },
                &mut sink,
            )
            .expect("apply");

        assert!(matches!(action, Some(DocumentAction::Open { .. })));
        assert_eq!(sink.opened.len(), 1);
        assert!(sink.changed.is_empty());
    }

    #[test]
    fn test_removed_open_file_closes_the_document() {
        let tmp = TempDir::new("close");
        write_file(&tmp.path, "gone.rs", "fn x() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        sync.apply(
            &DocumentChange::Created {
                path: PathBuf::from("gone.rs"),
            },
            &mut sink,
        )
        .expect("open");
        std::fs::remove_file(tmp.path.join("gone.rs")).expect("remove");

        let action = sync
            .apply(
                &DocumentChange::Removed {
                    path: PathBuf::from("gone.rs"),
                },
                &mut sink,
            )
            .expect("close");

        assert!(matches!(action, Some(DocumentAction::Close { .. })));
        assert_eq!(sink.closed.len(), 1);
        assert!(!sync.is_open(Path::new("gone.rs")));
    }

    #[test]
    fn test_open_change_close_sequence_keeps_consistent_state() {
        let tmp = TempDir::new("sequence");
        write_file(&tmp.path, "a.rs", "1");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let created = DocumentChange::Created {
            path: PathBuf::from("a.rs"),
        };
        let modified = DocumentChange::Modified {
            path: PathBuf::from("a.rs"),
        };
        let removed = DocumentChange::Removed {
            path: PathBuf::from("a.rs"),
        };

        sync.apply(&created, &mut sink).expect("open");
        write_file(&tmp.path, "a.rs", "2");
        sync.apply(&modified, &mut sink).expect("change");
        write_file(&tmp.path, "a.rs", "3");
        sync.apply(&modified, &mut sink).expect("change again");
        std::fs::remove_file(tmp.path.join("a.rs")).expect("remove");
        sync.apply(&removed, &mut sink).expect("close");

        assert_eq!(sink.opened.len(), 1);
        assert_eq!(sink.changed.len(), 2);
        assert_eq!(sink.closed.len(), 1);
        assert_eq!(sync.open_count(), 0);
        // Versions are monotonic: open=0, change=1, change=2.
        assert_eq!(sink.changed[0].1, 1);
        assert_eq!(sink.changed[1].1, 2);
    }

    #[test]
    fn test_unrecognized_extension_opens_no_document() {
        let tmp = TempDir::new("unknown-ext");
        write_file(&tmp.path, "image.png", "not source");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let action = sync
            .apply(
                &DocumentChange::Created {
                    path: PathBuf::from("image.png"),
                },
                &mut sink,
            )
            .expect("apply");

        assert_eq!(action, None);
        assert!(sink.opened.is_empty());
        assert!(!sync.is_open(Path::new("image.png")));
    }

    #[test]
    fn test_removed_never_opened_file_is_a_noop() {
        let tmp = TempDir::new("remove-unopened");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let action = sync
            .apply(
                &DocumentChange::Removed {
                    path: PathBuf::from("never.rs"),
                },
                &mut sink,
            )
            .expect("apply");

        assert_eq!(action, None);
        assert!(sink.closed.is_empty());
    }

    #[test]
    fn test_only_changes_fed_to_sync_open_documents() {
        // Ignore-rule enforcement lives in the explorer snapshot, which excludes
        // target/, .git/, and .gitignore'd paths from the stream this module
        // consumes (verified by the explorer's own
        // test_watcher_excludes_writes_inside_ignored_dirs). Document sync has no
        // location-based special-casing — it opens exactly the files it is fed
        // and nothing else — so an ignored path the explorer never emits is never
        // opened. This pins that contract: a file present on disk but never
        // handed to `apply` stays closed.
        let tmp = TempDir::new("only-fed");
        write_file(&tmp.path, "target/debug/build.rs", "fn main() {}");
        write_file(&tmp.path, "src/lib.rs", "pub fn f() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        // Only the non-ignored file is fed, exactly as the explorer would.
        sync.apply(
            &DocumentChange::Created {
                path: PathBuf::from("src/lib.rs"),
            },
            &mut sink,
        )
        .expect("open tracked");

        assert_eq!(sink.opened.len(), 1);
        assert!(sync.is_open(Path::new("src/lib.rs")));
        assert!(
            !sync.is_open(Path::new("target/debug/build.rs")),
            "a path the explorer never emits is never opened"
        );
    }

    #[test]
    fn test_language_id_table_maps_known_extensions() {
        assert_eq!(language_id_for(Path::new("a.rs")), Some("rust"));
        assert_eq!(language_id_for(Path::new("a.ts")), Some("typescript"));
        assert_eq!(language_id_for(Path::new("a.tsx")), Some("typescriptreact"));
        assert_eq!(language_id_for(Path::new("a.py")), Some("python"));
        assert_eq!(language_id_for(Path::new("a.go")), Some("go"));
        assert_eq!(language_id_for(Path::new("a.cpp")), Some("cpp"));
        assert_eq!(language_id_for(Path::new("README.md")), Some("markdown"));
        assert_eq!(language_id_for(Path::new("Makefile")), None);
        assert_eq!(language_id_for(Path::new("a.unknownext")), None);
    }
}
