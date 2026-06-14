//! Code editor surface: open a file from the tree into a `gpui-component` code
//! editor and render it with Tree-sitter syntax highlighting (`docs/spec-editor.md`,
//! the editor surface; #187).
//!
//! This is the **client side of the buffer channel** (the protocol's first
//! request/response pair). The [`crate::workspace::WorkspaceView`] subscribes to
//! the file tree's [`crate::file_tree::FileTreeEvent::OpenFile`], issues an
//! `OpenFile` request over the daemon transport, and routes the
//! `FileContent { path, content, mtime }` reply back here via [`EditorView::load`].
//! The editor renders `content` byte-for-byte in a `gpui-component` `InputState`
//! in code-editor mode, deriving the highlighting language from the file
//! extension. The component's editor virtualizes (it paints only visible lines),
//! so a large file opens and scrolls without loading every line eagerly.
//!
//! **View only at this step** (`docs/spec-editor.md` step #187): the buffer is
//! displayed, never written back — write-back is #188. The base `mtime` is kept
//! with the open buffer so #188 can hand it back as `SaveFile`'s `base_mtime`,
//! but nothing here saves.
//!
//! **Timeout, not a hang.** A daemon refusal (binary / non-UTF-8 content, a path
//! that escapes the root) produces *no reply* — the daemon stays silent and logs
//! to its stderr (`crates/daemon/src/lib.rs::buffer_reply`). So the open carries
//! its own bounded timeout: if no `FileContent` arrives within [`OPEN_TIMEOUT`]
//! the editor falls back to an unobtrusive "could not open" state rather than
//! waiting forever. A reply is matched to the live open **by path** ([`EditorView::load`]
//! only accepts a `FileContent` whose path is the one currently `Loading`), so a
//! late or superseded reply is ignored — only the most recent open is live. The
//! timeout is fenced separately by a **monotonic request generation**, so a fired
//! timer for an open that has since been replaced never trips the current one.
//!
//! Opening a file touches no tmux pane or window state — the editor is a GUI
//! surface, not a pane — and nothing here inspects pane processes, agents, or
//! editor processes (agent-agnostic by construction; it only ever handles a file
//! path, its bytes, and its `mtime`).

use std::path::Path;
use std::time::{Duration, SystemTime};

use gpui::{
    div, px, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window,
};
use gpui_component::input::{Input, InputState};
use gpui_component::ActiveTheme as _;

/// How long the editor waits for a `FileContent` reply before giving up on an
/// open. The daemon answers a local read in well under this; the budget exists
/// only so a *refused* request (which gets no reply, by protocol) or a lost one
/// cannot wedge the editor. Generous enough not to trip on a slow link, short
/// enough to recover promptly.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Default tab width for the code editor, matching the gallery demo.
const TAB_SIZE: usize = 4;

/// What the editor is currently showing.
enum EditorState {
    /// No file opened yet — the initial empty surface.
    Empty,
    /// An open request is in flight, awaiting its `FileContent` reply (or the
    /// timeout). Carries the path being opened so a stale reply can be told from
    /// the live one.
    Loading { path: String },
    /// A file's content is rendered in the code editor.
    Loaded { path: String },
    /// The last open did not complete — the daemon refused it (binary / path
    /// escape, which gets no reply) or the reply was lost and the timeout fired.
    /// An unobtrusive recovery state, never a hang.
    Failed { path: String },
}

/// The code editor view: a `gpui-component` `InputState` in code-editor mode plus
/// the open buffer's bookkeeping.
pub struct EditorView {
    /// The code-editor input state. Created once; its content is replaced via
    /// [`InputState::set_value`] each time a file loads, and its highlighting
    /// language is reset per open to match the new file's extension.
    input: Entity<InputState>,
    state: EditorState,
    /// The base `mtime` the daemon reported for the open buffer — kept so #188's
    /// write-back can pass it back as `SaveFile`'s `base_mtime` for the conflict
    /// check. Unused at this view-only step beyond being retained.
    base_mtime: Option<SystemTime>,
    /// Monotonic open-request id. Incremented on every [`EditorView::begin_open`];
    /// the timeout and the reply both carry the generation they were issued for,
    /// so a late reply or a fired timer for a superseded open is ignored.
    generation: u64,
}

impl EditorView {
    /// Create an empty editor. The `InputState` starts in code-editor mode with a
    /// neutral language; [`EditorView::begin_open`] re-derives the language from
    /// each opened file's extension before its content loads.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("text")
                .line_number(true)
                .tab_size(gpui_component::input::TabSize {
                    tab_size: TAB_SIZE,
                    ..Default::default()
                })
        });
        Self {
            input,
            state: EditorState::Empty,
            base_mtime: None,
            generation: 0,
        }
    }

    /// The path of the file currently open or loading, if any — a headless handle
    /// for tests and for the workspace to correlate a reply.
    pub fn open_path(&self) -> Option<&str> {
        match &self.state {
            EditorState::Loading { path }
            | EditorState::Loaded { path }
            | EditorState::Failed { path } => Some(path.as_str()),
            EditorState::Empty => None,
        }
    }

    /// The base `mtime` of the open buffer — what #188's write-back hands back as
    /// `SaveFile`'s `base_mtime`. `None` until a file has loaded.
    pub fn base_mtime(&self) -> Option<SystemTime> {
        self.base_mtime
    }

    /// Begin opening `path`: switch the editor to its loading state, point the
    /// code editor at the file's language (derived from the extension), and arm
    /// the open timeout. The caller sends the matching `OpenFile` request; the
    /// reply is accepted by [`EditorView::load`] only when its path matches the
    /// open in flight.
    ///
    /// The content is *not* set here — it arrives on the `FileContent` reply. The
    /// editor is cleared to empty content meanwhile so a previous file's text is
    /// never shown under the new path.
    pub fn begin_open(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;

        let language = language_for_path(&path);
        let path_for_timer = path.clone();
        self.state = EditorState::Loading { path };
        self.base_mtime = None;

        // Recreate the input in the new file's language, which also clears the
        // previous file's text and highlighter — so no stale content or coloring
        // lingers under the new path while its content loads. The freshly built
        // state starts empty; the `FileContent` reply fills it in `load`.
        self.input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(true)
                .tab_size(gpui_component::input::TabSize {
                    tab_size: TAB_SIZE,
                    ..Default::default()
                })
        });

        // Arm the timeout on the GPUI executor (`smol::Timer`; tokio's timer does
        // not fire here — docs/patterns.md). If, when it fires, this is still the
        // live request and still loading, the reply never came (refused / lost):
        // fall back to the failed state rather than wait forever.
        cx.spawn(async move |this, cx| {
            smol::Timer::after(OPEN_TIMEOUT).await;
            let _ = cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        if let EditorState::Loading { .. } = this.state {
                            this.state = EditorState::Failed {
                                path: path_for_timer.clone(),
                            };
                            cx.notify();
                        }
                    }
                });
            });
        })
        .detach();

        cx.notify();
    }

    /// Render a `FileContent` reply: if it matches the file currently loading,
    /// load its `content` into the code editor and keep its `mtime` as the
    /// buffer's base. A reply whose path does not match the live open (a
    /// superseded or stray reply) is ignored — only the most recent open is live.
    pub fn load(
        &mut self,
        path: String,
        content: String,
        mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Accept only the reply for the open currently in flight; a reply for a
        // path we are no longer loading is stale (the user opened another file).
        let matches = matches!(&self.state, EditorState::Loading { path: p } if *p == path);
        if !matches {
            return;
        }

        self.base_mtime = Some(mtime);
        self.input.update(cx, |input, cx| {
            input.set_value(content, window, cx);
        });
        self.state = EditorState::Loaded { path };
        cx.notify();
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status: Option<String> = match &self.state {
            EditorState::Empty => Some("Select a file to open".to_owned()),
            EditorState::Loading { path } => Some(format!("Opening {path}\u{2026}")),
            EditorState::Failed { path } => Some(format!("Could not open {path}")),
            EditorState::Loaded { .. } => None,
        };

        if let Some(message) = status {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message)
                .into_any_element();
        }

        // The code editor fills its slot. Mono font and size mirror the gallery
        // demo so the editing surface matches the terminal's typography.
        div()
            .size_full()
            .child(
                Input::new(&self.input)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .size_full(),
            )
            .into_any_element()
    }
}

/// Derive the highlighting language token for a path from its extension.
///
/// `gpui-component`'s `code_editor` accepts an extension or a language name and
/// resolves the Tree-sitter grammar itself, falling back to plain text for an
/// unknown one (`Language::from_str`). So the extension is passed straight
/// through; a path with no extension uses `"text"` (plain). The leaf is matched
/// case-insensitively for the extension only — the full path never reaches the
/// editor.
fn language_for_path(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_else(|| "text".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_for_path_uses_extension() {
        assert_eq!(language_for_path("src/main.rs"), "rs");
        assert_eq!(language_for_path("Cargo.toml"), "toml");
        assert_eq!(language_for_path("docs/readme.MD"), "md");
        assert_eq!(language_for_path("a/b/script.py"), "py");
    }

    #[test]
    fn test_language_for_path_without_extension_is_plain_text() {
        assert_eq!(language_for_path("Makefile"), "text");
        assert_eq!(language_for_path("src/noext"), "text");
        assert_eq!(language_for_path(".gitignore"), "text");
    }

    #[test]
    fn test_language_for_path_lowercases_extension() {
        assert_eq!(language_for_path("MAIN.RS"), "rs");
        assert_eq!(language_for_path("Config.TOML"), "toml");
    }
}
