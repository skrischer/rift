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
//!
//! [`render`] is one of the two exclusive render modes
//! (`docs/spec-tmux-statusline-mirroring.md`, #221); [`StatusBarMode`] selects
//! between it and [`render_mirrored`], the mirrored tmux status line built
//! from [`MirroredStatusLine`]. The two never compose.

use std::collections::BTreeMap;
use std::ffi::OsString;

use gpui::{div, px, App, FontWeight, Hsla, IntoElement, ParentElement as _, Styled as _};
use gpui_component::ActiveTheme as _;
use rift_protocol::{AheadBehind, Diagnostic, DiagnosticSeverity};
use rift_terminal::{
    parse_status_options, parse_style, parse_style_runs, truncate_runs, ResolvedColor, StatusStyle,
    StyleRun,
};

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

/// Env var enabling the mirrored tmux status line (the spec's `RIFT_*`
/// opt-in toggle, `docs/spec-tmux-statusline-mirroring.md`).
const MIRROR_ENV_VAR: &str = "RIFT_STATUSLINE_MIRROR";

/// Which render mode the status bar uses: the native Phase 2d fields (this
/// module's default `render`) or the mirrored tmux status line
/// ([`render_mirrored`]) — mutually exclusive, resolved once at startup; the
/// two never compose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBarMode {
    Native,
    Mirrored,
}

impl StatusBarMode {
    /// Resolve the mode from [`MIRROR_ENV_VAR`]: any non-empty value selects
    /// the mirrored render, matching the `RIFT_TERMINAL_LEGACY` escape-hatch
    /// convention elsewhere in the app. Read once at startup — a launch-time
    /// mode switch, not a live UI toggle (the spec's v1 scope).
    pub fn from_env() -> Self {
        Self::resolve(std::env::var_os(MIRROR_ENV_VAR))
    }

    fn resolve(var: Option<OsString>) -> Self {
        if var.is_some_and(|v| !v.is_empty()) {
            Self::Mirrored
        } else {
            Self::Native
        }
    }
}

/// The mirrored tmux status line's render model (#221): the resolved
/// `status-style` base plus the parsed, length-truncated runs for the left
/// and right segments — everything [`render_mirrored`] needs. Carries no GPUI
/// state, so building it from a raw `StatusLineReply` stays unit-testable
/// without a GPUI context. The default (empty runs, the theme-deferring base
/// style) is what renders before the first reply arrives — never a blanked
/// bar, never a panic.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirroredStatusLine {
    pub base: StatusStyle,
    pub left: Vec<StyleRun>,
    pub right: Vec<StyleRun>,
}

/// Build the render model from a `StatusLineReply`'s three raw strings: the
/// `show-options -A` output and the two server-side-expanded segments
/// (`docs/spec-tmux-statusline-mirroring.md`). `status-style` resolves to the
/// base every run starts from and `#[default]` resets to; each segment is
/// then parsed and truncated to its own `status-*-length`.
pub fn build_mirrored_status_line(
    options: &str,
    status_left: &str,
    status_right: &str,
) -> MirroredStatusLine {
    let options = parse_status_options(options);
    let base = parse_style(&options.status_style);
    let left = truncate_runs(
        parse_style_runs(status_left, base),
        options.status_left_length,
    );
    let right = truncate_runs(
        parse_style_runs(status_right, base),
        options.status_right_length,
    );
    MirroredStatusLine { base, left, right }
}

/// Resolve a tmux color token against the active theme: `ResolvedColor::Theme`
/// (tmux `default`/`terminal`, or a style option itself left at `default`)
/// defers to `theme_color`; a concrete `ResolvedColor::Color` wins outright.
/// Re-run on every render, so a theme switch re-resolves `default` colors
/// live.
fn resolve_color(color: ResolvedColor, theme_color: Hsla) -> Hsla {
    match color {
        ResolvedColor::Theme => theme_color,
        ResolvedColor::Color(hsla) => hsla,
    }
}

/// One status-line run's fully resolved render data: colors resolved against
/// the theme, attributes mapped to their GPUI-facing form — before it becomes
/// a `div`. Kept as its own step so the `StyleRun` -> render mapping is
/// unit-testable without a GPUI context. `blink` and `overline` have no
/// static-frame GPUI equivalent and are accepted no-ops here, mirroring how
/// `acs` is a no-op in the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ResolvedSpan {
    fg: Hsla,
    bg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    opacity: f32,
}

fn resolve_span(style: &StatusStyle, base_fg: Hsla, base_bg: Hsla) -> ResolvedSpan {
    let fg = resolve_color(style.fg, base_fg);
    let bg = resolve_color(style.bg, base_bg);
    // tmux's `reverse` swaps the resolved fg/bg pair, not the tokens
    // themselves — mirroring the terminal cell renderer's own inverse handling
    // (`rift_terminal::pane_view::extract_row_cells`).
    let (fg, bg) = if style.attrs.reverse {
        (bg, fg)
    } else {
        (fg, bg)
    };
    let opacity = if style.attrs.hidden {
        0.0
    } else if style.attrs.dim {
        0.6
    } else {
        1.0
    };
    ResolvedSpan {
        fg,
        bg,
        bold: style.attrs.bold,
        italic: style.attrs.italic,
        underline: style.attrs.underline,
        strikethrough: style.attrs.strikethrough,
        opacity,
    }
}

/// Turn one resolved run into a styled `div`, applying only the attributes it
/// actually sets (leaving GPUI's defaults otherwise).
fn styled_run(run: &StyleRun, base_fg: Hsla, base_bg: Hsla) -> impl IntoElement {
    let span = resolve_span(&run.style, base_fg, base_bg);
    let mut el = div()
        .text_color(span.fg)
        .bg(span.bg)
        .opacity(span.opacity)
        .child(run.text.clone());
    if span.bold {
        el = el.font_weight(FontWeight::BOLD);
    }
    if span.italic {
        el = el.italic();
    }
    if span.underline {
        el = el.underline();
    }
    if span.strikethrough {
        el = el.line_through();
    }
    el
}

/// Render the mirrored tmux status line (#221): the left segment's runs, a
/// flexible spacer, then the right segment's runs — the exclusive alternative
/// to [`render`] selected by [`StatusBarMode::Mirrored`]. `status-style`
/// paints the bar's own background/foreground; `default` colors (in the base
/// or any run) resolve against `cx.theme()`, so a theme switch re-resolves
/// them live on the next render.
pub fn render_mirrored(mirrored: &MirroredStatusLine, cx: &App) -> impl IntoElement {
    let base_fg = resolve_color(mirrored.base.fg, cx.theme().foreground);
    let base_bg = resolve_color(mirrored.base.bg, cx.theme().background);

    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w_full()
        .h(px(HEIGHT))
        .px(px(8.0))
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(base_bg)
        .text_xs()
        .text_color(base_fg)
        .children(
            mirrored
                .left
                .iter()
                .map(|run| styled_run(run, base_fg, base_bg)),
        )
        .child(div().flex_1())
        .children(
            mirrored
                .right
                .iter()
                .map(|run| styled_run(run, base_fg, base_bg)),
        )
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

    // --- mirrored status line toggle + render model (#221) ---

    #[test]
    fn test_status_bar_mode_resolve_absent_var_is_native() {
        assert_eq!(StatusBarMode::resolve(None), StatusBarMode::Native);
    }

    #[test]
    fn test_status_bar_mode_resolve_empty_var_is_native() {
        assert_eq!(
            StatusBarMode::resolve(Some(OsString::from(""))),
            StatusBarMode::Native
        );
    }

    #[test]
    fn test_status_bar_mode_resolve_nonempty_var_is_mirrored() {
        assert_eq!(
            StatusBarMode::resolve(Some(OsString::from("1"))),
            StatusBarMode::Mirrored
        );
    }

    #[test]
    fn test_build_mirrored_status_line_resolves_base_and_truncates_segments() {
        let options = "\
status-style bg=colour234,fg=colour253
status-left-length 3
status-right-length 40
";
        let mirrored = build_mirrored_status_line(options, "#[fg=green]hello", "#[fg=yellow]world");

        assert_eq!(mirrored.base, parse_style("bg=colour234,fg=colour253"));
        assert_eq!(mirrored.left.len(), 1);
        assert_eq!(mirrored.left[0].text, "hel");
        assert_eq!(mirrored.right.len(), 1);
        assert_eq!(mirrored.right[0].text, "world");
    }

    #[test]
    fn test_build_mirrored_status_line_defaults_on_empty_input() {
        let mirrored = build_mirrored_status_line("", "", "");
        assert_eq!(mirrored.base, StatusStyle::default());
        assert!(mirrored.left.is_empty());
        assert!(mirrored.right.is_empty());
    }

    fn hsla(l: f32) -> Hsla {
        Hsla {
            h: 0.0,
            s: 0.0,
            l,
            a: 1.0,
        }
    }

    #[test]
    fn test_resolve_color_theme_defers_to_theme_color() {
        let theme = hsla(0.5);
        assert_eq!(resolve_color(ResolvedColor::Theme, theme), theme);
    }

    #[test]
    fn test_resolve_color_concrete_color_wins_over_theme() {
        let concrete = hsla(0.2);
        assert_eq!(
            resolve_color(ResolvedColor::Color(concrete), hsla(0.9)),
            concrete
        );
    }

    #[test]
    fn test_resolve_span_reverse_swaps_fg_and_bg() {
        let mut style = StatusStyle {
            fg: ResolvedColor::Color(hsla(0.1)),
            bg: ResolvedColor::Color(hsla(0.8)),
            ..Default::default()
        };
        style.attrs.reverse = true;
        let span = resolve_span(&style, hsla(0.0), hsla(1.0));
        assert_eq!(span.fg, hsla(0.8));
        assert_eq!(span.bg, hsla(0.1));
    }

    #[test]
    fn test_resolve_span_hidden_overrides_dim_to_zero_opacity() {
        let mut style = StatusStyle::default();
        style.attrs.dim = true;
        style.attrs.hidden = true;
        let span = resolve_span(&style, hsla(0.0), hsla(1.0));
        assert_eq!(span.opacity, 0.0);
    }

    #[test]
    fn test_resolve_span_dim_alone_reduces_opacity() {
        let mut style = StatusStyle::default();
        style.attrs.dim = true;
        let span = resolve_span(&style, hsla(0.0), hsla(1.0));
        assert_eq!(span.opacity, 0.6);
    }

    #[test]
    fn test_resolve_span_no_attrs_is_full_opacity_no_flags() {
        let style = StatusStyle::default();
        let span = resolve_span(&style, hsla(0.0), hsla(1.0));
        assert_eq!(span.opacity, 1.0);
        assert!(!span.bold);
        assert!(!span.italic);
        assert!(!span.underline);
        assert!(!span.strikethrough);
    }

    #[test]
    fn test_resolve_span_passes_through_bold_italic_underline_strikethrough() {
        let mut style = StatusStyle::default();
        style.attrs.bold = true;
        style.attrs.italic = true;
        style.attrs.underline = true;
        style.attrs.strikethrough = true;
        let span = resolve_span(&style, hsla(0.0), hsla(1.0));
        assert!(span.bold);
        assert!(span.italic);
        assert!(span.underline);
        assert!(span.strikethrough);
    }
}
