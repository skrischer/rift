//! The settings surface (`docs/spec-settings-theme.md`, issue #607): the
//! multi-page `gpui-component::setting::Settings` shell — a sidebar (search +
//! page nav) beside the active page — hosted near-full-window in the `Root`
//! dialog overlay, the same pattern `command_palette` uses. **Appearance** is
//! the one populated page (theme mode, named theme, whole-client font scale —
//! a view over live app state via
//! [`crate::set_theme_persisted`]/[`crate::set_theme_mode_persisted`] and
//! `SessionView`'s font-zoom state, not a config file per
//! `docs/spec-dogfooding-channels.md`'s standing "no new config layer"
//! decision). The remaining sections (Connection, Keybindings, Editor,
//! Terminal, General, About) are shell structure only, populated in later
//! phases; there is deliberately no "Agents" section (agent-agnostic
//! constitution rule). The theme dropdowns persist across a restart (issue
//! #365); the font scale field does not yet — that capture/restore wiring is
//! issue #225's scope.

use gpui::{div, px, App, Entity, ParentElement as _, SharedString, Styled as _, Window};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::{ActiveTheme as _, IconName, ThemeMode, WindowExt as _};
use rift_terminal::{SessionView, MAX_FONT_SIZE, MIN_FONT_SIZE};

use crate::{
    set_theme_mode_persisted, set_theme_persisted, CATPPUCCIN_MOCHA_THEME_NAME,
    DEFAULT_DARK_THEME_NAME, DEFAULT_LIGHT_THEME_NAME,
};

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
    pub fn open(&self, window: &mut Window, cx: &mut App) {
        let session = self.session.clone();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let viewport = window.viewport_size();
            let width = (f32::from(viewport.width) - HORIZONTAL_MARGIN).max(MIN_WIDTH);
            let height = (f32::from(viewport.height) - VERTICAL_MARGIN).max(MIN_HEIGHT);
            dialog
                .title("Settings")
                .w(px(width))
                .h(px(height))
                .child(Settings::new("app-settings").pages(settings_pages(session.clone())))
        });
    }
}

/// Every settings page: the populated **Appearance** page first (index 0, so
/// it is the default-selected page) followed by the shell-only sections
/// (`docs/spec-settings-theme.md`, issue #607).
fn settings_pages(session: Entity<SessionView>) -> Vec<SettingPage> {
    let mut pages = vec![appearance_page(session)];
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

/// The single "Appearance" page: theme mode, named theme, and font scale —
/// the three client UI preferences `docs/spec-theme-settings.md` scopes for
/// v1's settings surface.
fn appearance_page(session: Entity<SessionView>) -> SettingPage {
    let font_get = session.clone();
    let font_set = session;

    SettingPage::new("Appearance")
        .default_open(true)
        .groups(vec![
            SettingGroup::new().title("Theme").items(vec![
                SettingItem::new(
                    "Mode",
                    SettingField::dropdown(
                        vec![
                            ("light".into(), "Light".into()),
                            ("dark".into(), "Dark".into()),
                        ],
                        |cx: &App| {
                            SharedString::from(if cx.theme().mode.is_dark() {
                                "dark"
                            } else {
                                "light"
                            })
                        },
                        |val: SharedString, cx: &mut App| {
                            let mode = if val.as_ref() == "dark" {
                                ThemeMode::Dark
                            } else {
                                ThemeMode::Light
                            };
                            set_theme_mode_persisted(mode, None, cx);
                        },
                    ),
                )
                .description("Switch between light and dark."),
                SettingItem::new(
                    "Theme",
                    SettingField::dropdown(
                        theme_options(),
                        |cx: &App| cx.theme().theme_name().clone(),
                        |val: SharedString, cx: &mut App| {
                            set_theme_persisted(val.as_ref(), None, cx)
                        },
                    ),
                )
                .description("Pick a named theme."),
            ]),
            SettingGroup::new().title("Font").item(
                SettingItem::new(
                    "Font Scale",
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
                .description("Whole-client font size (Ctrl+=/Ctrl+- also adjust this)."),
            ),
        ])
}

/// The v1 named-theme picker options: rift's own default plus
/// `gpui-component`'s bundled Light/Dark (`docs/spec-theme-settings.md`: "a
/// ~3-theme v1 picker").
fn theme_options() -> Vec<(SharedString, SharedString)> {
    vec![
        (
            DEFAULT_LIGHT_THEME_NAME.into(),
            DEFAULT_LIGHT_THEME_NAME.into(),
        ),
        (
            DEFAULT_DARK_THEME_NAME.into(),
            DEFAULT_DARK_THEME_NAME.into(),
        ),
        (
            CATPPUCCIN_MOCHA_THEME_NAME.into(),
            CATPPUCCIN_MOCHA_THEME_NAME.into(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
