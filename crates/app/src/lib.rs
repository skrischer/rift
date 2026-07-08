use std::collections::HashMap;
use std::rc::Rc;

use gpui::{App, SharedString, Window};
use gpui_component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use tracing::{error, warn};

#[cfg(feature = "gallery")]
pub mod gallery;

pub mod activity_rail;
pub mod command_palette;
pub mod command_registry;
pub mod connection_screen;
pub mod diff_view;
pub mod editor;
pub mod file_tree;
pub mod outline_panel;
pub mod problems_panel;
pub mod recents;
pub mod results_panel;
pub mod settings;
pub mod source_control;
pub mod status_bar;
pub mod terminal_panel;
pub mod title_bar;
pub mod window_state;
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

/// The theme rift activates when no other selection is in effect: `apply_theme`'s
/// hardcoded default, and `window_state::WindowState::default`'s theme_name when
/// no preference is on disk yet.
pub const DEFAULT_THEME_NAME: &str = CATPPUCCIN_MOCHA_THEME_NAME;

/// Register rift's Catppuccin theme in the `ThemeRegistry` — the shared first
/// half of both `apply_theme` (hardcoded default) and `apply_persisted_theme`
/// (Phase 17 restore). Returns `false`, having already logged, when the
/// registry rejected the theme; callers skip activating anything in that case.
fn register_themes(cx: &mut App) -> bool {
    if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(CATPPUCCIN_MOCHA) {
        error!(%e, "failed to load catppuccin theme");
        return false;
    }
    true
}

/// Register the Catppuccin theme and activate rift's default, so all
/// gpui-component widgets render in rift's palette instead of the default light
/// theme `gpui_component::init` starts with.
///
/// Shared by both binaries (`rift` and `gallery`) so they activate the identical
/// theme — same-palette parity is a hard requirement (PR #34 lesson), and a copy
/// would drift.
pub fn apply_theme(cx: &mut App) {
    if register_themes(cx) {
        set_theme(DEFAULT_THEME_NAME, None, cx);
    }
}

/// Register the Catppuccin theme and activate the persisted preference from
/// `state` instead of the hardcoded default — the startup restore counterpart
/// to `apply_theme` (`docs/spec-theme-settings.md`). `set_theme` already
/// degrades an unknown persisted name to `DEFAULT_THEME_NAME`; `theme_mode` is
/// re-applied afterward so a mode toggled independently of the named theme
/// (`set_theme_mode`) survives a restart too. A persisted font-family override
/// (issue #608) is applied last, and only when non-empty — an empty field
/// means "no override", so the resolved theme's own font stands, exactly like
/// a fresh store predating these two fields.
pub fn apply_persisted_theme(state: &window_state::WindowState, cx: &mut App) {
    if register_themes(cx) {
        set_theme(&state.theme_name, None, cx);
        set_theme_mode(state.theme_mode, None, cx);
        if !state.ui_font_family.is_empty() {
            set_ui_font(&state.ui_font_family, None, cx);
        }
        if !state.mono_font_family.is_empty() {
            set_mono_font(&state.mono_font_family, None, cx);
        }
    }
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

/// `Theme::change`, then ensure a repaint reaches the screen even without a
/// `Window` handle (issue #493). `gpui-component`'s `SettingField` setters —
/// what the settings surface's dropdowns dispatch through — only ever hand
/// their closure a `cx: &mut App`, no window; `Theme::change` only calls
/// `window.refresh()` when given one, so a `None` window silently skipped the
/// repaint and the dialog sat stale until the next unrelated notification.
/// `App::refresh_windows` schedules every open window for redraw and is the
/// same fallback `gpui-component`'s own `ThemeRegistry` reload path uses for
/// this exact gap.
fn apply_theme_mode(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    let has_window = window.is_some();
    Theme::change(mode, window, cx);
    if !has_window {
        cx.refresh_windows();
    }
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
    apply_theme_mode(mode, window, cx);
}

/// Switch only the light/dark mode, keeping whichever named theme is currently
/// assigned to that slot (set by the last `set_theme` call, or `apply_theme` at
/// startup). A thin, discoverable wrapper around `Theme::change` for the
/// settings surface and command palette to dispatch (`docs/spec-theme-settings.md`).
pub fn set_theme_mode(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    apply_theme_mode(mode, window, cx);
}

/// Flip between light and dark mode, keeping whichever named theme is
/// currently assigned to each slot — the command palette's "Toggle Light/Dark
/// Theme" entry (`docs/spec-theme-settings.md`).
pub fn toggle_theme_mode(window: Option<&mut Window>, cx: &mut App) {
    let mode = if Theme::global(cx).mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    set_theme_mode(mode, window, cx);
}

// ── Persisting variants ──────────────────────────────────────────────────────
//
// Issue #365: a theme change from either UI surface (the command palette's
// theme actions, wired in `workspace.rs`, or the settings surface in
// `settings.rs`) should survive a restart. These wrap the live-apply
// functions above with a save into the window-state store, reading the
// now-active theme back from `Theme::global` so the persisted name/mode
// always match what was actually applied. Never called from
// `apply_persisted_theme` (startup restore) — that would re-save unchanged
// state on every launch.
//
// A named-theme selection persists name + mode; a mode flip persists the
// mode alone. `Theme::change` only swaps which slot is active, and
// `Theme::theme_name()` reports the now-active slot's theme — so saving
// name + mode on a mode flip would overwrite the persisted named selection
// with the other slot's theme (issue #443).

/// Save the currently active theme (name + mode, read from the live `Theme`
/// global) into the window-state store at `path` — the persist half of a
/// named-theme selection ([`set_theme_persisted`]). Split out from
/// [`persist_theme`] so tests can exercise it against a scratch path instead
/// of the live platform state directory.
fn persist_theme_to(path: &std::path::Path, cx: &App) -> Result<(), window_state::StoreError> {
    let theme = Theme::global(cx);
    window_state::save_theme(path, theme.theme_name(), theme.mode)
}

/// Save only the currently active mode (read from the live `Theme` global)
/// into the window-state store at `path`, leaving the persisted named-theme
/// selection untouched — the persist half of a mode flip
/// ([`set_theme_mode_persisted`], [`toggle_theme_mode_persisted`]; issue
/// #443). Split out from [`persist_theme_mode`] so tests can exercise it
/// against a scratch path, mirroring [`persist_theme_to`].
fn persist_theme_mode_to(path: &std::path::Path, cx: &App) -> Result<(), window_state::StoreError> {
    window_state::save_theme_mode(path, Theme::global(cx).mode)
}

/// Resolve the live platform state path — the same one `apply_persisted_theme`
/// restores from — and run `save_to` against it. Best-effort: a missing
/// platform state directory or a write failure only logs; the live theme
/// change already applied regardless.
fn persist_best_effort(
    save_to: impl FnOnce(&std::path::Path) -> Result<(), window_state::StoreError>,
) {
    match window_state::state_path() {
        Ok(path) => {
            if let Err(e) = save_to(path.as_path()) {
                warn!(%e, "failed to persist theme change");
            }
        }
        Err(e) => warn!(%e, "no platform state directory, theme change not persisted"),
    }
}

/// Persist the active theme (name + mode) into the live platform state path.
fn persist_theme(cx: &App) {
    persist_best_effort(|path| persist_theme_to(path, cx));
}

/// Persist only the active mode into the live platform state path.
fn persist_theme_mode(cx: &App) {
    persist_best_effort(|path| persist_theme_mode_to(path, cx));
}

/// [`set_theme`], then persists the result so it survives a restart.
pub fn set_theme_persisted(name: &str, window: Option<&mut Window>, cx: &mut App) {
    set_theme(name, window, cx);
    persist_theme(cx);
}

/// [`set_theme_mode`], then persists the mode so the flip survives a restart —
/// mode only, so the persisted named-theme selection is not overwritten with
/// the other slot's theme (issue #443).
pub fn set_theme_mode_persisted(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    set_theme_mode(mode, window, cx);
    persist_theme_mode(cx);
}

/// [`toggle_theme_mode`], then persists the mode so the flip survives a
/// restart — mode only, like [`set_theme_mode_persisted`].
pub fn toggle_theme_mode_persisted(window: Option<&mut Window>, cx: &mut App) {
    let mode = if Theme::global(cx).mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    set_theme_mode_persisted(mode, window, cx);
}

// ── Font-family setters (issue #608) ─────────────────────────────────────────
//
// The Appearance page's "UI font" / "Editor & panes font" dropdowns
// (`crate::settings`) mutate `Theme::global`'s `font_family` /
// `mono_font_family` directly rather than through `Theme::change` — no
// `ThemeConfig`/color reapplication is needed, just the one field. `Root`'s
// own render already reads `cx.theme().font_family` as the whole app's base
// text style, and every mono-font call site already reads
// `cx.theme().mono_font_family` (editor, status bar, source control, diff
// view, outline/results panels, title bar, connection screen), so both take
// effect live with no further wiring. The raw terminal PTY grid is the one
// deliberate exception: `rift_terminal`'s `session_view`/`pane_view` pin a
// Nerd Font mono variant for their icon glyphs and tie cell measurement to
// it, so they stay off this setting (out of this issue's scope).

/// Switch the UI font family live, without touching any other `Theme` field.
/// Mirrors [`apply_theme_mode`]'s window-refresh fallback (issue #493): a
/// `SettingField` setter only ever hands its closure `cx: &mut App`, never a
/// `Window`, and a plain global mutation does not by itself schedule a
/// repaint.
pub fn set_ui_font(name: &str, window: Option<&mut Window>, cx: &mut App) {
    Theme::global_mut(cx).font_family = SharedString::from(name.to_string());
    match window {
        Some(window) => window.refresh(),
        None => cx.refresh_windows(),
    }
}

/// Switch the editor/status-bar/source-control mono font family live, mirroring
/// [`set_ui_font`]'s shape and refresh fallback.
pub fn set_mono_font(name: &str, window: Option<&mut Window>, cx: &mut App) {
    Theme::global_mut(cx).mono_font_family = SharedString::from(name.to_string());
    match window {
        Some(window) => window.refresh(),
        None => cx.refresh_windows(),
    }
}

/// [`set_ui_font`], then persists the choice so it survives a restart.
pub fn set_ui_font_persisted(name: &str, window: Option<&mut Window>, cx: &mut App) {
    set_ui_font(name, window, cx);
    persist_best_effort(|path| window_state::save_ui_font_family(path, name));
}

/// [`set_mono_font`], then persists the choice so it survives a restart.
pub fn set_mono_font_persisted(name: &str, window: Option<&mut Window>, cx: &mut App) {
    set_mono_font(name, window, cx);
    persist_best_effort(|path| window_state::save_mono_font_family(path, name));
}

// ── Command-palette theme actions ────────────────────────────────────────────
//
// Dispatchable, parameterless actions (issue #367, `docs/spec-theme-settings.md`):
// registered in `command_registry::COMMANDS` (#358) so the palette can
// discover and dispatch them, and wired to the functions above via
// `on_action` handlers in `workspace::WorkspaceView::render` — mirroring how
// the Phase 16 shell command actions in `workspace.rs` are defined beside
// what they target and wired at the render root.

/// Toggle between light and dark mode.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ToggleThemeMode;

/// Select `gpui-component`'s bundled light theme.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectDefaultLightTheme;

/// Select `gpui-component`'s bundled dark theme.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectDefaultDarkTheme;

/// Select rift's own Catppuccin Mocha theme.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectCatppuccinMochaTheme;

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
    use crate::window_state::{self, WindowState};
    use crate::{
        apply_persisted_theme, apply_theme, persist_theme_mode_to, persist_theme_to, resolve_theme,
        set_mono_font, set_theme, set_theme_mode, set_ui_font, toggle_theme_mode,
        CATPPUCCIN_MOCHA_THEME_NAME, DEFAULT_LIGHT_THEME_NAME, DEFAULT_THEME_NAME,
    };

    fn theme_config(name: &str, mode: ThemeMode) -> ThemeConfig {
        ThemeConfig {
            name: name.into(),
            mode,
            ..Default::default()
        }
    }

    /// A minimal view that counts its own `render` calls — lets a test
    /// observe whether an open window actually redrew, rather than only
    /// asserting on `Theme` global state.
    struct CountingView {
        render_count: Rc<std::cell::Cell<usize>>,
    }

    impl gpui::Render for CountingView {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            self.render_count.set(self.render_count.get() + 1);
            gpui::Empty
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

    /// The startup restore path activates whatever named theme was persisted,
    /// not the hardcoded `apply_theme` default.
    #[gpui::test]
    fn test_apply_persisted_theme_activates_the_stored_named_theme(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let state = WindowState {
                theme_name: DEFAULT_LIGHT_THEME_NAME.to_string(),
                theme_mode: ThemeMode::Light,
                ..WindowState::default()
            };

            apply_persisted_theme(&state, cx);

            assert!(!cx.theme().is_dark());
            assert_eq!(cx.theme().theme_name().as_ref(), DEFAULT_LIGHT_THEME_NAME);
        });
    }

    /// An unknown persisted theme name degrades to `DEFAULT_THEME_NAME` via
    /// `set_theme`'s existing fallback, mirroring #364's crash-avoidance
    /// guarantee for the restore path too.
    #[gpui::test]
    fn test_apply_persisted_theme_unknown_name_falls_back_to_default(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let state = WindowState {
                theme_name: "does not exist".to_string(),
                ..WindowState::default()
            };

            apply_persisted_theme(&state, cx);

            assert!(cx.theme().is_dark());
            assert_eq!(
                cx.theme().theme_name().as_ref(),
                CATPPUCCIN_MOCHA_THEME_NAME
            );
        });
    }

    /// `theme_mode` is re-applied after `theme_name`, so a mode persisted
    /// independently of the named theme (via `set_theme_mode`) still wins on
    /// restore instead of silently reverting to the named theme's own mode.
    #[gpui::test]
    fn test_apply_persisted_theme_reapplies_a_mode_independent_of_the_named_theme(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            // Catppuccin Mocha is a dark-mode theme; persisting it alongside an
            // explicit light mode simulates a user who toggled mode after
            // picking the named theme.
            let state = WindowState {
                theme_name: CATPPUCCIN_MOCHA_THEME_NAME.to_string(),
                theme_mode: ThemeMode::Light,
                ..WindowState::default()
            };

            apply_persisted_theme(&state, cx);

            assert!(!cx.theme().is_dark());
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

    /// Issue #493: the settings surface's dropdowns dispatch through
    /// `gpui-component`'s `SettingField` setters, whose closures only ever
    /// receive `cx: &mut App` — never a `Window` — so `set_theme_mode(_,
    /// None, _)` must still make an open window repaint on its own, instead
    /// of relying on `Theme::change`'s `window.refresh()` (which is skipped
    /// entirely when `window` is `None`). Verified by counting an open
    /// window's own `render` calls rather than only asserting on `Theme`
    /// global state, since a global mutation alone does not redraw a window.
    #[gpui::test]
    fn test_set_theme_mode_without_a_window_still_refreshes_open_windows(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);
        });

        let render_count = Rc::new(std::cell::Cell::new(0usize));
        let view_render_count = render_count.clone();
        let _window = cx.add_window(move |_, _| CountingView {
            render_count: view_render_count,
        });
        assert_eq!(render_count.get(), 1, "window draws once on creation");

        cx.update(|cx| {
            set_theme_mode(ThemeMode::Light, None, cx);
        });

        assert_eq!(
            render_count.get(),
            2,
            "refresh_windows should schedule a redraw even without a Window handle"
        );
    }

    /// The command palette's "Toggle Light/Dark Theme" entry flips dark to
    /// light and back, keeping whichever named theme is assigned to each slot.
    #[gpui::test]
    fn test_toggle_theme_mode_flips_dark_to_light_and_back(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);
            assert!(cx.theme().is_dark());

            toggle_theme_mode(None, cx);
            assert!(!cx.theme().is_dark());
            assert_eq!(cx.theme().theme_name().as_ref(), DEFAULT_LIGHT_THEME_NAME);

            toggle_theme_mode(None, cx);
            assert!(cx.theme().is_dark());
            assert_eq!(
                cx.theme().theme_name().as_ref(),
                CATPPUCCIN_MOCHA_THEME_NAME
            );
        });
    }

    /// The `_persisted` wrappers (issue #365) save the active theme via
    /// `window_state::save_theme` after applying it. Exercised at the
    /// `persist_theme_to` seam against a scratch path rather than through
    /// `set_theme_persisted` directly, since the public wrapper resolves the
    /// live platform state path (`window_state::state_path`) and a unit test
    /// must never touch the real one.
    #[gpui::test]
    fn test_persist_theme_to_writes_the_active_name_and_mode(cx: &mut TestAppContext) {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "rift-app-lib-persist-theme-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);
            set_theme(DEFAULT_LIGHT_THEME_NAME, None, cx);

            persist_theme_to(&path, cx).expect("persist_theme_to");
        });

        let loaded = window_state::load(&path);
        assert_eq!(loaded.theme_name, DEFAULT_LIGHT_THEME_NAME);
        assert_eq!(loaded.theme_mode, ThemeMode::Light);

        let _ = std::fs::remove_file(&path);
    }

    /// The regression #443 fixes, acceptance flow persist-side: select a named
    /// theme, toggle light/dark — the persisted store must keep the named
    /// selection and record only the flipped mode, not the other slot's theme
    /// name. (Restore-side, `apply_persisted_theme` re-applying a divergent
    /// mode is covered above.) Exercised at the `persist_*_to` seams for the
    /// same scratch-path reason as the previous test.
    #[gpui::test]
    fn test_persist_theme_mode_to_after_a_toggle_preserves_the_named_selection(
        cx: &mut TestAppContext,
    ) {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "rift-app-lib-persist-theme-mode-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);
            // The named selection: dark Catppuccin Mocha.
            persist_theme_to(&path, cx).expect("persist named selection");

            // Live mode flip: the active slot is now the light one
            // ("Default Light"), but the selection stays Mocha.
            toggle_theme_mode(None, cx);
            persist_theme_mode_to(&path, cx).expect("persist mode flip");
        });

        let loaded = window_state::load(&path);
        assert_eq!(loaded.theme_name, CATPPUCCIN_MOCHA_THEME_NAME);
        assert_eq!(loaded.theme_mode, ThemeMode::Light);

        let _ = std::fs::remove_file(&path);
    }

    // --- font-family setters (issue #608) -----------------------------------

    #[gpui::test]
    fn test_set_ui_font_updates_the_live_theme_font_family(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);

            set_ui_font("Inter", None, cx);

            assert_eq!(cx.theme().font_family.as_ref(), "Inter");
        });
    }

    #[gpui::test]
    fn test_set_mono_font_updates_the_live_theme_mono_font_family(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);

            set_mono_font("Consolas", None, cx);

            assert_eq!(cx.theme().mono_font_family.as_ref(), "Consolas");
        });
    }

    /// Same class of regression `test_set_theme_mode_without_a_window_still_refreshes_open_windows`
    /// guards against (issue #493): `set_ui_font` mutates `Theme::global`
    /// directly rather than through `Theme::change`, so it needs its own
    /// window-refresh fallback rather than inheriting one.
    #[gpui::test]
    fn test_set_ui_font_without_a_window_still_refreshes_open_windows(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply_theme(cx);
        });

        let render_count = Rc::new(std::cell::Cell::new(0usize));
        let view_render_count = render_count.clone();
        let _window = cx.add_window(move |_, _| CountingView {
            render_count: view_render_count,
        });
        assert_eq!(render_count.get(), 1, "window draws once on creation");

        cx.update(|cx| {
            set_ui_font("Inter", None, cx);
        });

        assert_eq!(
            render_count.get(),
            2,
            "refresh_windows should schedule a redraw even without a Window handle"
        );
    }

    /// A persisted font override (non-empty) is re-applied on restore, over
    /// whatever the resolved theme's own font would otherwise be.
    #[gpui::test]
    fn test_apply_persisted_theme_applies_a_persisted_font_override(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let state = WindowState {
                ui_font_family: "Segoe UI".to_string(),
                mono_font_family: "Cascadia Mono".to_string(),
                ..WindowState::default()
            };

            apply_persisted_theme(&state, cx);

            assert_eq!(cx.theme().font_family.as_ref(), "Segoe UI");
            assert_eq!(cx.theme().mono_font_family.as_ref(), "Cascadia Mono");
        });
    }

    /// Empty font fields (a fresh store, or one predating issue #608) leave
    /// the resolved theme's own font untouched rather than clobbering it with
    /// an empty family name.
    #[gpui::test]
    fn test_apply_persisted_theme_empty_font_fields_leave_the_theme_default(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let font_before_restore = cx.theme().font_family.clone();

            apply_persisted_theme(&WindowState::default(), cx);

            assert_eq!(cx.theme().font_family, font_before_restore);
            assert_ne!(cx.theme().font_family.as_ref(), "");
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
