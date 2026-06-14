use gpui::{App, SharedString};
use gpui_component::{Theme, ThemeMode, ThemeRegistry};
use tracing::error;

#[cfg(feature = "gallery")]
pub mod gallery;

pub mod file_tree;
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
