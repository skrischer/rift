//! The post-connect session picker (issue #706, `docs/spec-post-connect-picker.md`):
//! a pre-cockpit `Shell` state shown after the SSH connect + daemon handshake
//! and before the cockpit attach, but only while the session is unresolved.
//!
//! This PR (33a of the phase-33 picker) wires the SAFE-INTERIM trigger: the
//! Connection screen's Session field left blank. A filled field (or one
//! prefilled by `RIFT_SESSION`) still attaches directly, exactly like before
//! — the picker is only reachable by deliberately clearing the field, so the
//! normal/dogfooding connect path stays untouched even while this surface is
//! new. Issue #707 replaces the trigger with full entry-point routing (a
//! remembered recent, `RIFT_SESSION`, or "Connect \u{2192}" always picking);
//! issue #705 removes the field entirely.
//!
//! Deliberately GPUI-view-only, mirroring [`crate::connection_screen::ConnectionScreen`]:
//! it emits [`SessionPickerEvent`] and never touches the daemon client or SSH
//! thread directly — `main.rs` owns the cross-thread coordination (the
//! `picker_list_tx`/`picker_choice_rx` pair) and drives the `Shell` state
//! swap on a pick.

use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName};

use rift_terminal::SessionListItem;

use crate::session_order;
use crate::title_bar;

/// Card width, matching [`crate::connection_screen`]'s own `CARD_WIDTH` (design
/// contract: "card ~470px") — this screen is styled as its direct sibling.
const CARD_WIDTH: f32 = 470.0;

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
        })
        .collect()
}

/// Emitted by [`SessionPicker`]; `main.rs`'s Shell subscribes and forwards the
/// chosen name to the daemon thread over `picker_choice_tx`, then swaps to
/// the cockpit `Workspace`. Carries either an existing row's name (a plain
/// attach) or a freshly typed one (the daemon's attach-or-create `new-session
/// -A` creates it) — the two are indistinguishable on the wire, and the
/// picker does not need to know which happened.
pub enum SessionPickerEvent {
    Pick(String),
}

/// An in-progress "+ New session..." inline prompt, mirroring
/// `rift_terminal::SessionView`'s own `NewSessionPrompt` (the phase-32 strip's
/// new-session affordance this picker's footer is styled after): `input`
/// holds the edit state, `_subscription` keeps the submit/blur handler alive
/// while the prompt is active.
struct NewSessionPrompt {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The session picker view (issue #706): a centered card listing every
/// session on the host (name, window count, attached marker) plus a "+ New
/// session..." footer, styled after [`crate::connection_screen::ConnectionScreen`]
/// (same theme tokens, centered card, logo/wordmark).
pub struct SessionPicker {
    ssh_label: SharedString,
    rows: Vec<SessionRow>,
    new_session_prompt: Option<NewSessionPrompt>,
    focus_handle: FocusHandle,
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
            new_session_prompt: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// A row click: attach (or, for a name typed into the new-session prompt,
    /// create) the picked session.
    fn pick(&mut self, name: String, cx: &mut Context<Self>) {
        cx.emit(SessionPickerEvent::Pick(name));
    }

    /// Activate the footer's inline input: seed an empty field, focus it, and
    /// subscribe for submit/blur — mirrors
    /// `rift_terminal::SessionView::start_new_session_prompt` verbatim.
    fn start_new_session_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("session name"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, _input, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_new_session_prompt(cx),
                InputEvent::Blur => this.cancel_new_session_prompt(cx),
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.new_session_prompt = Some(NewSessionPrompt {
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Commit the new-session prompt (Enter): a non-empty trimmed name picks
    /// it (the daemon's attach-or-create handles a fresh vs. duplicate name
    /// identically). An empty name sends nothing and dismisses the prompt
    /// back to the trailing "+ New session..." row.
    fn submit_new_session_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.new_session_prompt.take() else {
            return;
        };
        let value = prompt.input.read(cx).value();
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            cx.emit(SessionPickerEvent::Pick(trimmed));
        }
        cx.notify();
    }

    /// Cancel an in-progress new-session prompt without emitting anything.
    fn cancel_new_session_prompt(&mut self, cx: &mut Context<Self>) {
        if self.new_session_prompt.is_some() {
            self.new_session_prompt = None;
            cx.notify();
        }
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
            div()
                .flex_1()
                .min_w_0()
                .font_family(mono)
                .text_size(px(13.0))
                .text_color(foreground)
                .truncate()
                .child(name),
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

/// The "+ New session..." footer: a plain clickable row until clicked, then
/// an inline mono input (Enter picks/creates, Escape or blur cancels) —
/// mirrors `rift_terminal::SessionView::render_session_strip`'s trailing
/// new-session chip, adapted to this screen's row shape. `active_input` is
/// `self.new_session_prompt`'s input, read by the caller before this runs —
/// never fetched here via `cx.entity().read(cx)`, which would re-borrow the
/// view's own data while its `render` call already holds `&mut self`.
fn render_new_session_footer(
    cx: &mut Context<SessionPicker>,
    active_input: Option<Entity<InputState>>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let hover_bg = cx.theme().list_hover;

    if let Some(input) = active_input {
        let cancel_entity = cx.entity();
        return h_flex()
            .id("session-picker-new-session-active")
            .w_full()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    cancel_entity.update(cx, |view, cx| {
                        view.cancel_new_session_prompt(cx);
                    });
                    cx.stop_propagation();
                }
            })
            .child(Icon::new(IconName::Plus).text_color(muted))
            .child(Input::new(&input).flex_1())
            .into_any_element();
    }

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
        .on_click(cx.listener(|this, _event, window, cx| {
            this.start_new_session_prompt(window, cx);
        }))
        .child(Icon::new(IconName::Plus).text_color(muted))
        .child(div().child("New session..."))
        .into_any_element()
}

impl Render for SessionPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connection =
            title_bar::ConnectionGroup::connected(cx.theme().success, self.ssh_label.clone(), None);
        let title_bar = title_bar::render(connection, None, cx);

        let caption = SharedString::from(format!(
            "connected to {} \u{b7} pick a session",
            self.ssh_label
        ));

        let rows: Vec<AnyElement> = self.rows.iter().map(|row| render_row(cx, row)).collect();
        let body = if rows.is_empty() {
            render_empty_state(cx)
        } else {
            v_flex().gap(px(4.0)).children(rows).into_any_element()
        };

        let active_input = self.new_session_prompt.as_ref().map(|p| p.input.clone());
        let footer = render_new_session_footer(cx, active_input);

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
