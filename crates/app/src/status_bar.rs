//! The composite status line (`docs/spec-status-line.md`): one 28px workspace
//! bar, all mono, on the darkest ground token — replacing the three competing
//! bars the wave-1 gap analysis found (the 24px workspace strip, SessionView's
//! own statusbar, and the env-gated tmux mirror).
//!
//! Left group: the `>_ rift` wordmark, the live tmux window list (each window a
//! clickable `index:name` chip — the active one on a surface chip, a dot on
//! busy/attention windows; clicks select the window through the terminal's
//! existing command channel), and a transient PREFIX indicator while a chord is
//! pending. Right group: the git branch with its ahead/behind counts, the
//! working-tree `+N -M` line totals (hidden when clean), the aggregate error/
//! warning counts (hidden at zero), the language-server health dot + name, the
//! editor cursor `Ln L, Col C`, and a minute clock. Every value is read from an
//! existing model plus the two new streams; the pure formatting helpers below
//! are unit-tested, the element is assembled in [`render`].

use std::collections::BTreeMap;

use gpui::{
    div, px, App, Entity, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, Styled as _,
};
use gpui_component::{h_flex, ActiveTheme as _};
use rift_protocol::{AheadBehind, Diagnostic, DiagnosticSeverity, LspServerState};
use rift_terminal::{PaneActivity, SessionView, StatusWindow};

/// Fixed height of the composite status line, in pixels (the design's 28px).
const HEIGHT: f32 = 28.0;

/// Mono text size shared by every segment (the design's 12px).
const TEXT_SIZE: f32 = 12.0;

/// Label shown when a received `RepoState` carries no branch (a genuine
/// detached HEAD — the daemon only emits `RepoState` for git-repo roots). While
/// no `RepoState` has arrived the branch segment is omitted entirely rather than
/// claiming this (#490).
const NO_BRANCH_LABEL: &str = "detached HEAD";

/// The workspace's latest host resource sample, folded from the daemon's
/// `DaemonMessage::HostMetrics` push (`docs/spec-host-telemetry.md`). `protocol`
/// carries the full sample inline on the enum variant rather than as a separate
/// reusable type (unlike `LspServerState`), so this narrows it to the fields the
/// composite status line's MEM/CPU segment actually reads; a later phase widens
/// it as the pressure warning (Phase 44) and per-pane attribution (Phase 45)
/// need more of the daemon's coherent sample (`swap_*`, `load`, `cpu_count`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostMetrics {
    /// Aggregate CPU load, 0.0-100.0.
    pub cpu: f32,
    /// Total host RAM, in bytes.
    pub mem_total: u64,
    /// `MemAvailable` from `/proc/meminfo`, in bytes — the basis for "how much
    /// RAM is really free" (`docs/spec-host-telemetry.md`).
    pub mem_available: u64,
}

/// The full set of values the composite status line renders, borrowed from the
/// workspace's existing models plus the two new streams. Kept as one struct so
/// [`render`]'s signature stays legible and the workspace assembles the read in
/// one place.
pub struct StatusLineModel<'a> {
    /// The live tmux window list, in tab order (`SessionView::status_windows`).
    pub windows: &'a [StatusWindow],
    /// Whether the focused pane is mid-chord after the tmux prefix.
    pub prefix_pending: bool,
    /// Whether a `RepoState` has arrived (gates the branch segment, #490).
    pub repo_state_received: bool,
    /// Current branch, or `None` for a detached HEAD.
    pub branch: Option<&'a str>,
    /// Ahead/behind vs the upstream, or `None` when there is none.
    pub ahead_behind: Option<AheadBehind>,
    /// Working-tree lines added vs HEAD (#520).
    pub lines_added: u32,
    /// Working-tree lines removed vs HEAD (#520).
    pub lines_removed: u32,
    /// The workspace's aggregate diagnostics map (path -> server -> items).
    pub diagnostics: &'a BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>>,
    /// Language-server health, keyed by stable server name (`LspStatus`).
    pub lsp: &'a BTreeMap<String, LspServerState>,
    /// The host's latest resource sample (`HostMetrics`), or `None` before the
    /// first sample arrives — hides the MEM/CPU segment entirely, mirroring
    /// the LSP dot before a server is known (`docs/spec-host-telemetry.md`).
    pub host_metrics: Option<&'a HostMetrics>,
    /// The active editor tab's zero-based cursor `(line, column)`, or `None`
    /// when no tab is open.
    pub cursor: Option<(u32, u32)>,
    /// The client-local minute clock, pre-formatted (`format_clock`).
    pub clock: &'a str,
}

/// Format the branch + ahead/behind label, or `None` while no `RepoState` has
/// arrived (#490). Ahead/behind is appended only when there is something to
/// show — no upstream, or an up-to-date `0`/`0`, both omit it, mirroring git's
/// own porcelain output.
fn branch_text(
    repo_state_received: bool,
    branch: Option<&str>,
    ahead_behind: Option<AheadBehind>,
) -> Option<String> {
    if !repo_state_received {
        return None;
    }
    let mut text = branch.unwrap_or(NO_BRANCH_LABEL).to_owned();
    if let Some(AheadBehind { ahead, behind }) = ahead_behind {
        if ahead > 0 || behind > 0 {
            text.push_str(&format!(" \u{2191}{ahead} \u{2193}{behind}"));
        }
    }
    Some(text)
}

/// Total error/warning diagnostic counts across every file and server. A small
/// local aggregation — the shared `DiagnosticSeverity` derives no `Ord`, so
/// this counts with a match per item (like `problems_panel::SeverityCounts`).
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

/// The `+N` / `-M` working-tree line-total labels, or `None` on a clean
/// worktree (both zero) so the segment hides itself. Both labels are always
/// present together once shown, even when one side is zero, matching `git diff
/// --numstat`'s paired totals.
fn line_totals_text(added: u32, removed: u32) -> Option<(String, String)> {
    if added == 0 && removed == 0 {
        return None;
    }
    Some((format!("+{added}"), format!("-{removed}")))
}

/// The `Ln L, Col C` cursor label (1-based for display), or `None` when no
/// editor tab is open. The model carries the zero-based `(line, column)` the
/// editor's `InputState` reports.
fn cursor_text(cursor: Option<(u32, u32)>) -> Option<String> {
    cursor.map(|(line, column)| format!("Ln {}, Col {}", line + 1, column + 1))
}

/// The client-local minute clock, `HH:MM`, zero-padded. Pure so the caller
/// feeds `chrono::Local::now()`'s hour/minute and this stays testable without a
/// clock.
pub fn format_clock(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

/// The `MEM <n>% \u{b7} CPU <n>%` host-resource segment text
/// (`docs/spec-host-telemetry.md`): both percentages integer-rounded, a
/// middot separator, literal `MEM`/`CPU` labels. RAM% is
/// `(mem_total - mem_available) / mem_total`; `mem_total == 0` (a degenerate
/// sample) is guarded to `0%` rather than dividing by zero. Whether to render
/// at all (hidden before the first sample) is the caller's concern via
/// `StatusLineModel.host_metrics: Option<...>`, mirroring `cursor_text`.
fn metrics_text(cpu: f32, mem_total: u64, mem_available: u64) -> String {
    let mem_pct = if mem_total == 0 {
        0.0
    } else {
        (mem_total.saturating_sub(mem_available)) as f64 / mem_total as f64 * 100.0
    };
    format!(
        "MEM {}% \u{b7} CPU {}%",
        mem_pct.round() as i64,
        (cpu as f64).round() as i64
    )
}

/// Build the composite status line element. Theme tokens only: the bar sits on
/// the sidebar (darkest chrome) ground, mono at [`TEXT_SIZE`]; counts and the
/// LSP dot color by severity/state via `success`/`warning`/`danger`. Window
/// chips dispatch `select-window` through `session_view` (the existing tmux
/// command channel) on click.
pub fn render(
    model: StatusLineModel,
    session_view: &Entity<SessionView>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mono = theme.mono_font_family.clone();

    // --- left group: wordmark, window list, PREFIX ---------------------------
    let wordmark = div()
        .font_weight(FontWeight::BOLD)
        .text_color(theme.primary)
        .child(">_ rift");

    let mut window_list = h_flex().gap(px(4.0)).items_center();
    for w in model.windows {
        window_list = window_list.child(window_chip(w, session_view, cx));
    }

    let prefix = model.prefix_pending.then(|| {
        div()
            .px(px(6.0))
            .rounded(px(3.0))
            .bg(theme.warning)
            .text_color(theme.background)
            .font_weight(FontWeight::BOLD)
            .child("PREFIX")
    });

    let left = h_flex()
        .gap(px(16.0))
        .items_center()
        .child(wordmark)
        .child(window_list)
        .children(prefix);

    // --- right group: branch, totals, counts, LSP, cursor, clock ------------
    let branch =
        branch_text(model.repo_state_received, model.branch, model.ahead_behind).map(|t| {
            let color = if model.branch.is_some() {
                theme.foreground
            } else {
                theme.muted_foreground
            };
            div().text_color(color).child(SharedString::from(t))
        });

    let totals =
        line_totals_text(model.lines_added, model.lines_removed).map(|(added, removed)| {
            h_flex()
                .gap(px(6.0))
                .items_center()
                .child(
                    div()
                        .text_color(theme.success)
                        .child(SharedString::from(added)),
                )
                .child(
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(removed)),
                )
        });

    let (errors, warnings) = diagnostic_counts(model.diagnostics);
    let counts = (errors > 0 || warnings > 0).then(|| {
        let mut row = h_flex().gap(px(10.0)).items_center();
        if errors > 0 {
            row = row.child(count_segment(theme.danger, errors));
        }
        if warnings > 0 {
            row = row.child(count_segment(theme.warning, warnings));
        }
        row
    });

    let mut lsp = h_flex().gap(px(10.0)).items_center();
    for (server, state) in model.lsp {
        lsp = lsp.child(
            h_flex()
                .gap(px(4.0))
                .items_center()
                .child(dot(lsp_state_color(*state, cx)))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(server.clone())),
                ),
        );
    }

    let cursor = cursor_text(model.cursor).map(|t| {
        div()
            .text_color(theme.muted_foreground)
            .child(SharedString::from(t))
    });

    // Host resource segment (`docs/spec-host-telemetry.md`): hidden until the
    // first `HostMetrics` sample arrives, neutral-colored — threshold/pressure
    // coloring is Phase 44.
    let metrics = model.host_metrics.map(|m| {
        div()
            .text_color(theme.muted_foreground)
            .child(SharedString::from(metrics_text(
                m.cpu,
                m.mem_total,
                m.mem_available,
            )))
    });

    let clock = div()
        .text_color(theme.muted_foreground)
        .child(SharedString::from(model.clock.to_owned()));

    let right = h_flex()
        .gap(px(16.0))
        .items_center()
        .children(branch)
        .children(totals)
        .children(counts)
        .children((!model.lsp.is_empty()).then_some(lsp))
        .children(cursor)
        .children(metrics)
        .child(clock);

    h_flex()
        .flex_shrink_0()
        .justify_between()
        .items_center()
        .w_full()
        .h(px(HEIGHT))
        .px(px(12.0))
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .font_family(mono)
        .text_size(px(TEXT_SIZE))
        .text_color(theme.foreground)
        .child(left)
        .child(right)
}

/// One window as a clickable `index:name` chip. The active window sits on a
/// surface chip (`list_active`); a busy/attention window carries a leading dot
/// (success / danger). Click dispatches `select-window` through `session_view`
/// (the existing tmux command channel) — never a parallel path.
fn window_chip(w: &StatusWindow, session_view: &Entity<SessionView>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let activity_color = match w.activity {
        PaneActivity::Busy => Some(theme.success),
        PaneActivity::Attention => Some(theme.danger),
        PaneActivity::Free => None,
    };
    let label = format!("{}:{}", w.index, w.name);
    let entity = session_view.clone();
    let window_id = w.id.clone();

    let mut chip = div()
        .id(SharedString::from(format!("status-window-{}", w.id)))
        .flex()
        .items_center()
        .gap(px(4.0))
        .px(px(6.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .text_color(if w.is_active {
            theme.foreground
        } else {
            theme.muted_foreground
        });
    if w.is_active {
        chip = chip.bg(theme.list_active);
    } else {
        chip = chip.hover(|s| s.bg(theme.list_hover));
    }
    chip.children(activity_color.map(dot))
        .child(SharedString::from(label))
        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
            entity.update(cx, |view, cx| view.select_window(&window_id, cx));
        })
}

/// A colored dot + count, for one diagnostic severity (`●e` / `⚠w` in the
/// design, rendered as a token-colored dot so no emoji glyph is used).
fn count_segment(color: gpui::Hsla, count: usize) -> impl IntoElement {
    h_flex()
        .gap(px(4.0))
        .items_center()
        .child(dot(color))
        .child(SharedString::from(count.to_string()))
}

/// A 6px filled status dot in `color`.
fn dot(color: gpui::Hsla) -> impl IntoElement {
    div().size(px(6.0)).rounded_full().bg(color)
}

/// The health-dot color for one language-server state: running = success,
/// starting = warning, crashed = danger.
fn lsp_state_color(state: LspServerState, cx: &App) -> gpui::Hsla {
    match state {
        LspServerState::Running => cx.theme().success,
        LspServerState::Starting => cx.theme().warning,
        LspServerState::Crashed => cx.theme().danger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_protocol::{Position, Range};

    #[test]
    fn test_branch_text_shows_branch_name_when_present_with_no_upstream() {
        assert_eq!(
            branch_text(true, Some("main"), None),
            Some("main".to_owned())
        );
    }

    #[test]
    fn test_branch_text_before_repo_state_arrives_is_hidden() {
        assert_eq!(branch_text(false, None, None), None);
    }

    #[test]
    fn test_branch_text_shows_detached_head_only_after_repo_state_arrived() {
        assert_eq!(
            branch_text(true, None, None),
            Some("detached HEAD".to_owned())
        );
    }

    #[test]
    fn test_branch_text_appends_ahead_behind_counts() {
        assert_eq!(
            branch_text(
                true,
                Some("main"),
                Some(AheadBehind {
                    ahead: 2,
                    behind: 1
                })
            ),
            Some("main \u{2191}2 \u{2193}1".to_owned())
        );
    }

    #[test]
    fn test_branch_text_omits_ahead_behind_when_up_to_date() {
        assert_eq!(
            branch_text(
                true,
                Some("main"),
                Some(AheadBehind {
                    ahead: 0,
                    behind: 0
                })
            ),
            Some("main".to_owned())
        );
    }

    fn diag(severity: DiagnosticSeverity) -> Diagnostic {
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
    fn test_line_totals_text_hidden_on_clean_worktree() {
        assert_eq!(line_totals_text(0, 0), None);
    }

    #[test]
    fn test_line_totals_text_shows_both_sides_when_shown_even_if_one_is_zero() {
        assert_eq!(
            line_totals_text(12, 0),
            Some(("+12".to_owned(), "-0".to_owned()))
        );
        assert_eq!(
            line_totals_text(0, 3),
            Some(("+0".to_owned(), "-3".to_owned()))
        );
        assert_eq!(
            line_totals_text(12, 3),
            Some(("+12".to_owned(), "-3".to_owned()))
        );
    }

    #[test]
    fn test_cursor_text_is_hidden_without_a_tab() {
        assert_eq!(cursor_text(None), None);
    }

    #[test]
    fn test_cursor_text_is_one_based_for_display() {
        // Zero-based (0, 0) from the editor reads as Ln 1, Col 1.
        assert_eq!(cursor_text(Some((0, 0))), Some("Ln 1, Col 1".to_owned()));
        assert_eq!(cursor_text(Some((41, 7))), Some("Ln 42, Col 8".to_owned()));
    }

    #[test]
    fn test_format_clock_zero_pads_hour_and_minute() {
        assert_eq!(format_clock(9, 5), "09:05");
        assert_eq!(format_clock(23, 59), "23:59");
        assert_eq!(format_clock(0, 0), "00:00");
    }

    #[test]
    fn test_metrics_text_formats_mem_and_cpu_rounded() {
        assert_eq!(
            metrics_text(42.4, 16_000_000_000, 8_000_000_000),
            "MEM 50% \u{b7} CPU 42%"
        );
    }

    #[test]
    fn test_metrics_text_rounds_half_away_from_zero() {
        // 2/3 of mem_total used -> 66.67%; cpu 42.5 -> 43.
        assert_eq!(
            metrics_text(42.5, 3_000_000_000, 1_000_000_000),
            "MEM 67% \u{b7} CPU 43%"
        );
    }

    #[test]
    fn test_metrics_text_guards_against_zero_mem_total() {
        assert_eq!(metrics_text(10.0, 0, 0), "MEM 0% \u{b7} CPU 10%");
    }
}
