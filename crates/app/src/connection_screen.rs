//! The Connection screen (issue #477, `docs/spec-connection-robustness.md`):
//! the app's startup state on every launch — a centered connect card
//! (Host / User+Port / SSH key / Session, prefilled from env and baked
//! defaults), a RECENT list backed by [`crate::recents`], and the surface
//! that owns a connect failure or a canceled reconnect (`main.rs` routes back
//! here instead of leaving a dead cockpit up). Auto-connect-on-launch is
//! deliberately not implemented (gate decision in the spec): the user always
//! takes the explicit Connect step, even when every field is already correct.
//!
//! This module is deliberately GPUI-view-only: it emits [`ConnectionScreenEvent`]
//! and never touches SSH, threads, or the recents *file* directly — `main.rs`
//! owns the connect pipeline and the recents read/write, mirroring how
//! `rift_terminal::SessionView` only emits terminal input and never touches
//! the SSH connection itself.

use std::path::PathBuf;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName};

use crate::recents::{self, RecentConnection};

/// Connect card width (design contract: "card ~470px").
const CARD_WIDTH: f32 = 470.0;
/// Height of each labeled input field (design contract: "labeled inputs 38px").
const FIELD_HEIGHT: f32 = 38.0;
/// Height of the full-width primary Connect button (design contract: "40px").
const CONNECT_BUTTON_HEIGHT: f32 = 40.0;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_USER: &str = "developer";
const DEFAULT_PORT: u16 = 22;
const DEFAULT_SESSION: &str = "rift";
const WINDOWS_FALLBACK_HOME: &str = "C:\\Users\\Default";
const UNIX_FALLBACK_HOME: &str = "/home/developer";

/// The connect card's prefilled values, resolved once at screen-construction
/// time from env vars and `just promote`'s compile-time bakes — the exact
/// resolution the pre-#477 startup path used to build its `SshConfig`
/// directly, now pure and testable via [`resolve_defaults`].
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectDefaults {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub key: PathBuf,
    pub session: String,
}

/// Explicit inputs to [`resolve_defaults`], grouped into a struct so the
/// function stays under clippy's argument-count threshold — every field
/// mirrors one live-environment input [`live_defaults`] reads.
#[derive(Default)]
pub struct DefaultsInputs<'a> {
    pub host: Option<&'a str>,
    pub user: Option<&'a str>,
    pub port: Option<&'a str>,
    pub key: Option<&'a str>,
    pub baked_key: Option<&'a str>,
    pub home: Option<&'a str>,
    pub session: Option<&'a str>,
    pub windows: bool,
}

/// Resolve the connect card's prefill values from explicit inputs (pure, for
/// tests) — [`live_defaults`] wraps this with the live environment. Mirrors
/// the pre-#477 inline `SshConfig` resolution in `main.rs` verbatim (no
/// behavior change from the refactor): a field falls back only when its env
/// var is entirely unset, never on an empty-but-set value, and an unparsable
/// port silently falls back to [`DEFAULT_PORT`] rather than surfacing an
/// error this far back.
pub fn resolve_defaults(inputs: DefaultsInputs) -> ConnectDefaults {
    let host = inputs.host.unwrap_or(DEFAULT_HOST).to_string();
    let user = inputs.user.unwrap_or(DEFAULT_USER).to_string();
    let port = inputs
        .port
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let key = inputs
        .key
        .or(inputs.baked_key)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_key_path(inputs.home, inputs.windows));
    let session = inputs.session.unwrap_or(DEFAULT_SESSION).to_string();

    ConnectDefaults {
        host,
        user,
        port,
        key,
        session,
    }
}

/// `~/.ssh/id_ed25519` under `home` (or a hardcoded per-OS fallback when
/// `home` itself is unset) — the last-resort key path when no `RIFT_SSH_KEY`
/// and no baked `RIFT_DEFAULT_SSH_KEY` are configured at all.
fn default_key_path(home: Option<&str>, windows: bool) -> PathBuf {
    let home = home.unwrap_or(if windows {
        WINDOWS_FALLBACK_HOME
    } else {
        UNIX_FALLBACK_HOME
    });
    PathBuf::from(home).join(".ssh").join("id_ed25519")
}

/// [`resolve_defaults`] read from the live environment: `RIFT_SSH_HOST`/
/// `RIFT_SSH_USER`/`RIFT_SSH_PORT`/`RIFT_SSH_KEY`/`RIFT_SESSION` (runtime),
/// `RIFT_DEFAULT_SSH_KEY` (the `just promote` compile-time bake), and
/// `USERPROFILE`/`HOME` for the last-resort key path.
pub fn live_defaults() -> ConnectDefaults {
    let host = std::env::var("RIFT_SSH_HOST").ok();
    let user = std::env::var("RIFT_SSH_USER").ok();
    let port = std::env::var("RIFT_SSH_PORT").ok();
    let key = std::env::var("RIFT_SSH_KEY").ok();
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let session = std::env::var("RIFT_SESSION").ok();

    resolve_defaults(DefaultsInputs {
        host: host.as_deref(),
        user: user.as_deref(),
        port: port.as_deref(),
        key: key.as_deref(),
        baked_key: option_env!("RIFT_DEFAULT_SSH_KEY"),
        home: home.as_deref(),
        session: session.as_deref(),
        windows: cfg!(target_os = "windows"),
    })
}

/// A submitted connect attempt: the Connect button, Enter in any field, or a
/// RECENT row click, all resolve to one of these.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectRequest {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub key: PathBuf,
    pub session: String,
}

/// Emitted by [`ConnectionScreen`]; `main.rs`'s Shell subscribes and drives
/// the actual SSH connect pipeline.
pub enum ConnectionScreenEvent {
    Connect(ConnectRequest),
}

impl EventEmitter<ConnectionScreenEvent> for ConnectionScreen {}

/// The Connection screen view: the connect card's five inputs, the RECENT
/// list it was constructed with, and an optional error surfaced from a
/// previous connect attempt (field/banner, never log-only —
/// `docs/spec-connection-robustness.md`).
pub struct ConnectionScreen {
    host_input: Entity<InputState>,
    user_input: Entity<InputState>,
    port_input: Entity<InputState>,
    key_input: Entity<InputState>,
    session_input: Entity<InputState>,
    recents: Vec<RecentConnection>,
    error: Option<String>,
}

impl ConnectionScreen {
    /// Construct the screen prefilled from `defaults`, with `recents` as the
    /// RECENT list and `error` as an already-surfaced connect failure (`None`
    /// on a fresh startup; `Some(reason)` when the Shell returns here after a
    /// non-retryable connect failure or a canceled reconnect).
    pub fn new(
        defaults: &ConnectDefaults,
        recents: Vec<RecentConnection>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host_input =
            cx.new(|cx| InputState::new(window, cx).default_value(defaults.host.clone()));
        let user_input =
            cx.new(|cx| InputState::new(window, cx).default_value(defaults.user.clone()));
        let port_input =
            cx.new(|cx| InputState::new(window, cx).default_value(defaults.port.to_string()));
        let key_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(defaults.key.display().to_string())
        });
        let session_input =
            cx.new(|cx| InputState::new(window, cx).default_value(defaults.session.clone()));

        // Enter in any field submits the card, matching the design contract
        // ("one click connects" applies equally to Enter).
        for input in [
            &host_input,
            &user_input,
            &port_input,
            &key_input,
            &session_input,
        ] {
            cx.subscribe_in(
                input,
                window,
                |this, _input, event: &InputEvent, _window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.try_connect(cx);
                    }
                },
            )
            .detach();
        }

        Self {
            host_input,
            user_input,
            port_input,
            key_input,
            session_input,
            recents,
            error,
        }
    }

    /// Validate the card's current field values, emitting [`ConnectionScreenEvent::Connect`]
    /// on success or setting the field/banner error on failure — never both.
    fn try_connect(&mut self, cx: &mut Context<Self>) {
        match self.build_request(cx) {
            Ok(request) => {
                self.error = None;
                cx.emit(ConnectionScreenEvent::Connect(request));
                cx.notify();
            }
            Err(message) => {
                self.error = Some(message);
                cx.notify();
            }
        }
    }

    /// Read the five inputs and validate them into a [`ConnectRequest`]. Host
    /// and User must be non-empty; Port must parse as a `u16`; the SSH key
    /// path must be non-empty; an empty Session defaults to `"rift"` rather
    /// than erroring, mirroring the pre-#477 `RIFT_SESSION` fallback.
    fn build_request(&self, cx: &App) -> Result<ConnectRequest, String> {
        let host = self.host_input.read(cx).value().trim().to_string();
        if host.is_empty() {
            return Err("Host is required.".to_string());
        }
        let user = self.user_input.read(cx).value().trim().to_string();
        if user.is_empty() {
            return Err("User is required.".to_string());
        }
        let port_text = self.port_input.read(cx).value();
        let port_text = port_text.trim();
        let port: u16 = port_text
            .parse()
            .map_err(|_| format!("\"{port_text}\" is not a valid port."))?;
        let key_text = self.key_input.read(cx).value().trim().to_string();
        if key_text.is_empty() {
            return Err("SSH key path is required.".to_string());
        }
        let session = self.session_input.read(cx).value().trim().to_string();
        let session = if session.is_empty() {
            DEFAULT_SESSION.to_string()
        } else {
            session
        };

        Ok(ConnectRequest {
            host,
            user,
            port,
            key: PathBuf::from(key_text),
            session,
        })
    }

    /// A RECENT row click: prefill every field with that entry's values (so
    /// a failed attempt still shows what was tried) and emit `Connect`
    /// immediately — "clickable (prefill + connect)" per the issue's
    /// acceptance. Silently ignored if `index` is stale (the list changed
    /// under a slow click), rather than panicking.
    fn connect_from_recent(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recent) = self.recents.get(index).cloned() else {
            return;
        };
        self.host_input.update(cx, |input, cx| {
            input.set_value(recent.host.clone(), window, cx)
        });
        self.user_input.update(cx, |input, cx| {
            input.set_value(recent.user.clone(), window, cx)
        });
        self.port_input.update(cx, |input, cx| {
            input.set_value(recent.port.to_string(), window, cx)
        });
        self.key_input.update(cx, |input, cx| {
            input.set_value(recent.key.clone(), window, cx)
        });
        self.session_input.update(cx, |input, cx| {
            input.set_value(recent.session.clone(), window, cx)
        });

        self.error = None;
        cx.emit(ConnectionScreenEvent::Connect(ConnectRequest {
            host: recent.host,
            user: recent.user,
            port: recent.port,
            key: PathBuf::from(recent.key),
            session: recent.session,
        }));
        cx.notify();
    }
}

impl Focusable for ConnectionScreen {
    /// The Host field takes focus first — mirroring the workspace's own
    /// startup-focus convention (`main.rs` defers a focus call the same way
    /// after both constructing this screen and returning to it).
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.host_input.focus_handle(cx)
    }
}

/// The connect card's header row: title plus an "SSH" pill.
fn render_header(cx: &mut Context<ConnectionScreen>) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child("Connect to host"),
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
                .child("SSH"),
        )
}

/// One labeled input row: a small muted label above a mono-valued, leading-
/// icon input (design contract: "labeled inputs 38px ... mono values,
/// leading icons").
fn render_field(
    cx: &mut Context<ConnectionScreen>,
    label: &'static str,
    input: &Entity<InputState>,
    icon: IconName,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .gap(px(4.0))
        .child(div().text_size(px(12.0)).text_color(muted).child(label))
        .child(
            Input::new(input)
                .h(px(FIELD_HEIGHT))
                .font_family(mono)
                .prefix(Icon::new(icon).text_color(muted)),
        )
}

/// The right-aligned mono caption below the Session field (design contract:
/// "caption right `tmux -CC -A` mono").
fn render_tmux_caption(cx: &mut Context<ConnectionScreen>) -> impl IntoElement {
    div()
        .w_full()
        .text_right()
        .text_size(px(11.0))
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(cx.theme().muted_foreground)
        .child("tmux -CC -A")
}

/// The connect-failure banner (design §7 shape, reused here for a connect
/// failure rather than a live reconnect — `rift_terminal::session_view`'s
/// danger banner is the same visual pattern, kept as its own small
/// implementation since it lives in a different crate for a different
/// state).
fn render_error_banner(cx: &mut Context<ConnectionScreen>, message: &str) -> impl IntoElement {
    let danger = cx.theme().danger;
    h_flex()
        .w_full()
        .items_start()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(danger.opacity(0.12))
        .border_1()
        .border_color(danger.opacity(0.35))
        .child(Icon::new(IconName::TriangleAlert).text_color(danger))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.0))
                .text_color(cx.theme().foreground)
                .child(SharedString::from(message.to_string())),
        )
}

/// The centered logo block: a 60px icon tile, the "rift" wordmark (mono,
/// bold), and a muted tagline.
fn render_logo(cx: &mut Context<ConnectionScreen>) -> impl IntoElement {
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

/// One RECENT row: a host icon tile, host (mono) + "user · session <name>"
/// caption, and a trailing relative-time label. Clicking anywhere on the row
/// prefills and connects ([`ConnectionScreen::connect_from_recent`]).
fn render_recent_row(
    cx: &mut Context<ConnectionScreen>,
    index: usize,
    recent: &RecentConnection,
    now_unix_secs: u64,
) -> AnyElement {
    let hover_bg = cx.theme().list_hover;
    let muted = cx.theme().muted_foreground;
    let foreground = cx.theme().foreground;
    let tile_bg = cx.theme().muted;
    let mono = cx.theme().mono_font_family.clone();

    let host = SharedString::from(recent.host.clone());
    let caption = SharedString::from(format!("{} \u{b7} session {}", recent.user, recent.session));
    let when = SharedString::from(recents::relative_time(
        now_unix_secs,
        recent.last_connected_unix_secs,
    ));

    h_flex()
        .id(("recent-row", index))
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
            cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                this.connect_from_recent(index, window, cx);
            }),
        )
        .child(
            div()
                .flex_none()
                .size(px(28.0))
                .rounded(px(8.0))
                .bg(tile_bg)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(IconName::HardDrive).text_color(muted)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .font_family(mono)
                        .text_size(px(13.0))
                        .text_color(foreground)
                        .truncate()
                        .child(host),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(muted)
                        .truncate()
                        .child(caption),
                ),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.0))
                .text_color(muted)
                .child(when),
        )
        .into_any_element()
}

/// The RECENT section (eyebrow + rows), or `None` when there are no recents
/// yet — a fresh install shows just the card, no empty "RECENT" heading.
fn render_recents_section(
    cx: &mut Context<ConnectionScreen>,
    entries: &[RecentConnection],
) -> Option<AnyElement> {
    if entries.is_empty() {
        return None;
    }
    let now = recents::now_unix_secs();
    let mut rows: Vec<AnyElement> = Vec::with_capacity(entries.len());
    for (index, recent) in entries.iter().enumerate() {
        rows.push(render_recent_row(cx, index, recent, now));
    }

    Some(
        v_flex()
            .w(px(CARD_WIDTH))
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("RECENT"),
            )
            .children(rows)
            .into_any_element(),
    )
}

impl Render for ConnectionScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error_banner = self
            .error
            .clone()
            .map(|error| render_error_banner(cx, &error));
        let recents_section = render_recents_section(cx, &self.recents);

        let card = v_flex()
            .w(px(CARD_WIDTH))
            .p(px(24.0))
            .gap(px(16.0))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(12.0))
            .child(render_header(cx))
            .child(render_field(
                cx,
                "Host",
                &self.host_input,
                IconName::HardDrive,
            ))
            .child(
                h_flex()
                    .w_full()
                    .gap(px(12.0))
                    .child(div().flex_1().child(render_field(
                        cx,
                        "User",
                        &self.user_input,
                        IconName::User,
                    )))
                    .child(div().w(px(96.0)).child(render_field(
                        cx,
                        "Port",
                        &self.port_input,
                        IconName::Network,
                    ))),
            )
            .child(render_field(cx, "SSH key", &self.key_input, IconName::File))
            .child(render_field(
                cx,
                "Session",
                &self.session_input,
                IconName::SquareTerminal,
            ))
            .child(render_tmux_caption(cx))
            .children(error_banner)
            .child(
                Button::new("connect-button")
                    .primary()
                    .label("Connect \u{2192}")
                    .w_full()
                    .h(px(CONNECT_BUTTON_HEIGHT))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.try_connect(cx);
                    })),
            );

        div()
            .size_full()
            .bg(cx.theme().background)
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(render_logo(cx))
                    .child(card)
                    .children(recents_section),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_defaults (pure) ──────────────────────────────────────────

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_uses_hardcoded_defaults_when_nothing_is_set() {
        let defaults = resolve_defaults(DefaultsInputs::default());

        assert_eq!(defaults.host, DEFAULT_HOST);
        assert_eq!(defaults.user, DEFAULT_USER);
        assert_eq!(defaults.port, DEFAULT_PORT);
        assert_eq!(defaults.session, DEFAULT_SESSION);
        assert_eq!(
            defaults.key,
            PathBuf::from(UNIX_FALLBACK_HOME)
                .join(".ssh")
                .join("id_ed25519")
        );
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_uses_every_explicit_value_when_set() {
        let defaults = resolve_defaults(DefaultsInputs {
            host: Some("100.64.0.1"),
            user: Some("alice"),
            port: Some("2222"),
            key: Some("/keys/mine"),
            baked_key: Some("/keys/baked"),
            home: Some("/home/alice"),
            session: Some("work"),
            windows: false,
        });

        assert_eq!(defaults.host, "100.64.0.1");
        assert_eq!(defaults.user, "alice");
        assert_eq!(defaults.port, 2222);
        assert_eq!(defaults.key, PathBuf::from("/keys/mine"));
        assert_eq!(defaults.session, "work");
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_unparsable_port_falls_back_to_default() {
        let defaults = resolve_defaults(DefaultsInputs {
            port: Some("not-a-port"),
            ..Default::default()
        });

        assert_eq!(defaults.port, DEFAULT_PORT);
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_prefers_runtime_key_over_baked_default() {
        let defaults = resolve_defaults(DefaultsInputs {
            key: Some("/keys/runtime"),
            baked_key: Some("/keys/baked"),
            ..Default::default()
        });

        assert_eq!(defaults.key, PathBuf::from("/keys/runtime"));
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_falls_back_to_baked_key_when_runtime_unset() {
        let defaults = resolve_defaults(DefaultsInputs {
            baked_key: Some("/keys/baked"),
            ..Default::default()
        });

        assert_eq!(defaults.key, PathBuf::from("/keys/baked"));
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_windows_home_fallback_when_home_unset() {
        let defaults = resolve_defaults(DefaultsInputs {
            windows: true,
            ..Default::default()
        });

        assert_eq!(
            defaults.key,
            PathBuf::from(WINDOWS_FALLBACK_HOME)
                .join(".ssh")
                .join("id_ed25519")
        );
    }
}
