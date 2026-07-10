//! The post-connect session picker (issue #706, `docs/spec-post-connect-picker.md`):
//! a pre-cockpit `Shell` state shown after the SSH connect + daemon handshake
//! and before the cockpit attach, but only while the session is unresolved.
//!
//! Issue #707 wires the full entry-point routing: a RECENT row's remembered
//! session resolves to `SessionIntent::Preferred` and attaches directly if
//! still present on the live host, else shows this picker; the plain
//! "Connect \u{2192}" button resolves to `SessionIntent::Pick` and always
//! shows it (issue #808 retires the `RIFT_SESSION`-driven `Fixed` fast-path
//! that used to bypass it). Issue #705 removes the connect card's Session
//! field entirely — `main.rs`'s entry point is the only session source now.
//!
//! Deliberately GPUI-view-only, mirroring [`crate::connection_screen::ConnectionScreen`]:
//! it emits [`SessionPickerEvent`] and never touches the daemon client or SSH
//! thread directly — `main.rs` owns the cross-thread coordination (the
//! `picker_list_tx`/`picker_choice_rx` pair) and drives the `Shell` state
//! swap on a pick.

use gpui::*;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName};

use rift_terminal::SessionListItem;

use crate::session_order;
use crate::title_bar;

/// Card width, matching [`crate::connection_screen`]'s own `CARD_WIDTH` (design
/// contract: "card ~470px") — this screen is styled as its direct sibling.
const CARD_WIDTH: f32 = 470.0;

/// The max height of the rows region before it scrolls internally (issue
/// #792: an unbounded list runs the card off-screen with many sessions),
/// matching `quick_open::QUICK_OPEN_LIST_MAX_HEIGHT` /
/// `command_palette::PALETTE_LIST_MAX_HEIGHT`'s own cap.
const ROWS_MAX_HEIGHT: f32 = 360.0;

/// One session row's display-ready fields: the pure mapping from the daemon's
/// [`SessionListItem`] (sorted by the client-side order store, exactly like
/// the cockpit strip) into what [`render_row`] draws. Split out from the
/// [`SessionPicker`] view so the mapping — the pluralized windows caption in
/// particular — is unit-testable without a GPUI window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    /// tmux's rename-stable session id (`SessionListItem::id`) — carried
    /// along for a stable row key; this PR's picker never renames or kills a
    /// row (that rides along in a later PR reusing the phase-32 affordances).
    pub id: u32,
    pub name: String,
    /// "1 window" / "N windows" — pluralized once here rather than in the
    /// render closure.
    pub windows_caption: SharedString,
    pub attached: bool,
    /// The session's project root (`SessionListItem::root`,
    /// `docs/spec-session-root-picker.md`), rendered as a secondary path
    /// label below the name. `None` when the session has never been stamped
    /// with `@root`.
    pub root: Option<String>,
}

/// Pluralize the window count for [`SessionRow::windows_caption`].
fn windows_caption(windows: u32) -> SharedString {
    if windows == 1 {
        SharedString::from("1 window")
    } else {
        SharedString::from(format!("{windows} windows"))
    }
}

/// Sort `sessions` via the persisted client-side order (exactly like the
/// cockpit strip's `session_order::sort_sessions`, `docs/spec-session-management.md`)
/// and map each into a display-ready [`SessionRow`].
pub fn build_rows(sessions: Vec<SessionListItem>, order: &[String]) -> Vec<SessionRow> {
    session_order::sort_sessions(sessions, order)
        .into_iter()
        .map(|item| SessionRow {
            id: item.id,
            name: item.name,
            windows_caption: windows_caption(item.windows),
            attached: item.attached,
            root: item.root,
        })
        .collect()
}

/// Emitted by [`SessionPicker`]; `main.rs`'s Shell subscribes.
pub enum SessionPickerEvent {
    /// A row click: attach an existing session. `main.rs`'s Shell forwards
    /// the name to the daemon thread over `picker_choice_tx`, then swaps to
    /// the cockpit `Workspace`.
    Pick(String),
    /// The "+ New session..." footer: ask the owner to open the root picker
    /// (`docs/spec-session-root-picker.md`, issue #769) instead of a bare
    /// name prompt — every create now travels through
    /// `Attach { session, root: Some(picked) }`, never a plain typed name.
    NewSession,
}

/// The session picker view (issue #706): a centered card listing every
/// session on the host (name, window count, attached marker) plus a "+ New
/// session..." footer, styled after [`crate::connection_screen::ConnectionScreen`]
/// (same theme tokens, centered card, logo/wordmark).
pub struct SessionPicker {
    ssh_label: SharedString,
    rows: Vec<SessionRow>,
    focus_handle: FocusHandle,
    /// Tracks the rows region's scroll offset (issue #804): shared between
    /// the scrolling `v_flex` (`.track_scroll`) and the overlay [`Scrollbar`]
    /// so the thumb reflects and drives the same scroll position.
    scroll_handle: ScrollHandle,
}

impl SessionPicker {
    /// Construct the picker from the daemon's `QuerySessionList` reply
    /// (already sorted via [`build_rows`]) and the connected host's label
    /// (e.g. `user@host`, for the "connected to ... pick a session" caption).
    pub fn new(
        ssh_label: SharedString,
        sessions: Vec<SessionListItem>,
        order: &[String],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            ssh_label,
            rows: build_rows(sessions, order),
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::default(),
        }
    }

    /// A row click: attach the picked session.
    fn pick(&mut self, name: String, cx: &mut Context<Self>) {
        cx.emit(SessionPickerEvent::Pick(name));
    }

    /// The "+ New session..." footer: ask the owner to open the root picker.
    fn request_new_session(&mut self, cx: &mut Context<Self>) {
        cx.emit(SessionPickerEvent::NewSession);
    }
}

impl EventEmitter<SessionPickerEvent> for SessionPicker {}

impl Focusable for SessionPicker {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// The card's header row: title plus a "tmux" pill, mirroring
/// `connection_screen::render_header`'s anatomy.
fn render_header(cx: &mut Context<SessionPicker>) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child("Pick a session"),
        )
        .child(
            div()
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(cx.theme().border)
                .text_size(px(11.0))
                .text_color(cx.theme().muted_foreground)
                .child("tmux"),
        )
}

/// The centered logo block, matching `connection_screen::render_logo`'s
/// anatomy (a 60px icon tile, the "rift" wordmark, a muted tagline) — kept as
/// its own small implementation rather than exported from `connection_screen`,
/// mirroring that module's own precedent for small duplicated visual
/// primitives (`render_error_banner`'s doc comment).
fn render_logo(cx: &mut Context<SessionPicker>) -> impl IntoElement {
    v_flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_none()
                .size(px(60.0))
                .rounded(px(14.0))
                .bg(cx.theme().primary)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::SquareTerminal).text_color(cx.theme().primary_foreground),
                ),
        )
        .child(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .font_weight(FontWeight::BOLD)
                .text_size(px(24.0))
                .text_color(cx.theme().foreground)
                .child("rift"),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(cx.theme().muted_foreground)
                .child("Reactive IDE awareness for terminal coding agents."),
        )
}

/// One session row: mono name, "N windows" muted caption, and a trailing
/// attached-success dot when at least one client is attached — clicking
/// anywhere on the row picks it. Mirrors `connection_screen::render_recent_row`'s
/// anatomy (icon tile aside, this row has no host icon to show).
fn render_row(cx: &mut Context<SessionPicker>, row: &SessionRow) -> AnyElement {
    let hover_bg = cx.theme().list_hover;
    let muted = cx.theme().muted_foreground;
    let foreground = cx.theme().foreground;
    let mono = cx.theme().mono_font_family.clone();
    let attached_dot = cx.theme().success;

    let name = SharedString::from(row.name.clone());
    let click_name = row.name.clone();

    h_flex()
        .id(("session-picker-row", row.id as usize))
        .w_full()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(move |el| el.bg(hover_bg))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                this.pick(click_name.clone(), cx);
            }),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(1.0))
                .child(
                    div()
                        .font_family(mono)
                        .text_size(px(13.0))
                        .text_color(foreground)
                        .truncate()
                        .child(name),
                )
                // Secondary project-path label (`SessionEntry.root`,
                // `docs/spec-session-root-picker.md`) — muted, truncated,
                // omitted for a session never stamped with a root.
                .children(row.root.as_ref().map(|root| {
                    div()
                        .text_size(px(11.0))
                        .text_color(muted)
                        .truncate()
                        .child(SharedString::from(root.clone()))
                })),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.0))
                .text_color(muted)
                .child(row.windows_caption.clone()),
        )
        .children(row.attached.then(|| {
            div()
                .flex_none()
                .size(px(6.0))
                .rounded_full()
                .bg(attached_dot)
        }))
        .into_any_element()
}

/// The zero-sessions state: "No sessions on this host yet" — the footer below
/// stays the only affordance (design contract: "only the create affordance").
fn render_empty_state(cx: &mut Context<SessionPicker>) -> AnyElement {
    div()
        .w_full()
        .py(px(8.0))
        .text_size(px(12.0))
        .text_color(cx.theme().muted_foreground)
        .child("No sessions on this host yet")
        .into_any_element()
}

/// The "+ New session..." footer: a plain clickable row that asks the owner
/// to open the root picker (`docs/spec-session-root-picker.md`) — no inline
/// name prompt anymore, mirroring `rift_terminal::SessionView::render_session_strip`'s
/// trailing chip.
fn render_new_session_footer(cx: &mut Context<SessionPicker>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let hover_bg = cx.theme().list_hover;

    h_flex()
        .id("session-picker-new-session")
        .w_full()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .text_color(muted)
        .hover(move |el| el.bg(hover_bg))
        .on_click(cx.listener(|this, _event, _window, cx| {
            this.request_new_session(cx);
        }))
        .child(Icon::new(IconName::Plus).text_color(muted))
        .child(div().child("New session..."))
        .into_any_element()
}

impl Render for SessionPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connection =
            title_bar::ConnectionGroup::connected(cx.theme().success, self.ssh_label.clone());
        let title_bar = title_bar::render(connection, None, None, cx);

        let caption = SharedString::from(format!(
            "connected to {} \u{b7} pick a session",
            self.ssh_label
        ));

        let rows: Vec<AnyElement> = self.rows.iter().map(|row| render_row(cx, row)).collect();
        let body = if rows.is_empty() {
            render_empty_state(cx)
        } else {
            // Bounded height + internal scroll (issue #792): a short list
            // still renders compact since `max_h` only caps growth, and
            // `overflow_y_scroll` only kicks in once the rows exceed it.
            // The vertical `Scrollbar` (issue #804) is a sibling overlay in
            // a `relative()` wrapper, bound to the same `scroll_handle` the
            // rows track via `track_scroll` — gpui-component only paints it
            // once the tracked content overflows the capped height, so a
            // short list still stays scrollbar-free.
            div()
                .relative()
                .child(
                    v_flex()
                        .id("session-picker-rows")
                        .gap(px(4.0))
                        .max_h(px(ROWS_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .children(rows),
                )
                .child(
                    Scrollbar::vertical(&self.scroll_handle).scrollbar_show(ScrollbarShow::Always),
                )
                .into_any_element()
        };

        let footer = render_new_session_footer(cx);

        let card = v_flex()
            .w(px(CARD_WIDTH))
            .p(px(24.0))
            .gap(px(16.0))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(12.0))
            .track_focus(&self.focus_handle)
            .child(render_header(cx))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(caption),
            )
            .child(body)
            .child(footer);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(title_bar)
            .child(
                div().flex_1().flex().items_center().justify_center().child(
                    v_flex()
                        .items_center()
                        .gap(px(24.0))
                        .child(render_logo(cx))
                        .child(card),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u32, name: &str, windows: u32, attached: bool) -> SessionListItem {
        SessionListItem {
            id,
            name: name.to_string(),
            windows,
            attached,
            root: None,
        }
    }

    #[::core::prelude::v1::test]
    fn test_windows_caption_one_window_uses_singular() {
        assert_eq!(windows_caption(1).as_ref(), "1 window");
    }

    #[::core::prelude::v1::test]
    fn test_windows_caption_zero_or_many_uses_plural() {
        assert_eq!(windows_caption(0).as_ref(), "0 windows");
        assert_eq!(windows_caption(3).as_ref(), "3 windows");
    }

    #[::core::prelude::v1::test]
    fn test_build_rows_empty_sessions_returns_empty() {
        assert!(build_rows(Vec::new(), &[]).is_empty());
    }

    #[::core::prelude::v1::test]
    fn test_build_rows_maps_every_field() {
        let rows = build_rows(vec![item(7, "work", 2, true)], &[]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 7);
        assert_eq!(rows[0].name, "work");
        assert_eq!(rows[0].windows_caption.as_ref(), "2 windows");
        assert!(rows[0].attached);
        assert_eq!(rows[0].root, None);
    }

    #[::core::prelude::v1::test]
    fn test_build_rows_carries_root_when_present() {
        let mut session = item(7, "work", 2, true);
        session.root = Some("/home/dev/work".to_string());

        let rows = build_rows(vec![session], &[]);

        assert_eq!(rows[0].root, Some("/home/dev/work".to_string()));
    }

    #[::core::prelude::v1::test]
    fn test_build_rows_sorts_via_the_persisted_order() {
        let sessions = vec![item(1, "b", 1, false), item(2, "a", 1, false)];

        let rows = build_rows(sessions, &["a".to_string(), "b".to_string()]);

        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[::core::prelude::v1::test]
    fn test_build_rows_unknown_order_entries_fall_back_to_name_sort() {
        let sessions = vec![item(1, "zeta", 1, false), item(2, "alpha", 1, false)];

        let rows = build_rows(sessions, &[]);

        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }
}
