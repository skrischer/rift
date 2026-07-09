//! Pure extension -> file-type-icon mapping for the explorer's reserved icon
//! slot (`docs/spec-explorer-icons.md`, issue #669; broadened + corrected by
//! `docs/spec-explorer-polish.md`, issue #712). Authored in Zed's icon-theme
//! JSON *shape* — `default_file` / `default_folder` / `default_folder_open`
//! plus a `file_types` extension map
//! (<https://zed.dev/docs/extensions/icon-themes>) — as a Rust static: a
//! single bundled set for v1, not a loadable JSON theme (the spec's prior
//! decisions record externalization as a deferred follow-up this shape keeps
//! mechanical). The mapping itself adopts Zed's default (Seti-derived) icon
//! theme so the common language/config types resolve to a recognizable
//! glyph instead of the curated subset issue #669 shipped.
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
/// `Overlay` (Phase 28's collapsed-folder tint) is deliberately **not** a
/// variant here: the shipped Catppuccin Mocha theme maps `overlay` to
/// `#11111bcc`, a near-black scrim that reads as invisible on the dark
/// sidebar (`docs/spec-explorer-polish.md`, issue #712). `overlay` is
/// forbidden as an icon fill; every tint below clears a contrast bar against
/// the sidebar/background surface.
///
/// [`resolve`]: TintRole::resolve
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TintRole {
    /// Folder, expanded (open); `.ts`/`.c`/`.cpp`/`.css`/`Dockerfile` —
    /// artboard primary/blue `#89B4FA`.
    Primary,
    /// Folder, collapsed; disclosure chevron + generic-file fallback —
    /// artboard subtext `#A6ADC8`.
    MutedForeground,
    /// `.rs`; `.html`/`.htm`; `.gitignore`/`.gitattributes` — artboard peach
    /// `#FAB387`.
    Warning,
    /// `.toml`; `.tsx`/`.jsx`; `.go` — artboard teal `#94E2D5`.
    Cyan,
    /// `.md`/`.markdown` — artboard info/sky `#89DCEB`.
    Info,
    /// `.js`/`.cjs`/`.mjs`; `.py` — artboard yellow `#F9E2AF`.
    Yellow,
    /// `.sh`/`.bash`/`.zsh` — artboard green `#A6E3A1`.
    Green,
    /// `.rb`; `.java` — artboard red `#F38BA8`.
    Red,
    /// `.yaml`/`.yml`; `.scss`/`.sass` — artboard magenta `#CBA6F7`.
    Magenta,
}

impl TintRole {
    /// Resolve to the live theme's color for this role.
    pub fn resolve(self, theme: &ThemeColor) -> Hsla {
        match self {
            TintRole::Primary => theme.primary,
            TintRole::MutedForeground => theme.muted_foreground,
            TintRole::Warning => theme.warning,
            TintRole::Cyan => theme.cyan,
            TintRole::Info => theme.info,
            TintRole::Yellow => theme.yellow,
            TintRole::Green => theme.green,
            TintRole::Red => theme.red,
            TintRole::Magenta => theme.magenta,
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

/// Closed-folder glyph (`EntryKind::Dir`, collapsed). Tinted
/// `muted_foreground`, not `overlay` — `docs/spec-explorer-polish.md`,
/// issue #712 (REVISES Phase 28: `overlay` resolves to a near-black scrim in
/// the shipped theme and reads as invisible on the dark sidebar).
pub const DEFAULT_FOLDER: IconEntry = IconEntry {
    glyph: Glyph::Chrome(ChromeGlyph::Folder),
    tint: TintRole::MutedForeground,
};

/// Open-folder glyph (`EntryKind::Dir`, expanded).
pub const DEFAULT_FOLDER_OPEN: IconEntry = IconEntry {
    glyph: Glyph::Chrome(ChromeGlyph::FolderOpen),
    tint: TintRole::Primary,
};

/// Case-insensitive extension -> icon-entry table: the industry-standard set
/// adopted from Zed's default (Seti-derived) icon theme (`docs/
/// spec-explorer-polish.md`, issue #712 — REVISES issue #669's curated
/// subset). The long tail beyond this common set falls back to
/// [`DEFAULT_FILE`], not a new table entry (`docs/spec-explorer-polish.md`
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
        "ts",
        IconEntry {
            glyph: Glyph::Svg("file_icons/typescript.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "cts",
        IconEntry {
            glyph: Glyph::Svg("file_icons/typescript.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "mts",
        IconEntry {
            glyph: Glyph::Svg("file_icons/typescript.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "tsx",
        IconEntry {
            glyph: Glyph::Svg("file_icons/react.svg"),
            tint: TintRole::Cyan,
        },
    ),
    (
        "jsx",
        IconEntry {
            glyph: Glyph::Svg("file_icons/react.svg"),
            tint: TintRole::Cyan,
        },
    ),
    (
        "js",
        IconEntry {
            glyph: Glyph::Svg("file_icons/javascript.svg"),
            tint: TintRole::Yellow,
        },
    ),
    (
        "cjs",
        IconEntry {
            glyph: Glyph::Svg("file_icons/javascript.svg"),
            tint: TintRole::Yellow,
        },
    ),
    (
        "mjs",
        IconEntry {
            glyph: Glyph::Svg("file_icons/javascript.svg"),
            tint: TintRole::Yellow,
        },
    ),
    (
        "py",
        IconEntry {
            glyph: Glyph::Svg("file_icons/python.svg"),
            tint: TintRole::Yellow,
        },
    ),
    (
        "go",
        IconEntry {
            glyph: Glyph::Svg("file_icons/go.svg"),
            tint: TintRole::Cyan,
        },
    ),
    (
        "rb",
        IconEntry {
            glyph: Glyph::Svg("file_icons/ruby.svg"),
            tint: TintRole::Red,
        },
    ),
    (
        "java",
        IconEntry {
            glyph: Glyph::Svg("file_icons/java.svg"),
            tint: TintRole::Red,
        },
    ),
    (
        "c",
        IconEntry {
            glyph: Glyph::Svg("file_icons/c.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "h",
        IconEntry {
            glyph: Glyph::Svg("file_icons/c.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "cpp",
        IconEntry {
            glyph: Glyph::Svg("file_icons/cpp.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "cc",
        IconEntry {
            glyph: Glyph::Svg("file_icons/cpp.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "cxx",
        IconEntry {
            glyph: Glyph::Svg("file_icons/cpp.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "hpp",
        IconEntry {
            glyph: Glyph::Svg("file_icons/cpp.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "hh",
        IconEntry {
            glyph: Glyph::Svg("file_icons/cpp.svg"),
            tint: TintRole::Primary,
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
        "markdown",
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
        "jsonc",
        IconEntry {
            glyph: Glyph::Svg("file_icons/json.svg"),
            tint: TintRole::MutedForeground,
        },
    ),
    (
        "yaml",
        IconEntry {
            glyph: Glyph::Svg("file_icons/yaml.svg"),
            tint: TintRole::Magenta,
        },
    ),
    (
        "yml",
        IconEntry {
            glyph: Glyph::Svg("file_icons/yaml.svg"),
            tint: TintRole::Magenta,
        },
    ),
    (
        "html",
        IconEntry {
            glyph: Glyph::Svg("file_icons/html.svg"),
            tint: TintRole::Warning,
        },
    ),
    (
        "htm",
        IconEntry {
            glyph: Glyph::Svg("file_icons/html.svg"),
            tint: TintRole::Warning,
        },
    ),
    (
        "css",
        IconEntry {
            glyph: Glyph::Svg("file_icons/css.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "scss",
        IconEntry {
            glyph: Glyph::Svg("file_icons/sass.svg"),
            tint: TintRole::Magenta,
        },
    ),
    (
        "sass",
        IconEntry {
            glyph: Glyph::Svg("file_icons/sass.svg"),
            tint: TintRole::Magenta,
        },
    ),
    (
        "sh",
        IconEntry {
            glyph: Glyph::Svg("file_icons/shell.svg"),
            tint: TintRole::Green,
        },
    ),
    (
        "bash",
        IconEntry {
            glyph: Glyph::Svg("file_icons/shell.svg"),
            tint: TintRole::Green,
        },
    ),
    (
        "zsh",
        IconEntry {
            glyph: Glyph::Svg("file_icons/shell.svg"),
            tint: TintRole::Green,
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
            tint: TintRole::Warning,
        },
    ),
    (
        ".gitattributes",
        IconEntry {
            glyph: Glyph::Svg("file_icons/git_ignore.svg"),
            tint: TintRole::Warning,
        },
    ),
    (
        "license",
        IconEntry {
            glyph: Glyph::Svg("file_icons/license.svg"),
            tint: TintRole::MutedForeground,
        },
    ),
    (
        "dockerfile",
        IconEntry {
            glyph: Glyph::Svg("file_icons/docker.svg"),
            tint: TintRole::Primary,
        },
    ),
    (
        "containerfile",
        IconEntry {
            glyph: Glyph::Svg("file_icons/docker.svg"),
            tint: TintRole::Primary,
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
    fn test_folder_icon_for_collapsed_is_closed_folder_with_muted_foreground_tint() {
        let entry = folder_icon_for(false);
        assert_eq!(entry.glyph, Glyph::Chrome(ChromeGlyph::Folder));
        assert_eq!(entry.tint, TintRole::MutedForeground);
    }

    #[test]
    fn test_file_icon_for_ts_maps_to_typescript_glyph_and_primary_tint() {
        let entry = file_icon_for("index.ts");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/typescript.svg"));
        assert_eq!(entry.tint, TintRole::Primary);
    }

    #[test]
    fn test_file_icon_for_tsx_maps_to_react_glyph_and_cyan_tint() {
        let entry = file_icon_for("App.tsx");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/react.svg"));
        assert_eq!(entry.tint, TintRole::Cyan);
    }

    #[test]
    fn test_file_icon_for_jsx_maps_to_react_glyph() {
        let entry = file_icon_for("App.jsx");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/react.svg"));
    }

    #[test]
    fn test_file_icon_for_js_maps_to_javascript_glyph_and_yellow_tint() {
        let entry = file_icon_for("index.js");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/javascript.svg"));
        assert_eq!(entry.tint, TintRole::Yellow);
    }

    #[test]
    fn test_file_icon_for_py_maps_to_python_glyph_and_yellow_tint() {
        let entry = file_icon_for("main.py");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/python.svg"));
        assert_eq!(entry.tint, TintRole::Yellow);
    }

    #[test]
    fn test_file_icon_for_go_maps_to_go_glyph_and_cyan_tint() {
        let entry = file_icon_for("main.go");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/go.svg"));
        assert_eq!(entry.tint, TintRole::Cyan);
    }

    #[test]
    fn test_file_icon_for_rb_maps_to_ruby_glyph_and_red_tint() {
        let entry = file_icon_for("main.rb");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/ruby.svg"));
        assert_eq!(entry.tint, TintRole::Red);
    }

    #[test]
    fn test_file_icon_for_java_maps_to_java_glyph_and_red_tint() {
        let entry = file_icon_for("Main.java");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/java.svg"));
        assert_eq!(entry.tint, TintRole::Red);
    }

    #[test]
    fn test_file_icon_for_c_and_h_map_to_c_glyph_and_primary_tint() {
        assert_eq!(
            file_icon_for("main.c").glyph,
            Glyph::Svg("file_icons/c.svg")
        );
        assert_eq!(file_icon_for("main.c").tint, TintRole::Primary);
        assert_eq!(
            file_icon_for("main.h").glyph,
            Glyph::Svg("file_icons/c.svg")
        );
    }

    #[test]
    fn test_file_icon_for_cpp_maps_to_cpp_glyph_and_primary_tint() {
        let entry = file_icon_for("main.cpp");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/cpp.svg"));
        assert_eq!(entry.tint, TintRole::Primary);
    }

    #[test]
    fn test_file_icon_for_json_maps_to_json_glyph_and_muted_foreground_tint() {
        let entry = file_icon_for("package.json");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/json.svg"));
        assert_eq!(entry.tint, TintRole::MutedForeground);
    }

    #[test]
    fn test_file_icon_for_yaml_and_yml_map_to_yaml_glyph_and_magenta_tint() {
        assert_eq!(
            file_icon_for("ci.yaml").glyph,
            Glyph::Svg("file_icons/yaml.svg")
        );
        assert_eq!(file_icon_for("ci.yaml").tint, TintRole::Magenta);
        assert_eq!(
            file_icon_for("ci.yml").glyph,
            Glyph::Svg("file_icons/yaml.svg")
        );
    }

    #[test]
    fn test_file_icon_for_html_maps_to_html_glyph_and_warning_tint() {
        let entry = file_icon_for("index.html");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/html.svg"));
        assert_eq!(entry.tint, TintRole::Warning);
    }

    #[test]
    fn test_file_icon_for_css_maps_to_css_glyph_and_primary_tint() {
        let entry = file_icon_for("style.css");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/css.svg"));
        assert_eq!(entry.tint, TintRole::Primary);
    }

    #[test]
    fn test_file_icon_for_scss_and_sass_map_to_sass_glyph_and_magenta_tint() {
        assert_eq!(
            file_icon_for("style.scss").glyph,
            Glyph::Svg("file_icons/sass.svg")
        );
        assert_eq!(file_icon_for("style.scss").tint, TintRole::Magenta);
        assert_eq!(
            file_icon_for("style.sass").glyph,
            Glyph::Svg("file_icons/sass.svg")
        );
    }

    #[test]
    fn test_file_icon_for_sh_maps_to_shell_glyph_and_green_tint() {
        let entry = file_icon_for("build.sh");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/shell.svg"));
        assert_eq!(entry.tint, TintRole::Green);
    }

    #[test]
    fn test_file_icon_for_lock_maps_to_lock_glyph_and_muted_foreground_tint() {
        let entry = file_icon_for("Cargo.lock");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/lock.svg"));
        assert_eq!(entry.tint, TintRole::MutedForeground);
    }

    #[test]
    fn test_file_icon_for_gitattributes_maps_on_full_leaf() {
        let entry = file_icon_for(".gitattributes");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/git_ignore.svg"));
        assert_eq!(entry.tint, TintRole::Warning);
    }

    #[test]
    fn test_file_icon_for_license_maps_on_full_leaf_case_insensitive() {
        let entry = file_icon_for("LICENSE");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/license.svg"));
    }

    #[test]
    fn test_file_icon_for_dockerfile_maps_to_docker_glyph_and_primary_tint() {
        let entry = file_icon_for("Dockerfile");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/docker.svg"));
        assert_eq!(entry.tint, TintRole::Primary);
    }

    #[test]
    fn test_file_icon_for_containerfile_maps_to_docker_glyph() {
        let entry = file_icon_for("Containerfile");
        assert_eq!(entry.glyph, Glyph::Svg("file_icons/docker.svg"));
    }
}
