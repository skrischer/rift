//! The built-in language → server table and the document selector that maps a
//! file to the languages whose servers should diagnose it.
//!
//! The table is *data*, not code: adding a language is a [`ServerSpec`] entry in
//! [`BUILTIN_SERVERS`], never a new code path. A [`DocumentSelector`] decides
//! which servers a given path matches; in v1 that decision is by file
//! extension, the minimal selector the spec needs (`docs/spec-daemon-lsp.md` —
//! single root, no per-project config). The richer LSP `DocumentSelector`
//! (language / scheme / glob patterns) is a later refinement, gated behind a
//! real need rather than built eagerly (constitution: no premature abstraction).

use std::path::Path;

/// A language identifier as the LSP wire uses it (the `languageId` field of a
/// `TextDocumentItem`, e.g. `"rust"`). Also the registry key under which the
/// servers for that language are grouped.
pub type LanguageId = &'static str;

/// The binary name of a language server as resolved on the daemon's `$PATH`,
/// e.g. `"rust-analyzer"`. rift never installs servers — it consumes whatever is
/// already on the remote's `$PATH`, mirroring how it consumes already-installed
/// agents (`docs/spec-daemon-lsp.md`).
pub type ServerName = &'static str;

/// One entry in the built-in language → server table.
///
/// Several specs may target the same [`language`](ServerSpec::language) — that
/// is exactly the multi-server-per-language case (a linter plus a type-checker),
/// which the [`Registry`](crate::Registry) keys by language so both coexist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerSpec {
    /// The language this server diagnoses, as the LSP `languageId`.
    pub language: LanguageId,
    /// The server binary, resolved on `$PATH` at spawn time.
    pub binary: ServerName,
    /// Arguments passed to the server binary on launch. Empty for servers (like
    /// rust-analyzer) that need none.
    pub args: &'static [&'static str],
    /// File extensions (without the leading dot) this server's language owns.
    /// A path matches the server when its extension is in this list.
    pub extensions: &'static [&'static str],
}

/// The built-in language → server table.
///
/// Data, not code: rust-analyzer is the proving server (`docs/spec-daemon-lsp.md`,
/// the #173 spike). Further languages are added as rows here, requiring no
/// change to the registry or lifecycle logic.
pub const BUILTIN_SERVERS: &[ServerSpec] = &[ServerSpec {
    language: "rust",
    binary: "rust-analyzer",
    args: &[],
    extensions: &["rs"],
}];

/// Maps a filesystem path to the servers that should diagnose it.
///
/// v1 matches purely by file extension against [`BUILTIN_SERVERS`]; the type is
/// the seam where a richer selector (globs, multiple tables, per-project config)
/// would later slot in without the registry needing to know how matching works.
#[derive(Debug, Clone, Copy)]
pub struct DocumentSelector {
    table: &'static [ServerSpec],
}

impl DocumentSelector {
    /// A selector backed by the built-in table.
    pub const fn builtin() -> Self {
        Self {
            table: BUILTIN_SERVERS,
        }
    }

    /// A selector backed by a caller-supplied table — used by tests to drive
    /// the registry with stub servers without touching the built-in defaults.
    pub const fn with_table(table: &'static [ServerSpec]) -> Self {
        Self { table }
    }

    /// The server specs whose language owns `path`'s extension.
    ///
    /// Returns every matching spec, so a path owned by two servers of the same
    /// language (linter + type-checker) yields both — the multi-server case the
    /// registry then starts and addresses independently. An unmatched path
    /// (no known extension, or none at all) yields an empty iterator and drives
    /// no server.
    pub fn matching(&self, path: &Path) -> impl Iterator<Item = &'static ServerSpec> + '_ {
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
        self.table.iter().filter(move |spec| match ext.as_deref() {
            Some(ext) => spec.extensions.contains(&ext),
            None => false,
        })
    }
}

impl Default for DocumentSelector {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matching_rust_extension_yields_rust_analyzer() {
        let selector = DocumentSelector::builtin();
        let matches: Vec<_> = selector.matching(Path::new("src/main.rs")).collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].language, "rust");
        assert_eq!(matches[0].binary, "rust-analyzer");
    }

    #[test]
    fn test_matching_unknown_extension_yields_nothing() {
        let selector = DocumentSelector::builtin();
        let matches: Vec<_> = selector.matching(Path::new("README.md")).collect();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_matching_extensionless_path_yields_nothing() {
        let selector = DocumentSelector::builtin();
        let matches: Vec<_> = selector.matching(Path::new("Makefile")).collect();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_matching_multiple_servers_same_language_yields_all() {
        const TABLE: &[ServerSpec] = &[
            ServerSpec {
                language: "rust",
                binary: "type-checker",
                args: &[],
                extensions: &["rs"],
            },
            ServerSpec {
                language: "rust",
                binary: "linter",
                args: &[],
                extensions: &["rs"],
            },
        ];
        let selector = DocumentSelector::with_table(TABLE);
        let matches: Vec<_> = selector.matching(Path::new("lib.rs")).collect();
        assert_eq!(matches.len(), 2);
        let binaries: Vec<_> = matches.iter().map(|s| s.binary).collect();
        assert!(binaries.contains(&"type-checker"));
        assert!(binaries.contains(&"linter"));
    }
}
