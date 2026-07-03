//! The command palette modal (`docs/spec-command-palette.md`, issue #359): a
//! `gpui-component` [`List`] — which composes its own public search `input`
//! when made searchable — hosted in the `Root` dialog overlay, rendering the
//! [`command_registry`] entries. Opened by [`OpenCommandPalette`] (bound to
//! `Ctrl+Shift+P` / `Cmd+Shift+P` in `main.rs`, unscoped so it reaches the
//! palette regardless of which surface currently has focus).
//!
//! Deliberately not `gpui_component::searchable_list` (`docs/spec-command-
//! palette.md`): its `SearchableListState` is `pub(crate)`, reachable only
//! through the `Select`/`ComboBox` dropdown chrome.
//!
//! This issue wires the modal's open/close and renders the full, unfiltered
//! registry. Typing a query narrows nothing yet (the `List`'s default
//! `ListDelegate::perform_search` is a no-op) and selecting a row dispatches
//! nothing (the default `ListDelegate::confirm` is a no-op) — the subsequence
//! filter, arrow/Enter navigation semantics, and action dispatch are #360.

use gpui::prelude::FluentBuilder as _;
use gpui::{px, App, AppContext as _, Context, Entity, ParentElement as _, Styled as _, Window};
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme as _, IndexPath, WindowExt as _};

use crate::command_registry::COMMANDS;

/// Open the command palette. Bound with no key-context scope in `main.rs`
/// (mirroring the terminal's global `SelectWindow` binding), so the shortcut
/// opens the palette from anywhere, including while the terminal is focused.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct OpenCommandPalette;

/// The palette's `ListDelegate`: renders every [`COMMANDS`] entry, in
/// registry order, as a plain `gpui_component::list::ListItem` (its name plus
/// its keybinding hint, where bound). No filtering, no dispatch — #360.
struct CommandPaletteDelegate {
    selected_index: Option<IndexPath>,
}

impl ListDelegate for CommandPaletteDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        COMMANDS.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let command = COMMANDS.get(ix.row)?;
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
