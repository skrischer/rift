//! The custom 38px title bar (#511, `docs/spec-cockpit-chrome.md`): rift logo
//! + wordmark, connection/session group, settings gear, window controls.
//!
//! Renders identically on the Connection screen (a "not connected" group) and
//! the cockpit workspace (live connection state) — `main.rs` opens every
//! window with `gpui_component::TitleBar::title_bar_options()` and no native
//! OS chrome, so [`connection_screen::ConnectionScreen`] and
//! [`workspace::WorkspaceView`] each mount one of these as the first flex
//! child of their own render tree, replacing what the native bar used to
//! provide.
//!
//! Built on gpui-component's vendored `TitleBar` (gallery-proven) — window
//! drag, snap layouts, double-click-maximize, and the min/max/close controls
//! are that widget's own `WindowControlArea` wiring, never reimplemented
//! here. Its default 34px height is styled up to the design's 38px via its
//! `Styled` impl (the spec's prior decision: never fork the widget).
//!
//! The connection group's popover anchor (the phase-19 session switcher) is a
//! plain, inert label in this issue: #512 relocates the actual switcher
//! trigger into this slot as a follow-up (`Depends on: #511`).
//!
//! [`connection_screen::ConnectionScreen`]: crate::connection_screen::ConnectionScreen
//! [`workspace::WorkspaceView`]: crate::workspace::WorkspaceView

use gpui::{
    div, px, AnyElement, App, FontWeight, Hsla, IntoElement, ParentElement as _, Pixels,
    SharedString, Styled as _,
};
use gpui_component::{h_flex, ActiveTheme as _, Icon, IconName, Sizable as _, TitleBar};

/// Design-contract height of the custom title bar.
pub const HEIGHT: Pixels = px(38.0);

/// The connection/session group's live content: a status-dot color and the
/// label text beside it. Callers build one from their own state — the
/// cockpit's live `SessionView` fields, or [`ConnectionGroup::not_connected`]
/// on the Connection screen, before any session exists.
pub struct ConnectionGroup {
    pub dot_color: Hsla,
    pub label: SharedString,
}

impl ConnectionGroup {
    /// The Connection screen's group before any session exists: a muted dot
    /// and a plain "not connected" label — same anatomy as the cockpit's
    /// group, just no live session to describe yet.
    pub fn not_connected(cx: &App) -> Self {
        Self {
            dot_color: cx.theme().muted_foreground,
            label: SharedString::from("not connected"),
        }
    }
}

/// Build the custom title bar: the rift logo + wordmark flush against the
/// left edge, the connection group and (when supplied) the settings gear
/// flush against the window controls. `settings_button` is `None` on the
/// Connection screen — the settings surface needs a live `SessionView`
/// (#366), so no gear renders there rather than shipping a dead control
/// (the spec's "every rendered icon acts" constraint).
pub fn render(
    connection: ConnectionGroup,
    settings_button: Option<AnyElement>,
    cx: &App,
) -> impl IntoElement {
    TitleBar::new().h(HEIGHT).child(render_brand(cx)).child(
        h_flex()
            .items_center()
            .gap(px(12.0))
            .pr(px(4.0))
            .child(render_connection_group(connection, cx))
            .children(settings_button),
    )
}

/// The rift logo tile + mono-bold wordmark — the same icon the Connection
/// screen's big centered logo uses (`crate::connection_screen::render_logo`),
/// scaled down to fit the 38px bar.
fn render_brand(cx: &App) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_none()
                .size(px(20.0))
                .rounded(px(5.0))
                .bg(cx.theme().primary)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::SquareTerminal)
                        .xsmall()
                        .text_color(cx.theme().primary_foreground),
                ),
        )
        .child(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .font_weight(FontWeight::BOLD)
                .text_size(px(13.0))
                .text_color(cx.theme().foreground)
                .child("rift"),
        )
}

/// The connection/session group: a status dot plus its label, mono per the
/// design contract (session name, cwd, and every numeric render mono).
fn render_connection_group(connection: ConnectionGroup, cx: &App) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(12.0))
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(cx.theme().muted_foreground)
        .child(
            div()
                .flex_none()
                .size(px(6.0))
                .rounded_full()
                .bg(connection.dot_color),
        )
        .child(connection.label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn test_connection_group_not_connected_reads_muted_label_and_color(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let group = ConnectionGroup::not_connected(cx);
            assert_eq!(group.label.as_ref(), "not connected");
            assert_eq!(group.dot_color, cx.theme().muted_foreground);
        });
    }
}
