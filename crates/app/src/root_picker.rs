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
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, IconName};

use rift_protocol::{DirBrowseError, DirEntry};

/// Card width, matching `session_picker`/`connection_screen`'s own
/// `CARD_WIDTH` (design contract: "card ~470px") — kept as its own constant
/// rather than exported from either sibling module, mirroring their own
/// precedent for small duplicated visual primitives.
const CARD_WIDTH: f32 = 470.0;

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
fn join_child(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
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

/// Emitted by [`RootPicker`]; the owner never touches this view's internals
/// directly, mirroring [`crate::session_picker::SessionPickerEvent`].
pub enum RootPickerEvent {
    /// Ask the owner to send `ClientMessage::QueryDirEntries { path }` — the
    /// owner drives the actual protocol round-trip and later feeds the
    /// reply back through [`RootPicker::apply_dir_entries_reply`]. Emitted
    /// by every [`RootPicker::browse`] call: the caller's initial seed, a
    /// row click, a breadcrumb segment, or the parent affordance.
    Browse(String),
    /// The user confirmed Create: the picked root (the current resolved
    /// level) and the session name (the name field's value, trimmed).
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
        Self {
            current_path: String::new(),
            parent: None,
            entries: Vec::new(),
            loading: false,
            error: None,
            name_input,
            _name_subscription: subscription,
            focus_handle: cx.focus_handle(),
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
    /// level and reseeds the name field with the new path's basename; on
    /// failure, keeps the last good level and renders `error` inline
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

/// The inline browse-error banner (`docs/spec-session-root-picker.md`'s
/// "renders inline without closing the picker"), mirroring
/// `connection_screen::render_error_banner`'s shape.
fn render_error_banner(cx: &mut Context<RootPicker>, error: DirBrowseError) -> impl IntoElement {
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
                .child(describe_dir_browse_error(error)),
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

impl Render for RootPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let breadcrumb = breadcrumb_segments(&self.current_path);
        let has_parent = self.parent.is_some();
        let loading = self.loading;
        let error = self.error;
        let can_create =
            !self.current_path.is_empty() && !self.name_input.read(cx).value().trim().is_empty();

        let breadcrumb_el = render_breadcrumb(cx, &breadcrumb, loading);

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if has_parent {
            rows.push(render_parent_row(cx).into_any_element());
        }
        rows.extend(
            self.entries
                .iter()
                .enumerate()
                .map(|(index, entry)| render_entry_row(cx, index, entry).into_any_element()),
        );

        let body = if rows.is_empty() {
            render_empty_state(cx, loading).into_any_element()
        } else {
            v_flex().gap(px(2.0)).children(rows).into_any_element()
        };

        let error_banner = error.map(|error| render_error_banner(cx, error));
        let name_input = self.name_input.clone();
        let footer = render_footer(cx, &name_input, can_create);

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
            .child(breadcrumb_el)
            .children(error_banner)
            .child(body)
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    // --- pure helpers --------------------------------------------------------

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
}
