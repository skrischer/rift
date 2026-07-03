//! Status bar: a thin, read-only strip along the bottom of the window — a
//! `flex_col` sibling of the `DockArea`, not a dock `Panel`
//! (`docs/spec-status-bar.md`). Renders two groups: the left group is the
//! current git branch plus its ahead/behind counts against the upstream,
//! sourced from `WorktreeModel::branch()` / `ahead_behind()` (`RepoState`
//! folds); the right group is the aggregate error/warning diagnostic counts,
//! aggregated locally from `WorktreeModel::all_diagnostics()` (`Diagnostics`
//! folds) — the shared `DiagnosticSeverity` derives no `Ord`, so the counting
//! (unlike `problems_panel`'s sorted grouping) needs no ordinal, just a match
//! per item.

use std::collections::BTreeMap;

use gpui::{div, px, App, IntoElement, ParentElement as _, Styled as _};
use gpui_component::ActiveTheme as _;
use rift_protocol::{AheadBehind, Diagnostic, DiagnosticSeverity};

/// Fixed height of the status bar strip, in pixels — a thin single row, never
/// competing with the dock area for vertical space.
const HEIGHT: f32 = 24.0;

/// Label shown when there is no branch to report: HEAD is detached, or the
/// worktree is not a git repo. The client cannot tell the two apart — both
/// collapse to a `None` `RepoState.branch` (`crates/protocol/src/lib.rs`) — so
/// one muted indicator covers both, per the spec's degrade-cleanly outcome.
const NO_BRANCH_LABEL: &str = "detached HEAD";

/// Format the branch + ahead/behind label. The ahead/behind counts are
/// appended only when there is something to show: a `None` `ahead_behind` (no
/// upstream) or an up-to-date `0`/`0` both omit them, mirroring git's own
/// porcelain output (`git status` drops the bracket when there is nothing to
/// report).
fn segment_text(branch: Option<&str>, ahead_behind: Option<AheadBehind>) -> String {
    let mut text = branch.unwrap_or(NO_BRANCH_LABEL).to_owned();
    if let Some(AheadBehind { ahead, behind }) = ahead_behind {
        if ahead > 0 || behind > 0 {
            text.push_str(&format!(" \u{2191}{ahead} \u{2193}{behind}"));
        }
    }
    text
}

/// Total error/warning diagnostic counts across every file and server in the
/// model's diagnostics map. A small local aggregation — the shared
/// `DiagnosticSeverity` derives no `Ord`, mirroring the reason
/// `problems_panel::SeverityCounts` also computes locally; only the two
/// counts the status bar needs are isolated here, per the spec's
/// optional-shared-helper note (duplicating `problems_panel`'s counting is
/// accepted for v1).
fn diagnostic_counts(
    diagnostics: &BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>>,
) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for item in diagnostics.values().flat_map(BTreeMap::values).flatten() {
        match item.severity {
            DiagnosticSeverity::Error => errors += 1,
            DiagnosticSeverity::Warning => warnings += 1,
            DiagnosticSeverity::Information | DiagnosticSeverity::Hint => {}
        }
    }
    (errors, warnings)
}

/// `count noun`/`count nouns` — singular for exactly one, plural otherwise.
fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Format the diagnostic-counts segment text, or `None` when there is
/// nothing to report — a zero/zero total renders quietly (the segment is
/// omitted entirely), mirroring how the left group omits ahead/behind when
/// up to date.
fn diagnostics_text(errors: usize, warnings: usize) -> Option<String> {
    if errors == 0 && warnings == 0 {
        return None;
    }
    Some(format!(
        "{}, {}",
        pluralize(errors, "error"),
        pluralize(warnings, "warning")
    ))
}

/// Render the status bar strip: the left group (branch + ahead/behind) and
/// the right group (aggregate diagnostic counts), pushed apart by a flexible
/// spacer. A missing branch (detached HEAD / no repo) renders muted; zero
/// diagnostics omits the right group entirely — neither is ever a crash.
pub fn render(
    branch: Option<&str>,
    ahead_behind: Option<AheadBehind>,
    diagnostics: &BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>>,
    cx: &App,
) -> impl IntoElement {
    let branch_color = if branch.is_some() {
        cx.theme().foreground
    } else {
        cx.theme().muted_foreground
    };

    let (errors, warnings) = diagnostic_counts(diagnostics);
    let counts_text = diagnostics_text(errors, warnings);
    let counts_color = if errors > 0 {
        cx.theme().danger
    } else {
        cx.theme().warning
    };

    let bar = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w_full()
        .h(px(HEIGHT))
        .px(px(8.0))
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .text_xs()
        .child(
            div()
                .text_color(branch_color)
                .child(segment_text(branch, ahead_behind)),
        )
        .child(div().flex_1());

    match counts_text {
        Some(text) => bar.child(div().text_color(counts_color).child(text)),
        None => bar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_text_shows_branch_name_when_present_with_no_upstream() {
        assert_eq!(segment_text(Some("main"), None), "main");
    }

    #[test]
    fn test_segment_text_shows_muted_indicator_when_detached_or_no_repo() {
        assert_eq!(segment_text(None, None), "detached HEAD");
    }

    #[test]
    fn test_segment_text_appends_ahead_behind_counts() {
        assert_eq!(
            segment_text(
                Some("main"),
                Some(AheadBehind {
                    ahead: 2,
                    behind: 1
                })
            ),
            "main \u{2191}2 \u{2193}1"
        );
    }

    #[test]
    fn test_segment_text_omits_ahead_behind_when_up_to_date() {
        assert_eq!(
            segment_text(
                Some("main"),
                Some(AheadBehind {
                    ahead: 0,
                    behind: 0
                })
            ),
            "main"
        );
    }

    #[test]
    fn test_segment_text_includes_both_counts_when_only_one_side_is_nonzero() {
        assert_eq!(
            segment_text(
                Some("main"),
                Some(AheadBehind {
                    ahead: 3,
                    behind: 0
                })
            ),
            "main \u{2191}3 \u{2193}0"
        );
    }

    #[test]
    fn test_segment_text_detached_with_ahead_behind_still_appends_counts() {
        // Defensive: the daemon never pairs a `None` branch with `Some`
        // ahead/behind in practice, but the formatter must not special-case
        // that combination away — it just composes the two independently.
        assert_eq!(
            segment_text(
                None,
                Some(AheadBehind {
                    ahead: 1,
                    behind: 0
                })
            ),
            "detached HEAD \u{2191}1 \u{2193}0"
        );
    }

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
    fn test_diagnostic_counts_over_empty_map_is_zero_zero() {
        assert_eq!(diagnostic_counts(&BTreeMap::new()), (0, 0));
    }

    #[test]
    fn test_diagnostic_counts_aggregates_errors_and_warnings_across_files_and_servers() {
        let map = map_of(vec![
            (
                "a.rs",
                "rust-analyzer",
                vec![
                    diag(DiagnosticSeverity::Error),
                    diag(DiagnosticSeverity::Error),
                ],
            ),
            ("a.rs", "clippy", vec![diag(DiagnosticSeverity::Warning)]),
            (
                "b.rs",
                "rust-analyzer",
                vec![
                    diag(DiagnosticSeverity::Warning),
                    diag(DiagnosticSeverity::Hint),
                ],
            ),
        ]);

        assert_eq!(diagnostic_counts(&map), (2, 2));
    }

    #[test]
    fn test_diagnostic_counts_ignores_information_and_hint_severities() {
        let map = map_of(vec![(
            "a.rs",
            "rust-analyzer",
            vec![
                diag(DiagnosticSeverity::Information),
                diag(DiagnosticSeverity::Hint),
            ],
        )]);

        assert_eq!(diagnostic_counts(&map), (0, 0));
    }

    #[test]
    fn test_diagnostics_text_hides_when_zero_errors_and_zero_warnings() {
        assert_eq!(diagnostics_text(0, 0), None);
    }

    #[test]
    fn test_diagnostics_text_pluralizes_singular_counts() {
        assert_eq!(
            diagnostics_text(1, 0),
            Some("1 error, 0 warnings".to_owned())
        );
        assert_eq!(
            diagnostics_text(0, 1),
            Some("0 errors, 1 warning".to_owned())
        );
    }

    #[test]
    fn test_diagnostics_text_shows_both_counts_when_both_nonzero() {
        assert_eq!(
            diagnostics_text(2, 3),
            Some("2 errors, 3 warnings".to_owned())
        );
    }
}
