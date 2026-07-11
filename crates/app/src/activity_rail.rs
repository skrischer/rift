//! The 48px activity rail (#513, `docs/spec-cockpit-chrome.md`): a fixed-width
//! flex column left of the dock, toggling area visibility and opening
//! settings.
//!
//! Rewired by `docs/spec-workspace-visibility-rail.md` (issue #819) from a
//! per-dock open/close toggle to the rift-owned area-visibility set: each
//! icon's active state now reads [`RailState`]'s `*_visible` fields — sourced
//! by the caller from `WorkspaceView`'s own visibility set, not
//! `dock.is_open`. The four area icons' `on_click` handlers
//! (`on_toggle_explorer_editor`/`on_toggle_terminal`/`on_toggle_source_control`/
//! `on_toggle_problems`) are built by the caller and passed into [`render`]
//! (`docs/spec-visibility-rail-focus.md`, issue #848): `WorkspaceView` binds
//! them to itself via `cx.listener` (a weak reference into
//! `Entity<WorkspaceView>`, no retain cycle) so a click invokes
//! `WorkspaceView::toggle_area` directly, immune to where keyboard focus
//! currently sits — replacing the earlier focus-coupled
//! `window.dispatch_action` of the `Toggle*` `Action`s, which stayed dropped
//! once a hide/solo transition unrendered the focused panel. Those `Action`s
//! and their root `on_action` handlers remain for the keyboard, command
//! palette, and any agent-driven dispatch; only the rail's own mouse-click
//! path is decoupled. Settings keeps dispatching `OpenSettings` — it is not
//! an area toggle. Presentational only, mirroring [`crate::title_bar`]: the
//! badge data is read off the existing client models
//! (`WorktreeModel::git_statuses` for the changed-file count,
//! `WorktreeModel::all_diagnostics` for [`worst_severity`]) — no new state
//! lives here.
//!
//! Full visual fidelity with the Paper "Workspace visibility rail" artboard
//! (issue #856) needs [`crate::workspace::Area`] itself, to pick each icon's
//! design hue and to compare against the solo target — the one exception to
//! this module otherwise never naming `WorkspaceView` types, mirroring how
//! `file_tree.rs` already imports `workspace::{solo_button, SoloExplorerEditor}`
//! for the same reason. [`RailState::solo`] carries the rift-owned solo
//! target (`Option<Area>`) alongside the existing `*_visible` flags: an
//! area's icon renders in its own hue while visible, the shared solo hue
//! while it is the solo target, or `muted_foreground` while hidden — with a
//! matching 2px accent bar on the icon's left edge whenever it renders with a
//! hue (the artboard's "tint + 2px bar" active state; never drawn while
//! muted). Settings is not an area and stays unhued, unchanged.

use std::collections::BTreeMap;

use gpui::{
    div, px, AnyElement, App, ClickEvent, Hsla, IntoElement, ParentElement as _, Pixels,
    Styled as _, Window,
};
use gpui_component::badge::Badge;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{v_flex, ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _};
use rift_protocol::{Diagnostic, DiagnosticSeverity};

use crate::settings::OpenSettings;
use crate::workspace::Area;

/// Fixed width of the activity rail, per the design contract.
pub const WIDTH: Pixels = px(48.0);

/// Side length of each icon button in the rail, per the design contract.
const BUTTON_SIZE: Pixels = px(36.0);

/// Live badge/active-state data the rail renders, read by the caller from the
/// rift-owned visibility set (`docs/spec-workspace-visibility-rail.md`) and
/// worktree models — never derived or cached here.
pub struct RailState {
    /// Whether the Explorer+Editor area is visible (`Area::ExplorerEditor`,
    /// one rail icon for both the left-dock explorer and the center editor).
    pub explorer_editor_visible: bool,
    /// Whether the Terminal area is visible (`Area::Terminal`, issue #821):
    /// a fully symmetric peer like the other three — hiding it or soloing a
    /// different area removes it from the center `h_split` entirely, never
    /// re-arranging or demoting it while it does show.
    pub terminal_visible: bool,
    /// Whether the Git area is visible (`Area::Git`: the right dock's source
    /// control + diff view).
    pub git_visible: bool,
    /// Whether the Diagnostics area is visible (`Area::Diagnostics`: the
    /// bottom dock's problems panel).
    pub diagnostics_visible: bool,
    /// The current solo target, if any (issue #856): fed by the caller from
    /// the rift-owned visibility set's own solo field. `None` outside solo —
    /// every visible area then renders in its own design hue. `Some(area)`
    /// renders `area`'s icon in the shared solo hue instead of its own;
    /// since solo already forces every other area's `*_visible` flag to
    /// `false` (`Visibility::is_visible`), this only ever changes *which*
    /// hue the one still-visible icon uses, never which icons render as
    /// visible.
    pub solo: Option<Area>,
    /// Changed-file count from `WorktreeModel::git_statuses` — the
    /// source-control badge (hidden by `Badge` itself when zero).
    pub changed_count: usize,
    /// Worst severity across `WorktreeModel::all_diagnostics`, or `None` when
    /// clean — the diagnostics dot (omitted entirely when clean).
    pub worst_diagnostic: Option<DiagnosticSeverity>,
}

/// Local severity ordinal: `DiagnosticSeverity` derives no `Ord` (a protocol
/// change), so this mirrors `problems_panel::severity_ordinal` /
/// `status_bar`'s local match rather than adding one to the shared type.
fn severity_ordinal(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 3,
    }
}

/// The worst severity across every file and server in the model's
/// diagnostics map, or `None` when there are no diagnostics at all — the
/// rail's "clean" state, where no dot renders.
pub fn worst_severity(
    diagnostics: &BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>>,
) -> Option<DiagnosticSeverity> {
    diagnostics
        .values()
        .flat_map(BTreeMap::values)
        .flatten()
        .map(|item| item.severity)
        .min_by_key(|severity| severity_ordinal(*severity))
}

/// The diagnostics dot's color for a given worst severity, matching
/// `problems_panel`'s per-severity palette (theme tokens only).
fn severity_color(severity: DiagnosticSeverity, cx: &App) -> Hsla {
    match severity {
        DiagnosticSeverity::Error => cx.theme().danger,
        DiagnosticSeverity::Warning => cx.theme().warning,
        DiagnosticSeverity::Information => cx.theme().info,
        DiagnosticSeverity::Hint => cx.theme().muted_foreground,
    }
}

/// Build one rail icon button: `active` maps to the design's "surface bg + fg
/// icon" selected state; `on_click` is whatever the caller hands in — an
/// entity-bound listener for the area toggles, a bubbling `Action` dispatch
/// for Settings — this helper stays presentational either way. Used directly
/// only by Settings, which is not an area and carries no design hue; the four
/// area icons go through [`area_button`] instead.
fn rail_button(
    id: &'static str,
    icon: IconName,
    tooltip: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
        .ghost()
        .with_size(BUTTON_SIZE)
        .icon(icon)
        .selected(active)
        .tooltip(tooltip)
        .on_click(on_click)
}

/// `area`'s design hue while it renders plainly visible (not soloed) — the
/// artboard's per-area color system (`docs/spec-workspace-visibility-rail.md`,
/// issue #856): Explorer+Editor blue, Terminal amber, Diagnostics red, Git
/// green. Explorer+Editor/Diagnostics/Git resolve to the base `blue`/`red`/
/// `green` theme tokens, exact matches for the artboard's `#89B4FA`/
/// `#F38BA8`/`#A6E3A1` under the shipped Catppuccin Mocha theme (`cx.theme()`
/// is live, so a theme switch re-tints automatically — no hardcoded hex).
/// Terminal has no dedicated "peach"/amber base token, so it resolves to
/// `warning` instead: the same substitution `file_icons::TintRole::Warning`
/// already uses for the identical artboard peach `#FAB387` reference on the
/// `.rs` file-type glyph, kept consistent here rather than hardcoding the hex.
fn area_hue(area: Area, cx: &App) -> Hsla {
    match area {
        Area::ExplorerEditor => cx.theme().blue,
        Area::Terminal => cx.theme().warning,
        Area::Diagnostics => cx.theme().red,
        Area::Git => cx.theme().green,
    }
}

/// Which color role an area's rail icon uses — pure and unit-testable
/// independent of a live theme, mirroring how `file_icons::TintRole` itself
/// stays resolve-free (the Hsla lookup against `cx.theme()` happens
/// separately, in [`area_button`]). Solo takes precedence over plain
/// visibility: [`RailState::solo`] already forces every non-target area's
/// `*_visible` flag to `false` (`Visibility::is_visible`), so in practice a
/// `Some(area)` match against `solo` implies `visible` is also true for that
/// same area — this only decides *which* hue the one still-visible icon
/// uses, never whether it renders as visible at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TintState {
    /// `area` is the current solo target — the shared solo hue.
    Solo,
    /// `area` is visible and not soloed — its own design hue ([`area_hue`]).
    Own,
    /// `area` is hidden — `muted_foreground`.
    Muted,
}

/// Resolve the [`TintState`] for `area` given its own visibility and the
/// current solo target.
fn tint_state(area: Area, visible: bool, solo: Option<Area>) -> TintState {
    if solo == Some(area) {
        TintState::Solo
    } else if visible {
        TintState::Own
    } else {
        TintState::Muted
    }
}

/// Build one rail icon button for a workspace **area**: the icon tints per
/// [`tint_state`] — [`area_hue`] while visible, the shared solo hue
/// (`theme().magenta`, matching the artboard's mauve `#CBA6F7`) while soloed,
/// or `muted_foreground` while hidden — with a matching 2px accent bar on the
/// icon's left edge whenever it renders with a hue (the artboard's "tint +
/// 2px bar" active state; never drawn while muted). The existing `selected`
/// surface-bg treatment (`Button::selected`) is kept alongside the new hue +
/// bar, not replaced by them — the design adds to today's selected state
/// rather than superseding it.
#[allow(clippy::too_many_arguments)]
fn area_button(
    id: &'static str,
    icon: impl Into<Icon>,
    tooltip: &'static str,
    area: Area,
    visible: bool,
    solo: Option<Area>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Button {
    let tint = match tint_state(area, visible, solo) {
        TintState::Solo => cx.theme().magenta,
        TintState::Own => area_hue(area, cx),
        TintState::Muted => cx.theme().muted_foreground,
    };
    let button = Button::new(id)
        .ghost()
        .with_size(BUTTON_SIZE)
        .icon(icon.into().text_color(tint))
        .selected(visible)
        .tooltip(tooltip)
        .on_click(on_click);
    if visible {
        button.border_l_2().border_color(tint)
    } else {
        button
    }
}

/// Render the 48px activity rail: files / terminal / source-control /
/// diagnostics toggles, a flexible spacer, then settings at the bottom — no
/// search icon, since no search panel exists yet (the spec's "no dead
/// controls" constraint). Theme tokens only: rail background/border match
/// the title bar's sidebar surface, the active state matches the design's
/// selected surface.
///
/// The four `on_toggle_*` callbacks are the caller's entity-bound listeners
/// (issue #848) — this function only wires them to the matching button's
/// `on_click`, it never builds a click handler itself for an area toggle.
pub fn render(
    state: RailState,
    on_toggle_explorer_editor: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_toggle_terminal: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_toggle_source_control: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_toggle_problems: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let explorer = area_button(
        "activity-rail-explorer",
        IconName::PanelLeft,
        "Explorer",
        Area::ExplorerEditor,
        state.explorer_editor_visible,
        state.solo,
        on_toggle_explorer_editor,
        cx,
    );

    let terminal = area_button(
        "activity-rail-terminal",
        IconName::SquareTerminal,
        "Terminal",
        Area::Terminal,
        state.terminal_visible,
        state.solo,
        on_toggle_terminal,
        cx,
    );

    let source_control = Badge::new().count(state.changed_count).child(area_button(
        "activity-rail-source-control",
        Icon::empty().path("file_icons/git-branch.svg"),
        "Source Control",
        Area::Git,
        state.git_visible,
        state.solo,
        on_toggle_source_control,
        cx,
    ));

    let problems_button = area_button(
        "activity-rail-problems",
        IconName::TriangleAlert,
        "Problems",
        Area::Diagnostics,
        state.diagnostics_visible,
        state.solo,
        on_toggle_problems,
        cx,
    );
    let problems: AnyElement = match state.worst_diagnostic {
        Some(severity) => Badge::new()
            .dot()
            .color(severity_color(severity, cx))
            .child(problems_button)
            .into_any_element(),
        None => problems_button.into_any_element(),
    };

    let settings = rail_button(
        "activity-rail-settings",
        IconName::Settings,
        "Settings",
        false,
        |_event, window, cx| window.dispatch_action(Box::new(OpenSettings), cx),
    );

    v_flex()
        .flex_none()
        .w(WIDTH)
        .h_full()
        .items_center()
        .py(px(6.0))
        .gap(px(4.0))
        .bg(cx.theme().sidebar)
        .border_r_1()
        .border_color(cx.theme().sidebar_border)
        .child(explorer)
        .child(terminal)
        .child(source_control)
        .child(problems)
        .child(div().flex_1())
        .child(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(severity: DiagnosticSeverity) -> Diagnostic {
        use rift_protocol::{Position, Range};

        Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            severity,
            message: "message".to_owned(),
            source: None,
            code: None,
        }
    }

    fn map_of(
        entries: Vec<(&str, &str, Vec<Diagnostic>)>,
    ) -> BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>> {
        let mut map: BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>> = BTreeMap::new();
        for (path, server, items) in entries {
            map.entry(path.to_owned())
                .or_default()
                .insert(server.to_owned(), items);
        }
        map
    }

    #[test]
    fn test_worst_severity_over_empty_map_is_none() {
        assert_eq!(worst_severity(&BTreeMap::new()), None);
    }

    #[test]
    fn test_worst_severity_picks_error_over_coexisting_warning() {
        let map = map_of(vec![(
            "a.rs",
            "rust-analyzer",
            vec![
                diag(DiagnosticSeverity::Warning),
                diag(DiagnosticSeverity::Error),
            ],
        )]);
        assert_eq!(worst_severity(&map), Some(DiagnosticSeverity::Error));
    }

    #[test]
    fn test_worst_severity_across_files_picks_the_single_worst() {
        let map = map_of(vec![
            (
                "a.rs",
                "rust-analyzer",
                vec![diag(DiagnosticSeverity::Hint)],
            ),
            (
                "b.rs",
                "rust-analyzer",
                vec![diag(DiagnosticSeverity::Warning)],
            ),
        ]);
        assert_eq!(worst_severity(&map), Some(DiagnosticSeverity::Warning));
    }

    #[test]
    fn test_worst_severity_only_hints_present_is_hint() {
        let map = map_of(vec![(
            "a.rs",
            "rust-analyzer",
            vec![diag(DiagnosticSeverity::Hint)],
        )]);
        assert_eq!(worst_severity(&map), Some(DiagnosticSeverity::Hint));
    }

    #[test]
    fn test_tint_state_visible_and_not_soloed_is_own() {
        assert_eq!(tint_state(Area::Git, true, None), TintState::Own);
    }

    #[test]
    fn test_tint_state_hidden_and_not_soloed_is_muted() {
        assert_eq!(tint_state(Area::Git, false, None), TintState::Muted);
    }

    #[test]
    fn test_tint_state_soloed_area_is_solo() {
        assert_eq!(
            tint_state(Area::Git, true, Some(Area::Git)),
            TintState::Solo
        );
    }

    #[test]
    fn test_tint_state_other_area_soloed_is_muted() {
        assert_eq!(
            tint_state(Area::Git, false, Some(Area::Terminal)),
            TintState::Muted
        );
    }
}
