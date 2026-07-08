//! The settings surface (`docs/spec-settings-theme.md`, issue #607): the
//! multi-page `gpui-component::setting::Settings` shell — a sidebar (search +
//! page nav) beside the active page — hosted near-full-window in the `Root`
//! dialog overlay, the same pattern `command_palette` uses. **Appearance** is
//! the one populated page (issue #608): a Theme group of selectable cards —
//! one per theme registered in the `ThemeRegistry`, previewed from that
//! theme's own tokens — and a Font & size group (UI font, editor/terminal
//! mono font, whole-client font size). Every control is a view over live app
//! state ([`crate::set_theme_persisted`], [`crate::set_ui_font_persisted`],
//! [`crate::set_mono_font_persisted`], and `SessionView`'s font-zoom state —
//! the same state the command palette and `Ctrl+=`/`Ctrl+-` already mutate),
//! persisted via the window-state store, never a config file
//! (`docs/spec-dogfooding-channels.md`'s standing "no new config layer"
//! decision). The remaining sections (Connection, Keybindings, Editor,
//! Terminal, General, About) are shell structure only, populated in later
//! phases; there is deliberately no "Agents" section (agent-agnostic
//! constitution rule).

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::{
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, Theme, ThemeColor, ThemeConfig,
    ThemeRegistry, WindowExt as _,
};
use rift_terminal::{SessionView, MAX_FONT_SIZE, MIN_FONT_SIZE};

/// Open the settings surface. Bound with no key-context scope in `main.rs`
/// (mirroring [`crate::command_palette::OpenCommandPalette`]), so the
/// shortcut reaches it regardless of which surface is focused.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct OpenSettings;

/// Horizontal gutter (total, both sides) left around the near-full-window
/// settings surface, so it reads as a hosted surface rather than a fullscreen
/// takeover.
const HORIZONTAL_MARGIN: f32 = 160.0;
/// Vertical gutter (total, both sides).
const VERTICAL_MARGIN: f32 = 120.0;
/// Lower bound for the surface width, so a small window still shows the
/// sidebar and the active page side by side.
const MIN_WIDTH: f32 = 720.0;
/// Lower bound for the surface height.
const MIN_HEIGHT: f32 = 480.0;

/// The settings surface. Holds the terminal session entity so the font-scale
/// field can read/write `SessionView`'s whole-client font size directly — the
/// same state `Ctrl+=`/`Ctrl+-` already mutate.
pub struct SettingsView {
    session: Entity<SessionView>,
}

impl SettingsView {
    pub fn new(session: Entity<SessionView>) -> Self {
        Self { session }
    }

    /// Open the settings surface as a near-full-window `Root` dialog hosting
    /// the multi-page `setting::Settings` shell.
    ///
    /// The theme registry's entries and the system font list are both
    /// snapshotted once, here, rather than re-read inside the dialog's build
    /// closure: neither changes while the dialog is open (rift never calls
    /// `ThemeRegistry::watch_dir`), so reading them once avoids re-enumerating
    /// system fonts on every dialog repaint.
    pub fn open(&self, window: &mut Window, cx: &mut App) {
        let session = self.session.clone();
        let theme_configs = registered_theme_configs(cx);
        let font_names = system_font_names(cx);
        window.open_dialog(cx, move |dialog, window, _cx| {
            let viewport = window.viewport_size();
            let width = (f32::from(viewport.width) - HORIZONTAL_MARGIN).max(MIN_WIDTH);
            let height = (f32::from(viewport.height) - VERTICAL_MARGIN).max(MIN_HEIGHT);
            dialog.title("Settings").w(px(width)).h(px(height)).child(
                Settings::new("app-settings").pages(settings_pages(
                    session.clone(),
                    theme_configs.clone(),
                    font_names.clone(),
                )),
            )
        });
    }
}

/// Every theme registered in the `ThemeRegistry`, in
/// [`ThemeRegistry::sorted_themes`]'s stable order (default themes first,
/// light before dark, then alphabetical) — the Theme group's card order.
fn registered_theme_configs(cx: &App) -> Vec<Rc<ThemeConfig>> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .cloned()
        .collect()
}

/// Every font family the OS reports, sorted and deduplicated by
/// `gpui`'s `TextSystem` — the option list for the "UI font" / "Editor &
/// terminal font" dropdowns. A real, live list rather than a curated
/// hardcoded set, so the picker never offers a font that is not actually
/// installed.
fn system_font_names(cx: &App) -> Vec<SharedString> {
    cx.text_system()
        .all_font_names()
        .into_iter()
        .map(SharedString::from)
        .collect()
}

/// Every settings page: the populated **Appearance** page first (index 0, so
/// it is the default-selected page) followed by the shell-only sections
/// (`docs/spec-settings-theme.md`, issue #607).
fn settings_pages(
    session: Entity<SessionView>,
    theme_configs: Vec<Rc<ThemeConfig>>,
    font_names: Vec<SharedString>,
) -> Vec<SettingPage> {
    let mut pages = vec![appearance_page(session, theme_configs, font_names)];
    pages.extend(shell_pages());
    pages
}

/// The shell-only sections — structure without content, populated in later
/// phases as real client state appears. There is deliberately no "Agents"
/// section: agent-specific settings violate the agent-agnostic constitution
/// rule.
fn shell_pages() -> Vec<SettingPage> {
    shell_sections()
        .into_iter()
        .map(|(title, icon, note)| shell_page(title, icon, note))
        .collect()
}

/// The shell-only sections as `(nav title, nav icon, placeholder note)`. The
/// note names what the section will hold; it is informational, never a
/// non-functional control (`spec-cockpit-chrome.md`: "no dead icons").
fn shell_sections() -> Vec<(SharedString, IconName, SharedString)> {
    vec![
        (
            "Connection".into(),
            IconName::Network,
            "SSH host, port and session settings will appear here.".into(),
        ),
        (
            "Keybindings".into(),
            IconName::Asterisk,
            "Keyboard shortcut customization will appear here.".into(),
        ),
        (
            "Editor".into(),
            IconName::File,
            "Editor preferences will appear here.".into(),
        ),
        (
            "Terminal".into(),
            IconName::SquareTerminal,
            "Terminal preferences will appear here.".into(),
        ),
        (
            "General".into(),
            IconName::Settings,
            "General application preferences will appear here.".into(),
        ),
        (
            "About".into(),
            IconName::Info,
            format!(
                "rift {} - open source, always free.",
                env!("CARGO_PKG_VERSION")
            )
            .into(),
        ),
    ]
}

/// Build a shell-only section page: a single group whose one informational
/// item names the section. The item is required so the page survives the
/// widget's empty-page filter (`filtered_pages`) and appears in the nav — the
/// same `SettingItem::render` element the vendored "About" page uses — without
/// shipping a non-functional control.
fn shell_page(title: SharedString, icon: IconName, note: SharedString) -> SettingPage {
    SettingPage::new(title)
        .icon(icon)
        .group(
            SettingGroup::new().item(SettingItem::render(move |_options, _window, cx| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(note.clone())
            })),
        )
}

/// The single "Appearance" page: a Theme group of registry-driven cards and a
/// Font & size group (issue #608).
fn appearance_page(
    session: Entity<SessionView>,
    theme_configs: Vec<Rc<ThemeConfig>>,
    font_names: Vec<SharedString>,
) -> SettingPage {
    let font_get = session.clone();
    let font_set = session;
    let ui_font_options = font_dropdown_options(&font_names);
    let mono_font_options = font_dropdown_options(&font_names);

    SettingPage::new("Appearance")
        .default_open(true)
        .groups(vec![
            SettingGroup::new().title("Theme").item(SettingItem::render(
                move |_options, _window, cx| theme_cards(&theme_configs, cx),
            )),
            SettingGroup::new().title("Font & Size").items(vec![
                SettingItem::new(
                    "UI font",
                    SettingField::scrollable_dropdown(
                        ui_font_options,
                        |cx: &App| cx.theme().font_family.clone(),
                        |val: SharedString, cx: &mut App| {
                            crate::set_ui_font_persisted(val.as_ref(), None, cx)
                        },
                    ),
                )
                .description("Used for the interface chrome."),
                // "Panes" here means the dock panels that already read
                // `cx.theme().mono_font_family` (editor, status bar, source
                // control, diff view, outline/results panels) — not the raw
                // terminal PTY grid, which `rift_terminal` pins to a Nerd Font
                // mono variant for its icon glyphs and ties to cell-size
                // measurement; wiring that grid to this setting is out of this
                // issue's scope.
                SettingItem::new(
                    "Editor & terminal font",
                    SettingField::scrollable_dropdown(
                        mono_font_options,
                        |cx: &App| cx.theme().mono_font_family.clone(),
                        |val: SharedString, cx: &mut App| {
                            crate::set_mono_font_persisted(val.as_ref(), None, cx)
                        },
                    ),
                )
                .description("Monospace for code and panes."),
                SettingItem::new(
                    "Font size",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: f64::from(MIN_FONT_SIZE),
                            max: f64::from(MAX_FONT_SIZE),
                            step: 1.0,
                        },
                        move |cx: &App| f64::from(f32::from(font_get.read(cx).font_size())),
                        move |val: f64, cx: &mut App| {
                            font_set.update(cx, |session, cx| {
                                session.set_font_size(px(val as f32), cx);
                            });
                        },
                    ),
                )
                .description("Base size for editor and terminal (Ctrl+=/Ctrl+- also adjust this)."),
            ]),
        ])
}

/// Pair each font name with itself as both the dropdown's stored value and
/// its displayed label — the option shape `SettingField::scrollable_dropdown`
/// expects.
fn font_dropdown_options(names: &[SharedString]) -> Vec<(SharedString, SharedString)> {
    names
        .iter()
        .map(|name| (name.clone(), name.clone()))
        .collect()
}

/// The Theme group's row of cards, one per `configs` entry, wrapping onto a
/// new line if the settings surface is narrow. Read fresh on every render
/// (via `cx.theme()`) so a theme picked from the command palette or another
/// card marks the right card active without reopening the dialog.
fn theme_cards(configs: &[Rc<ThemeConfig>], cx: &App) -> impl IntoElement {
    let active_name = cx.theme().theme_name().clone();
    h_flex().flex_wrap().gap(px(16.0)).children(
        configs
            .iter()
            .map(|config| theme_card(config, config.name == active_name, cx)),
    )
}

/// One selectable theme card: a preview swatch composed from `config`'s own
/// tokens, a radio dot + name label below it, and a click handler that drives
/// [`crate::set_theme_persisted`] — the whole card is clickable, not just the
/// radio dot.
fn theme_card(config: &Rc<ThemeConfig>, is_active: bool, cx: &App) -> impl IntoElement {
    let colors = preview_colors(config);
    let name = config.name.clone();
    let click_name = name.clone();

    v_flex()
        .id(SharedString::from(format!("theme-card-{name}")))
        .cursor_pointer()
        .w(px(156.0))
        .gap(px(10.0))
        .on_click(move |_event, _window, cx| {
            crate::set_theme_persisted(click_name.as_ref(), None, cx);
        })
        .child(theme_swatch(colors, is_active))
        .child(
            h_flex()
                .items_center()
                .gap(px(8.0))
                .child(radio_dot(colors, is_active))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(name),
                ),
        )
}

/// Resolve `config`'s full color set without touching the live global
/// `Theme` — a scratch `Theme::default()` overlaid with `config` via
/// `gpui-component`'s own `Theme::apply_config`, so the preview always
/// matches exactly what selecting the theme would actually apply (same
/// fallback rules, same per-mode base), with zero color-parsing code of our
/// own to keep in sync.
fn preview_colors(config: &Rc<ThemeConfig>) -> ThemeColor {
    let mut preview = Theme::default();
    preview.apply_config(config);
    preview.colors
}

/// A small preview panel: a mock window-chrome dot row plus three content
/// bars, all colored from the theme's own tokens — never a hardcoded hex
/// (constitution: theme tokens only). The active theme's card gets a filled
/// checkmark badge in the corner.
fn theme_swatch(colors: ThemeColor, is_active: bool) -> impl IntoElement {
    v_flex()
        .relative()
        .h(px(100.0))
        .rounded(px(10.0))
        .p(px(11.0))
        .gap(px(6.0))
        .overflow_hidden()
        .bg(colors.background)
        .border_1()
        .border_color(if is_active {
            colors.primary
        } else {
            colors.border
        })
        .child(
            h_flex()
                .gap(px(4.0))
                .child(swatch_dot(colors.red))
                .child(swatch_dot(colors.yellow))
                .child(swatch_dot(colors.green)),
        )
        .child(swatch_bar(colors.primary, px(84.0)))
        .child(swatch_bar(colors.border, px(114.0)))
        .child(swatch_bar(colors.green, px(64.0)))
        .when(is_active, |this| {
            this.child(
                div()
                    .absolute()
                    .top(px(8.0))
                    .right(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.0))
                    .rounded_full()
                    .bg(colors.primary)
                    .child(
                        Icon::new(IconName::Check)
                            .xsmall()
                            .text_color(colors.primary_foreground),
                    ),
            )
        })
}

/// One small rounded dot in the swatch's mock window-chrome row.
fn swatch_dot(color: Hsla) -> impl IntoElement {
    div().flex_none().size(px(6.0)).rounded_full().bg(color)
}

/// One mock content line in the swatch.
fn swatch_bar(color: Hsla, width: Pixels) -> impl IntoElement {
    div()
        .flex_none()
        .h(px(5.0))
        .w(width)
        .rounded(px(3.0))
        .bg(color)
}

/// The radio indicator in a card's label row: a hollow ring, filled with the
/// theme's own primary color when that card is the active selection.
fn radio_dot(colors: ThemeColor, is_active: bool) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(15.0))
        .rounded_full()
        .border_1()
        .border_color(if is_active {
            colors.primary
        } else {
            colors.border
        })
        .flex()
        .items_center()
        .justify_center()
        .when(is_active, |this| {
            this.child(div().size(px(7.0)).rounded_full().bg(colors.primary))
        })
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use gpui_component::ThemeMode;

    use super::*;
    use crate::{CATPPUCCIN_MOCHA_THEME_NAME, DEFAULT_DARK_THEME_NAME, DEFAULT_LIGHT_THEME_NAME};

    /// The shell exposes every planned section, in order, and — per the
    /// agent-agnostic constitution rule — never an "Agents" section (issue
    /// #607).
    #[test]
    fn test_shell_sections_cover_all_areas_and_exclude_agents() {
        let sections = shell_sections();
        let titles: Vec<&str> = sections
            .iter()
            .map(|(title, _, _)| title.as_ref())
            .collect();
        assert_eq!(
            titles,
            [
                "Connection",
                "Keybindings",
                "Editor",
                "Terminal",
                "General",
                "About"
            ],
        );
        assert!(
            !titles.iter().any(|t| t.eq_ignore_ascii_case("agents")),
            "no agent-specific settings section"
        );
    }

    /// Each shell section maps to exactly one nav page.
    #[test]
    fn test_shell_pages_one_per_section() {
        assert_eq!(shell_pages().len(), shell_sections().len());
    }

    /// The Theme group's card list is data-driven from the `ThemeRegistry`,
    /// not a hardcoded set — v1 resolves to Default Light, Default Dark,
    /// Catppuccin Mocha, in `sorted_themes`'s stable order (issue #608).
    #[gpui::test]
    fn test_registered_theme_configs_lists_every_theme_in_stable_order(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::apply_theme(cx);

            let names: Vec<String> = registered_theme_configs(cx)
                .iter()
                .map(|config| config.name.to_string())
                .collect();

            assert_eq!(
                names,
                vec![
                    DEFAULT_LIGHT_THEME_NAME.to_string(),
                    DEFAULT_DARK_THEME_NAME.to_string(),
                    CATPPUCCIN_MOCHA_THEME_NAME.to_string(),
                ]
            );
        });
    }

    #[test]
    fn test_font_dropdown_options_pairs_each_name_with_itself() {
        let names = vec![SharedString::from("Inter"), SharedString::from("Consolas")];

        let options = font_dropdown_options(&names);

        assert_eq!(
            options,
            vec![
                (SharedString::from("Inter"), SharedString::from("Inter")),
                (
                    SharedString::from("Consolas"),
                    SharedString::from("Consolas")
                ),
            ]
        );
    }

    /// `preview_colors` reflects a config's own color override without
    /// mutating the live global `Theme`. Built via `serde_json` rather than a
    /// `ThemeConfigColors` struct literal: some of its fields (the ANSI
    /// `base.*` keys issue #609 will read) are crate-private in
    /// `gpui-component`, deserialization-only from outside the crate.
    #[test]
    fn test_preview_colors_reflects_the_configs_own_background_override() {
        let json = r##"{"name": "Test", "mode": "dark", "colors": {"background": "#1e1e2e"}}"##;
        let config = Rc::new(serde_json::from_str::<ThemeConfig>(json).expect("parse config"));

        let colors = preview_colors(&config);

        assert_ne!(colors.background, ThemeColor::default().background);
    }

    /// Two configs that differ only in `mode` resolve to different base
    /// colors, proving the preview honors each theme's own mode rather than
    /// always resolving against one hardcoded base.
    #[test]
    fn test_preview_colors_differs_between_light_and_dark_mode() {
        let light = Rc::new(ThemeConfig {
            name: "Light".into(),
            mode: ThemeMode::Light,
            ..Default::default()
        });
        let dark = Rc::new(ThemeConfig {
            name: "Dark".into(),
            mode: ThemeMode::Dark,
            ..Default::default()
        });

        assert_ne!(
            preview_colors(&light).background,
            preview_colors(&dark).background
        );
    }
}
