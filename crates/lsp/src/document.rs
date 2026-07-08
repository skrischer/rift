//! Document sync (issue #175; live-buffer feed #189).
//!
//! Turns worktree change events into LSP `didOpen` / `didChange` / `didClose`
//! actions. By default each document's full text is read from disk so
//! diagnostics reflect the on-disk state. Sync is full-text
//! (`TextDocumentSyncKind::Full`): there are no incremental deltas to send — the
//! daemon only ever has the whole new file.
//!
//! v1 `didOpen` breadth is the *observed / changed* file set: the first time a
//! matching file is seen as created or modified it is opened; later
//! modifications drive `didChange`; removal drives `didClose`. There is no
//! eager whole-tree open (spec prior decision).
//!
//! ## Live-buffer feed (the disk→buffer source-of-truth shift, #189)
//!
//! Once rift's own editor opens a file, its **live buffer** becomes the LSP's
//! source of truth for that path (`spec-editor.md` cut C, executing the forward
//! note the LSP spec reserved). [`DocumentSync::apply_buffer_change`] feeds the
//! buffer's text as a `didChange`, and while a path has a live buffer the
//! disk-driven path for it is **suppressed** ([`DocumentSync::apply`] no-ops a
//! disk modify for a live path) so an agent's on-disk write cannot clobber the
//! buffer's diagnostics. [`DocumentSync::apply_buffer_close`] ends the override
//! and reverts the path to the disk-backed baseline (re-reading disk and pushing
//! it as a `didChange`). This is bounded to the open file(s) and consumes the
//! existing disk-backed model — it does not redesign it.
//!
//! This module is deliberately self-contained: it consumes a minimal
//! [`DocumentChange`] stream and emits actions through a small [`DocumentSink`]
//! abstraction, so it is unit-testable without a real language server. Mapping
//! the explorer's worktree change stream onto [`DocumentChange`] and wiring a
//! concrete server registry as the sink is the daemon's job (issue #177); this
//! module knows nothing of either.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, Url, VersionedTextDocumentIdentifier,
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
    /// Notify that the document was saved (`textDocument/didSave`). The
    /// disk-backed model treats every observed write as a save, so this fires
    /// after each open/change to make save-triggered server checks re-run.
    fn did_save(&mut self, params: DidSaveTextDocumentParams) -> Result<()>;
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
    /// Paths whose source of truth is currently the editor's live buffer, not
    /// disk (the disk→buffer shift, #189). While a path is here the disk-driven
    /// `apply` no-ops a modify for it; `apply_buffer_close` removes it and reverts
    /// to disk.
    live: HashSet<PathBuf>,
}

impl DocumentSync {
    /// Create a sync rooted at the watched worktree root. `root` is the same
    /// directory the explorer snapshot was scanned from; change paths are
    /// resolved relative to it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            open: HashMap::new(),
            live: HashSet::new(),
        }
    }

    /// Whether the document at `relative` is currently open.
    pub fn is_open(&self, relative: &Path) -> bool {
        self.open.contains_key(relative)
    }

    /// Whether `relative`'s source of truth is currently the editor's live buffer
    /// (a buffer feed is active for it).
    pub fn is_live(&self, relative: &Path) -> bool {
        self.live.contains(relative)
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
    /// - A `Created` / `Modified` for a path with a **live buffer** is a no-op:
    ///   the editor's buffer is the source of truth, so an agent's on-disk write
    ///   must not clobber it (#189). The buffer feed drives `didChange` instead.
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
                // The live buffer owns this path: disk modifications are
                // suppressed so the buffer's diagnostics are not overwritten.
                if self.live.contains(path) {
                    return Ok(None);
                }
                let text = self.read_disk(path)?;
                self.upsert_with_text(path, text, sink)
            }
            DocumentChange::Removed { path } => self.apply_remove(path, sink),
        }
    }

    /// Feed the editor's live buffer for `relative` (#189): the disk→buffer
    /// source-of-truth shift. Marks the path live (so disk modifications for it
    /// are thereafter suppressed) and drives a `didOpen` on first observation or a
    /// full-text `didChange` once open — same as the disk path, but the text comes
    /// from the buffer, not disk. An unrecognized-language path is a no-op.
    pub fn apply_buffer_change<S: DocumentSink>(
        &mut self,
        relative: &Path,
        content: String,
        sink: &mut S,
    ) -> Result<Option<DocumentAction>> {
        if language_id_for(relative).is_none() {
            return Ok(None);
        }
        self.live.insert(relative.to_path_buf());
        self.upsert_with_text(relative, content, sink)
    }

    /// End the live-buffer feed for `relative` (#189): revert the path to the
    /// disk-backed baseline. Clears the live flag and, when the document is open
    /// and still on disk, re-reads disk and pushes it as a `didChange` so
    /// diagnostics converge back to the on-disk state. A path with no active
    /// buffer is a no-op; a buffer whose file no longer exists on disk leaves the
    /// last buffer state in place (the next worktree change reconciles).
    pub fn apply_buffer_close<S: DocumentSink>(
        &mut self,
        relative: &Path,
        sink: &mut S,
    ) -> Result<Option<DocumentAction>> {
        if !self.live.remove(relative) {
            return Ok(None);
        }
        // Only re-sync from disk if the document is open and the file is readable;
        // a removed file is left to the next worktree change to close.
        if !self.open.contains_key(relative) {
            return Ok(None);
        }
        match self.read_disk(relative) {
            Ok(text) => self.upsert_with_text(relative, text, sink),
            // The file is gone (or unreadable): keep the last buffer state; the
            // next worktree `Removed` / `Modified` reconciles it.
            Err(_) => Ok(None),
        }
    }

    /// Read `relative`'s full UTF-8 text from disk, mapping an I/O failure to
    /// [`LspError::ReadDocument`].
    fn read_disk(&self, relative: &Path) -> Result<String> {
        let absolute = self.root.join(relative);
        std::fs::read_to_string(&absolute).map_err(|source| LspError::ReadDocument {
            path: absolute.display().to_string(),
            source,
        })
    }

    /// Open `relative` on first observation, else push a full-text `didChange`,
    /// using `text` as the document content whatever its source (disk or live
    /// buffer). Emits a trailing `didSave` so save-triggered server checks re-run
    /// (#272). The shared core of the disk path and the live-buffer feed.
    fn upsert_with_text<S: DocumentSink>(
        &mut self,
        relative: &Path,
        text: String,
        sink: &mut S,
    ) -> Result<Option<DocumentAction>> {
        let Some(language_id) = language_id_for(relative) else {
            return Ok(None);
        };
        let absolute = self.root.join(relative);
        let uri = Url::from_file_path(&absolute)
            .map_err(|()| LspError::InvalidUri(absolute.display().to_string()))?;

        let action = match self.open.get_mut(relative) {
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
                DocumentAction::Change {
                    uri: uri.clone(),
                    version,
                    text,
                }
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
                DocumentAction::Open {
                    uri: uri.clone(),
                    language_id: language_id.to_owned(),
                    version,
                    text,
                }
            }
        };

        // Emit didSave after each open/change so servers whose checks only run on
        // save (rust-analyzer's `checkOnSave` / cargo check) re-run and clear
        // stale diagnostics on a fix — without it a corrected file's check-sourced
        // diagnostics never refresh (issue #272). The disk path treats every
        // observed write as a save; the live-buffer feed (#189) does the same so
        // its diagnostics stay as fresh as the disk path's. The server already has
        // the text from the open/change, so the save carries none.
        sink.did_save(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text: None,
        })?;
        Ok(Some(action))
    }

    /// Handle a removal: close an open document, ignore a never-opened one. A
    /// removed path also drops any live-buffer override for it — the file is gone,
    /// so the buffer feed can no longer be the source of truth.
    fn apply_remove<S: DocumentSink>(
        &mut self,
        relative: &Path,
        sink: &mut S,
    ) -> Result<Option<DocumentAction>> {
        self.live.remove(relative);
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
        saved: Vec<Url>,
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

        fn did_save(&mut self, params: DidSaveTextDocumentParams) -> Result<()> {
            self.saved.push(params.text_document.uri);
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
        assert_eq!(sink.saved.len(), 1, "a disk-backed open is also a save");
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
        // A fix is a disk write, so each open/change saves: the save after the
        // change is what makes a server's checkOnSave re-run and clear stale
        // check-sourced diagnostics (issue #272).
        assert_eq!(sink.saved.len(), 2, "open and change each emit a didSave");
        assert_eq!(
            sink.saved[1], sink.changed[0].0,
            "save targets the changed doc"
        );
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
        assert_eq!(sink.saved.len(), 1, "the open saved; the close does not");
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
        assert_eq!(
            sink.saved.len(),
            3,
            "each open/change is a save; close does not save"
        );
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

    // --- live-buffer feed (#189): the disk→buffer source-of-truth shift ---

    #[test]
    fn test_buffer_change_opens_with_buffer_text_not_disk() {
        // The first buffer feed for a never-opened file opens it with the buffer's
        // content — the LSP's source of truth is the buffer, not the on-disk text.
        let tmp = TempDir::new("buf-open");
        write_file(&tmp.path, "main.rs", "fn main() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let action = sync
            .apply_buffer_change(
                Path::new("main.rs"),
                "fn main() { let x: u32 = \"oops\"; }".to_owned(),
                &mut sink,
            )
            .expect("buffer change opens");

        assert!(matches!(action, Some(DocumentAction::Open { .. })));
        assert_eq!(sink.opened.len(), 1);
        let (_, _, text) = &sink.opened[0];
        assert_eq!(text, "fn main() { let x: u32 = \"oops\"; }");
        assert!(sync.is_live(Path::new("main.rs")));
        assert!(sync.is_open(Path::new("main.rs")));
        assert_eq!(sink.saved.len(), 1, "a buffer change is also a save");
    }

    #[test]
    fn test_unsaved_buffer_edit_surfaces_change_without_a_disk_write() {
        // The core acceptance: an edit to an open buffer drives a didChange from
        // the buffer text while the on-disk file is untouched (still the original).
        let tmp = TempDir::new("buf-unsaved");
        write_file(&tmp.path, "lib.rs", "pub fn ok() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        // Open via the disk path (an existing file), then edit the live buffer.
        sync.apply(
            &DocumentChange::Created {
                path: PathBuf::from("lib.rs"),
            },
            &mut sink,
        )
        .expect("disk open");

        let action = sync
            .apply_buffer_change(Path::new("lib.rs"), "pub fn ok( {}".to_owned(), &mut sink)
            .expect("buffer change");

        assert!(matches!(action, Some(DocumentAction::Change { .. })));
        let (_, _, text) = sink.changed.last().expect("a change was pushed");
        assert_eq!(
            text, "pub fn ok( {}",
            "the server sees the buffer, not disk"
        );
        // The on-disk file is unchanged — the feed wrote nothing to disk.
        assert_eq!(
            std::fs::read_to_string(tmp.path.join("lib.rs")).unwrap(),
            "pub fn ok() {}"
        );
    }

    #[test]
    fn test_disk_modify_is_suppressed_while_buffer_is_live() {
        // While a buffer is live, an agent's on-disk write for the same path is a
        // no-op: the buffer owns the LSP's source of truth, so the agent's edit
        // cannot clobber the buffer's diagnostics.
        let tmp = TempDir::new("buf-suppress");
        write_file(&tmp.path, "a.rs", "fn a() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        sync.apply_buffer_change(Path::new("a.rs"), "fn a( {}".to_owned(), &mut sink)
            .expect("buffer change opens");
        let changes_before = sink.changed.len();

        // An agent rewrites the file on disk; the worktree surfaces a Modified.
        write_file(&tmp.path, "a.rs", "fn a() { agent_edit(); }");
        let action = sync
            .apply(
                &DocumentChange::Modified {
                    path: PathBuf::from("a.rs"),
                },
                &mut sink,
            )
            .expect("disk modify is suppressed");

        assert_eq!(action, None, "a live path's disk modify is a no-op");
        assert_eq!(
            sink.changed.len(),
            changes_before,
            "no didChange from the disk write"
        );
    }

    #[test]
    fn test_buffer_close_reverts_to_disk_baseline() {
        // Closing the buffer ends the override and pushes the on-disk content as a
        // didChange, so diagnostics converge back to the disk state (the saved /
        // agent-written version).
        let tmp = TempDir::new("buf-close");
        write_file(&tmp.path, "x.rs", "fn x() {}");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        sync.apply_buffer_change(Path::new("x.rs"), "fn x( {}".to_owned(), &mut sink)
            .expect("buffer open");
        assert!(sync.is_live(Path::new("x.rs")));

        // The disk now holds a different (valid) version — e.g. the save landed.
        write_file(&tmp.path, "x.rs", "fn x() { ok(); }");
        let action = sync
            .apply_buffer_close(Path::new("x.rs"), &mut sink)
            .expect("buffer close");

        assert!(matches!(action, Some(DocumentAction::Change { .. })));
        let (_, _, text) = sink.changed.last().expect("a change was pushed");
        assert_eq!(text, "fn x() { ok(); }", "reverted to the on-disk content");
        assert!(!sync.is_live(Path::new("x.rs")));

        // And a later disk modify is no longer suppressed.
        write_file(&tmp.path, "x.rs", "fn x() { ok(); more(); }");
        let action = sync
            .apply(
                &DocumentChange::Modified {
                    path: PathBuf::from("x.rs"),
                },
                &mut sink,
            )
            .expect("disk modify after close");
        assert!(matches!(action, Some(DocumentAction::Change { .. })));
    }

    #[test]
    fn test_buffer_close_for_inactive_path_is_a_noop() {
        let tmp = TempDir::new("buf-close-noop");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let action = sync
            .apply_buffer_close(Path::new("never.rs"), &mut sink)
            .expect("close with no live buffer");
        assert_eq!(action, None);
        assert!(sink.changed.is_empty());
    }

    #[test]
    fn test_buffer_change_for_unrecognized_extension_is_a_noop() {
        let tmp = TempDir::new("buf-unknown");
        let mut sync = DocumentSync::new(&tmp.path);
        let mut sink = RecordingSink::default();

        let action = sync
            .apply_buffer_change(Path::new("notes.txt"), "hello".to_owned(), &mut sink)
            .expect("apply");
        assert_eq!(action, None);
        assert!(!sync.is_live(Path::new("notes.txt")));
        assert!(sink.opened.is_empty());
    }
}
