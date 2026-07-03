use std::collections::HashMap;
use std::rc::Rc;

use gpui::{App, SharedString, Window};
use gpui_component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use tracing::{error, warn};

#[cfg(feature = "gallery")]
pub mod gallery;

pub mod command_palette;
pub mod command_registry;
pub mod diff_view;
pub mod editor;
pub mod file_tree;
pub mod problems_panel;
pub mod settings;
pub mod source_control;
pub mod status_bar;
pub mod terminal_panel;
pub mod workspace;
pub mod worktree;

/// Catppuccin Mocha theme in gpui-component's native theme format. Registered in
/// the `ThemeRegistry` alongside the built-in Light/Dark themes, giving rift's
/// runtime theme selection three named themes to choose from in v1
/// (`docs/spec-theme-settings.md`).
const CATPPUCCIN_MOCHA: &str = include_str!("../assets/themes/catppuccin-mocha.json");

/// Display name of gpui-component's bundled light theme, as loaded by
/// `gpui_component::init`.
pub const DEFAULT_LIGHT_THEME_NAME: &str = "Default Light";
/// Display name of gpui-component's bundled dark theme, as loaded by
/// `gpui_component::init`.
pub const DEFAULT_DARK_THEME_NAME: &str = "Default Dark";
/// Display name of rift's own theme, loaded by `apply_theme`.
pub const CATPPUCCIN_MOCHA_THEME_NAME: &str = "Catppuccin Mocha";

/// The theme rift activates when no other selection is in effect. Today this is
/// the sole entry point (startup); once Phase 17's persistence issue lands, a
/// stored preference is read and passed to `set_theme` instead.
pub const DEFAULT_THEME_NAME: &str = CATPPUCCIN_MOCHA_THEME_NAME;

/// Register the Catppuccin theme and activate rift's default, so all
/// gpui-component widgets render in rift's palette instead of the default light
/// theme `gpui_component::init` starts with.
///
/// Shared by both binaries (`rift` and `gallery`) so they activate the identical
/// theme — same-palette parity is a hard requirement (PR #34 lesson), and a copy
/// would drift.
pub fn apply_theme(cx: &mut App) {
    if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(CATPPUCCIN_MOCHA) {
        error!(%e, "failed to load catppuccin theme");
        return;
    }
    set_theme(DEFAULT_THEME_NAME, None, cx);
}

/// Look up `name` in the `ThemeRegistry`'s themes, falling back to
/// `DEFAULT_THEME_NAME` when it is unknown. Returns `None` only if the fallback
/// itself is not loaded either — the registry was never initialized, which
/// never happens once `apply_theme` has run once.
fn resolve_theme(
    themes: &HashMap<SharedString, Rc<ThemeConfig>>,
    name: &str,
) -> Option<Rc<ThemeConfig>> {
    themes
        .get(name)
        .or_else(|| themes.get(DEFAULT_THEME_NAME))
        .cloned()
}

/// Switch the active theme by name, live — the runtime counterpart to the
/// startup hardcode `apply_theme` used to be. Looks `name` up in the
/// `ThemeRegistry`, assigns it into whichever of `Theme`'s `light_theme` /
/// `dark_theme` slots matches the theme's own mode, then calls `Theme::change`
/// so every `ActiveTheme` reader — the whole UI — restyles immediately, no
/// restart.
///
/// An unknown or missing name degrades to `DEFAULT_THEME_NAME` (today's dark
/// default) rather than crashing; if even that is not loaded, the call is a
/// no-op, which only happens if `apply_theme` was never run.
pub fn set_theme(name: &str, window: Option<&mut Window>, cx: &mut App) {
    let Some(theme) = resolve_theme(ThemeRegistry::global(cx).themes(), name) else {
        error!(theme = name, "theme registry has no themes loaded");
        return;
    };
    if theme.name.as_ref() != name {
        warn!(
            requested = name,
            using = theme.name.as_ref(),
            "unknown theme name, falling back to default"
        );
    }
    let mode = theme.mode;
    if mode.is_dark() {
        Theme::global_mut(cx).dark_theme = theme;
    } else {
        Theme::global_mut(cx).light_theme = theme;
    }
    Theme::change(mode, window, cx);
}

/// Switch only the light/dark mode, keeping whichever named theme is currently
/// assigned to that slot (set by the last `set_theme` call, or `apply_theme` at
/// startup). A thin, discoverable wrapper around `Theme::change` for the
/// settings surface and command palette to dispatch (`docs/spec-theme-settings.md`).
pub fn set_theme_mode(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(mode, window, cx);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::rc::Rc;

    use gpui::{SharedString, TestAppContext};
    use gpui_component::dock::Panel as _;
    use gpui_component::{ActiveTheme as _, ThemeConfig, ThemeMode};

    use crate::diff_view::DIFF_VIEW_PANEL_NAME;
    use crate::editor::EDITOR_PANEL_NAME;
    use crate::file_tree::{FileTree, FILE_TREE_PANEL_NAME};
    use crate::problems_panel::PROBLEMS_PANEL_NAME;
    use crate::source_control::SOURCE_CONTROL_PANEL_NAME;
    use crate::terminal_panel::TERMINAL_PANEL_NAME;
    use crate::{
        apply_theme, resolve_theme, set_theme, set_theme_mode, CATPPUCCIN_MOCHA_THEME_NAME,
        DEFAULT_LIGHT_THEME_NAME, DEFAULT_THEME_NAME,
    };

    fn theme_config(name: &str, mode: ThemeMode) -> ThemeConfig {
        ThemeConfig {
            name: name.into(),
            mode,
            ..Default::default()
        }
    }

    #[test]
    fn test_resolve_theme_returns_the_requested_theme_when_known() {
        let mut themes = HashMap::new();
        themes.insert(
            SharedString::from("Light One"),
            Rc::new(theme_config("Light One", ThemeMode::Light)),
        );
        themes.insert(
            SharedString::from(DEFAULT_THEME_NAME),
            Rc::new(theme_config(DEFAULT_THEME_NAME, ThemeMode::Dark)),
        );

        let resolved = resolve_theme(&themes, "Light One").expect("theme is registered");
        assert_eq!(resolved.name.as_ref(), "Light One");
    }

    #[test]
    fn test_resolve_theme_falls_back_to_default_when_name_is_unknown() {
        let mut themes = HashMap::new();
        themes.insert(
            SharedString::from(DEFAULT_THEME_NAME),
            Rc::new(theme_config(DEFAULT_THEME_NAME, ThemeMode::Dark)),
        );

        let resolved = resolve_theme(&themes, "does not exist").expect("default is registered");
        assert_eq!(resolved.name.as_ref(), DEFAULT_THEME_NAME);
    }

    #[test]
    fn test_resolve_theme_returns_none_when_the_registry_is_empty() {
        let themes: HashMap<SharedString, Rc<ThemeConfig>> = HashMap::new();
        assert!(resolve_theme(&themes, "anything").is_none());
    }

    /// `apply_theme` replaces `gpui_component::init`'s light startup default
    /// with rift's dark Catppuccin Mocha (today's default, per `DEFAULT_THEME_NAME`).
    #[gpui::test]
    fn test_apply_theme_activates_catppuccin_mocha_dark(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);

            assert!(cx.theme().is_dark());
            assert_eq!(
                cx.theme().theme_name().as_ref(),
                CATPPUCCIN_MOCHA_THEME_NAME
            );
        });
    }

    /// Selecting a named theme restyles live: mode and theme name both flip to
    /// match the chosen theme's own `mode`, no restart.
    #[gpui::test]
    fn test_set_theme_switches_to_a_light_named_theme_live(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);
            assert!(cx.theme().is_dark());

            set_theme(DEFAULT_LIGHT_THEME_NAME, None, cx);

            assert!(!cx.theme().is_dark());
            assert_eq!(cx.theme().theme_name().as_ref(), DEFAULT_LIGHT_THEME_NAME);
        });
    }

    /// An unknown theme name degrades to `DEFAULT_THEME_NAME` rather than
    /// crashing or leaving the UI unstyled (acceptance criterion of #364).
    #[gpui::test]
    fn test_set_theme_unknown_name_falls_back_to_default(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);

            set_theme("does not exist", None, cx);

            assert!(cx.theme().is_dark());
            assert_eq!(
                cx.theme().theme_name().as_ref(),
                CATPPUCCIN_MOCHA_THEME_NAME
            );
        });
    }

    /// If the requested name and the fallback are both absent from the registry
    /// (Catppuccin Mocha not yet loaded), `set_theme` is a no-op rather than a
    /// crash — the active theme stays whatever it was.
    #[gpui::test]
    fn test_set_theme_is_a_no_op_when_no_theme_matches(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            assert!(!cx.theme().is_dark());

            set_theme("does not exist", None, cx);

            assert!(!cx.theme().is_dark());
        });
    }

    /// `set_theme_mode` toggles mode alone, reusing whichever named theme is
    /// already assigned to that slot rather than forgetting the earlier
    /// `set_theme` selection.
    #[gpui::test]
    fn test_set_theme_mode_reuses_the_previously_assigned_named_theme(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);
            set_theme(DEFAULT_LIGHT_THEME_NAME, None, cx);
            assert!(!cx.theme().is_dark());

            set_theme_mode(ThemeMode::Dark, None, cx);

            assert!(cx.theme().is_dark());
            assert_eq!(
                cx.theme().theme_name().as_ref(),
                CATPPUCCIN_MOCHA_THEME_NAME
            );
        });
    }

    /// `EditorView`, `TerminalPanel`, `ProblemsPanel`, `SourceControlPanel`, and
    /// `DiffView` need a live GPUI `Window`/`Context` to construct, so their
    /// `panel_name()` is asserted against the constant that backs the trait impl
    /// (the impl body is `EDITOR_PANEL_NAME` / `TERMINAL_PANEL_NAME` /
    /// `PROBLEMS_PANEL_NAME` / `SOURCE_CONTROL_PANEL_NAME` / `DIFF_VIEW_PANEL_NAME`
    /// verbatim — see `editor.rs` / `terminal_panel.rs` / `problems_panel.rs` /
    /// `source_control.rs` / `diff_view.rs`).
    /// `FileTree::new()` stays cx-free, so its call goes through the real
    /// `Panel::panel_name()` trait method.
    #[test]
    fn test_panel_names_are_stable_and_distinct() {
        assert_eq!(FileTree::new().panel_name(), FILE_TREE_PANEL_NAME);
        assert_eq!(FILE_TREE_PANEL_NAME, "explorer");
        assert_eq!(EDITOR_PANEL_NAME, "editor");
        assert_eq!(TERMINAL_PANEL_NAME, "terminal");
        assert_eq!(SOURCE_CONTROL_PANEL_NAME, "source-control");
        assert_eq!(PROBLEMS_PANEL_NAME, "problems");
        assert_eq!(DIFF_VIEW_PANEL_NAME, "diff-view");

        let names = [
            FILE_TREE_PANEL_NAME,
            EDITOR_PANEL_NAME,
            TERMINAL_PANEL_NAME,
            SOURCE_CONTROL_PANEL_NAME,
            PROBLEMS_PANEL_NAME,
            DIFF_VIEW_PANEL_NAME,
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b, "panel names must be distinct");
            }
        }
    }
}
