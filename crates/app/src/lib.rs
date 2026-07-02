use gpui::{App, SharedString};
use gpui_component::{Theme, ThemeMode, ThemeRegistry};
use tracing::error;

#[cfg(feature = "gallery")]
pub mod gallery;

pub mod editor;
pub mod file_tree;
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
    use crate::editor::EDITOR_PANEL_NAME;
    use crate::file_tree::FILE_TREE_PANEL_NAME;
    use crate::terminal_panel::TERMINAL_PANEL_NAME;

    /// Each dock panel's `Panel::panel_name` is a fixed `&'static str` chosen
    /// once at the trait impl (`crates/app/src/{file_tree,editor,terminal_panel}.rs`)
    /// and never derived from instance state — so asserting the module
    /// constants that back those impls is equivalent to asserting the trait
    /// method's return value, without needing a GPUI `App`/`Window` to
    /// construct an `EditorView` or `TerminalPanel` instance.
    #[test]
    fn test_panel_names_are_stable_and_distinct() {
        assert_eq!(FILE_TREE_PANEL_NAME, "explorer");
        assert_eq!(EDITOR_PANEL_NAME, "editor");
        assert_eq!(TERMINAL_PANEL_NAME, "terminal");

        let names = [FILE_TREE_PANEL_NAME, EDITOR_PANEL_NAME, TERMINAL_PANEL_NAME];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b, "panel names must be distinct");
            }
        }
    }
}
