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

use flume::Sender;
use gpui::{
    div, px, Anchor, App, Entity, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, Styled as _,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    v_flex, ActiveTheme as _, Sizable as _,
};
use rift_protocol::{
    AheadBehind, ClientMessage, Diagnostic, DiagnosticSeverity, LspServerState, MemoryPressure,
    PaneMetric,
};
use rift_terminal::{PaneActivity, SessionView, StatusWindow};
use tracing::debug;

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
/// composite status line's MEM/CPU segment and [`pressure_level`]
/// (`docs/spec-memory-pressure.md`) read; per-pane attribution (Phase 45) may
/// widen it further.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostMetrics {
    /// Aggregate CPU load, 0.0-100.0.
    pub cpu: f32,
    /// Total host RAM, in bytes.
    pub mem_total: u64,
    /// `MemAvailable` from `/proc/meminfo`, in bytes — the basis for "how much
    /// RAM is really free" (`docs/spec-host-telemetry.md`).
    pub mem_available: u64,
    /// Total configured swap, in bytes — the denominator for the swap-used
    /// ratio [`pressure_level`] reads (`docs/spec-memory-pressure.md`).
    pub swap_total: u64,
    /// Swap currently in use, in bytes.
    pub swap_used: u64,
    /// Linux PSI memory-stall averages, where the kernel exposes
    /// `/proc/pressure/memory` (`None` on hosts without `CONFIG_PSI`, e.g. the
    /// stock `microsoft-standard-WSL2` kernel) — an optional escalation signal
    /// for [`pressure_level`].
    pub psi: Option<MemoryPressure>,
}

/// The recolour states for the composite status line's MEM/CPU segment,
/// computed by [`pressure_level`] from the pushed [`HostMetrics`] sample
/// (`docs/spec-memory-pressure.md`). Declared `Normal < Warning < Critical` so
/// `PressureLevel::max` picks "the worse of the two" when combining the
/// independent mem-available / swap-used / PSI triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum PressureLevel {
    #[default]
    Normal,
    Warning,
    Critical,
}

/// Warning enters when the `MemAvailable` ratio falls below this fraction of
/// total RAM (the "Standard" threshold band,
/// `docs/spec-memory-pressure.md`).
const WARNING_MEM_AVAILABLE_ENTER: f64 = 0.20;
/// Warning exits back to `Normal` once the `MemAvailable` ratio recovers above
/// this fraction — separate from the enter threshold so the segment does not
/// flap at the boundary (hysteresis).
const WARNING_MEM_AVAILABLE_EXIT: f64 = 0.25;
/// Critical enters when the `MemAvailable` ratio falls below this fraction.
const CRITICAL_MEM_AVAILABLE_ENTER: f64 = 0.10;
/// Critical exits back toward `Warning` once the `MemAvailable` ratio
/// recovers above this fraction (hysteresis).
const CRITICAL_MEM_AVAILABLE_EXIT: f64 = 0.15;
/// Warning triggers once the swap-used ratio exceeds this fraction of total
/// swap. Unlike the mem-available axis, the spec gives a single boundary here
/// (no separate exit value), so this axis is a plain threshold check.
const WARNING_SWAP_USED: f64 = 0.50;
/// Critical triggers once the swap-used ratio exceeds this fraction.
const CRITICAL_SWAP_USED: f64 = 0.80;
/// PSI `some_avg10` (percent of wall-clock time at least one task stalled on
/// memory, over the trailing 10s) above this cutoff escalates a `Normal`
/// baseline to `Warning`.
const PSI_SOME_AVG10_WARNING_CUTOFF: f64 = 5.0;

/// Compute the client-side memory-pressure level from a pushed [`HostMetrics`]
/// sample, given the previously computed level (for hysteresis,
/// `docs/spec-memory-pressure.md`). Memory-only trigger — `cpu`/load never
/// factor in. The portable baseline (`MemAvailable` ratio + swap-used ratio)
/// always drives the level so every host, including the PSI-less stock WSL2
/// kernel, gets a working signal; PSI, where present, only ever escalates the
/// baseline upward, never lowers it.
pub fn pressure_level(sample: HostMetrics, previous: PressureLevel) -> PressureLevel {
    let mem_available_ratio = if sample.mem_total == 0 {
        1.0
    } else {
        sample.mem_available as f64 / sample.mem_total as f64
    };
    let swap_used_ratio = if sample.swap_total == 0 {
        0.0
    } else {
        sample.swap_used as f64 / sample.swap_total as f64
    };

    let mem_level = mem_available_level(mem_available_ratio, previous);
    let swap_level = if swap_used_ratio > CRITICAL_SWAP_USED {
        PressureLevel::Critical
    } else if swap_used_ratio > WARNING_SWAP_USED {
        PressureLevel::Warning
    } else {
        PressureLevel::Normal
    };
    let baseline = mem_level.max(swap_level);

    escalate_with_psi(baseline, sample.psi)
}

/// The `MemAvailable`-ratio axis of [`pressure_level`], with enter/exit
/// hysteresis: `previous` decides which boundary applies so the level holds
/// steady inside the enter/exit gap instead of flapping. Descending out of
/// `Critical` lands on `Warning` unless the ratio also clears the `Warning`
/// exit threshold, matching a gradual recovery rather than a jump straight to
/// `Normal`.
fn mem_available_level(ratio: f64, previous: PressureLevel) -> PressureLevel {
    match previous {
        PressureLevel::Critical => {
            if ratio <= CRITICAL_MEM_AVAILABLE_EXIT {
                PressureLevel::Critical
            } else if ratio <= WARNING_MEM_AVAILABLE_EXIT {
                PressureLevel::Warning
            } else {
                PressureLevel::Normal
            }
        }
        PressureLevel::Warning => {
            if ratio < CRITICAL_MEM_AVAILABLE_ENTER {
                PressureLevel::Critical
            } else if ratio > WARNING_MEM_AVAILABLE_EXIT {
                PressureLevel::Normal
            } else {
                PressureLevel::Warning
            }
        }
        PressureLevel::Normal => {
            if ratio < CRITICAL_MEM_AVAILABLE_ENTER {
                PressureLevel::Critical
            } else if ratio < WARNING_MEM_AVAILABLE_ENTER {
                PressureLevel::Warning
            } else {
                PressureLevel::Normal
            }
        }
    }
}

/// PSI escalation over `baseline` (`docs/spec-memory-pressure.md`): a present
/// stall only ever raises the level, never lowers it. Absent PSI (`None`,
/// e.g. stock WSL2) leaves `baseline` untouched.
fn escalate_with_psi(baseline: PressureLevel, psi: Option<MemoryPressure>) -> PressureLevel {
    let Some(psi) = psi else {
        return baseline;
    };
    let mut level = baseline;
    if psi.some_avg10 > PSI_SOME_AVG10_WARNING_CUTOFF {
        level = level.max(PressureLevel::Warning);
    }
    if psi.full_avg10 > 0.0 {
        level = level.max(PressureLevel::Critical);
    }
    level
}

/// The upward-transition memory-pressure toast message
/// (`docs/spec-memory-pressure.md`), e.g. `"Host memory low - 8% available"`:
/// the `MemAvailable` ratio as an integer percentage, guarded against
/// `mem_total == 0` the same way [`metrics_text`] is. The caller
/// (`workspace.rs`'s host-metrics fold loop) picks the `NotificationType`
/// (`Warning`/`Error`) from the new [`PressureLevel`]; this only names the
/// condition.
pub fn pressure_toast_message(mem_total: u64, mem_available: u64) -> String {
    let available_pct = if mem_total == 0 {
        0.0
    } else {
        mem_available as f64 / mem_total as f64 * 100.0
    };
    format!(
        "Host memory low - {}% available",
        available_pct.round() as i64
    )
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
    /// The memory-pressure level computed from `host_metrics`
    /// (`docs/spec-memory-pressure.md`), which recolours the MEM/CPU segment
    /// text — `Normal` -> `theme.muted_foreground`, `Warning` -> `theme.warning`,
    /// `Critical` -> `theme.danger`. Unused while `host_metrics` is `None`.
    pub pressure_level: PressureLevel,
    /// The attached session's latest per-pane breakdown, folded from the
    /// daemon's per-connection `PaneMetrics` push
    /// (`docs/spec-pane-attribution.md`, #881) — empty before the first push
    /// arrives (the popover renders a brief "sampling" placeholder for that
    /// case, [`pane_metrics_popover_content`]).
    pub pane_metrics: &'a [PaneMetric],
    /// The breakdown popover's open/close toggle sender: a clone is captured
    /// by the popover's `on_open_change` callback in [`render`], which
    /// forwards it onto the protocol as `ClientMessage::SetPaneMetricsEnabled`
    /// — `true` on open (starts this connection's per-pane sampling on the
    /// daemon), `false` on close (stops it).
    pub pane_metrics_enabled_tx: Sender<ClientMessage>,
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

/// One ranked row in the pane-metrics breakdown popover
/// (`docs/spec-pane-attribution.md`, #881): the pane's agnostic `command`
/// label plus its RSS/CPU, pre-formatted for display.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneMetricRow {
    pub pane_id: u32,
    pub command: String,
    pub rss_text: String,
    pub cpu_text: String,
}

/// Rank a `PaneMetrics` push into the breakdown popover's display rows
/// (`docs/spec-pane-attribution.md`): sorted by RSS descending, ties broken
/// by CPU descending, so the pane most likely "the cause" of host pressure
/// reads first. Pure/unit-tested; [`render`] calls this each time the
/// popover's content is built (the pane-metrics fold loop keeps the input
/// current).
pub fn pane_metric_rows(entries: &[PaneMetric]) -> Vec<PaneMetricRow> {
    let mut ranked: Vec<&PaneMetric> = entries.iter().collect();
    ranked.sort_by(|a, b| b.rss.cmp(&a.rss).then(b.cpu.total_cmp(&a.cpu)));
    ranked
        .into_iter()
        .map(|m| PaneMetricRow {
            pane_id: m.pane_id,
            command: m.command.clone(),
            rss_text: format_rss_mb(m.rss),
            cpu_text: format!("{}%", (m.cpu as f64).round() as i64),
        })
        .collect()
}

/// The breakdown popover's RSS label: whole megabytes, rounded — coarser
/// than the MEM/CPU segment's percentage ([`metrics_text`]) since an
/// absolute per-pane byte count is more useful here than a host-relative
/// percent.
fn format_rss_mb(rss: u64) -> String {
    format!("{} MB", (rss as f64 / (1024.0 * 1024.0)).round() as u64)
}

/// The breakdown popover's content (`docs/spec-pane-attribution.md`, #881):
/// a brief "sampling..." placeholder while `rows` is still empty (no push
/// has arrived since the popover opened), otherwise one row per pane —
/// the agnostic `command` label left, RSS + CPU right. Theme tokens only.
fn pane_metrics_popover_content(rows: &[PaneMetricRow], cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = v_flex().gap(px(4.0)).min_w(px(180.0));
    if rows.is_empty() {
        list = list.child(
            div()
                .text_color(theme.muted_foreground)
                .child("sampling..."),
        );
    } else {
        for row in rows {
            list = list.child(
                h_flex()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(SharedString::from(row.command.clone())),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(row.rss_text.clone())),
                            )
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(row.cpu_text.clone())),
                            ),
                    ),
            );
        }
    }
    list
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
    // first `HostMetrics` sample arrives; text color follows the client-side
    // `pressure_level` (`docs/spec-memory-pressure.md`) — neutral / warning /
    // critical, the same semantic tokens the diagnostic counts and LSP dot use.
    let pressure_color = match model.pressure_level {
        PressureLevel::Normal => theme.muted_foreground,
        PressureLevel::Warning => theme.warning,
        PressureLevel::Critical => theme.danger,
    };
    // Per-pane breakdown popover (`docs/spec-pane-attribution.md`, #881):
    // clicking the segment toggles a `Popover` listing the attached
    // session's panes ranked by RSS/CPU. `on_open_change` forwards the new
    // open state onto the protocol as `ClientMessage::SetPaneMetricsEnabled`
    // so the daemon samples only while this popover is open; `pane_metrics`
    // (folded from the daemon's per-connection pushes) drives the content,
    // rebuilt fresh on every render per `Popover::content`'s own contract.
    let metrics = model.host_metrics.map(|m| {
        let text = metrics_text(m.cpu, m.mem_total, m.mem_available);
        let rows = pane_metric_rows(model.pane_metrics);
        let enabled_tx = model.pane_metrics_enabled_tx.clone();
        Popover::new("status-pane-metrics")
            .anchor(Anchor::BottomRight)
            .trigger(
                Button::new("status-pane-metrics-trigger")
                    .text()
                    .xsmall()
                    .label(text)
                    .text_color(pressure_color),
            )
            .on_open_change(move |open, _window, _cx| {
                let enabled = *open;
                if let Err(e) =
                    enabled_tx.try_send(ClientMessage::SetPaneMetricsEnabled { enabled })
                {
                    debug!(error = %e, enabled, "failed to send pane-metrics enabled toggle");
                }
            })
            .content(move |_state, _window, cx| pane_metrics_popover_content(&rows, cx))
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
/// (success working / warning idle-awaiting-input / danger attention). Click
/// dispatches `select-window` through `session_view` (the existing tmux
/// command channel) — never a parallel path.
fn window_chip(w: &StatusWindow, session_view: &Entity<SessionView>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let activity_color = activity_dot_color(w.activity, theme.success, theme.warning, theme.danger);
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

/// The window chip's leading-dot color for a folded [`PaneActivity`]: success
/// while actively working, warning while idle-awaiting-input (the busy
/// refinement `docs/spec-agent-activity.md` adds), danger on attention, and
/// no dot while free. GPUI-free (the theme colors are passed in) so the
/// mapping is unit-testable without an app context.
fn activity_dot_color(
    activity: PaneActivity,
    success: gpui::Hsla,
    warning: gpui::Hsla,
    danger: gpui::Hsla,
) -> Option<gpui::Hsla> {
    match activity {
        PaneActivity::Busy => Some(success),
        PaneActivity::BusyIdle => Some(warning),
        PaneActivity::Attention => Some(danger),
        PaneActivity::Free => None,
    }
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
        // Compile stub: full informational rendering lands in #913.
        LspServerState::NotInstalled => cx.theme().muted_foreground,
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

    #[gpui::test]
    fn test_activity_dot_color_maps_each_activity_to_its_theme_color(
        cx: &mut gpui::TestAppContext,
    ) {
        // Reads the live gpui-component theme tokens (never a raw color
        // constructor, `docs/spec-settings-theme.md`) so the mapping is
        // checked against the colors `window_chip` actually renders with.
        cx.update(|cx| {
            gpui_component::init(cx);
            let theme = cx.theme();
            let success = theme.success;
            let warning = theme.warning;
            let danger = theme.danger;

            assert_eq!(
                activity_dot_color(PaneActivity::Busy, success, warning, danger),
                Some(success)
            );
            assert_eq!(
                activity_dot_color(PaneActivity::BusyIdle, success, warning, danger),
                Some(warning)
            );
            assert_eq!(
                activity_dot_color(PaneActivity::Attention, success, warning, danger),
                Some(danger)
            );
            assert_eq!(
                activity_dot_color(PaneActivity::Free, success, warning, danger),
                None
            );
        });
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

    // --- pressure_toast_message (docs/spec-memory-pressure.md) ---------------

    #[test]
    fn test_pressure_toast_message_names_available_percentage() {
        assert_eq!(
            pressure_toast_message(16_000_000_000, 1_280_000_000),
            "Host memory low - 8% available"
        );
    }

    #[test]
    fn test_pressure_toast_message_guards_against_zero_mem_total() {
        assert_eq!(
            pressure_toast_message(0, 0),
            "Host memory low - 0% available"
        );
    }

    // --- pressure_level (docs/spec-memory-pressure.md) -----------------------

    /// Builds a sample with a 16 GB host and a 4 GB swap, at the given
    /// `mem_available` / `swap_used` byte counts, no PSI.
    fn sample(mem_available: u64, swap_used: u64) -> HostMetrics {
        HostMetrics {
            cpu: 0.0,
            mem_total: 16_000_000_000,
            mem_available,
            swap_total: 4_000_000_000,
            swap_used,
            psi: None,
        }
    }

    fn sample_with_psi(mem_available: u64, psi: MemoryPressure) -> HostMetrics {
        HostMetrics {
            psi: Some(psi),
            ..sample(mem_available, 0)
        }
    }

    fn no_stall() -> MemoryPressure {
        MemoryPressure {
            some_avg10: 0.0,
            some_avg60: 0.0,
            some_avg300: 0.0,
            full_avg10: 0.0,
            full_avg60: 0.0,
            full_avg300: 0.0,
        }
    }

    #[test]
    fn test_pressure_level_mem_available_bands_from_normal_baseline() {
        // 30% available: comfortably normal.
        assert_eq!(
            pressure_level(sample(4_800_000_000, 0), PressureLevel::Normal),
            PressureLevel::Normal
        );
        // 15% available: inside the warning band (<20%, not yet <10%).
        assert_eq!(
            pressure_level(sample(2_400_000_000, 0), PressureLevel::Normal),
            PressureLevel::Warning
        );
        // 5% available: inside the critical band (<10%).
        assert_eq!(
            pressure_level(sample(800_000_000, 0), PressureLevel::Normal),
            PressureLevel::Critical
        );
    }

    #[test]
    fn test_pressure_level_swap_used_bands_trigger_independent_of_mem_available() {
        // Ample mem-available (80%), but swap 60% used -> warning.
        assert_eq!(
            pressure_level(sample(12_800_000_000, 2_400_000_000), PressureLevel::Normal),
            PressureLevel::Warning
        );
        // Ample mem-available (80%), but swap 90% used -> critical.
        assert_eq!(
            pressure_level(sample(12_800_000_000, 3_600_000_000), PressureLevel::Normal),
            PressureLevel::Critical
        );
    }

    #[test]
    fn test_pressure_level_warning_hysteresis_holds_inside_enter_exit_gap() {
        // 22% available sits inside the warning enter (<20%) / exit (>25%)
        // gap: from a Normal previous it never entered warning...
        assert_eq!(
            pressure_level(sample(3_520_000_000, 0), PressureLevel::Normal),
            PressureLevel::Normal
        );
        // ...but from a Warning previous it holds at warning rather than
        // dropping back to normal, since 22% has not cleared the 25% exit.
        assert_eq!(
            pressure_level(sample(3_520_000_000, 0), PressureLevel::Warning),
            PressureLevel::Warning
        );
        // Once available clears the 25% exit threshold, it returns to normal.
        assert_eq!(
            pressure_level(sample(4_100_000_000, 0), PressureLevel::Warning),
            PressureLevel::Normal
        );
    }

    #[test]
    fn test_pressure_level_critical_hysteresis_holds_inside_enter_exit_gap() {
        // 12% available sits inside the critical enter (<10%) / exit (>15%)
        // gap: holds at critical from a Critical previous...
        assert_eq!(
            pressure_level(sample(1_920_000_000, 0), PressureLevel::Critical),
            PressureLevel::Critical
        );
        // ...but from a Warning previous, 12% never re-entered critical.
        assert_eq!(
            pressure_level(sample(1_920_000_000, 0), PressureLevel::Warning),
            PressureLevel::Warning
        );
        // Past the 15% critical exit but still under the 25% warning exit:
        // descends to warning, not straight to normal.
        assert_eq!(
            pressure_level(sample(2_560_000_000, 0), PressureLevel::Critical),
            PressureLevel::Warning
        );
        // Past both exits: descends all the way to normal.
        assert_eq!(
            pressure_level(sample(4_100_000_000, 0), PressureLevel::Critical),
            PressureLevel::Normal
        );
    }

    #[test]
    fn test_pressure_level_psi_present_escalates_normal_baseline() {
        // Ample mem-available (80%) so the baseline alone is Normal.
        let base_mem_available = 12_800_000_000;
        // some_avg10 above the 5% cutoff escalates Normal -> Warning.
        let stalling = MemoryPressure {
            some_avg10: 6.0,
            ..no_stall()
        };
        assert_eq!(
            pressure_level(
                sample_with_psi(base_mem_available, stalling),
                PressureLevel::Normal
            ),
            PressureLevel::Warning
        );
        // Any full_avg10 > 0 escalates all the way to Critical.
        let fully_stalled = MemoryPressure {
            full_avg10: 0.1,
            ..no_stall()
        };
        assert_eq!(
            pressure_level(
                sample_with_psi(base_mem_available, fully_stalled),
                PressureLevel::Normal
            ),
            PressureLevel::Critical
        );
    }

    #[test]
    fn test_pressure_level_psi_absent_leaves_baseline_intact() {
        // Warning-band mem-available (15%), no PSI: baseline stands alone.
        assert_eq!(
            pressure_level(sample(2_400_000_000, 0), PressureLevel::Normal),
            PressureLevel::Warning
        );
    }

    #[test]
    fn test_pressure_level_psi_never_lowers_a_worse_baseline() {
        // Critical baseline (5% available) with a quiet PSI reading: PSI only
        // ever raises the level, so it must not pull critical back down.
        assert_eq!(
            pressure_level(
                sample_with_psi(800_000_000, no_stall()),
                PressureLevel::Normal
            ),
            PressureLevel::Critical
        );
    }

    // --- pane_metric_rows (docs/spec-pane-attribution.md, #881) --------------

    fn pane(pane_id: u32, rss: u64, cpu: f32, command: &str) -> PaneMetric {
        PaneMetric {
            pane_id,
            rss,
            cpu,
            command: command.to_owned(),
        }
    }

    #[test]
    fn test_pane_metric_rows_ranks_by_rss_descending() {
        let entries = vec![
            pane(1, 1_048_576, 5.0, "vim"),
            pane(2, 209_715_200, 42.0, "cargo"),
            pane(3, 10_485_760, 1.0, "zsh"),
        ];
        let rows = pane_metric_rows(&entries);
        assert_eq!(
            rows.iter().map(|r| r.pane_id).collect::<Vec<_>>(),
            vec![2, 3, 1],
            "heaviest RSS pane ranks first"
        );
    }

    #[test]
    fn test_pane_metric_rows_ties_broken_by_cpu_descending() {
        let entries = vec![
            pane(1, 1_048_576, 5.0, "vim"),
            pane(2, 1_048_576, 42.0, "cargo"),
        ];
        let rows = pane_metric_rows(&entries);
        assert_eq!(
            rows.iter().map(|r| r.pane_id).collect::<Vec<_>>(),
            vec![2, 1],
            "equal RSS breaks the tie on CPU descending"
        );
    }

    #[test]
    fn test_pane_metric_rows_formats_command_rss_and_cpu() {
        let entries = vec![pane(7, 209_715_200, 42.4, "cargo")];
        let rows = pane_metric_rows(&entries);
        assert_eq!(
            rows,
            vec![PaneMetricRow {
                pane_id: 7,
                command: "cargo".to_owned(),
                rss_text: "200 MB".to_owned(),
                cpu_text: "42%".to_owned(),
            }]
        );
    }

    #[test]
    fn test_pane_metric_rows_empty_input_yields_no_rows() {
        assert!(pane_metric_rows(&[]).is_empty());
    }
}
