use gpui::{App, SharedString};
use gpui_component::{Theme, ThemeMode, ThemeRegistry};
use tracing::error;

#[cfg(feature = "gallery")]
pub mod gallery;

pub mod editor;
pub mod file_tree;
pub mod source_control;
pub mod terminal_panel;
pub mod workspace;
pub mod worktree;

/// Catppuccin Mocha theme in gpui-component's native theme format. Registered in
/// the `ThemeRegistry` alongside the built-in Light/Dark themes, leaving room to
/// add more selectable themes later.
const CATPPUCCIN_MOCHA: &str = include_str!("../assets/themes/catppuccin-mocha.json");

/// Register the Catppuccin theme and make it the active app-wide theme so all
/// gpui-component widgets render in rift's palette instead of the default light theme.
///
/// Shared by both binaries (`rift` and `gallery`) so they activate the identical
/// theme — same-palette parity is a hard requirement (PR #34 lesson), and a copy
/// would drift.
pub fn apply_theme(cx: &mut App) {
    if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(CATPPUCCIN_MOCHA) {
        error!(%e, "failed to load catppuccin theme");
        return;
    }
    let Some(theme) = ThemeRegistry::global(cx)
        .themes()
        .get(&SharedString::from("Catppuccin Mocha"))
        .cloned()
    else {
        error!("catppuccin theme not found after load");
        return;
    };
    Theme::global_mut(cx).dark_theme = theme;
    Theme::change(ThemeMode::Dark, None, cx);
}

#[cfg(test)]
mod tests {
    use gpui_component::dock::Panel as _;

    use crate::editor::EDITOR_PANEL_NAME;
    use crate::file_tree::{FileTree, FILE_TREE_PANEL_NAME};
    use crate::source_control::SOURCE_CONTROL_PANEL_NAME;
    use crate::terminal_panel::TERMINAL_PANEL_NAME;

    /// `EditorView`, `TerminalPanel`, and `SourceControlPanel` need a live GPUI
    /// `Window`/`Context` to construct, so their `panel_name()` is asserted
    /// against the constant that backs the trait impl (the impl body is
    /// `EDITOR_PANEL_NAME` / `TERMINAL_PANEL_NAME` / `SOURCE_CONTROL_PANEL_NAME`
    /// verbatim — see `editor.rs` / `terminal_panel.rs` / `source_control.rs`).
    /// `FileTree::new()` stays cx-free, so its call goes through the real
    /// `Panel::panel_name()` trait method.
    #[test]
    fn test_panel_names_are_stable_and_distinct() {
        assert_eq!(FileTree::new().panel_name(), FILE_TREE_PANEL_NAME);
        assert_eq!(FILE_TREE_PANEL_NAME, "explorer");
        assert_eq!(EDITOR_PANEL_NAME, "editor");
        assert_eq!(TERMINAL_PANEL_NAME, "terminal");
        assert_eq!(SOURCE_CONTROL_PANEL_NAME, "source-control");

        let names = [
            FILE_TREE_PANEL_NAME,
            EDITOR_PANEL_NAME,
            TERMINAL_PANEL_NAME,
            SOURCE_CONTROL_PANEL_NAME,
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b, "panel names must be distinct");
            }
        }
    }
}
