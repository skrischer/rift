//! Pure extension -> file-type-icon mapping for the explorer's reserved icon
//! slot (`docs/spec-explorer-icons.md`, issue #669). Authored in Zed's
//! icon-theme JSON *shape* — `default_file` / `default_folder` /
//! `default_folder_open` plus a `file_types` extension map
//! (<https://zed.dev/docs/extensions/icon-themes>) — as a Rust static: a
//! single bundled set for v1, not a loadable JSON theme (the spec's prior
//! decisions record externalization as a deferred follow-up this shape keeps
//! mechanical).
//!
//! Every entry resolves to a **glyph** ([`Glyph::Svg`] for a vendored
//! `file_icons/*.svg` asset the delegating `RiftAssets` source in `main.rs`
//! serves, or [`Glyph::Chrome`] for one of gpui-component's already-embedded
//! `IconName` glyphs) and a **theme-token tint role** ([`TintRole`]) — never a
//! hex literal, so the icon re-tints on a theme switch (constitution: theme
//! tokens only).
//!
//! Derives purely from a row's leaf name / extension — no path I/O, no model
//! access, agent-agnostic by construction (constitution).

use gpui::Hsla;
use gpui_component::{IconName, ThemeColor};

/// A chrome-tier glyph, resolved through gpui-component's already-embedded
/// `IconName` set. A dedicated enum (rather than storing `IconName` itself)
/// because the generated `IconName` only derives `IntoElement` + `Clone` —
/// no `Debug` / `PartialEq` / `Eq` / `Copy` — so this is the seam that keeps
/// [`IconEntry`] comparable and testable with plain `assert_eq!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeGlyph {
    /// Generic-file fallback ([`DEFAULT_FILE`]).
    File,
    /// Closed folder ([`DEFAULT_FOLDER`]).
    Folder,
    /// Open folder ([`DEFAULT_FOLDER_OPEN`]).
    FolderOpen,
}

impl ChromeGlyph {
    /// Resolve to the concrete gpui-component icon.
    pub fn icon_name(self) -> IconName {
        match self {
            ChromeGlyph::File => IconName::File,
            ChromeGlyph::Folder => IconName::Folder,
            ChromeGlyph::FolderOpen => IconName::FolderOpen,
        }
    }
}

/// Where an icon's glyph comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// A vendored file-type SVG asset path (e.g. `file_icons/rust.svg`),
    /// served by the delegating `RiftAssets` source (`main.rs`).
    Svg(&'static str),
    /// A chrome-tier glyph (folder / open-folder / generic-file).
    Chrome(ChromeGlyph),
}

/// The theme-token role an icon's fill resolves against — the spec's
/// icon-tint -> theme-token mapping table. Never a hex literal: [`resolve`]
/// reads the live [`ThemeColor`], so a theme switch re-tints automatically.
///
/// [`resolve`]: TintRole::resolve
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TintRole {
    /// Folder, collapsed — artboard overlay `#6C7086`.
    Overlay,
    /// Folder, expanded (open) — artboard primary `#89B4FA`.
    Primary,
    /// Disclosure chevron + generic-file fallback — artboard subtext
    /// `#A6ADC8`.
    MutedForeground,
    /// `.rs` — artboard peach `#FAB387`.
    Warning,
    /// `.toml` — artboard teal `#94E2D5`.
    Cyan,
    /// `.md` — artboard info/sky `#89DCEB`.
    Info,
}

impl TintRole {
    /// Resolve to the live theme's color for this role.
    pub fn resolve(self, theme: &ThemeColor) -> Hsla {
        match self {
            TintRole::Overlay => theme.overlay,
            TintRole::Primary => theme.primary,
            TintRole::MutedForeground => theme.muted_foreground,
            TintRole::Warning => theme.warning,
            TintRole::Cyan => theme.cyan,
            TintRole::Info => theme.info,
        }
    }
}

/// One mapping entry: the glyph to render plus its tint role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconEntry {
    pub glyph: Glyph,
    pub tint: TintRole,
}

/// Fallback glyph for any extension [`FILE_TYPES`] does not cover — never a
/// blank icon slot.
pub const DEFAULT_FILE: IconEntry = IconEntry {
    glyph: Glyph::Chrome(ChromeGlyph::File),
    tint: TintRole::MutedForeground,
};

/// Closed-folder glyph (`EntryKind::Dir`, collapsed).
pub const DEFAULT_FOLDER: IconEntry = IconEntry {
    glyph: Glyph::Chrome(ChromeGlyph::Folder),
    tint: TintRole::Overlay,
};

/// Open-folder glyph (`EntryKind::Dir`, expanded).
pub const DEFAULT_FOLDER_OPEN: IconEntry = IconEntry {
    glyph: Glyph::Chrome(ChromeGlyph::FolderOpen),
    tint: TintRole::Primary,
};

/// Case-insensitive extension -> icon-entry table: the curated file-type
/// tier (rift's own repo types plus the artboard's three — `docs/
/// spec-explorer-icons.md` scope; a full upstream set is explicitly out of
/// scope). Extensions are stored lowercase; [`file_icon_for`] lowercases the
/// query before matching.
const FILE_TYPES: &[(&str, IconEntry)] = &[
    (
        "rs",
        IconEntry {
            glyph: Glyph::Svg("file_icons/rust.svg"),
            tint: TintRole::Warning,
        },
    ),
    (
        "toml",
        IconEntry {
            glyph: Glyph::Svg("file_icons/toml.svg"),
            tint: TintRole::Cyan,
        },
    ),
    (
        "md",
        IconEntry {
            glyph: Glyph::Svg("file_icons/markdown.svg"),
            tint: TintRole::Info,
        },
    ),
    (
        "json",
        IconEntry {
            glyph: Glyph::Svg("file_icons/json.svg"),
            tint: TintRole::MutedForeground,
        },
    ),
    (
        "sh",
        IconEntry {
            glyph: Glyph::Svg("file_icons/shell.svg"),
            tint: TintRole::MutedForeground,
        },
    ),
    (
        "lock",
        IconEntry {
            glyph: Glyph::Svg("file_icons/lock.svg"),
            tint: TintRole::MutedForeground,
        },
    ),
];

/// Dotfile / extensionless leaves mapped on their full lowercase name — the
/// Zed shape's allowance for "a dotfile with no extension may map on its
/// full leaf". `Path::extension` returns `None` for a leading-dot-only name
/// like `.gitignore` (the whole name is its stem, not an extension), so
/// these never collide with [`FILE_TYPES`].
const FULL_NAME_TYPES: &[(&str, IconEntry)] = &[
    (
        ".gitignore",
        IconEntry {
            glyph: Glyph::Svg("file_icons/git_ignore.svg"),
            tint: TintRole::MutedForeground,
        },
    ),
    (
        "license",
        IconEntry {
            glyph: Glyph::Svg("file_icons/license.svg"),
            tint: TintRole::MutedForeground,
        },
    ),
];

/// Map a file row's leaf name (e.g. `main.rs`, `.gitignore`, `LICENSE`) to
/// its icon entry: a case-insensitive full-leaf match first (dotfiles /
/// extensionless names), then a case-insensitive extension match, falling
/// back to [`DEFAULT_FILE`] so an unmapped extension never renders blank.
/// Total operations only (`to_ascii_lowercase` + `rsplit_once`) — no panic on
/// any input, including an empty string.
pub fn file_icon_for(leaf: &str) -> IconEntry {
    let lower = leaf.to_ascii_lowercase();

    if let Some((_, entry)) = FULL_NAME_TYPES.iter().find(|(name, _)| *name == lower) {
        return *entry;
    }

    let extension = match lower.rsplit_once('.') {
        // A leading-dot-only name (e.g. ".gitignore") has no extension: the
        // split point is index 0, so there is nothing before the dot.
        Some((stem, ext)) if !stem.is_empty() => Some(ext),
        _ => None,
    };

    extension
        .and_then(|ext| {
            FILE_TYPES
                .iter()
                .find(|(candidate, _)| *candidate == ext)
                .map(|(_, entry)| *entry)
        })
        .unwrap_or(DEFAULT_FILE)
}

/// Select the folder glyph for a directory row's collapse state.
pub fn folder_icon_for(expanded: bool) -> IconEntry {
    if expanded {
        DEFAULT_FOLDER_OPEN
    } else {
        DEFAULT_FOLDER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_icon_for_rs_maps_to_rust_glyph_and_warning_tint() {
        let entry = file_icon_for("main.rs");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/rust.svg"));
        assert_eq!(entry.tint, TintRole::Warning);
    }

    #[test]
    fn test_file_icon_for_toml_maps_to_toml_glyph_and_cyan_tint() {
        let entry = file_icon_for("Cargo.toml");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/toml.svg"));
        assert_eq!(entry.tint, TintRole::Cyan);
    }

    #[test]
    fn test_file_icon_for_md_maps_to_markdown_glyph_and_info_tint() {
        let entry = file_icon_for("README.md");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/markdown.svg"));
        assert_eq!(entry.tint, TintRole::Info);
    }

    #[test]
    fn test_file_icon_for_is_case_insensitive_on_extension() {
        assert_eq!(file_icon_for("main.RS"), file_icon_for("main.rs"));
    }

    #[test]
    fn test_file_icon_for_unmapped_extension_falls_back_to_default_file() {
        assert_eq!(file_icon_for("notes.xyz"), DEFAULT_FILE);
    }

    #[test]
    fn test_file_icon_for_no_extension_falls_back_to_default_file() {
        assert_eq!(file_icon_for("Makefile"), DEFAULT_FILE);
    }

    #[test]
    fn test_file_icon_for_dotfile_maps_on_full_leaf() {
        let entry = file_icon_for(".gitignore");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/git_ignore.svg"));
    }

    #[test]
    fn test_file_icon_for_empty_leaf_falls_back_to_default_file() {
        assert_eq!(file_icon_for(""), DEFAULT_FILE);
    }

    #[test]
    fn test_folder_icon_for_expanded_is_open_folder_with_primary_tint() {
        let entry = folder_icon_for(true);
        assert_eq!(entry.glyph, Glyph::Chrome(ChromeGlyph::FolderOpen));
        assert_eq!(entry.tint, TintRole::Primary);
    }

    #[test]
    fn test_folder_icon_for_collapsed_is_closed_folder_with_overlay_tint() {
        let entry = folder_icon_for(false);
        assert_eq!(entry.glyph, Glyph::Chrome(ChromeGlyph::Folder));
        assert_eq!(entry.tint, TintRole::Overlay);
    }
}
