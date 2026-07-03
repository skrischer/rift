//! The settings surface (`docs/spec-theme-settings.md`, issue #366): a small
//! `gpui-component::setting::Settings` panel exposing theme mode, named
//! theme, and the whole-client font scale — hosted in the `Root` dialog
//! overlay, the same pattern `command_palette` uses. A view over live app
//! state (the `Theme` global via
//! [`crate::set_theme_persisted`]/[`crate::set_theme_mode_persisted`], and
//! `SessionView`'s font-zoom state — the "existing `Ctrl+=`/`Ctrl+-`" path
//! `docs/spec-window-state-persistence.md` refers to), not a config file
//! (`docs/spec-dogfooding-channels.md`'s standing "no new config layer"
//! decision). The theme dropdowns persist across a restart (issue #365); the
//! font scale field does not yet — that capture/restore wiring is issue
//! #225's scope.

use gpui::{px, App, Entity, ParentElement as _, SharedString, Styled as _, Window};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::{ActiveTheme as _, ThemeMode, WindowExt as _};
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

/// Width of the settings dialog: roomy enough for `setting::Settings`'s own
/// sidebar + page layout.
const DIALOG_WIDTH: f32 = 720.0;
/// Height of the settings dialog.
const DIALOG_HEIGHT: f32 = 480.0;

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

    /// Open the settings surface as a `Root` dialog.
    pub fn open(&self, window: &mut Window, cx: &mut App) {
        let session = self.session.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("Settings")
                .w(px(DIALOG_WIDTH))
                .h(px(DIALOG_HEIGHT))
                .child(Settings::new("app-settings").page(appearance_page(session.clone())))
        });
    }
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
