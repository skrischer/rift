//! The Connection screen (issue #477, `docs/spec-connection-robustness.md`):
//! the app's startup state on every launch — a centered connect card
//! (Host / User+Port / SSH key, prefilled from env and baked defaults), a
//! RECENT list backed by [`crate::recents`], and the surface that owns a
//! connect failure or a canceled reconnect (`main.rs` routes back here
//! instead of leaving a dead cockpit up). Auto-connect-on-launch is
//! deliberately not implemented (gate decision in the spec): the user always
//! takes the explicit Connect step, even when every field is already correct.
//!
//! Issue #478 adds passphrase-protected key support: a probe
//! ([`rift_ssh::key_requires_passphrase`]) reacts to the SSH key field and
//! shows a masked Passphrase row only while the current path is detected as
//! encrypted; the value is carried on [`ConnectRequest`] but never persisted
//! (excluded from [`crate::recents::RecentConnection`]) and never logged
//! ([`ConnectRequest`]'s `Debug` impl redacts it below).
//!
//! Issues #706/#707/#705 (`docs/spec-post-connect-picker.md`) retire the
//! card's Session field: the card no longer picks a session at all, and a
//! [`SessionIntent`] carried on [`ConnectRequest`] tells `main.rs`'s entry
//! point how to resolve one instead — `RIFT_SESSION` set (read at connect
//! time) is [`SessionIntent::Fixed`] (the dogfooding fast-path, no picker); a
//! RECENT row whose stored session is non-empty is [`SessionIntent::Preferred`]
//! (attached directly if still present on the live host, else the picker); a
//! plain Connect click or an empty recent session is [`SessionIntent::Pick`]
//! (always the picker).
//!
//! This module is deliberately GPUI-view-only: it emits [`ConnectionScreenEvent`]
//! and never touches SSH, threads, or the recents *file* directly — `main.rs`
//! owns the connect pipeline and the recents read/write, mirroring how
//! `rift_terminal::SessionView` only emits terminal input and never touches
//! the SSH connection itself.

use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName};

use crate::recents::{self, RecentConnection};
use crate::title_bar;

/// Connect card width (design contract: "card ~470px").
const CARD_WIDTH: f32 = 470.0;
/// Height of each labeled input field (design contract: "labeled inputs 38px").
const FIELD_HEIGHT: f32 = 38.0;
/// Height of the full-width primary Connect button (design contract: "40px").
const CONNECT_BUTTON_HEIGHT: f32 = 40.0;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_USER: &str = "developer";
const DEFAULT_PORT: u16 = 22;
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
    /// The Remote exec wrapper field's fresh-card prefill (issue #789,
    /// `docs/spec-remote-exec-wrapper-ui.md`) — the same runtime-over-bake
    /// value `main.rs`'s pre-#789 `resolve_remote_exec_wrapper()` resolved,
    /// now folded into this struct so the card's fields all prefill through
    /// one path. `None` prefills an empty field (a normal host connection).
    pub remote_exec_wrapper: Option<String>,
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
    pub windows: bool,
    /// `RIFT_REMOTE_EXEC_WRAPPER` (runtime).
    pub remote_exec_wrapper: Option<&'a str>,
    /// `RIFT_DEFAULT_REMOTE_EXEC_WRAPPER` (the `just promote` compile-time
    /// bake).
    pub baked_remote_exec_wrapper: Option<&'a str>,
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
    // Mirrors the pre-#789 `resolve_remote_exec_wrapper()` in `main.rs`
    // byte-for-byte: the runtime value wins only if non-empty after trim,
    // else the baked value is tried, and the value that is actually used is
    // filtered again (covers a blank baked fallback too).
    let remote_exec_wrapper = inputs
        .remote_exec_wrapper
        .filter(|s| !s.trim().is_empty())
        .or(inputs.baked_remote_exec_wrapper)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);

    ConnectDefaults {
        host,
        user,
        port,
        key,
        remote_exec_wrapper,
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
/// `RIFT_SSH_USER`/`RIFT_SSH_PORT`/`RIFT_SSH_KEY` (runtime),
/// `RIFT_DEFAULT_SSH_KEY` (the `just promote` compile-time bake), and
/// `USERPROFILE`/`HOME` for the last-resort key path. `RIFT_SESSION` is read
/// separately, at connect time, by [`session_intent_from_env`] (issue #707) —
/// not here, since it no longer prefills a card field. `RIFT_REMOTE_EXEC_WRAPPER`
/// / `RIFT_DEFAULT_REMOTE_EXEC_WRAPPER` (issue #789) resolve the Remote exec
/// wrapper field's fresh-card prefill the same way — the field itself is
/// authoritative at connect (`build_request`), this is only the seed value.
pub fn live_defaults() -> ConnectDefaults {
    let host = std::env::var("RIFT_SSH_HOST").ok();
    let user = std::env::var("RIFT_SSH_USER").ok();
    let port = std::env::var("RIFT_SSH_PORT").ok();
    let key = std::env::var("RIFT_SSH_KEY").ok();
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let remote_exec_wrapper = std::env::var("RIFT_REMOTE_EXEC_WRAPPER").ok();

    resolve_defaults(DefaultsInputs {
        host: host.as_deref(),
        user: user.as_deref(),
        port: port.as_deref(),
        key: key.as_deref(),
        baked_key: option_env!("RIFT_DEFAULT_SSH_KEY"),
        home: home.as_deref(),
        windows: cfg!(target_os = "windows"),
        remote_exec_wrapper: remote_exec_wrapper.as_deref(),
        baked_remote_exec_wrapper: option_env!("RIFT_DEFAULT_REMOTE_EXEC_WRAPPER"),
    })
}

/// How the entry point that triggered a connect resolves the session (issue
/// #707, `docs/spec-post-connect-picker.md`): the connect card carries no
/// Session field of its own, so `main.rs`'s Shell derives one of these from
/// which entry point fired and threads it end-to-end instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionIntent {
    /// `RIFT_SESSION` is set (env, e.g. the dogfooding dev channel):
    /// attach-or-create this session directly — no picker, the pre-#706
    /// behavior byte-for-byte.
    Fixed(String),
    /// A RECENT row whose stored session is non-empty: attach it directly if
    /// still present on the live host session list; if it is gone, show the
    /// picker instead of a blind attach.
    Preferred(String),
    /// The plain "Connect \u{2192}" button with no `RIFT_SESSION` set, or a
    /// RECENT row whose stored session is empty: always show the post-connect
    /// picker.
    Pick,
}

/// A submitted connect attempt: the Connect button, Enter in any field, or a
/// RECENT row click, all resolve to one of these.
///
/// `Debug` is hand-written rather than derived: `passphrase` must never reach
/// a log line (constitution: no secrets in logs), so a future `debug!(?request)`
/// prints `Some("<redacted>")` instead of the plaintext value.
#[derive(Clone, PartialEq)]
pub struct ConnectRequest {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub key: PathBuf,
    /// The Remote exec wrapper field's value at connect (issue #789,
    /// `docs/spec-remote-exec-wrapper-ui.md`), e.g. `docker exec -i devenv`;
    /// `None` for an empty/whitespace field (byte-for-byte passthrough, a
    /// normal host connection). Not a secret — logged in full, unlike
    /// `passphrase` below.
    pub remote_exec_wrapper: Option<String>,
    pub session_intent: SessionIntent,
    /// The passphrase entered for an encrypted SSH key (#478); `None` for a
    /// plain key. Never persisted to the recents store and never logged.
    pub passphrase: Option<String>,
}

impl std::fmt::Debug for ConnectRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectRequest")
            .field("host", &self.host)
            .field("user", &self.user)
            .field("port", &self.port)
            .field("key", &self.key)
            .field("remote_exec_wrapper", &self.remote_exec_wrapper)
            .field("session_intent", &self.session_intent)
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Resolve the plain "Connect \u{2192}" button's session intent (issue #707):
/// `RIFT_SESSION` read at connect time, not prefilled at screen-construction
/// time — a set, non-empty value attaches directly (the dogfooding
/// fast-path, unchanged from #706's SET path); unset or empty always shows
/// the post-connect picker.
fn session_intent_from_env(rift_session: Option<&str>) -> SessionIntent {
    match rift_session {
        Some(name) if !name.is_empty() => SessionIntent::Fixed(name.to_string()),
        _ => SessionIntent::Pick,
    }
}

/// Resolve a RECENT row's session intent (issue #707): a non-empty stored
/// session is tried directly against the live host list before falling back
/// to the picker ([`SessionIntent::Preferred`]); an empty one (an older
/// recents entry whose session was never resolved) always shows the picker.
fn session_intent_from_recent(recent_session: &str) -> SessionIntent {
    if recent_session.is_empty() {
        SessionIntent::Pick
    } else {
        SessionIntent::Preferred(recent_session.to_string())
    }
}

/// Emitted by [`ConnectionScreen`]; `main.rs`'s Shell subscribes and drives
/// the actual SSH connect pipeline.
pub enum ConnectionScreenEvent {
    Connect(ConnectRequest),
}

impl EventEmitter<ConnectionScreenEvent> for ConnectionScreen {}

/// A connect failure surfaced by the Shell (`main.rs`) after a non-retryable
/// connect attempt ends, and internally by [`ConnectionScreen::build_request`]'s
/// field validation — both funnel through the same shape so the screen has
/// one place that decides where an error renders:
/// [`ConnectError::General`] on the card's bottom banner,
/// [`ConnectError::Passphrase`] at the passphrase field (and forces its row
/// visible even if the encrypted-key probe had not already shown it) — a
/// wrong or missing passphrase points at the field to fix (#478,
/// `docs/spec-connection-robustness.md`).
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectError {
    General(String),
    Passphrase(String),
}

/// Whether the SSH key at `path` is passphrase-protected — decides whether
/// the passphrase row renders (design contract: "SSH key (+ passphrase row
/// when the key is encrypted)"). Any probe failure (missing file, unreadable,
/// unsupported format) is treated as "not encrypted" here: this probe only
/// decides the row's visibility, and the real connect attempt surfaces those
/// failures properly (general banner) instead of this best-effort UX hint
/// misreporting them as a passphrase problem.
fn key_needs_passphrase(path: &Path) -> bool {
    rift_ssh::key_requires_passphrase(path).unwrap_or(false)
}

/// The Remote exec wrapper field's connect-time value (issue #789): the field
/// is authoritative, mirroring `resolve_defaults`'s runtime-wrapper filter —
/// an empty/whitespace value is `None` (byte-for-byte passthrough), a
/// non-empty value is carried through as-is (not trimmed, exactly like the
/// env var it replaces at the connect site).
fn remote_exec_wrapper_from_field(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// The Connection screen view: the connect card's inputs (the passphrase
/// row renders only while `key_encrypted`), the RECENT list it was
/// constructed with, and the two error slots a previous connect attempt (or
/// this screen's own field validation) may have set — never both at once
/// (`docs/spec-connection-robustness.md`).
pub struct ConnectionScreen {
    host_input: Entity<InputState>,
    user_input: Entity<InputState>,
    port_input: Entity<InputState>,
    key_input: Entity<InputState>,
    /// The Remote exec wrapper field (issue #789,
    /// `docs/spec-remote-exec-wrapper-ui.md`); optional, free text.
    remote_exec_wrapper_input: Entity<InputState>,
    passphrase_input: Entity<InputState>,
    recents: Vec<RecentConnection>,
    /// The card's bottom banner (host/user/port/key validation, or a general
    /// connect failure).
    error: Option<String>,
    /// Rendered at the passphrase field instead of the bottom banner (a
    /// missing or wrong passphrase — #478).
    passphrase_error: Option<String>,
    /// Whether the SSH key field's current path is detected as encrypted;
    /// gates the passphrase row's visibility.
    key_encrypted: bool,
}

impl ConnectionScreen {
    /// Construct the screen prefilled from `defaults`, with `recents` as the
    /// RECENT list and `error` as an already-surfaced connect failure (`None`
    /// on a fresh startup; `Some(reason)` when the Shell returns here after a
    /// non-retryable connect failure or a canceled reconnect).
    pub fn new(
        defaults: &ConnectDefaults,
        recents: Vec<RecentConnection>,
        error: Option<ConnectError>,
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
        let remote_exec_wrapper_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(defaults.remote_exec_wrapper.clone().unwrap_or_default())
                .placeholder("docker exec -i <container>")
        });
        let passphrase_input = cx.new(|cx| InputState::new(window, cx).masked(true));

        // Enter in any field submits the card, matching the design contract
        // ("one click connects" applies equally to Enter).
        for input in [
            &host_input,
            &user_input,
            &port_input,
            &key_input,
            &remote_exec_wrapper_input,
            &passphrase_input,
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

        // The passphrase row's visibility reacts live to the SSH key field —
        // typing/pasting a different path re-probes it, independent of the
        // Enter-submits subscription above (which only reacts to
        // `PressEnter`).
        cx.subscribe_in(
            &key_input,
            window,
            |this, _input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh_key_encrypted(cx);
                }
            },
        )
        .detach();

        let (error, passphrase_error, force_encrypted) = match error {
            Some(ConnectError::General(message)) => (Some(message), None, false),
            Some(ConnectError::Passphrase(message)) => (None, Some(message), true),
            None => (None, None, false),
        };
        let key_encrypted = force_encrypted || key_needs_passphrase(&defaults.key);

        Self {
            host_input,
            user_input,
            port_input,
            key_input,
            remote_exec_wrapper_input,
            passphrase_input,
            recents,
            error,
            passphrase_error,
            key_encrypted,
        }
    }

    /// Re-probe the SSH key field's current path and update [`Self::key_encrypted`]
    /// (only notifying when it actually changes, so an unrelated keystroke in
    /// an already-settled state does not re-render for nothing).
    fn refresh_key_encrypted(&mut self, cx: &mut Context<Self>) {
        let key_text = self.key_input.read(cx).value().trim().to_string();
        let encrypted = !key_text.is_empty() && key_needs_passphrase(Path::new(&key_text));
        if encrypted != self.key_encrypted {
            self.key_encrypted = encrypted;
            cx.notify();
        }
    }

    /// Validate the card's current field values, emitting [`ConnectionScreenEvent::Connect`]
    /// on success or setting the field/banner error on failure — never both.
    fn try_connect(&mut self, cx: &mut Context<Self>) {
        match self.build_request(cx) {
            Ok(request) => {
                self.error = None;
                self.passphrase_error = None;
                cx.emit(ConnectionScreenEvent::Connect(request));
                cx.notify();
            }
            Err(ConnectError::General(message)) => {
                self.error = Some(message);
                self.passphrase_error = None;
                cx.notify();
            }
            Err(ConnectError::Passphrase(message)) => {
                self.error = None;
                self.passphrase_error = Some(message);
                cx.notify();
            }
        }
    }

    /// Read the inputs and validate them into a [`ConnectRequest`]. Host and
    /// User must be non-empty; Port must parse as a `u16`; the SSH key path
    /// must be non-empty. The session is no longer a card field (issues
    /// #706/#707/#705, `docs/spec-post-connect-picker.md`): the plain
    /// "Connect \u{2192}" button's [`SessionIntent`] is resolved from
    /// `RIFT_SESSION`, read at connect time via [`session_intent_from_env`].
    /// When the key is detected as encrypted, the passphrase field must be
    /// non-empty too (#478) — surfaced via [`ConnectError::Passphrase`] so it
    /// renders at that field rather than the bottom banner.
    fn build_request(&self, cx: &App) -> Result<ConnectRequest, ConnectError> {
        let host = self.host_input.read(cx).value().trim().to_string();
        if host.is_empty() {
            return Err(ConnectError::General("Host is required.".to_string()));
        }
        let user = self.user_input.read(cx).value().trim().to_string();
        if user.is_empty() {
            return Err(ConnectError::General("User is required.".to_string()));
        }
        let port_text = self.port_input.read(cx).value();
        let port_text = port_text.trim();
        let port: u16 = port_text
            .parse()
            .map_err(|_| ConnectError::General(format!("\"{port_text}\" is not a valid port.")))?;
        let key_text = self.key_input.read(cx).value().trim().to_string();
        if key_text.is_empty() {
            return Err(ConnectError::General(
                "SSH key path is required.".to_string(),
            ));
        }
        let passphrase = if self.key_encrypted {
            let value = self.passphrase_input.read(cx).value().to_string();
            if value.is_empty() {
                return Err(ConnectError::Passphrase(
                    "A passphrase is required for this SSH key.".to_string(),
                ));
            }
            Some(value)
        } else {
            None
        };
        let remote_exec_wrapper =
            remote_exec_wrapper_from_field(&self.remote_exec_wrapper_input.read(cx).value());

        Ok(ConnectRequest {
            host,
            user,
            port,
            key: PathBuf::from(key_text),
            remote_exec_wrapper,
            session_intent: session_intent_from_env(std::env::var("RIFT_SESSION").ok().as_deref()),
            passphrase,
        })
    }

    /// A RECENT row click: prefill every field with that entry's values (so
    /// a failed attempt still shows what was tried) and emit `Connect`
    /// immediately — "clickable (prefill + connect)" per the issue's
    /// acceptance. The recent's remembered session (issue #707) becomes a
    /// [`SessionIntent::Preferred`] via [`session_intent_from_recent`] —
    /// validated against the live host list once connected, not attached
    /// blindly. Recents never carry a passphrase (never persisted — #478),
    /// so a click landing on an encrypted key stops short of connecting and
    /// prompts for it instead of spinning up a connect attempt that would
    /// deterministically fail. The wrapper field is prefilled from the
    /// recent's stored `remote_exec_wrapper` (issue #790) — recent takes
    /// precedence over the fresh-card env/bake prefill, mirroring the
    /// host/user/port/key reset exactly (an empty stored wrapper clears the
    /// field, a normal host connection). Silently ignored if `index` is
    /// stale (the list changed under a slow click), rather than panicking.
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
        self.passphrase_input
            .update(cx, |input, cx| input.set_value(String::new(), window, cx));
        self.remote_exec_wrapper_input.update(cx, |input, cx| {
            input.set_value(recent.remote_exec_wrapper.clone(), window, cx)
        });
        self.refresh_key_encrypted(cx);

        if self.key_encrypted {
            self.error = None;
            self.passphrase_error = Some("Enter the passphrase for this SSH key.".to_string());
            cx.notify();
            return;
        }

        self.error = None;
        self.passphrase_error = None;
        let remote_exec_wrapper =
            remote_exec_wrapper_from_field(&self.remote_exec_wrapper_input.read(cx).value());
        cx.emit(ConnectionScreenEvent::Connect(ConnectRequest {
            host: recent.host,
            user: recent.user,
            port: recent.port,
            key: PathBuf::from(recent.key),
            remote_exec_wrapper,
            session_intent: session_intent_from_recent(&recent.session),
            passphrase: None,
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

/// The Passphrase row (design contract: "+ passphrase row when the key is
/// encrypted", #478): a masked [`render_field`], with its own error banner
/// directly beneath it when `error` is set — a field-level error, distinct
/// from the card's bottom banner, so a wrong or missing passphrase points at
/// the field to fix.
fn render_passphrase_field(
    cx: &mut Context<ConnectionScreen>,
    input: &Entity<InputState>,
    error: Option<&str>,
) -> impl IntoElement {
    v_flex()
        .gap(px(8.0))
        .child(render_field(cx, "Passphrase", input, IconName::Asterisk))
        .children(error.map(|message| render_error_banner(cx, message)))
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
/// caption, an optional muted wrapper indicator (issue #790 — only rendered
/// when the entry's `remote_exec_wrapper` is non-empty, so a container
/// connection is visually distinguishable from a plain one), and a trailing
/// relative-time label. Clicking anywhere on the row prefills and connects
/// ([`ConnectionScreen::connect_from_recent`]).
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
    let wrapper = SharedString::from(recent.remote_exec_wrapper.clone());
    let has_wrapper = !recent.remote_exec_wrapper.is_empty();
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
        .when(has_wrapper, |el| {
            el.child(
                div()
                    .flex_none()
                    .max_w(px(120.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .bg(tile_bg)
                    .text_size(px(10.0))
                    .text_color(muted)
                    .truncate()
                    .child(wrapper),
            )
        })
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
        let passphrase_row = self.key_encrypted.then(|| {
            render_passphrase_field(cx, &self.passphrase_input, self.passphrase_error.as_deref())
        });
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
                "Remote exec wrapper",
                &self.remote_exec_wrapper_input,
                IconName::SquareTerminal,
            ))
            .children(passphrase_row)
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

        // The custom title bar (#511, `docs/spec-cockpit-chrome.md`): the
        // Connection screen's "not connected" group — no settings gear here,
        // the settings surface needs a live `SessionView` that does not exist
        // before a connection succeeds (#366).
        let title_bar = title_bar::render(
            title_bar::ConnectionGroup::not_connected(cx),
            None,
            None,
            cx,
        );

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
                        .child(card)
                        .children(recents_section),
                ),
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
            windows: false,
            remote_exec_wrapper: Some("docker exec -i devenv"),
            baked_remote_exec_wrapper: Some("docker exec -i baked"),
        });

        assert_eq!(defaults.host, "100.64.0.1");
        assert_eq!(defaults.user, "alice");
        assert_eq!(defaults.port, 2222);
        assert_eq!(defaults.key, PathBuf::from("/keys/mine"));
        assert_eq!(
            defaults.remote_exec_wrapper,
            Some("docker exec -i devenv".to_string())
        );
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

    // ── resolve_defaults: remote_exec_wrapper prefill (issue #789) ─────────

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_remote_exec_wrapper_unset_is_none() {
        let defaults = resolve_defaults(DefaultsInputs::default());

        assert_eq!(defaults.remote_exec_wrapper, None);
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_remote_exec_wrapper_prefers_runtime_over_baked() {
        let defaults = resolve_defaults(DefaultsInputs {
            remote_exec_wrapper: Some("docker exec -i runtime"),
            baked_remote_exec_wrapper: Some("docker exec -i baked"),
            ..Default::default()
        });

        assert_eq!(
            defaults.remote_exec_wrapper,
            Some("docker exec -i runtime".to_string())
        );
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_remote_exec_wrapper_falls_back_to_baked_when_runtime_unset() {
        let defaults = resolve_defaults(DefaultsInputs {
            baked_remote_exec_wrapper: Some("docker exec -i baked"),
            ..Default::default()
        });

        assert_eq!(
            defaults.remote_exec_wrapper,
            Some("docker exec -i baked".to_string())
        );
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_remote_exec_wrapper_blank_runtime_falls_back_to_baked() {
        let defaults = resolve_defaults(DefaultsInputs {
            remote_exec_wrapper: Some("   "),
            baked_remote_exec_wrapper: Some("docker exec -i baked"),
            ..Default::default()
        });

        assert_eq!(
            defaults.remote_exec_wrapper,
            Some("docker exec -i baked".to_string())
        );
    }

    #[::core::prelude::v1::test]
    fn test_resolve_defaults_remote_exec_wrapper_blank_baked_is_none() {
        let defaults = resolve_defaults(DefaultsInputs {
            baked_remote_exec_wrapper: Some("   "),
            ..Default::default()
        });

        assert_eq!(defaults.remote_exec_wrapper, None);
    }

    // ── key_needs_passphrase ──────────────────────────────────────────────
    //
    // The detection algorithm itself (encrypted vs. plain vs. malformed key)
    // is `rift_ssh::key_requires_passphrase`'s own tested surface; this only
    // covers the one bit of logic this wrapper adds — an `Err` probe (missing
    // file, unreadable, unsupported format) coalesces to "not encrypted"
    // rather than propagating, so it never blocks the row from rendering.

    #[::core::prelude::v1::test]
    fn test_key_needs_passphrase_probe_error_coalesces_to_false() {
        let path = Path::new("/nonexistent/rift-connection-screen-test-key");
        assert!(!key_needs_passphrase(path));
    }

    // ── remote_exec_wrapper_from_field (issue #789, connect-time field) ────

    #[::core::prelude::v1::test]
    fn test_remote_exec_wrapper_from_field_non_empty_returns_some() {
        assert_eq!(
            remote_exec_wrapper_from_field("docker exec -i devenv"),
            Some("docker exec -i devenv".to_string())
        );
    }

    #[::core::prelude::v1::test]
    fn test_remote_exec_wrapper_from_field_empty_returns_none() {
        assert_eq!(remote_exec_wrapper_from_field(""), None);
    }

    #[::core::prelude::v1::test]
    fn test_remote_exec_wrapper_from_field_whitespace_returns_none() {
        assert_eq!(remote_exec_wrapper_from_field("   "), None);
    }

    #[::core::prelude::v1::test]
    fn test_remote_exec_wrapper_no_interaction_matches_resolved_default() {
        // A fresh card prefills the field from `resolve_defaults`'s wrapper
        // value (`ConnectionScreen::new`); with no edit, reading the field
        // back at connect (`remote_exec_wrapper_from_field`) must reproduce
        // exactly what the prefill resolved — the "env/bake dogfooding path
        // must not regress" requirement, without depending on live env vars.
        let defaults = resolve_defaults(DefaultsInputs {
            remote_exec_wrapper: Some("docker exec -i devenv"),
            ..Default::default()
        });
        let field_value = defaults.remote_exec_wrapper.clone().unwrap_or_default();

        assert_eq!(
            remote_exec_wrapper_from_field(&field_value),
            defaults.remote_exec_wrapper
        );
    }

    #[::core::prelude::v1::test]
    fn test_remote_exec_wrapper_no_interaction_empty_default_stays_none() {
        let defaults = resolve_defaults(DefaultsInputs::default());
        let field_value = defaults.remote_exec_wrapper.clone().unwrap_or_default();

        assert_eq!(defaults.remote_exec_wrapper, None);
        assert_eq!(remote_exec_wrapper_from_field(&field_value), None);
    }

    // ── ConnectRequest (Debug redaction) ──────────────────────────────────

    fn sample_request(passphrase: Option<&str>) -> ConnectRequest {
        ConnectRequest {
            host: "100.64.0.1".to_string(),
            user: "developer".to_string(),
            port: 22,
            key: PathBuf::from("/home/developer/.ssh/id_ed25519"),
            remote_exec_wrapper: None,
            session_intent: SessionIntent::Fixed("rift".to_string()),
            passphrase: passphrase.map(str::to_string),
        }
    }

    #[::core::prelude::v1::test]
    fn test_connect_request_debug_with_passphrase_redacts_it() {
        let debug = format!("{:?}", sample_request(Some("correct horse battery staple")));

        assert!(!debug.contains("correct horse battery staple"));
        assert!(debug.contains("<redacted>"));
    }

    #[::core::prelude::v1::test]
    fn test_connect_request_debug_without_passphrase_shows_none() {
        let debug = format!("{:?}", sample_request(None));

        assert!(debug.contains("passphrase: None"));
    }

    #[::core::prelude::v1::test]
    fn test_connect_request_debug_shows_remote_exec_wrapper_unredacted() {
        let mut request = sample_request(None);
        request.remote_exec_wrapper = Some("docker exec -i devenv".to_string());

        let debug = format!("{:?}", request);

        assert!(debug.contains("docker exec -i devenv"));
    }

    // ── SessionIntent (entry-point routing, issue #707) ────────────────────

    #[::core::prelude::v1::test]
    fn test_session_intent_from_env_set_returns_fixed() {
        assert_eq!(
            session_intent_from_env(Some("rift-dev")),
            SessionIntent::Fixed("rift-dev".to_string())
        );
    }

    #[::core::prelude::v1::test]
    fn test_session_intent_from_env_unset_returns_pick() {
        assert_eq!(session_intent_from_env(None), SessionIntent::Pick);
    }

    #[::core::prelude::v1::test]
    fn test_session_intent_from_env_empty_returns_pick() {
        assert_eq!(session_intent_from_env(Some("")), SessionIntent::Pick);
    }

    #[::core::prelude::v1::test]
    fn test_session_intent_from_recent_present_returns_preferred() {
        assert_eq!(
            session_intent_from_recent("work"),
            SessionIntent::Preferred("work".to_string())
        );
    }

    #[::core::prelude::v1::test]
    fn test_session_intent_from_recent_empty_returns_pick() {
        assert_eq!(session_intent_from_recent(""), SessionIntent::Pick);
    }
}
