//! App-side `Panel` adapter for the terminal surface (`docs/spec-ide-shell.md`,
//! issue #323).
//!
//! `SessionView` lives in `rift-terminal`, which must never learn about the
//! dock system (constitution: "crate boundaries are contracts"). `TerminalPanel`
//! is a newtype wrapping `Entity<SessionView>` that lives entirely in this
//! crate, implements `gpui-component`'s `Panel`, and delegates render and focus
//! to the inner session unchanged — so tmux windows/panes keep rendering via
//! `SessionView`'s own chrome, and terminal keystroke delivery is untouched.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Window,
};
use gpui_component::button::Button;
use gpui_component::dock::{Panel, PanelControl, PanelEvent};
use rift_terminal::SessionView;

use crate::workspace::{solo_button, SoloTerminal};

/// Stable, distinct dock-panel identity for the terminal (`Panel::panel_name`).
/// Once shipped this must not change — it is the persisted panel identifier.
pub const TERMINAL_PANEL_NAME: &str = "terminal";

/// Wraps the terminal surface as a dock panel. Purely additive: this type adds
/// no behavior of its own, it only adapts the existing `SessionView` entity to
/// the `Panel` trait.
pub struct TerminalPanel {
    session: Entity<SessionView>,
}

impl TerminalPanel {
    pub fn new(session: Entity<SessionView>) -> Self {
        Self { session }
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.session.focus_handle(cx)
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl Panel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        TERMINAL_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Terminal")
    }

    // gpui-component's own native zoom disabled (issue #820, superseding
    // #716): its `ToggleZoom` -> `PanelEvent` path would flip `TabPanel.
    // zoomed` + `DockArea.zoom_view` independently of the rift-owned
    // visible set (`docs/spec-workspace-visibility-rail.md`, "Single source
    // of truth for solo"). `toolbar_buttons` below replaces it with a header
    // button that solos this area through that set instead.
    fn zoomable(&self, _cx: &App) -> Option<PanelControl> {
        None
    }

    fn toolbar_buttons(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Vec<Button>> {
        Some(vec![solo_button(|_, window, cx| {
            window.dispatch_action(Box::new(SoloTerminal), cx);
        })])
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.session.clone()
    }
}
