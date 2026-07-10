//! The command palette modal (`docs/spec-command-palette.md`, issues #359,
//! #360): a `gpui-component` [`List`] — which composes its own public search
//! `input` when made searchable — hosted in the `Root` dialog overlay,
//! rendering the [`command_registry`] entries. Opened by
//! [`OpenCommandPalette`] (bound to `Ctrl+Shift+P` / `Cmd+Shift+P` in
//! `main.rs`, unscoped so it reaches the palette regardless of which surface
//! currently has focus).
//!
//! Deliberately not `gpui_component::searchable_list` (`docs/spec-command-
//! palette.md`): its `SearchableListState` is `pub(crate)`, reachable only
//! through the `Select`/`ComboBox` dropdown chrome.
//!
//! Typing filters the registry by [`filter_commands`]'s subsequence match; up
//! and down navigate the filtered results via the `List`'s own built-in
//! `SelectUp`/`SelectDown` handling; Enter closes the palette — restoring the
//! pre-palette focus — and then dispatches the selected command's action
//! (`CommandPaletteDelegate::confirm`).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    px, App, AppContext as _, Context, Entity, ParentElement as _, Styled as _, Task, Window,
};
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme as _, IndexPath, WindowExt as _};

use crate::command_registry::COMMANDS;

/// The indices into [`COMMANDS`] whose name subsequence-matches `query`, in
/// registry order. An empty (or whitespace-only) query matches every command
/// (`docs/spec-command-palette.md`: "an empty query lists all commands").
fn filter_commands(query: &str) -> Vec<usize> {
    let query = query.trim();
    COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, command)| query.is_empty() || is_subsequence(query, command.name))
        .map(|(index, _)| index)
        .collect()
}

/// Case-insensitive subsequence match: every character of `query` appears in
/// `candidate`, in order, not necessarily contiguously (the palette's "small
/// subsequence match", `docs/spec-command-palette.md` — no fuzzy-matching
/// dependency).
fn is_subsequence(query: &str, candidate: &str) -> bool {
    let mut candidate_chars = candidate.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| candidate_chars.any(|c| c == q))
}

/// Open the command palette. Bound with no key-context scope in `main.rs`
/// (mirroring the terminal's global `SelectWindow` binding), so the shortcut
/// opens the palette from anywhere, including while the terminal is focused.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct OpenCommandPalette;

/// The palette's `ListDelegate`: renders the [`COMMANDS`] entries currently
/// matching the query, as a plain `gpui_component::list::ListItem` (its name
/// plus its keybinding hint, where bound).
struct CommandPaletteDelegate {
    /// Indices into [`COMMANDS`] for the rows currently shown, in registry
    /// order restricted to [`filter_commands`]'s matches. Rebuilt on every
    /// query change by `perform_search`; starts as every command so the
    /// palette's first render lists everything.
    matches: Vec<usize>,
    selected_index: Option<IndexPath>,
}

impl ListDelegate for CommandPaletteDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.matches.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.matches = filter_commands(query);
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let index = *self.matches.get(ix.row)?;
        let command = COMMANDS.get(index)?;
        let selected = Some(ix) == self.selected_index;
        let hint = command.keybinding_hint;
        Some(
            ListItem::new(ix)
                .selected(selected)
                .child(Label::new(command.name))
                .when_some(hint, |item, hint| {
                    item.suffix(move |_, cx| {
                        Label::new(hint).text_color(cx.theme().muted_foreground)
                    })
                }),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }

    /// Close the palette, then dispatch the selected command's action (the
    /// same action its keybinding would dispatch,
    /// `docs/spec-command-palette.md`).
    ///
    /// Order matters (#434): `close_dialog` synchronously restores focus to
    /// the element focused before the palette opened, and `dispatch_action`
    /// captures the focused element at call time (only the dispatch itself is
    /// deferred). Dispatching before closing targets the palette's own search
    /// input, whose dispatch path climbs the dialog overlay — a sibling of
    /// the dock area — so editor-scoped actions (Save, the LSP-nav entries)
    /// never reach the editor's `on_action` handlers.
    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        let command = self
            .selected_index
            .and_then(|ix| self.matches.get(ix.row))
            .and_then(|&index| COMMANDS.get(index));

        window.close_dialog(cx);

        if let Some(command) = command {
            window.dispatch_action(command.action(), cx);
        }
    }
}

/// The width of the palette dialog.
const PALETTE_WIDTH: f32 = 480.0;

/// The max height of the palette's command list before it scrolls.
const PALETTE_LIST_MAX_HEIGHT: f32 = 360.0;

/// Owns the palette's [`ListState`] entity for the workspace's lifetime, so
/// reopening the palette reuses it rather than rebuilding the registry list
/// from scratch each time.
pub struct CommandPalette {
    list: Entity<ListState<CommandPaletteDelegate>>,
}

impl CommandPalette {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let delegate = CommandPaletteDelegate {
            matches: (0..COMMANDS.len()).collect(),
            selected_index: None,
        };
        let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
        Self { list }
    }

    /// Open the palette as a `Root` dialog. Resets the state left over from
    /// the previous time it was opened, so it always starts with an empty
    /// query listing every command (`docs/spec-command-palette.md`: "an empty
    /// query lists all commands") and the first row selected, so Enter on a
    /// freshly opened palette runs the top match (#434).
    pub fn open(&self, window: &mut Window, cx: &mut App) {
        self.list.update(cx, |list, cx| {
            // `set_query` writes the input silently (`InputState::set_value`
            // suppresses `InputEvent::Change`), so `perform_search` never
            // fires — rebuild the delegate's matches explicitly, otherwise
            // the previous query's match list survives under the now-empty
            // input (#434).
            //
            // Residual edge case, not fixable app-side: `ListState`'s query
            // dedupe field (`last_query`, private) is only updated by its
            // async search task, never by `set_query`, so it survives this
            // reset. If the first input change after reopening exactly equals
            // the previous session's final query, `ListState` skips
            // `perform_search` and the full unfiltered list stays displayed
            // under that query until the next keystroke self-heals it.
            list.set_query("", window, cx);
            list.delegate_mut().matches = filter_commands("");
            list.set_selected_index(Some(IndexPath::default()), window, cx);
            list.scroll_to_selected_item(window, cx);
        });

        let list = self.list.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("Command Palette")
                .close_button(false)
                .w(px(PALETTE_WIDTH))
                .child(
                    List::new(&list)
                        .search_placeholder("Type a command...")
                        .max_h(px(PALETTE_LIST_MAX_HEIGHT)),
                )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::TestAppContext;
    use gpui_component::Root;

    fn matched_names(query: &str) -> Vec<&'static str> {
        filter_commands(query)
            .into_iter()
            .map(|index| COMMANDS[index].name)
            .collect()
    }

    #[test]
    fn test_filter_commands_empty_query_returns_all_in_registry_order() {
        let all_names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        assert_eq!(matched_names(""), all_names);
    }

    #[test]
    fn test_filter_commands_whitespace_only_query_returns_all() {
        assert_eq!(matched_names("   "), matched_names(""));
    }

    #[test]
    fn test_filter_commands_matches_subsequence_case_insensitively() {
        assert_eq!(matched_names("SAVE"), vec!["Save"]);
    }

    #[test]
    fn test_filter_commands_matches_multiple_entries_in_registry_order() {
        assert_eq!(
            matched_names("toggle"),
            vec![
                "Toggle Explorer",
                "Toggle Outline",
                "Toggle Problems",
                "Toggle Source Control",
                "Toggle Terminal",
                "Toggle Light/Dark Theme"
            ]
        );
    }

    #[test]
    fn test_filter_commands_no_match_returns_empty() {
        assert!(matched_names("zzz-no-such-command").is_empty());
    }

    #[test]
    fn test_is_subsequence_matches_non_contiguous_characters() {
        assert!(is_subsequence("gtd", "Go to Definition"));
    }

    #[test]
    fn test_is_subsequence_rejects_out_of_order_characters() {
        assert!(!is_subsequence("dtg", "Go to Definition"));
    }

    /// Bare render stub hosting the palette's dialog in a `Root`-wrapped test
    /// window — the palette needs only the `Root` overlay state, not the full
    /// workspace.
    struct StubView;

    impl gpui::Render for StubView {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    fn palette_window(cx: &mut TestAppContext) -> (CommandPalette, gpui::WindowHandle<Root>) {
        let mut palette: Option<CommandPalette> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                palette = Some(CommandPalette::new(window, cx));
                let stub = cx.new(|_| StubView);
                cx.new(|cx| Root::new(stub, window, cx))
            })
            .unwrap()
        });
        (
            palette.expect("palette constructed inside the window callback"),
            window,
        )
    }

    /// Reopening the palette after a query was left behind must reset the
    /// match list and select the first row (#434): `ListState::set_query`
    /// writes the input silently (no `InputEvent::Change`), so without the
    /// explicit reset the previous query's matches survive under the
    /// now-empty input, and Enter on a fresh palette does nothing.
    #[gpui::test]
    fn test_open_with_stale_query_state_resets_matches_and_selects_first_row(
        cx: &mut TestAppContext,
    ) {
        let (palette, window) = palette_window(cx);

        // `update_window` instead of `window.update`: the latter leases the
        // `Root` entity, which `open`'s dialog wiring re-enters (same
        // pattern as the workspace's palette test).
        cx.update_window(window.into(), |_, window, cx| {
            // Simulate the previous session: a narrowed match list and a
            // cleared selection left behind by typing and dismissing.
            palette.list.update(cx, |list, cx| {
                list.set_query("save", window, cx);
                list.delegate_mut().matches = filter_commands("save");
                list.set_selected_index(None, window, cx);
            });

            palette.open(window, cx);

            let list = palette.list.read(cx);
            assert_eq!(
                list.delegate().matches,
                (0..COMMANDS.len()).collect::<Vec<_>>(),
                "reopening lists every command again"
            );
            assert_eq!(
                list.selected_index(),
                Some(IndexPath::default()),
                "reopening selects the first row so Enter runs the top match"
            );
        })
        .unwrap();
    }

    /// `confirm` must close the dialog — synchronously restoring the focus
    /// captured when the palette opened — before dispatching the command's
    /// action (#434): `dispatch_action` targets the element focused at call
    /// time, so closing afterwards would aim editor-scoped actions at the
    /// palette's own search input instead of the editor.
    #[gpui::test]
    fn test_confirm_closes_the_palette_and_restores_the_previous_focus(cx: &mut TestAppContext) {
        let (palette, window) = palette_window(cx);

        cx.update_window(window.into(), |_, window, cx| {
            let previous_focus = cx.focus_handle();
            window.focus(&previous_focus, cx);

            palette.open(window, cx);
            assert!(window.has_active_dialog(cx), "open sets an active dialog");
            assert_ne!(
                window.focused(cx),
                Some(previous_focus.clone()),
                "the palette dialog takes focus while open"
            );

            // `open` pre-selected the first row, so this confirms the top
            // match, exactly as Enter on a freshly opened palette does.
            palette.list.update(cx, |list, cx| {
                list.delegate_mut().confirm(false, window, cx);
            });

            assert!(
                !window.has_active_dialog(cx),
                "confirm closes the palette dialog"
            );
            assert_eq!(
                window.focused(cx),
                Some(previous_focus),
                "confirm restores the pre-palette focus before the action dispatch resolves"
            );
        })
        .unwrap();
    }
}
