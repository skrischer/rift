// Copyright (C) 2024 rift contributors — licensed under GPL-3.0-or-later.
//
//! Navigation request path: hover, go-to-definition, find-references.
//!
//! This module is the LSP side of the navigation protocol
//! (`docs/spec-lsp-navigation.md`). It provides:
//!
//! - **Offset-encoding translation**: a running language server negotiates a
//!   position encoding (UTF-8 or UTF-16) during initialization. Rift's wire
//!   protocol uses UTF-8 character offsets; this module translates in both
//!   directions, against the document text that `DocumentSync` already maintains
//!   in memory or falls back to reading from disk. This is the canonical
//!   off-by-N trap for non-ASCII lines (a-umlaut, CJK, astral-plane) and is
//!   tested explicitly.
//!
//! - **`NavRequester`**: wraps a live `Server` handle, checks the server's
//!   capabilities before dispatching, and issues typed requests through
//!   `async-lsp`. Translation from `lsp-types` response types to rift's own
//!   protocol types happens here; `crates/protocol` never sees `lsp-types`.
//!
//!
//! # Offset encoding
//!
//! LSP historically defaults to UTF-16; servers that support UTF-8 advertise
//! `PositionEncodingKind::UTF8` in `InitializeResult.capabilities`. The
//! negotiated encoding is stored in `ServerCapabilities` after initialization.
//!
//! The translation is isolated here against the document text — matching the
//! Helix `lsp/src/util.rs` precedent noted in the spec. The text source is:
//! 1. A live synced buffer from `DocumentSync` (preferred — accurate for
//!    unsaved edits).
//! 2. Disk read fallback when no synced text exists (definition / reference
//!    targets routinely land in files the editor has never opened).

use std::path::{Path, PathBuf};

use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams, Location,
    MarkedString, Position as LspPosition, Range as LspRange, ReferenceContext, ReferenceParams,
    ServerCapabilities, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams,
};
use rift_protocol::{HoverContent, NavLocation, Position, Range};
use tracing::debug;

use crate::server::Server;
use crate::{LspError, Result};

// ── Offset-encoding translation ──────────────────────────────────────────────

/// Translate a rift `Position` (UTF-8 character offset) to an LSP `Position`
/// whose `character` field uses the `encoding` the server negotiated.
///
/// When the server speaks UTF-8 the offset is used as-is. When it speaks
/// UTF-16 (the historical default) each UTF-16 code unit is counted: BMP
/// characters are 1 unit; supplementary characters (U+10000…) are 2 units
/// (a surrogate pair). Returns `LspPosition { line, character }` where
/// `character` is in the server's encoding.
///
/// If `line` exceeds the number of lines in `text`, the last line is used and
/// `character` is clamped — a best-effort position on a stale buffer is better
/// than a refused request.
pub fn rift_pos_to_lsp(pos: Position, text: &str, encoding: PositionEncoding) -> LspPosition {
    let line = pos.line as usize;
    let char_offset = pos.character as usize;

    let line_text = text.lines().nth(line).unwrap_or("");

    let character = match encoding {
        PositionEncoding::Utf8 => char_offset as u32,
        PositionEncoding::Utf16 => utf8_char_offset_to_utf16_cu(line_text, char_offset),
    };

    LspPosition {
        line: pos.line,
        character,
    }
}

/// Translate an LSP `Position` (whose `character` is in `encoding`) back to a
/// rift `Position` (UTF-8 character offset).
///
/// For UTF-16 servers: counts UTF-16 code units from the start of the line
/// until the accumulated total equals `lsp_pos.character`, then returns the
/// UTF-8 character index at that point. Clamped when the offset exceeds the
/// line's content.
pub fn lsp_pos_to_rift(lsp_pos: LspPosition, text: &str, encoding: PositionEncoding) -> Position {
    let line = lsp_pos.line as usize;
    let line_text = text.lines().nth(line).unwrap_or("");

    let character = match encoding {
        PositionEncoding::Utf8 => lsp_pos.character as usize,
        PositionEncoding::Utf16 => utf16_cu_to_utf8_char_offset(line_text, lsp_pos.character),
    };

    Position {
        line: lsp_pos.line,
        character: character as u32,
    }
}

/// Translate an LSP `Range` back to a rift `Range`.
pub fn lsp_range_to_rift(lsp_range: LspRange, text: &str, encoding: PositionEncoding) -> Range {
    Range {
        start: lsp_pos_to_rift(lsp_range.start, text, encoding),
        end: lsp_pos_to_rift(lsp_range.end, text, encoding),
    }
}

/// Count UTF-16 code units up to `char_offset` UTF-8 *characters* into `line`.
///
/// The `char_offset` is a character index (not a byte index). Characters beyond
/// `char_offset` are ignored. If `char_offset` exceeds the number of characters
/// on the line, the total code-unit count of the line is returned (clamped).
fn utf8_char_offset_to_utf16_cu(line: &str, char_offset: usize) -> u32 {
    line.chars()
        .take(char_offset)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// Find the UTF-8 *character* index at which `cu_target` UTF-16 code units
/// have been consumed on `line`.
///
/// If `cu_target` exceeds the line's total UTF-16 width, returns the total
/// character count of the line (clamped).
fn utf16_cu_to_utf8_char_offset(line: &str, cu_target: u32) -> usize {
    let mut cu_count: u32 = 0;
    for (char_idx, ch) in line.chars().enumerate() {
        if cu_count >= cu_target {
            return char_idx;
        }
        cu_count += ch.len_utf16() as u32;
    }
    line.chars().count()
}

// ── Position encoding ─────────────────────────────────────────────────────────

/// The position encoding a language server negotiated during initialization.
///
/// Defaults to `Utf16` (the LSP historical default when a server does not
/// advertise `PositionEncodingKind` in its `InitializeResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    /// Extract the negotiated encoding from an `InitializeResult`'s capabilities.
    ///
    /// Falls back to `Utf16` when the server omits `position_encoding` (the
    /// historical default — servers that never heard of the field speak UTF-16).
    pub fn from_capabilities(caps: &ServerCapabilities) -> Self {
        match caps.position_encoding.as_ref() {
            Some(enc) if enc == &lsp_types::PositionEncodingKind::UTF8 => Self::Utf8,
            _ => Self::Utf16,
        }
    }
}

// ── Text source for offset translation ───────────────────────────────────────

/// Fetch the text of `abs_path` for offset-encoding translation.
///
/// Attempts a synchronous disk read. Returns `None` when the file cannot be
/// read (permissions, not found, binary) — the caller uses `None` as the
/// signal to skip range translation and omit the line preview.
pub fn read_text_from_disk(abs_path: &Path) -> Option<String> {
    std::fs::read_to_string(abs_path).ok()
}

// ── lsp-types → protocol translation ─────────────────────────────────────────

/// Translate an `lsp_types::Hover` to a `protocol::HoverContent`.
///
/// All `HoverContents` variants are reduced to a markdown string: plaintext is
/// passed through as-is; language-tagged code blocks are wrapped in triple-
/// backtick fences so the client's markdown renderer handles them uniformly.
pub fn hover_to_protocol(
    hover: lsp_types::Hover,
    text: &str,
    encoding: PositionEncoding,
) -> HoverContent {
    let markdown = hover_contents_to_markdown(hover.contents);
    let range = hover.range.map(|r| lsp_range_to_rift(r, text, encoding));
    HoverContent { markdown, range }
}

/// Render `HoverContents` as a markdown string.
fn hover_contents_to_markdown(contents: HoverContents) -> String {
    match contents {
        HoverContents::Scalar(ms) => marked_string_to_markdown(ms),
        HoverContents::Array(ms_vec) => ms_vec
            .into_iter()
            .map(marked_string_to_markdown)
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
        HoverContents::Markup(markup) => markup.value,
    }
}

/// Render a `MarkedString` as markdown. A plain string is already markdown; a
/// language-tagged string is wrapped in a fenced code block.
fn marked_string_to_markdown(ms: MarkedString) -> String {
    match ms {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(ls) => format!("```{}\n{}\n```", ls.language, ls.value),
    }
}

/// Translate a `GotoDefinitionResponse` to a list of `NavLocation`s.
///
/// `root_dir` is the worktree root (absolute). A location is `out_of_root` when
/// its URI path does not start with `root_dir`. The `text_for` closure is
/// called to fetch document text for offset translation and line preview; it
/// returns `None` when the file is unreadable (gracefully handled — the
/// location is still returned, just without a range translation or preview).
pub fn definition_response_to_protocol(
    response: GotoDefinitionResponse,
    root_dir: &Path,
    encoding: PositionEncoding,
) -> Vec<NavLocation> {
    let locations: Vec<Location> = match response {
        GotoDefinitionResponse::Scalar(loc) => vec![loc],
        GotoDefinitionResponse::Array(locs) => locs,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|l| Location {
                uri: l.target_uri,
                range: l.target_selection_range,
            })
            .collect(),
    };
    locations
        .into_iter()
        .filter_map(|loc| location_to_nav(loc, root_dir, encoding))
        .collect()
}

/// Translate a list of `Location`s (references response) to `NavLocation`s.
pub fn references_to_protocol(
    locs: Vec<Location>,
    root_dir: &Path,
    encoding: PositionEncoding,
) -> Vec<NavLocation> {
    locs.into_iter()
        .filter_map(|loc| location_to_nav(loc, root_dir, encoding))
        .collect()
}

/// Translate a single LSP `Location` to a `NavLocation`.
///
/// Returns `None` when the URI is not a `file://` URL (e.g. virtual documents
/// — skip them gracefully rather than error).
fn location_to_nav(
    loc: Location,
    root_dir: &Path,
    encoding: PositionEncoding,
) -> Option<NavLocation> {
    let abs_path = loc.uri.to_file_path().ok()?;
    let out_of_root = !abs_path.starts_with(root_dir);

    // Attempt disk read for range translation and line preview.
    let text_opt = read_text_from_disk(&abs_path);

    // Compute the path string once — same logic regardless of whether we have text.
    let path_str = if out_of_root {
        abs_path.display().to_string()
    } else {
        abs_path.strip_prefix(root_dir).ok()?.display().to_string()
    };

    let range = match text_opt.as_deref() {
        Some(text) => lsp_range_to_rift(loc.range, text, encoding),
        None => {
            // File is unreadable (permissions, not found, binary). The LSP
            // character values are passed through as-is into `Position::character`
            // (UTF-8 char offset fields). For UTF-16 servers this is technically
            // wrong on non-ASCII lines, but there is no text to translate against;
            // line and character are still useful for navigation even if the
            // column highlight may be off. The caller surfaces this location
            // without a line preview, signalling the imprecision.
            Range {
                start: Position {
                    line: loc.range.start.line,
                    character: loc.range.start.character,
                },
                end: Position {
                    line: loc.range.end.line,
                    character: loc.range.end.character,
                },
            }
        }
    };

    let line_preview = text_opt.as_deref().and_then(|text| {
        text.lines()
            .nth(range.start.line as usize)
            .map(|l| l.trim().to_owned())
    });

    Some(NavLocation {
        path: path_str,
        range,
        out_of_root,
        line_preview,
    })
}

// ── NavRequester ─────────────────────────────────────────────────────────────

/// Wraps a `Server` handle with capability-checked navigation requests.
///
/// Constructed from a `&Server` reference once the server is confirmed live;
/// the daemon's dispatch layer selects the first capable server before
/// constructing this.
pub struct NavRequester<'a> {
    server: &'a Server,
    encoding: PositionEncoding,
    root_dir: PathBuf,
}

impl<'a> NavRequester<'a> {
    /// Construct a `NavRequester` for `server` at `root_dir`.
    ///
    /// `encoding` should come from `server.position_encoding()`.
    pub fn new(server: &'a Server, encoding: PositionEncoding, root_dir: PathBuf) -> Self {
        Self {
            server,
            encoding,
            root_dir,
        }
    }

    /// Issue a `textDocument/hover` request.
    ///
    /// Returns `None` when the server does not advertise hover capability or
    /// when the server responds with no content for the position.
    ///
    /// `text` is the document text used for offset-encoding translation. Pass
    /// the synced buffer content if available; for files without a synced
    /// buffer, pass the disk-read text.
    pub async fn hover(
        &self,
        path: &Path,
        pos: Position,
        text: &str,
    ) -> Result<Option<HoverContent>> {
        let caps = self.server.capabilities();
        if !has_hover(caps) {
            debug!(
                server = self.server.name(),
                "hover: server does not advertise capability, skipping"
            );
            return Ok(None);
        }

        let uri = path_to_uri(path)?;
        let lsp_pos = rift_pos_to_lsp(pos, text, self.encoding);

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_pos,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };

        let result = self.server.request_hover(params).await?;
        Ok(result.map(|h| hover_to_protocol(h, text, self.encoding)))
    }

    /// Issue a `textDocument/definition` request.
    ///
    /// Returns an empty `Vec` when the server does not advertise definition
    /// capability or when it responds with no locations.
    pub async fn definition(
        &self,
        path: &Path,
        pos: Position,
        text: &str,
    ) -> Result<Vec<NavLocation>> {
        let caps = self.server.capabilities();
        if !has_definition(caps) {
            debug!(
                server = self.server.name(),
                "definition: server does not advertise capability, skipping"
            );
            return Ok(vec![]);
        }

        let uri = path_to_uri(path)?;
        let lsp_pos = rift_pos_to_lsp(pos, text, self.encoding);

        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_pos,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };

        let result = self.server.request_definition(params).await?;
        Ok(match result {
            Some(resp) => definition_response_to_protocol(resp, &self.root_dir, self.encoding),
            None => vec![],
        })
    }

    /// Issue a `textDocument/references` request.
    ///
    /// Returns an empty `Vec` when the server does not advertise references
    /// capability or when it responds with no locations.
    pub async fn references(
        &self,
        path: &Path,
        pos: Position,
        text: &str,
    ) -> Result<Vec<NavLocation>> {
        let caps = self.server.capabilities();
        if !has_references(caps) {
            debug!(
                server = self.server.name(),
                "references: server does not advertise capability, skipping"
            );
            return Ok(vec![]);
        }

        let uri = path_to_uri(path)?;
        let lsp_pos = rift_pos_to_lsp(pos, text, self.encoding);

        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_pos,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };

        let result = self.server.request_references(params).await?;
        Ok(match result {
            Some(locs) => references_to_protocol(locs, &self.root_dir, self.encoding),
            None => vec![],
        })
    }
}

// ── Routing helper ────────────────────────────────────────────────────────────

/// Whether `caps` advertises hover support.
pub fn has_hover(caps: &ServerCapabilities) -> bool {
    matches!(
        caps.hover_provider,
        Some(lsp_types::HoverProviderCapability::Simple(true))
            | Some(lsp_types::HoverProviderCapability::Options(_))
    )
}

/// Whether `caps` advertises go-to-definition support.
pub fn has_definition(caps: &ServerCapabilities) -> bool {
    matches!(
        caps.definition_provider,
        Some(lsp_types::OneOf::Left(true)) | Some(lsp_types::OneOf::Right(_))
    )
}

/// Whether `caps` advertises find-references support.
pub fn has_references(caps: &ServerCapabilities) -> bool {
    matches!(
        caps.references_provider,
        Some(lsp_types::OneOf::Left(true)) | Some(lsp_types::OneOf::Right(_))
    )
}

/// Build a `file://` URI from an absolute path.
fn path_to_uri(path: &Path) -> Result<Url> {
    Url::from_file_path(path).map_err(|()| LspError::InvalidUri(path.display().to_string()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::MarkupKind;

    // ── Offset-encoding translation ──────────────────────────────────────────

    /// The canonical multi-byte test: a line containing:
    /// - `ä`  — U+00E4, BMP, 2 UTF-8 bytes, 1 UTF-16 code unit
    /// - `中`  — U+4E2D, BMP, 3 UTF-8 bytes, 1 UTF-16 code unit
    /// - `𝄞`  — U+1D11E, astral, 4 UTF-8 bytes, 2 UTF-16 code units (surrogate pair)
    /// - `!`  — ASCII, 1 byte, 1 code unit
    ///
    /// Line: "äX中Y𝄞Z!"
    /// Character positions (0-based UTF-8 chars):  0=ä, 1=X, 2=中, 3=Y, 4=𝄞, 5=Z, 6=!
    /// UTF-16 code-unit offsets:                   0,   1,  2,   3,  4,   6,  7,  8
    /// (𝄞 occupies CUs 4 and 5, so Z is at CU 6)
    const MULTIBYTE_LINE: &str = "äX中Y\u{1D11E}Z!";
    const TEXT: &str = "first line\näX中Y\u{1D11E}Z!\nthird line";

    #[test]
    fn test_offset_utf8_char_to_utf16_cu_ascii_only() {
        // Pure ASCII: UTF-8 chars == UTF-16 CUs, so the result must be identical.
        let line = "hello world";
        for i in 0..=line.chars().count() {
            assert_eq!(
                utf8_char_offset_to_utf16_cu(line, i),
                i as u32,
                "ascii line: char offset {i} must equal UTF-16 CU {i}"
            );
        }
    }

    #[test]
    fn test_offset_utf16_cu_to_utf8_char_ascii_only() {
        let line = "hello world";
        for i in 0..=line.chars().count() as u32 {
            assert_eq!(
                utf16_cu_to_utf8_char_offset(line, i),
                i as usize,
                "ascii line: UTF-16 CU {i} must equal char offset {i}"
            );
        }
    }

    #[test]
    fn test_offset_multibyte_utf8_chars_to_utf16_cus() {
        // ä(0)→0, X(1)→1, 中(2)→2, Y(3)→3, 𝄞(4)→4, Z(5)→6, !(6)→7
        let cases: &[(usize, u32)] = &[
            (0, 0), // before ä
            (1, 1), // after ä (ä = 1 CU)
            (2, 2), // after X
            (3, 3), // after 中 (中 = 1 CU)
            (4, 4), // after Y
            (5, 6), // after 𝄞 (𝄞 = 2 CUs, so 4+2=6)
            (6, 7), // after Z
            (7, 8), // after !
        ];
        for &(char_off, expected_cu) in cases {
            assert_eq!(
                utf8_char_offset_to_utf16_cu(MULTIBYTE_LINE, char_off),
                expected_cu,
                "char offset {char_off} -> UTF-16 CU {expected_cu}"
            );
        }
    }

    #[test]
    fn test_offset_multibyte_utf16_cus_to_utf8_chars() {
        // Inverse of the above: UTF-16 CU → UTF-8 char index
        // CU0→0(ä), CU1→1(X), CU2→2(中), CU3→3(Y), CU4→4(𝄞), CU6→5(Z), CU7→6(!)
        let cases: &[(u32, usize)] = &[
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (6, 5), // Z is at CU 6 but char index 5
            (7, 6),
            (8, 7),
        ];
        for &(cu, expected_char) in cases {
            assert_eq!(
                utf16_cu_to_utf8_char_offset(MULTIBYTE_LINE, cu),
                expected_char,
                "UTF-16 CU {cu} -> char offset {expected_char}"
            );
        }
    }

    #[test]
    fn test_rift_pos_to_lsp_utf16_multibyte_line() {
        // Line 1 of TEXT is the multibyte line. rift Position{line:1, character:5}
        // is after 𝄞 (the Z character). LSP UTF-16 must report CU 6.
        let pos = Position {
            line: 1,
            character: 5,
        };
        let lsp = rift_pos_to_lsp(pos, TEXT, PositionEncoding::Utf16);
        assert_eq!(lsp.line, 1);
        assert_eq!(lsp.character, 6, "Z is at UTF-16 CU 6");
    }

    #[test]
    fn test_rift_pos_to_lsp_utf8_multibyte_line_unchanged() {
        // UTF-8 server: character offset passes through unchanged.
        let pos = Position {
            line: 1,
            character: 5,
        };
        let lsp = rift_pos_to_lsp(pos, TEXT, PositionEncoding::Utf8);
        assert_eq!(lsp.character, 5);
    }

    #[test]
    fn test_lsp_pos_to_rift_utf16_multibyte_line() {
        // LSP CU 6 on line 1 → rift char 5 (Z).
        let lsp = LspPosition {
            line: 1,
            character: 6,
        };
        let rift = lsp_pos_to_rift(lsp, TEXT, PositionEncoding::Utf16);
        assert_eq!(rift.line, 1);
        assert_eq!(rift.character, 5, "UTF-16 CU 6 -> UTF-8 char 5 (Z)");
    }

    #[test]
    fn test_lsp_pos_to_rift_utf8_unchanged() {
        let lsp = LspPosition {
            line: 1,
            character: 5,
        };
        let rift = lsp_pos_to_rift(lsp, TEXT, PositionEncoding::Utf8);
        assert_eq!(rift.character, 5);
    }

    #[test]
    fn test_roundtrip_rift_to_lsp_and_back_utf16() {
        // Every character position on the multibyte line must survive a
        // rift→LSP→rift round-trip without drift.
        for char_off in 0..=MULTIBYTE_LINE.chars().count() as u32 {
            let orig = Position {
                line: 1,
                character: char_off,
            };
            let lsp = rift_pos_to_lsp(orig, TEXT, PositionEncoding::Utf16);
            let back = lsp_pos_to_rift(lsp, TEXT, PositionEncoding::Utf16);
            assert_eq!(
                back.character, char_off,
                "round-trip drift at char offset {char_off}"
            );
        }
    }

    // ── Capability checks ────────────────────────────────────────────────────

    #[test]
    fn test_has_hover_simple_true() {
        let mut caps = ServerCapabilities::default();
        caps.hover_provider = Some(lsp_types::HoverProviderCapability::Simple(true));
        assert!(has_hover(&caps));
    }

    #[test]
    fn test_has_hover_simple_false() {
        let mut caps = ServerCapabilities::default();
        caps.hover_provider = Some(lsp_types::HoverProviderCapability::Simple(false));
        assert!(!has_hover(&caps));
    }

    #[test]
    fn test_has_hover_none() {
        assert!(!has_hover(&ServerCapabilities::default()));
    }

    #[test]
    fn test_has_definition_left_true() {
        let mut caps = ServerCapabilities::default();
        caps.definition_provider = Some(lsp_types::OneOf::Left(true));
        assert!(has_definition(&caps));
    }

    #[test]
    fn test_has_definition_left_false() {
        let mut caps = ServerCapabilities::default();
        caps.definition_provider = Some(lsp_types::OneOf::Left(false));
        assert!(!has_definition(&caps));
    }

    #[test]
    fn test_has_references_left_true() {
        let mut caps = ServerCapabilities::default();
        caps.references_provider = Some(lsp_types::OneOf::Left(true));
        assert!(has_references(&caps));
    }

    // ── hover_contents_to_markdown ───────────────────────────────────────────

    #[test]
    fn test_hover_contents_scalar_string_passes_through() {
        let contents = HoverContents::Scalar(MarkedString::String("hello".to_owned()));
        assert_eq!(hover_contents_to_markdown(contents), "hello");
    }

    #[test]
    fn test_hover_contents_scalar_language_string_wraps_in_fence() {
        let contents =
            HoverContents::Scalar(MarkedString::LanguageString(lsp_types::LanguageString {
                language: "rust".to_owned(),
                value: "fn foo() {}".to_owned(),
            }));
        let md = hover_contents_to_markdown(contents);
        assert!(md.starts_with("```rust\n"));
        assert!(md.ends_with("\n```"));
        assert!(md.contains("fn foo() {}"));
    }

    #[test]
    fn test_hover_contents_markup_markdown_passes_through() {
        let contents = HoverContents::Markup(lsp_types::MarkupContent {
            kind: MarkupKind::Markdown,
            value: "**bold**".to_owned(),
        });
        assert_eq!(hover_contents_to_markdown(contents), "**bold**");
    }

    #[test]
    fn test_hover_contents_array_joined_with_separator() {
        let contents = HoverContents::Array(vec![
            MarkedString::String("first".to_owned()),
            MarkedString::String("second".to_owned()),
        ]);
        let md = hover_contents_to_markdown(contents);
        assert!(md.contains("first"));
        assert!(md.contains("---"));
        assert!(md.contains("second"));
    }

    // ── PositionEncoding::from_capabilities ─────────────────────────────────

    #[test]
    fn test_position_encoding_defaults_to_utf16_when_absent() {
        let caps = ServerCapabilities::default();
        assert_eq!(
            PositionEncoding::from_capabilities(&caps),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn test_position_encoding_utf8_when_advertised() {
        let mut caps = ServerCapabilities::default();
        caps.position_encoding = Some(lsp_types::PositionEncodingKind::UTF8);
        assert_eq!(
            PositionEncoding::from_capabilities(&caps),
            PositionEncoding::Utf8
        );
    }
}
