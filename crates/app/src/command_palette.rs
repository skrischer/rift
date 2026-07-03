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
//! `SelectUp`/`SelectDown` handling; Enter dispatches the selected command's
//! action and closes the palette (`CommandPaletteDelegate::confirm`).

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

    /// Dispatch the selected command's action into the focused context (the
    /// same action its keybinding would dispatch) and close the palette
    /// (`docs/spec-command-palette.md`: "selecting a command dispatches its
    /// action ... then closes the palette").
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

        if let Some(command) = command {
            window.dispatch_action(command.action(), cx);
        }

        window.close_dialog(cx);
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

    /// Open the palette as a `Root` dialog. Resets any query left over from
    /// the previous time it was opened, so it always starts listing every
    /// command (`docs/spec-command-palette.md`: "an empty query lists all
    /// commands").
    pub fn open(&self, window: &mut Window, cx: &mut App) {
        self.list.update(cx, |list, cx| {
            list.set_query("", window, cx);
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
                "Toggle Problems",
                "Toggle Source Control"
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
}
