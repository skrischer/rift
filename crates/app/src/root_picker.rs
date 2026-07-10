//! The remote root picker (issue #768, `docs/spec-session-root-picker.md`):
//! the Frame-C browse-and-name surface that resolves a project root on the
//! connected host — header, breadcrumb, name-sorted directory rows (folder
//! glyph, name, a git flag + branch when the row is a repo), and a footer
//! with a session-name field seeded from the current level's basename plus a
//! Create button. Browsing walks the daemon's directory-browse channel
//! (`ClientMessage::QueryDirEntries` / `DaemonMessage::DirEntriesReply`,
//! issues #766/#767) one level at a time.
//!
//! Deliberately GPUI-view-only, mirroring
//! [`crate::session_picker::SessionPicker`]: this view never sends or
//! receives a `ClientMessage`/`DaemonMessage` itself.
//! [`RootPickerEvent::Browse`] asks the owner to issue the request; the
//! owner feeds the reply back through
//! [`RootPicker::apply_dir_entries_reply`]. An `error` reply is rendered
//! inline without tearing the picker down.
//!
//! Wiring this picker into the Phase-33 picker container, the in-cockpit
//! session-strip "+", and the create-with-root `Attach` send are issue
//! #769's job — this module only builds and tests the surface + its browse
//! state, exposing [`RootPickerEvent::Picked`] for the caller to act on.

use gpui::{
    div, px, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _,
    Sizable as _,
};

use rift_protocol::{CloneError, DirBrowseError, DirEntry};

/// Card width, matching `session_picker`/`connection_screen`'s own
/// `CARD_WIDTH` (design contract: "card ~470px") — kept as its own constant
/// rather than exported from either sibling module, mirroring their own
/// precedent for small duplicated visual primitives.
const CARD_WIDTH: f32 = 470.0;

/// The max height of the rows region before it scrolls internally (issue
/// #802, mirroring #792's `session_picker::ROWS_MAX_HEIGHT`): an unbounded
/// directory list runs the card off-screen with many entries.
const ROWS_MAX_HEIGHT: f32 = 360.0;

/// Whether an incoming `DirEntriesReply`'s resolved `path` answers the
/// currently outstanding `QueryDirEntries` request (issue #769: the owner's
/// correlation guard — `RootPicker` itself tracks no in-flight path, so a
/// stale/duplicate/out-of-order reply, e.g. from a request abandoned when the
/// owner closed and reopened a fresh picker, must never clobber the CURRENT
/// level). `pending` is the exact path string the owner last sent on
/// `QueryDirEntries` (`None` when nothing is outstanding, e.g. no browse has
/// been issued yet, or the last reply was already consumed).
///
/// A plain equality check is exact for every non-seed request: every path
/// after the first is built from a previously RESOLVED value
/// (`current_path`/`parent`/a breadcrumb segment), so the daemon echoes it
/// back unchanged. The one request whose resolution the client cannot
/// predict up front is the seed (`""` or a `~`-prefixed recent root,
/// resolving to `$HOME`) — matched unconditionally, since at most one such
/// request is ever outstanding for a freshly opened picker.
pub fn browse_reply_matches(pending: Option<&str>, reply_path: &str) -> bool {
    match pending {
        None => false,
        Some(pending) => pending.is_empty() || pending.starts_with('~') || pending == reply_path,
    }
}

/// Disambiguate a picked session `name` against `existing` live session names
/// (`docs/spec-session-root-picker.md`'s create-with-root guarantee): tmux's
/// `new-session -A -s <name>` attaches an existing session of that name
/// instead of creating one — ignoring `-c` — so a picked root's basename
/// colliding with a live session would silently land the create in, and
/// re-stamp the `@root` of, an unrelated project. Returns `name` unchanged
/// when it does not collide; otherwise appends the lowest `-<n>` (`n >= 2`)
/// that is not itself already taken (`rift` -> `rift-2` -> `rift-3` ...). The
/// owner calls this right before sending `Attach` on every
/// [`RootPickerEvent::Picked`] — the client-side guarantee that a create
/// never lands in an unrelated existing session.
pub fn disambiguate_session_name(name: &str, existing: &[String]) -> String {
    if !existing.iter().any(|session| session == name) {
        return name.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{name}-{suffix}");
        if !existing.iter().any(|session| session == &candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Pick the picker's start level: the most-recently-picked root (the
/// phase-9 `window_state` store's `recent_roots`, front = newest) if any,
/// else an empty string — the daemon's own `QueryDirEntries` resolution of
/// `""` to `$HOME` (`docs/spec-session-root-picker.md`). A pure function so
/// the caller (eventually issue #769's entry-point wiring) can compute it
/// without touching the store from inside this view.
pub fn start_path(recent_roots: &[String]) -> String {
    recent_roots.first().cloned().unwrap_or_default()
}

/// The last path segment of an absolute POSIX host `path` — the folder name
/// the session-name field seeds from. `"/"` (the filesystem root) and `""`
/// (not yet resolved) have no name segment of their own, so both fall back
/// to `path` verbatim rather than an empty session name.
fn basename(path: &str) -> String {
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => path.to_string(),
    }
}

/// Join the current level's resolved `parent` with a child directory `name`
/// picked from its listing. Always POSIX `/` — the daemon's browse targets
/// the remote host regardless of the client's own OS, so
/// `std::path::Path::join` (which would use the client's separator) is
/// deliberately not used here.
///
/// Also used by `main.rs`/`workspace.rs` (issue #829) to predict the
/// `<parent>/<name>` a `ClientMessage::CloneRepo` will echo back on
/// `DaemonMessage::CloneResult::path`, the same correlation role
/// [`browse_reply_matches`] plays for browse replies — `pub`, since those
/// owners reach it through the `rift_app` library crate's public surface
/// (`rift_app::root_picker::join_child`), the same boundary
/// [`browse_reply_matches`]/[`disambiguate_session_name`]/[`start_path`]
/// already cross.
pub fn join_child(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// Derive the clone-mode name field's default from a git `url`
/// (`docs/spec-clone-repo.md`'s "name ... default = the repo basename from
/// the URL"): the last `/`-separated path segment, with a trailing `.git`
/// stripped. Handles scp-like remotes (`git@host:org/repo.git`) by also
/// splitting on `:` when the final segment still carries one (no `/` in the
/// whole URL, e.g. a bare `host:repo.git`). Returns an empty string for a
/// blank/whitespace-only `url` — the picker leaves the name field blank
/// until a URL is entered, rather than guessing.
pub fn repo_basename_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    let after_slash = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let after_colon = after_slash.rsplit(':').next().unwrap_or(after_slash);
    after_colon
        .strip_suffix(".git")
        .unwrap_or(after_colon)
        .to_string()
}

/// One clickable breadcrumb segment: its display label and the absolute
/// path selecting it browses to. `path.is_empty()` (nothing resolved yet)
/// yields no segments; `"/"` alone yields the single root segment; every
/// other absolute path is prefixed with the root segment, then one segment
/// per `/`-separated component.
fn breadcrumb_segments(path: &str) -> Vec<(String, String)> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut segments = vec![("/".to_string(), "/".to_string())];
    let mut acc = String::new();
    for part in path.split('/').filter(|segment| !segment.is_empty()) {
        acc.push('/');
        acc.push_str(part);
        segments.push((part.to_string(), acc.clone()));
    }
    segments
}

/// Short, user-facing text for a [`DirBrowseError`] rendered inline by
/// [`render_error_banner`] — mirrors `file_tree::describe_file_op_error`'s
/// shape.
fn describe_dir_browse_error(error: DirBrowseError) -> &'static str {
    match error {
        DirBrowseError::NotFound => "This folder no longer exists",
        DirBrowseError::PermissionDenied => "Permission denied",
        DirBrowseError::NotADirectory => "Not a folder",
        DirBrowseError::Io => "Could not read this folder",
    }
}

/// Short, user-facing text for a [`CloneError`], mirroring
/// [`describe_dir_browse_error`]'s shape for the clone channel
/// (`docs/spec-clone-repo.md`).
fn describe_clone_error(error: CloneError) -> &'static str {
    match error {
        CloneError::InvalidUrl => "Not a valid git URL",
        CloneError::AuthFailed => "Authentication failed for this repository",
        CloneError::TargetExists => "A folder with that name already exists",
        CloneError::Network => "Could not reach the repository",
        CloneError::GitUnavailable => "git is not installed on the host",
        CloneError::Other => "Clone failed",
    }
}

/// True when `name` cannot be a clone target directory name — mirrors the
/// daemon's own `clone::validate_name` (`docs/spec-clone-repo.md`, issue
/// #841): empty, `.`, `..`, or containing a path separator. Checked
/// client-side before sending `CloneRepo` (issue #839) so a doomed request
/// never round-trips to the daemon and leaves the spinner with no resolved
/// path to correlate against.
fn invalid_clone_name(name: &str) -> bool {
    name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\'])
}

/// Which surface the root picker's card body renders (issue #829, "Browse ⇄
/// Clone toggle", `docs/spec-clone-repo.md`'s "clone surface is a mode inside
/// Frame C" decision) — a browse-and-pick level, or a clone-from-URL form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerMode {
    Browse,
    Clone,
}

/// Emitted by [`RootPicker`]; the owner never touches this view's internals
/// directly, mirroring [`crate::session_picker::SessionPickerEvent`].
pub enum RootPickerEvent {
    /// Ask the owner to send `ClientMessage::QueryDirEntries { path }` — the
    /// owner drives the actual protocol round-trip and later feeds the
    /// reply back through [`RootPicker::apply_dir_entries_reply`]. Emitted
    /// by every [`RootPicker::browse`] call: the caller's initial seed, a
    /// row click, a breadcrumb segment, or the parent affordance.
    Browse(String),
    /// Ask the owner to send `ClientMessage::CloneRepo { url, parent, name }`
    /// (issue #829, `docs/spec-clone-repo.md`) — the owner sends the request
    /// and later feeds the reply back through
    /// [`RootPicker::apply_clone_result`]. Emitted by
    /// [`RootPicker::start_clone`] (the Clone action / an Enter press in the
    /// URL or name field).
    Clone {
        url: String,
        parent: String,
        name: String,
    },
    /// The user confirmed Create (browse mode) or a clone finished
    /// successfully (clone mode, `path` as the root): the picked root and
    /// the session name. Both paths drive the same create-with-root
    /// `Attach { root: Some(...) }` flow — the owner does not distinguish
    /// where the root came from.
    Picked { root: String, name: String },
}

/// The root picker view (issue #768): a card with a breadcrumb, a
/// name-sorted directory listing, and a name + Create footer, styled after
/// [`crate::session_picker::SessionPicker`] (same theme tokens, same card
/// shape) — presented as a modal/panel by the caller, not a full screen of
/// its own.
pub struct RootPicker {
    /// Absolute path of the currently displayed level, once resolved by the
    /// daemon. Empty until the first successful `DirEntriesReply`.
    current_path: String,
    /// The current level's parent, or `None` at the filesystem root.
    parent: Option<String>,
    /// The current level's child directories, as replied (already
    /// name-sorted by the daemon).
    entries: Vec<DirEntry>,
    /// True while a `QueryDirEntries` request is in flight. A [`Self::browse`]
    /// call while loading is a no-op — no overlapping requests.
    loading: bool,
    /// The last browse failure, rendered inline; cleared by the next
    /// successful reply. The picker keeps the last good level visible
    /// underneath rather than tearing down.
    error: Option<DirBrowseError>,
    /// The session-name field: seeded with the current level's basename on
    /// every successful browse, freely editable from there.
    name_input: Entity<InputState>,
    _name_subscription: Subscription,
    focus_handle: FocusHandle,
    /// Tracks the rows region's scroll offset (issue #804): shared between
    /// the scrolling `v_flex` (`.track_scroll`) and the overlay [`Scrollbar`]
    /// so the thumb reflects and drives the same scroll position.
    scroll_handle: ScrollHandle,
    /// Which body the card currently renders — the Browse ⇄ Clone toggle
    /// (issue #829).
    mode: PickerMode,
    /// The clone-mode git-URL field.
    clone_url_input: Entity<InputState>,
    _clone_url_subscription: Subscription,
    /// The clone-mode target-parent field, defaulting to (and reseeded on
    /// every successful browse alongside [`Self::name_input`] with) the
    /// currently browsed level — editable, so the operator can clone
    /// somewhere other than where they last browsed to.
    clone_parent_input: Entity<InputState>,
    /// The clone-mode target-name field, defaulting to
    /// [`repo_basename_from_url`] of the URL field's value; editable, and
    /// once a user edit is observed (`clone_name_touched`) no longer
    /// overwritten by further URL edits.
    clone_name_input: Entity<InputState>,
    _clone_name_subscription: Subscription,
    clone_name_touched: bool,
    /// True while a `CloneRepo` request is in flight — the Clone action is a
    /// no-op while set, mirroring `loading`'s guard against overlapping
    /// browse requests.
    cloning: bool,
    /// The last clone failure, rendered inline; cleared by the next
    /// [`Self::start_clone`] call or a successful [`Self::apply_clone_result`].
    clone_error: Option<CloneError>,
    /// A client-side rejection of the name field, set by [`Self::start_clone`]
    /// instead of emitting [`RootPickerEvent::Clone`] (issue #839): a name
    /// containing a path separator, `.`, or `..` would make the daemon's own
    /// `validate_name` reject the request early and never round-trip a
    /// resolved path, so the picker catches it before send rather than
    /// leaving the spinner with nothing to correlate against. Distinct from
    /// [`Self::clone_error`] (a daemon-reported [`CloneError`]) since this
    /// never reaches the wire.
    clone_name_error: Option<&'static str>,
}

impl RootPicker {
    /// Construct an unresolved picker (no level loaded yet). The caller
    /// kicks off the first browse explicitly — typically right after
    /// subscribing to this entity's events — via
    /// `picker.update(cx, |picker, cx| picker.browse(start_path(&recents), cx))`,
    /// mirroring [`crate::session_picker::SessionPicker`]'s "the caller drives
    /// the cross-thread coordination" split.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("session name"));
        let subscription = cx.subscribe_in(
            &name_input,
            window,
            |this, _input, event: &InputEvent, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.create(cx);
                }
            },
        );

        let clone_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://github.com/org/repo.git"));
        let clone_url_subscription = cx.subscribe_in(
            &clone_url_input,
            window,
            |this, _input, event: &InputEvent, window, cx| match event {
                InputEvent::Change if !this.clone_name_touched => {
                    let url = this.clone_url_input.read(cx).value().to_string();
                    let suggested = repo_basename_from_url(&url);
                    this.clone_name_input
                        .update(cx, |input, cx| input.set_value(suggested, window, cx));
                }
                InputEvent::PressEnter { .. } => this.start_clone(cx),
                _ => {}
            },
        );

        let clone_parent_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("parent folder"));

        let clone_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("repo name"));
        let clone_name_subscription = cx.subscribe_in(
            &clone_name_input,
            window,
            |this, _input, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => this.clone_name_touched = true,
                InputEvent::PressEnter { .. } => this.start_clone(cx),
                _ => {}
            },
        );

        Self {
            current_path: String::new(),
            parent: None,
            entries: Vec::new(),
            loading: false,
            error: None,
            name_input,
            _name_subscription: subscription,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::default(),
            mode: PickerMode::Browse,
            clone_url_input,
            _clone_url_subscription: clone_url_subscription,
            clone_parent_input,
            clone_name_input,
            _clone_name_subscription: clone_name_subscription,
            clone_name_touched: false,
            cloning: false,
            clone_error: None,
            clone_name_error: None,
        }
    }

    /// Request `path` (an absolute host path, or `""` for `$HOME`): sets the
    /// loading state and asks the owner to send `QueryDirEntries` via
    /// [`RootPickerEvent::Browse`]. A request already in flight makes this a
    /// no-op, so a fast double-click can never race two replies.
    pub fn browse(&mut self, path: String, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        cx.emit(RootPickerEvent::Browse(path));
        cx.notify();
    }

    /// Feed back the daemon's reply to the most recent [`Self::browse`] call
    /// — the owner calls this with a `DaemonMessage::DirEntriesReply`'s
    /// fields when one arrives while this picker is showing (the owner
    /// destructures the enum variant, mirroring
    /// `EditorView::apply_document_symbol_response`'s shape rather than
    /// taking the raw `DaemonMessage`). On success, replaces the displayed
    /// level and reseeds the name field with the new path's basename, and
    /// the clone-mode target-parent field with the resolved path itself
    /// (issue #829's "default = the current browse path" — reseeded here,
    /// alongside the name field, rather than only on a mode switch, so it
    /// always reflects the level last browsed to); on failure, keeps the
    /// last good level and renders `error` inline
    /// (`docs/spec-session-root-picker.md`'s browse-error mitigation — never
    /// tears down).
    pub fn apply_dir_entries_reply(
        &mut self,
        path: String,
        parent: Option<String>,
        entries: Vec<DirEntry>,
        error: Option<DirBrowseError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        if let Some(error) = error {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.error = None;
        self.current_path = path;
        self.parent = parent;
        self.entries = entries;
        let seeded = basename(&self.current_path);
        self.name_input
            .update(cx, |input, cx| input.set_value(seeded, window, cx));
        let current_path = self.current_path.clone();
        self.clone_parent_input
            .update(cx, |input, cx| input.set_value(current_path, window, cx));
        cx.notify();
    }

    /// A row click: descend into `name`, a child of the current level.
    fn select_entry(&mut self, name: &str, cx: &mut Context<Self>) {
        let path = join_child(&self.current_path, name);
        self.browse(path, cx);
    }

    /// The parent (`..`) affordance: ascend one level. A no-op at the
    /// filesystem root, where `parent` is `None`.
    fn go_to_parent(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.parent.clone() {
            self.browse(parent, cx);
        }
    }

    /// The Create button / name-field Enter: emit the picked root and
    /// trimmed session name. A no-op until a level has resolved
    /// (`current_path` non-empty) and the name field holds non-whitespace
    /// text — a tmux session needs a name.
    fn create(&mut self, cx: &mut Context<Self>) {
        if self.current_path.is_empty() {
            return;
        }
        let name = self.name_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        cx.emit(RootPickerEvent::Picked {
            root: self.current_path.clone(),
            name,
        });
    }

    /// The Browse ⇄ Clone toggle: switches which body the card renders. A
    /// no-op when already in `mode` — this only flips a display switch, the
    /// underlying browse level and clone fields are untouched either way.
    fn set_mode(&mut self, mode: PickerMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        cx.notify();
    }

    /// The Clone action / an Enter press in the URL or name field: emit the
    /// trimmed URL/parent/name via [`RootPickerEvent::Clone`]. A no-op while
    /// a clone is already in flight, or until all three fields hold
    /// non-whitespace text — mirroring [`Self::browse`]'s in-flight guard and
    /// [`Self::create`]'s blank-field guard. A `name` [`invalid_clone_name`]
    /// rejects (a path separator, `.`, or `..`) is caught here rather than
    /// sent (issue #839): the daemon would reject it too, but only after
    /// echoing an unresolved path nothing downstream can correlate — this
    /// picker shows the reason inline instead and never sends the doomed
    /// request.
    fn start_clone(&mut self, cx: &mut Context<Self>) {
        if self.cloning {
            return;
        }
        let url = self.clone_url_input.read(cx).value().trim().to_string();
        let parent = self.clone_parent_input.read(cx).value().trim().to_string();
        let name = self.clone_name_input.read(cx).value().trim().to_string();
        if url.is_empty() || parent.is_empty() || name.is_empty() {
            return;
        }
        if invalid_clone_name(&name) {
            self.clone_name_error = Some("Name must not be '.', '..', or contain a path separator");
            cx.notify();
            return;
        }
        self.cloning = true;
        self.clone_error = None;
        self.clone_name_error = None;
        cx.emit(RootPickerEvent::Clone { url, parent, name });
        cx.notify();
    }

    /// Feed back the daemon's reply to the most recent [`Self::start_clone`]
    /// call — the owner calls this with a `DaemonMessage::CloneResult`'s
    /// fields when one arrives while this picker is showing, mirroring
    /// [`Self::apply_dir_entries_reply`]'s shape. On success, emits
    /// [`RootPickerEvent::Picked`] with `path` as the root and the clone's
    /// name field as the session name — the exact event the browse-and-pick
    /// Create emits, so the owner drives the same create-with-root path
    /// without a clone-specific branch (`docs/spec-clone-repo.md`: "drive the
    /// existing create-with-root path ... reuse it, do not reinvent"). On
    /// failure, renders `error` inline and leaves the clone form in place for
    /// a retry.
    pub fn apply_clone_result(
        &mut self,
        path: String,
        error: Option<CloneError>,
        cx: &mut Context<Self>,
    ) {
        self.cloning = false;
        if let Some(error) = error {
            self.clone_error = Some(error);
            cx.notify();
            return;
        }
        self.clone_error = None;
        let name = self.clone_name_input.read(cx).value().trim().to_string();
        cx.emit(RootPickerEvent::Picked { root: path, name });
        cx.notify();
    }
}

impl EventEmitter<RootPickerEvent> for RootPicker {}

impl Focusable for RootPicker {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// The card's header row: the picker's title.
fn render_header(cx: &mut Context<RootPicker>) -> impl IntoElement {
    div()
        .text_size(px(15.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().foreground)
        .child("Choose a project root")
}

/// The breadcrumb: one clickable chip per [`breadcrumb_segments`] entry,
/// separated by a muted "/" — clicking a chip browses to its path. Empty
/// while no level has resolved yet.
fn render_breadcrumb(
    cx: &mut Context<RootPicker>,
    segments: &[(String, String)],
    loading: bool,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    if segments.is_empty() {
        let label = if loading { "Loading\u{2026}" } else { "" };
        return div()
            .text_size(px(12.0))
            .text_color(muted)
            .child(label)
            .into_any_element();
    }

    let foreground = cx.theme().foreground;
    let hover_bg = cx.theme().list_hover;
    let mono = cx.theme().mono_font_family.clone();
    let last_index = segments.len() - 1;

    let mut row = h_flex().w_full().items_center().flex_wrap();
    for (index, (label, target)) in segments.iter().enumerate() {
        if index > 0 {
            row = row.child(div().text_size(px(12.0)).text_color(muted).child("/"));
        }
        let is_last = index == last_index;
        let target = target.clone();
        row = row.child(
            div()
                .id(("root-picker-crumb", index))
                .px(px(4.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(move |el| el.bg(hover_bg))
                .font_family(mono.clone())
                .text_size(px(12.0))
                .text_color(if is_last { foreground } else { muted })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                        this.browse(target.clone(), cx);
                    }),
                )
                .child(SharedString::from(label.clone())),
        );
    }
    row.into_any_element()
}

/// The parent (`..`) row, shown when the current level has a parent.
fn render_parent_row(cx: &mut Context<RootPicker>) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let hover_bg = cx.theme().list_hover;
    h_flex()
        .id("root-picker-row-parent")
        .w_full()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(move |el| el.bg(hover_bg))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                this.go_to_parent(cx);
            }),
        )
        .child(Icon::new(IconName::Folder).text_color(muted))
        .child(div().text_size(px(13.0)).text_color(muted).child(".."))
}

/// One directory row: folder glyph, name, and a trailing git badge
/// (`\u{2442} <branch>`, or the bare glyph on a detached HEAD) when the entry
/// is a repo. Clicking anywhere on the row descends into it.
fn render_entry_row(
    cx: &mut Context<RootPicker>,
    index: usize,
    entry: &DirEntry,
) -> impl IntoElement {
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let hover_bg = cx.theme().list_hover;
    let mono = cx.theme().mono_font_family.clone();
    let name = SharedString::from(entry.name.clone());
    let click_name = entry.name.clone();
    let git_badge = entry.is_git_repo.then(|| {
        let text = match &entry.git_branch {
            Some(branch) => format!("\u{2442} {branch}"),
            None => "\u{2442}".to_string(),
        };
        div()
            .flex_none()
            .text_size(px(11.0))
            .text_color(muted)
            .child(SharedString::from(text))
    });

    h_flex()
        .id(("root-picker-row", index))
        .w_full()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(move |el| el.bg(hover_bg))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                this.select_entry(&click_name, cx);
            }),
        )
        .child(Icon::new(IconName::Folder).text_color(muted))
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
        .children(git_badge)
}

/// The zero-entries state: a loading placeholder while the first request of
/// a level is in flight, else a plain "empty folder" note.
fn render_empty_state(cx: &mut Context<RootPicker>, loading: bool) -> impl IntoElement {
    let label = if loading {
        "Loading\u{2026}"
    } else {
        "No folders here"
    };
    div()
        .w_full()
        .py(px(8.0))
        .text_size(px(12.0))
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

/// The inline error banner, shared by the browse and clone modes
/// (`docs/spec-session-root-picker.md`'s "renders inline without closing the
/// picker", extended to clone errors by `docs/spec-clone-repo.md`), mirroring
/// `connection_screen::render_error_banner`'s shape. `message` is a
/// mode-specific `describe_*_error` call at each of the two call sites.
fn render_error_banner(cx: &mut Context<RootPicker>, message: &'static str) -> impl IntoElement {
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
                .child(message),
        )
}

/// The footer: the session-name field plus the Create button, disabled until
/// a level has resolved and the name field holds non-whitespace text.
fn render_footer(
    cx: &mut Context<RootPicker>,
    name_input: &Entity<InputState>,
    can_create: bool,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap(px(8.0))
        .child(Input::new(name_input).flex_1())
        .child(
            Button::new("root-picker-create")
                .primary()
                .label("Create")
                .disabled(!can_create)
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.create(cx);
                })),
        )
}

/// The Browse ⇄ Clone toggle (issue #829): a compact, outlined
/// [`ButtonGroup`] of two tabs, styled after `diff_view`'s Unified/Split
/// toggle — the same "two-option mode switch" widget, reused rather than a
/// bespoke tab bar.
fn render_mode_toggle(cx: &mut Context<RootPicker>, mode: PickerMode) -> impl IntoElement {
    ButtonGroup::new("root-picker-mode")
        .compact()
        .outline()
        .xsmall()
        .child(
            Button::new("root-picker-mode-browse")
                .label("Browse")
                .selected(mode == PickerMode::Browse),
        )
        .child(
            Button::new("root-picker-mode-clone")
                .label("Clone")
                .selected(mode == PickerMode::Clone),
        )
        .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
            let mode = if clicks.contains(&1) {
                PickerMode::Clone
            } else {
                PickerMode::Browse
            };
            this.set_mode(mode, cx);
        }))
}

/// One clone-mode labeled input row: a small muted label above a mono-valued,
/// leading-icon input, mirroring `connection_screen::render_field`'s shape.
fn render_clone_field(
    cx: &mut Context<RootPicker>,
    label: &'static str,
    input: &Entity<InputState>,
    icon: IconName,
    disabled: bool,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .gap(px(4.0))
        .child(div().text_size(px(12.0)).text_color(muted).child(label))
        .child(
            Input::new(input)
                .font_family(mono)
                .disabled(disabled)
                .prefix(Icon::new(icon).text_color(muted)),
        )
}

/// The clone-mode body (issue #829, `docs/spec-clone-repo.md`): URL / target
/// parent / name fields, an inline error when the last clone failed, and the
/// Clone action — disabled until every field holds non-whitespace text, and
/// showing its own in-progress state (fields disabled, button labeled
/// "Cloning…" with a spinner) while a request is in flight.
#[allow(clippy::too_many_arguments)]
fn render_clone_body(
    cx: &mut Context<RootPicker>,
    url_input: &Entity<InputState>,
    parent_input: &Entity<InputState>,
    name_input: &Entity<InputState>,
    cloning: bool,
    error: Option<CloneError>,
    name_error: Option<&'static str>,
    can_clone: bool,
) -> impl IntoElement {
    // The client-side name rejection (issue #839) takes priority — it is the
    // more specific, more recent explanation for why nothing was sent.
    let error_banner = name_error
        .map(|message| render_error_banner(cx, message))
        .or_else(|| error.map(|error| render_error_banner(cx, describe_clone_error(error))));
    v_flex()
        .gap(px(12.0))
        .child(render_clone_field(
            cx,
            "Repository URL",
            url_input,
            IconName::Github,
            cloning,
        ))
        .child(render_clone_field(
            cx,
            "Parent folder",
            parent_input,
            IconName::Folder,
            cloning,
        ))
        .child(render_clone_field(
            cx,
            "Name",
            name_input,
            IconName::Frame,
            cloning,
        ))
        .children(error_banner)
        .child(
            h_flex().w_full().items_center().justify_end().child(
                Button::new("root-picker-clone")
                    .primary()
                    .label(if cloning { "Cloning\u{2026}" } else { "Clone" })
                    .loading(cloning)
                    .disabled(cloning || !can_clone)
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.start_clone(cx);
                    })),
            ),
        )
}

impl Render for RootPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode;
        let mode_toggle = render_mode_toggle(cx, mode);

        let content =
            match mode {
                PickerMode::Browse => {
                    let breadcrumb = breadcrumb_segments(&self.current_path);
                    let has_parent = self.parent.is_some();
                    let loading = self.loading;
                    let error = self.error;
                    let can_create = !self.current_path.is_empty()
                        && !self.name_input.read(cx).value().trim().is_empty();

                    let breadcrumb_el = render_breadcrumb(cx, &breadcrumb, loading);

                    let mut rows: Vec<gpui::AnyElement> = Vec::new();
                    if has_parent {
                        rows.push(render_parent_row(cx).into_any_element());
                    }
                    rows.extend(self.entries.iter().enumerate().map(|(index, entry)| {
                        render_entry_row(cx, index, entry).into_any_element()
                    }));

                    let body = if rows.is_empty() {
                        render_empty_state(cx, loading).into_any_element()
                    } else {
                        // Bounded height + internal scroll (issue #802, mirroring
                        // #792): a short list still renders compact since `max_h` only
                        // caps growth, and `overflow_y_scroll` only kicks in once the
                        // rows exceed it.
                        // The vertical `Scrollbar` (issue #804) is a sibling overlay in
                        // a `relative()` wrapper, bound to the same `scroll_handle` the
                        // rows track via `track_scroll` — gpui-component only paints it
                        // once the tracked content overflows the capped height, so a
                        // short list still stays scrollbar-free.
                        div()
                            .relative()
                            .child(
                                v_flex()
                                    .id("root-picker-rows")
                                    .gap(px(2.0))
                                    .max_h(px(ROWS_MAX_HEIGHT))
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll_handle)
                                    .children(rows),
                            )
                            .child(
                                Scrollbar::vertical(&self.scroll_handle)
                                    .scrollbar_show(ScrollbarShow::Always),
                            )
                            .into_any_element()
                    };

                    let error_banner = error
                        .map(|error| render_error_banner(cx, describe_dir_browse_error(error)));
                    let name_input = self.name_input.clone();
                    let footer = render_footer(cx, &name_input, can_create);

                    v_flex()
                        .gap(px(16.0))
                        .child(breadcrumb_el)
                        .children(error_banner)
                        .child(body)
                        .child(footer)
                        .into_any_element()
                }
                PickerMode::Clone => {
                    let url_input = self.clone_url_input.clone();
                    let parent_input = self.clone_parent_input.clone();
                    let name_input = self.clone_name_input.clone();
                    let can_clone = !url_input.read(cx).value().trim().is_empty()
                        && !parent_input.read(cx).value().trim().is_empty()
                        && !name_input.read(cx).value().trim().is_empty();
                    render_clone_body(
                        cx,
                        &url_input,
                        &parent_input,
                        &name_input,
                        self.cloning,
                        self.clone_error,
                        self.clone_name_error,
                        can_clone,
                    )
                    .into_any_element()
                }
            };

        v_flex()
            .w(px(CARD_WIDTH))
            .p(px(24.0))
            .gap(px(16.0))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(12.0))
            .track_focus(&self.focus_handle)
            .child(render_header(cx))
            .child(mode_toggle)
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    // --- pure helpers --------------------------------------------------------

    #[test]
    fn test_browse_reply_matches_none_pending_drops_every_reply() {
        assert!(!browse_reply_matches(None, "/home/dev"));
        assert!(!browse_reply_matches(None, ""));
    }

    #[test]
    fn test_browse_reply_matches_exact_path_accepts_only_that_path() {
        assert!(browse_reply_matches(Some("/home/dev"), "/home/dev"));
        assert!(!browse_reply_matches(Some("/home/dev"), "/home/other"));
    }

    #[test]
    fn test_browse_reply_matches_seed_request_accepts_any_resolved_path() {
        assert!(browse_reply_matches(Some(""), "/home/dev"));
        assert!(browse_reply_matches(Some("~"), "/home/dev"));
        assert!(browse_reply_matches(
            Some("~/projects"),
            "/home/dev/projects"
        ));
    }

    #[test]
    fn test_disambiguate_session_name_no_collision_returns_name_unchanged() {
        let existing = vec!["agent".to_string(), "tests".to_string()];
        assert_eq!(disambiguate_session_name("rift", &existing), "rift");
    }

    #[test]
    fn test_disambiguate_session_name_collision_appends_lowest_free_suffix() {
        let existing = vec!["rift".to_string()];
        assert_eq!(disambiguate_session_name("rift", &existing), "rift-2");

        let existing = vec!["rift".to_string(), "rift-2".to_string()];
        assert_eq!(disambiguate_session_name("rift", &existing), "rift-3");
    }

    #[test]
    fn test_disambiguate_session_name_empty_existing_list_returns_name_unchanged() {
        assert_eq!(disambiguate_session_name("rift", &[]), "rift");
    }

    #[test]
    fn test_start_path_prefers_the_first_recent_root_else_falls_back_to_empty() {
        assert_eq!(start_path(&["/a".to_string(), "/b".to_string()]), "/a");
        assert_eq!(start_path(&[]), "");
    }

    #[test]
    fn test_basename_takes_the_last_segment_falling_back_to_the_path_itself() {
        assert_eq!(basename("/home/dev/project"), "project");
        assert_eq!(basename("/home/dev/project/"), "project");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn test_join_child_appends_with_a_slash_avoiding_a_double_slash_at_root() {
        assert_eq!(join_child("/home/dev", "project"), "/home/dev/project");
        assert_eq!(join_child("/", "home"), "/home");
    }

    #[test]
    fn test_breadcrumb_segments_splits_absolute_root_and_empty_paths() {
        assert_eq!(
            breadcrumb_segments("/home/dev"),
            vec![
                ("/".to_string(), "/".to_string()),
                ("home".to_string(), "/home".to_string()),
                ("dev".to_string(), "/home/dev".to_string()),
            ]
        );
        assert_eq!(
            breadcrumb_segments("/"),
            vec![("/".to_string(), "/".to_string())]
        );
        assert!(breadcrumb_segments("").is_empty());
    }

    #[test]
    fn test_describe_dir_browse_error_maps_every_variant_to_distinct_text() {
        let messages = [
            describe_dir_browse_error(DirBrowseError::NotFound),
            describe_dir_browse_error(DirBrowseError::PermissionDenied),
            describe_dir_browse_error(DirBrowseError::NotADirectory),
            describe_dir_browse_error(DirBrowseError::Io),
        ];
        let unique: std::collections::HashSet<_> = messages.iter().collect();
        assert_eq!(
            unique.len(),
            messages.len(),
            "every reason reads distinctly"
        );
    }

    #[test]
    fn test_describe_clone_error_maps_every_variant_to_distinct_text() {
        let messages = [
            describe_clone_error(CloneError::InvalidUrl),
            describe_clone_error(CloneError::AuthFailed),
            describe_clone_error(CloneError::TargetExists),
            describe_clone_error(CloneError::Network),
            describe_clone_error(CloneError::GitUnavailable),
            describe_clone_error(CloneError::Other),
        ];
        let unique: std::collections::HashSet<_> = messages.iter().collect();
        assert_eq!(
            unique.len(),
            messages.len(),
            "every reason reads distinctly"
        );
    }

    #[test]
    fn test_invalid_clone_name_rejects_empty_dot_dotdot_and_separators() {
        for name in ["", ".", "..", "a/b", "a\\b"] {
            assert!(invalid_clone_name(name), "name {name:?} must be rejected");
        }
    }

    #[test]
    fn test_invalid_clone_name_accepts_a_plain_component() {
        assert!(!invalid_clone_name("repo"));
    }

    #[test]
    fn test_repo_basename_from_url_https_with_and_without_git_suffix() {
        assert_eq!(
            repo_basename_from_url("https://github.com/org/repo.git"),
            "repo"
        );
        assert_eq!(
            repo_basename_from_url("https://github.com/org/repo"),
            "repo"
        );
    }

    #[test]
    fn test_repo_basename_from_url_strips_a_trailing_slash() {
        assert_eq!(
            repo_basename_from_url("https://github.com/org/repo/"),
            "repo"
        );
        assert_eq!(
            repo_basename_from_url("https://github.com/org/repo.git/"),
            "repo"
        );
    }

    #[test]
    fn test_repo_basename_from_url_handles_scp_like_remotes() {
        assert_eq!(repo_basename_from_url("git@host:org/repo.git"), "repo");
        assert_eq!(repo_basename_from_url("git@host:repo.git"), "repo");
    }

    #[test]
    fn test_repo_basename_from_url_blank_url_returns_empty() {
        assert_eq!(repo_basename_from_url(""), "");
        assert_eq!(repo_basename_from_url("   "), "");
    }

    // --- entity behavior -------------------------------------------------------
    //
    // `browse`/`select_entry`/`go_to_parent`/`create` only need `Context`, so
    // they run through a plain `cx.update`; only `load` (which calls
    // `apply_dir_entries_reply`, seeding the `InputState` name field) needs a
    // live `Window`, mirroring `file_tree.rs`'s `open_tree` split.

    fn build_picker(
        cx: &mut TestAppContext,
    ) -> (Entity<RootPicker>, gpui::WindowHandle<gpui_component::Root>) {
        let mut picker = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let rp = cx.new(|cx| RootPicker::new(window, cx));
                picker = Some(rp.clone());
                cx.new(|cx| gpui_component::Root::new(rp, window, cx))
            })
            .expect("open window")
        });
        (picker.expect("picker constructed in window"), window)
    }

    fn subscribe_events(
        picker: &Entity<RootPicker>,
        cx: &mut TestAppContext,
    ) -> std::rc::Rc<std::cell::RefCell<Vec<String>>> {
        let events: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let sink = events.clone();
        cx.update(|cx| {
            cx.subscribe(picker, move |_picker, event: &RootPickerEvent, _cx| {
                sink.borrow_mut().push(match event {
                    RootPickerEvent::Browse(path) => format!("browse:{path}"),
                    RootPickerEvent::Clone { url, parent, name } => {
                        format!("clone:{url}:{parent}:{name}")
                    }
                    RootPickerEvent::Picked { root, name } => format!("picked:{root}:{name}"),
                });
            })
            .detach();
        });
        events
    }

    fn dir(name: &str) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            is_git_repo: false,
            git_branch: None,
        }
    }

    /// Browse to `browse_path` and immediately answer with a successful
    /// reply resolving to `resolved_path` — the call-site shorthand every
    /// test below uses to reach a loaded level without asserting on the
    /// intermediate `Browse` event.
    fn load(
        picker: &Entity<RootPicker>,
        window: &gpui::WindowHandle<gpui_component::Root>,
        cx: &mut TestAppContext,
        browse_path: &str,
        resolved_path: &str,
        parent: Option<&str>,
        entries: Vec<DirEntry>,
    ) {
        window
            .update(cx, |_, window, cx| {
                picker.update(cx, |picker, cx| {
                    picker.browse(browse_path.to_string(), cx);
                    picker.apply_dir_entries_reply(
                        resolved_path.to_string(),
                        parent.map(str::to_string),
                        entries,
                        None,
                        window,
                        cx,
                    );
                });
            })
            .expect("load level");
    }

    #[gpui::test]
    fn test_browse_sets_loading_and_suppresses_an_overlapping_request(cx: &mut TestAppContext) {
        let (picker, _window) = build_picker(cx);
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| {
            picker.update(cx, |picker, cx| {
                picker.browse("/home/dev".to_string(), cx);
                picker.browse("/etc".to_string(), cx); // in flight -> suppressed
            });
        });

        cx.update(|cx| assert!(picker.read(cx).loading));
        assert_eq!(
            events.borrow().as_slice(),
            ["browse:/home/dev"],
            "a browse already in flight suppresses the second request"
        );
    }

    #[gpui::test]
    fn test_load_level_updates_the_displayed_path_and_seeds_the_name_input(
        cx: &mut TestAppContext,
    ) {
        let (picker, window) = build_picker(cx);
        load(
            &picker,
            &window,
            cx,
            "",
            "/home/dev",
            Some("/home"),
            vec![dir("project"), dir("scratch")],
        );

        cx.update(|cx| {
            let picker = picker.read(cx);
            assert!(!picker.loading);
            assert!(picker.error.is_none());
            assert_eq!(picker.current_path, "/home/dev");
            assert_eq!(picker.parent.as_deref(), Some("/home"));
            assert_eq!(picker.entries.len(), 2);
            assert_eq!(picker.name_input.read(cx).value().as_ref(), "dev");
        });
    }

    #[gpui::test]
    fn test_error_reply_keeps_the_last_good_level_and_sets_the_error(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        load(
            &picker,
            &window,
            cx,
            "",
            "/home/dev",
            Some("/home"),
            vec![dir("project")],
        );

        window
            .update(cx, |_, window, cx| {
                picker.update(cx, |picker, cx| {
                    picker.browse("/home/dev/locked".to_string(), cx);
                    picker.apply_dir_entries_reply(
                        "/home/dev/locked".to_string(),
                        None,
                        Vec::new(),
                        Some(DirBrowseError::PermissionDenied),
                        window,
                        cx,
                    );
                });
            })
            .expect("apply failed reply");

        cx.update(|cx| {
            let picker = picker.read(cx);
            assert!(!picker.loading);
            assert_eq!(picker.error, Some(DirBrowseError::PermissionDenied));
            assert_eq!(
                picker.current_path, "/home/dev",
                "the last good level stays displayed"
            );
            assert_eq!(picker.entries.len(), 1);
        });
    }

    #[gpui::test]
    fn test_select_entry_browses_into_the_child_path(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        load(
            &picker,
            &window,
            cx,
            "",
            "/home/dev",
            Some("/home"),
            vec![dir("project")],
        );
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| picker.update(cx, |picker, cx| picker.select_entry("project", cx)));

        assert_eq!(events.borrow().as_slice(), ["browse:/home/dev/project"]);
    }

    #[gpui::test]
    fn test_go_to_parent_browses_up_and_is_noop_at_the_filesystem_root(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        load(
            &picker,
            &window,
            cx,
            "",
            "/home/dev",
            Some("/home"),
            Vec::new(),
        );
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| picker.update(cx, |picker, cx| picker.go_to_parent(cx)));
        assert_eq!(events.borrow().as_slice(), ["browse:/home"]);

        // At the filesystem root, `parent` is `None` — the affordance is a
        // no-op (`loading` is reset first, mirroring the real reply path).
        cx.update(|cx| {
            picker.update(cx, |picker, cx| {
                picker.loading = false;
                picker.parent = None;
                picker.go_to_parent(cx);
            });
        });
        assert_eq!(
            events.borrow().as_slice(),
            ["browse:/home"],
            "no parent at the filesystem root"
        );
    }

    #[gpui::test]
    fn test_create_emits_picked_with_the_current_path_and_trimmed_name(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        load(
            &picker,
            &window,
            cx,
            "",
            "/home/dev",
            Some("/home"),
            Vec::new(),
        );
        window
            .update(cx, |_, window, cx| {
                picker.update(cx, |picker, cx| {
                    let input = picker.name_input.clone();
                    input.update(cx, |input, cx| {
                        input.set_value("  my-session  ", window, cx)
                    });
                });
            })
            .expect("edit name");
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| picker.update(cx, |picker, cx| picker.create(cx)));

        assert_eq!(events.borrow().as_slice(), ["picked:/home/dev:my-session"]);
    }

    #[gpui::test]
    fn test_create_is_noop_before_a_level_resolves_or_with_a_blank_name(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| picker.update(cx, |picker, cx| picker.create(cx)));
        assert!(events.borrow().is_empty(), "no level resolved yet");

        load(
            &picker,
            &window,
            cx,
            "",
            "/home/dev",
            Some("/home"),
            Vec::new(),
        );
        window
            .update(cx, |_, window, cx| {
                picker.update(cx, |picker, cx| {
                    let input = picker.name_input.clone();
                    input.update(cx, |input, cx| input.set_value("   ", window, cx));
                });
            })
            .expect("blank the name field");

        // Fresh sink: `load` above emits its own `Browse` event, which is not
        // what this second assertion is about.
        let events = subscribe_events(&picker, cx);
        cx.update(|cx| picker.update(cx, |picker, cx| picker.create(cx)));
        assert!(events.borrow().is_empty(), "a blank name never creates");
    }

    #[gpui::test]
    fn test_load_level_also_seeds_the_clone_parent_input(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        load(
            &picker,
            &window,
            cx,
            "",
            "/home/dev",
            Some("/home"),
            Vec::new(),
        );

        cx.update(|cx| {
            let picker = picker.read(cx);
            assert_eq!(
                picker.clone_parent_input.read(cx).value().as_ref(),
                "/home/dev"
            );
        });
    }

    /// Set the clone mode's three fields directly (mirroring `load`'s
    /// shorthand role for browse tests).
    fn set_clone_fields(
        picker: &Entity<RootPicker>,
        window: &gpui::WindowHandle<gpui_component::Root>,
        cx: &mut TestAppContext,
        url: &str,
        parent: &str,
        name: &str,
    ) {
        window
            .update(cx, |_, window, cx| {
                picker.update(cx, |picker, cx| {
                    let url_input = picker.clone_url_input.clone();
                    url_input.update(cx, |input, cx| input.set_value(url, window, cx));
                    let parent_input = picker.clone_parent_input.clone();
                    parent_input.update(cx, |input, cx| input.set_value(parent, window, cx));
                    let name_input = picker.clone_name_input.clone();
                    name_input.update(cx, |input, cx| input.set_value(name, window, cx));
                });
            })
            .expect("set clone fields");
    }

    #[gpui::test]
    fn test_start_clone_emits_clone_with_the_trimmed_fields(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        set_clone_fields(
            &picker,
            &window,
            cx,
            "  https://github.com/org/repo.git  ",
            "  /workspace  ",
            "  repo  ",
        );
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| picker.update(cx, |picker, cx| picker.start_clone(cx)));

        assert_eq!(
            events.borrow().as_slice(),
            ["clone:https://github.com/org/repo.git:/workspace:repo"]
        );
        cx.update(|cx| assert!(picker.read(cx).cloning));
    }

    #[gpui::test]
    fn test_start_clone_is_noop_with_a_blank_field(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        set_clone_fields(
            &picker,
            &window,
            cx,
            "https://github.com/org/repo.git",
            "",
            "repo",
        );
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| picker.update(cx, |picker, cx| picker.start_clone(cx)));

        assert!(
            events.borrow().is_empty(),
            "a blank parent field never starts a clone"
        );
        cx.update(|cx| assert!(!picker.read(cx).cloning));
    }

    #[gpui::test]
    fn test_start_clone_rejects_a_name_with_a_path_separator(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        set_clone_fields(
            &picker,
            &window,
            cx,
            "https://github.com/org/repo.git",
            "/workspace",
            "a/b",
        );
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| picker.update(cx, |picker, cx| picker.start_clone(cx)));

        assert!(
            events.borrow().is_empty(),
            "an invalid name is caught before ever sending a request (issue #839)"
        );
        cx.update(|cx| {
            let picker = picker.read(cx);
            assert!(!picker.cloning);
            assert!(picker.clone_name_error.is_some());
        });
    }

    #[gpui::test]
    fn test_start_clone_suppresses_an_overlapping_request(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        set_clone_fields(
            &picker,
            &window,
            cx,
            "https://github.com/org/repo.git",
            "/workspace",
            "repo",
        );
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| {
            picker.update(cx, |picker, cx| {
                picker.start_clone(cx);
                picker.start_clone(cx); // already in flight -> suppressed
            });
        });

        assert_eq!(
            events.borrow().len(),
            1,
            "a clone already in flight suppresses the second request"
        );
    }

    #[gpui::test]
    fn test_apply_clone_result_success_emits_picked_with_the_checkout_and_name(
        cx: &mut TestAppContext,
    ) {
        let (picker, window) = build_picker(cx);
        set_clone_fields(
            &picker,
            &window,
            cx,
            "https://github.com/org/repo.git",
            "/workspace",
            "repo",
        );
        cx.update(|cx| picker.update(cx, |picker, cx| picker.start_clone(cx)));
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| {
            picker.update(cx, |picker, cx| {
                picker.apply_clone_result("/workspace/repo".to_string(), None, cx);
            });
        });

        assert_eq!(events.borrow().as_slice(), ["picked:/workspace/repo:repo"]);
        cx.update(|cx| assert!(!picker.read(cx).cloning));
    }

    #[gpui::test]
    fn test_apply_clone_result_error_sets_the_error_and_clears_cloning(cx: &mut TestAppContext) {
        let (picker, window) = build_picker(cx);
        set_clone_fields(
            &picker,
            &window,
            cx,
            "https://github.com/org/repo.git",
            "/workspace",
            "repo",
        );
        cx.update(|cx| picker.update(cx, |picker, cx| picker.start_clone(cx)));
        let events = subscribe_events(&picker, cx);

        cx.update(|cx| {
            picker.update(cx, |picker, cx| {
                picker.apply_clone_result(
                    "/workspace/repo".to_string(),
                    Some(CloneError::TargetExists),
                    cx,
                );
            });
        });

        assert!(events.borrow().is_empty(), "a failed clone never picks");
        cx.update(|cx| {
            let picker = picker.read(cx);
            assert!(!picker.cloning);
            assert_eq!(picker.clone_error, Some(CloneError::TargetExists));
        });
    }

    #[gpui::test]
    fn test_set_mode_switches_between_browse_and_clone(cx: &mut TestAppContext) {
        let (picker, _window) = build_picker(cx);
        cx.update(|cx| assert_eq!(picker.read(cx).mode, PickerMode::Browse));

        cx.update(|cx| picker.update(cx, |picker, cx| picker.set_mode(PickerMode::Clone, cx)));
        cx.update(|cx| assert_eq!(picker.read(cx).mode, PickerMode::Clone));

        cx.update(|cx| picker.update(cx, |picker, cx| picker.set_mode(PickerMode::Browse, cx)));
        cx.update(|cx| assert_eq!(picker.read(cx).mode, PickerMode::Browse));
    }
}
