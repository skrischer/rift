//! The 48px activity rail (#513, `docs/spec-cockpit-chrome.md`): a fixed-width
//! flex column left of the dock, toggling the explorer / source-control /
//! problems docks and opening settings.
//!
//! Presentational only, mirroring [`crate::title_bar`]: it takes the live
//! badge data ([`RailState`]) the caller already reads off the existing
//! client models (`WorktreeModel::git_statuses` for the changed-file count,
//! `WorktreeModel::all_diagnostics` for [`worst_severity`]) — this phase adds
//! rendering only, no new state (the spec's prior-decisions row). Each
//! button's click dispatches the same shell-command `Action`
//! ([`crate::workspace::ToggleExplorer`] etc.) the command palette already
//! binds, via `window.dispatch_action`, so the rail never reaches into a
//! `WorkspaceView` entity directly and bubbles to the same `on_action`
//! handlers at the workspace root.

use std::collections::BTreeMap;

use gpui::{
    div, px, AnyElement, App, ClickEvent, Hsla, IntoElement, ParentElement as _, Pixels,
    Styled as _, Window,
};
use gpui_component::badge::Badge;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{v_flex, ActiveTheme as _, IconName, Selectable as _, Sizable as _};
use rift_protocol::{Diagnostic, DiagnosticSeverity};

use crate::settings::OpenSettings;
use crate::workspace::{ToggleExplorer, ToggleProblems, ToggleSourceControl};

/// Fixed width of the activity rail, per the design contract.
pub const WIDTH: Pixels = px(48.0);

/// Side length of each icon button in the rail, per the design contract.
const BUTTON_SIZE: Pixels = px(36.0);

/// Live badge/active-state data the rail renders, read by the caller from the
/// existing dock and worktree models — never derived or cached here.
pub struct RailState {
    pub explorer_open: bool,
    pub source_control_open: bool,
    pub problems_open: bool,
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
/// icon" selected state; `on_click` dispatches the bubbling `Action` rather
/// than reaching into an entity, keeping this module presentational.
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

/// Render the 48px activity rail: files / source-control / diagnostics
/// toggles, a flexible spacer, then settings at the bottom — no search icon,
/// since no search panel exists yet (the spec's "no dead controls"
/// constraint). Theme tokens only: rail background/border match the title
/// bar's sidebar surface, the active state matches the design's selected
/// surface.
pub fn render(state: RailState, cx: &App) -> impl IntoElement {
    let explorer = rail_button(
        "activity-rail-explorer",
        IconName::Folder,
        "Explorer",
        state.explorer_open,
        |_event, window, cx| window.dispatch_action(Box::new(ToggleExplorer), cx),
    );

    let source_control = Badge::new().count(state.changed_count).child(rail_button(
        "activity-rail-source-control",
        IconName::Github,
        "Source Control",
        state.source_control_open,
        |_event, window, cx| window.dispatch_action(Box::new(ToggleSourceControl), cx),
    ));

    let problems_button = rail_button(
        "activity-rail-problems",
        IconName::TriangleAlert,
        "Problems",
        state.problems_open,
        |_event, window, cx| window.dispatch_action(Box::new(ToggleProblems), cx),
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
}
