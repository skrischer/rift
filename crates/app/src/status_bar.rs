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
    canvas, div, px, Anchor, App, Bounds, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, PathBuilder, Pixels, SharedString, Styled as _,
    Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    hover_card::HoverCard,
    plot::origin_point,
    popover::Popover,
    v_flex, ActiveTheme as _, Sizable as _, Theme,
};
use rift_protocol::{
    AheadBehind, ClientMessage, Diagnostic, DiagnosticSeverity, LoadAverage, LspServerState,
    MemoryPressure, PaneMetric,
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
/// composite status line's MEM/CPU segment, [`pressure_level`]
/// (`docs/spec-memory-pressure.md`), the host-detail hover card
/// (`docs/spec-telemetry-detail.md`), and the DISK segment
/// (`docs/spec-telemetry-detail.md`) read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostMetrics {
    /// Aggregate CPU load, 0.0-100.0.
    pub cpu: f32,
    /// Total host RAM, in bytes.
    pub mem_total: u64,
    /// `MemAvailable` from `/proc/meminfo`, in bytes — the basis for "how much
    /// RAM is really free" (`docs/spec-host-telemetry.md`).
    pub mem_available: u64,
    /// `Cached` from `/proc/meminfo`, in bytes (`docs/spec-telemetry-detail.md`).
    pub mem_cached: u64,
    /// `Buffers` from `/proc/meminfo`, in bytes (`docs/spec-telemetry-detail.md`).
    pub mem_buffers: u64,
    /// Total configured swap, in bytes — the denominator for the swap-used
    /// ratio [`pressure_level`] reads (`docs/spec-memory-pressure.md`).
    pub swap_total: u64,
    /// Swap currently in use, in bytes.
    pub swap_used: u64,
    /// Host load average over 1/5/15 minutes (`docs/spec-telemetry-detail.md`).
    pub load: LoadAverage,
    /// Number of logical CPU cores (`docs/spec-telemetry-detail.md`).
    pub cpu_count: u32,
    /// Host uptime, in seconds (`docs/spec-telemetry-detail.md`).
    pub uptime_secs: u64,
    /// Total capacity of the daemon's own filesystem, in bytes
    /// (`docs/spec-telemetry-detail.md`) — the basis for the DISK status
    /// segment's used-percentage.
    pub disk_total: u64,
    /// Free space on the daemon's own filesystem, in bytes
    /// (`docs/spec-telemetry-detail.md`).
    pub disk_available: u64,
    /// Linux PSI memory-stall averages, where the kernel exposes
    /// `/proc/pressure/memory` (`None` on hosts without `CONFIG_PSI`, e.g. the
    /// stock `microsoft-standard-WSL2` kernel) — an optional escalation signal
    /// for [`pressure_level`].
    pub psi: Option<MemoryPressure>,
}

/// Retention window for the inline memory-history sparkline
/// (`docs/spec-telemetry-detail.md`): ~150 samples at the daemon's 2s
/// `HostMetrics` cadence is ~5 minutes — enough to show a trend toward the
/// limit without unbounded growth over a long session.
pub const MEMORY_HISTORY_CAPACITY: usize = 150;

/// A bounded, client-side ring buffer of recent memory-used percentages
/// (`docs/spec-telemetry-detail.md`): pure derived state folded from the
/// existing `HostMetrics` push in the workspace's fold loop, never sent on
/// the wire. The oldest sample drops once `capacity` is reached, so a long
/// session's history stays flat rather than growing unbounded.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryHistory {
    samples: Vec<f32>,
    capacity: usize,
}

impl MemoryHistory {
    /// A new, empty history bounded to `capacity` samples (at least 1).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            samples: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Push a new mem%-used sample, evicting the oldest sample first once
    /// already at capacity.
    pub fn push(&mut self, mem_pct: f32) {
        if self.samples.len() >= self.capacity {
            self.samples.remove(0);
        }
        self.samples.push(mem_pct);
    }

    /// The buffered samples, oldest first.
    pub fn as_slice(&self) -> &[f32] {
        &self.samples
    }
}

impl Default for MemoryHistory {
    fn default() -> Self {
        Self::new(MEMORY_HISTORY_CAPACITY)
    }
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
    /// The client-side memory-history ring buffer's current samples (oldest
    /// first), rendered as the host-detail hover card's inline sparkline
    /// (`docs/spec-telemetry-detail.md`). Empty before the first sample;
    /// `host_metrics` gates whether the segment (and so the hover card)
    /// renders at all.
    pub mem_history: &'a [f32],
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

/// The host's memory currently in use, in bytes: `mem_total - mem_available`
/// (`docs/spec-telemetry-detail.md`), saturating so a degenerate sample
/// (`mem_available > mem_total`) never underflows. The basis for both the
/// MEM% status-line segment ([`metrics_text`]) and the hover card's memory
/// breakdown ([`host_detail_content`]).
pub fn mem_used_bytes(mem_total: u64, mem_available: u64) -> u64 {
    mem_total.saturating_sub(mem_available)
}

/// The host's memory-used ratio as a percentage (0.0-100.0 when
/// `mem_total` > 0), guarded against a zero `mem_total` the same way
/// [`metrics_text`] is. Shared by the status-line MEM% text and the
/// memory-history ring buffer's per-sample push
/// (`docs/spec-telemetry-detail.md`).
pub fn mem_used_pct(mem_total: u64, mem_available: u64) -> f64 {
    if mem_total == 0 {
        0.0
    } else {
        mem_used_bytes(mem_total, mem_available) as f64 / mem_total as f64 * 100.0
    }
}

/// The `MEM <n>% \u{b7} CPU <n>%` host-resource segment text
/// (`docs/spec-host-telemetry.md`): both percentages integer-rounded, a
/// middot separator, literal `MEM`/`CPU` labels. Whether to render at all
/// (hidden before the first sample) is the caller's concern via
/// `StatusLineModel.host_metrics: Option<...>`, mirroring `cursor_text`.
fn metrics_text(cpu: f32, mem_total: u64, mem_available: u64) -> String {
    let mem_pct = mem_used_pct(mem_total, mem_available);
    format!(
        "MEM {}% \u{b7} CPU {}%",
        mem_pct.round() as i64,
        (cpu as f64).round() as i64
    )
}

/// The daemon filesystem's disk-used ratio as a percentage (0.0-100.0 when
/// `disk_total` > 0), guarded against a zero `disk_total` the same way
/// [`mem_used_pct`] is. Saturates so a degenerate sample (`disk_available >
/// disk_total`) never underflows. Shared by the status-line DISK segment.
pub fn disk_used_pct(disk_total: u64, disk_available: u64) -> f64 {
    if disk_total == 0 {
        0.0
    } else {
        disk_total.saturating_sub(disk_available) as f64 / disk_total as f64 * 100.0
    }
}

/// The `DISK <n>%` (used) status-line segment text
/// (`docs/spec-telemetry-detail.md`): integer-rounded, no threshold recolor
/// this phase (neutral-coloured — Phase 44 owns pressure). Whether to render
/// at all (hidden before the first sample) is the caller's concern via
/// `StatusLineModel.host_metrics: Option<...>`, mirroring [`metrics_text`].
fn disk_text(disk_total: u64, disk_available: u64) -> String {
    format!(
        "DISK {}%",
        disk_used_pct(disk_total, disk_available).round() as i64
    )
}

/// Map a memory-history ring buffer (oldest to newest, mem%-used values
/// expected in 0.0-100.0) onto `(x, y)` pixel offsets for the inline
/// sparkline (`docs/spec-telemetry-detail.md`): x spreads evenly across
/// `width`; y uses a fixed 0-100 scale — not autoscaled to the visible
/// window's min/max, so a flat line at, say, 50% always sits at mid-height —
/// and GPUI's top-left screen origin, so a *higher* percentage draws
/// *nearer the top*. A single sample maps to one point at `x = 0`; an empty
/// history yields no points.
pub fn sparkline_points(history: &[f32], width: f32, height: f32) -> Vec<(f32, f32)> {
    if history.is_empty() {
        return Vec::new();
    }
    let last_index = history.len() - 1;
    history
        .iter()
        .enumerate()
        .map(|(i, &value)| {
            let x = if last_index == 0 {
                0.0
            } else {
                width * i as f32 / last_index as f32
            };
            let pct = value.clamp(0.0, 100.0);
            let y = height - height * pct / 100.0;
            (x, y)
        })
        .collect()
}

/// Fixed size of the inline memory-history sparkline canvas
/// (`docs/spec-telemetry-detail.md`).
const SPARKLINE_WIDTH: f32 = 96.0;
const SPARKLINE_HEIGHT: f32 = 24.0;

/// Paint the inline memory-history sparkline as a stroked polyline
/// (`docs/spec-telemetry-detail.md`), mapped by [`sparkline_points`] within
/// the canvas element's own `bounds`. A no-op for an empty or single-sample
/// history — nothing to connect yet.
fn paint_sparkline(bounds: Bounds<Pixels>, history: &[f32], color: Hsla, window: &mut Window) {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    let mut points = sparkline_points(history, width, height).into_iter();
    let Some((x0, y0)) = points.next() else {
        return;
    };
    let mut builder = PathBuilder::stroke(px(1.5));
    builder.move_to(origin_point(px(x0), px(y0), bounds.origin));
    for (x, y) in points {
        builder.line_to(origin_point(px(x), px(y), bounds.origin));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Bytes as gigabytes with one decimal place, e.g. `15.6 GB`
/// (`docs/spec-telemetry-detail.md`'s memory-breakdown rows): binary GiB
/// scale (1024^3) — close enough for a detail card, not a precise byte count.
fn format_bytes_gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// The host uptime as a compact `<d>d <h>h <m>m` label, dropping leading
/// zero components (e.g. `3h 15m`, or `15m` under an hour) — the `uptime`
/// command's day/hour/minute granularity, without its seconds.
fn format_uptime(uptime_secs: u64) -> String {
    let minutes = uptime_secs / 60;
    let days = minutes / (24 * 60);
    let hours = (minutes / 60) % 24;
    let mins = minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

/// The `1 / 5 / 15` load-average label, two decimal places each — matching
/// `uptime`'s conventional load-average precision.
fn format_load(load: LoadAverage) -> String {
    format!("{:.2} / {:.2} / {:.2}", load.one, load.five, load.fifteen)
}

/// One label/value row in the host-detail hover card, label muted, value
/// on the foreground token.
fn detail_row(theme: &Theme, label: &'static str, value: String) -> impl IntoElement {
    h_flex()
        .justify_between()
        .gap(px(12.0))
        .child(div().text_color(theme.muted_foreground).child(label))
        .child(
            div()
                .text_color(theme.foreground)
                .child(SharedString::from(value)),
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

/// The host-detail hover card's content (`docs/spec-telemetry-detail.md`):
/// the memory breakdown (total/used/available/cached/buffers), swap, load
/// 1/5/15, uptime, and core count, plus the inline memory-history
/// sparkline. Independent of [`pane_metrics_popover_content`]'s per-pane
/// breakdown — this is host-global detail, not per-pane attribution.
/// Theme tokens only.
fn host_detail_content(sample: &HostMetrics, mem_history: &[f32], cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let used = mem_used_bytes(sample.mem_total, sample.mem_available);
    let stroke = theme.chart_1;
    let points = mem_history.to_vec();

    v_flex()
        .gap(px(6.0))
        .min_w(px(220.0))
        .child(detail_row(
            theme,
            "Memory total",
            format_bytes_gb(sample.mem_total),
        ))
        .child(detail_row(theme, "Used", format_bytes_gb(used)))
        .child(detail_row(
            theme,
            "Available",
            format_bytes_gb(sample.mem_available),
        ))
        .child(detail_row(
            theme,
            "Cached",
            format_bytes_gb(sample.mem_cached),
        ))
        .child(detail_row(
            theme,
            "Buffers",
            format_bytes_gb(sample.mem_buffers),
        ))
        .child(detail_row(
            theme,
            "Swap",
            format!(
                "{} / {}",
                format_bytes_gb(sample.swap_used),
                format_bytes_gb(sample.swap_total)
            ),
        ))
        .child(detail_row(theme, "Load (1/5/15)", format_load(sample.load)))
        .child(detail_row(
            theme,
            "Uptime",
            format_uptime(sample.uptime_secs),
        ))
        .child(detail_row(theme, "Cores", sample.cpu_count.to_string()))
        .child(
            div().w(px(SPARKLINE_WIDTH)).h(px(SPARKLINE_HEIGHT)).child(
                canvas(
                    move |_bounds, _window, _cx| points,
                    move |bounds, points, window, _cx| {
                        paint_sparkline(bounds, &points, stroke, window);
                    },
                )
                .size_full(),
            ),
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
                )
                .children(lsp_state_note(*state).map(|note| {
                    div()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(format!("({note})")))
                })),
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
    //
    // Host-detail hover card (`docs/spec-telemetry-detail.md`): an
    // independent surface from the click-driven popover above — hovering
    // the segment (rather than clicking it) shows the memory breakdown,
    // swap, load, uptime, cores, and the inline memory-history sparkline.
    // It wraps the popover as its trigger so both interactions share the
    // one `MEM % \u{b7} CPU %` segment: `HoverCard` only listens for hover on
    // its own wrapper div, so the inner popover's click handling still
    // reaches its button untouched.
    let metrics = model.host_metrics.map(|m| {
        let text = metrics_text(m.cpu, m.mem_total, m.mem_available);
        let rows = pane_metric_rows(model.pane_metrics);
        let enabled_tx = model.pane_metrics_enabled_tx.clone();
        let pane_metrics_popover = Popover::new("status-pane-metrics")
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
            .content(move |_state, _window, cx| pane_metrics_popover_content(&rows, cx));

        let sample = *m;
        let mem_history = model.mem_history.to_vec();
        HoverCard::new("status-host-detail")
            .anchor(Anchor::BottomRight)
            .trigger(pane_metrics_popover)
            .content(move |_state, _window, cx| host_detail_content(&sample, &mem_history, cx))
    });

    // Disk-headroom segment (`docs/spec-telemetry-detail.md`): beside the
    // MEM/CPU segment, reading the daemon-global `disk_total`/
    // `disk_available` fields. Hidden until the first sample arrives
    // (mirroring `metrics` above); neutral-coloured — no threshold recolor
    // this phase (Phase 44 owns pressure).
    let disk = model.host_metrics.map(|m| {
        let text = disk_text(m.disk_total, m.disk_available);
        div()
            .text_color(theme.muted_foreground)
            .child(SharedString::from(text))
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
        .children(disk)
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
/// starting = warning, crashed = danger. `NotInstalled` is deliberately
/// `muted_foreground`, not `danger` — it is informational ("nobody put this
/// server on the host"), not an alarming failure (`docs/spec-lsp-servers.md`
/// — graceful degradation).
fn lsp_state_color(state: LspServerState, cx: &App) -> gpui::Hsla {
    match state {
        LspServerState::Running => cx.theme().success,
        LspServerState::Starting => cx.theme().warning,
        LspServerState::Crashed => cx.theme().danger,
        LspServerState::NotInstalled => cx.theme().muted_foreground,
    }
}

/// A short explanatory note shown next to a language server's name in the
/// health line. Only `NotInstalled` carries one: the color dot alone tells
/// you *something's* off, but "not on $PATH" is what tells a user with
/// several languages configured that this one simply isn't installed on the
/// remote host, not that it crashed. `Running`/`Starting`/`Crashed` are
/// already distinguished by `lsp_state_color` and need no extra text.
fn lsp_state_note(state: LspServerState) -> Option<&'static str> {
    match state {
        LspServerState::NotInstalled => Some("not on $PATH"),
        LspServerState::Running | LspServerState::Starting | LspServerState::Crashed => None,
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
    fn test_lsp_state_note_not_installed_explains_missing_path_entry() {
        assert_eq!(
            lsp_state_note(LspServerState::NotInstalled),
            Some("not on $PATH")
        );
    }

    #[test]
    fn test_lsp_state_note_running_starting_crashed_have_no_note() {
        for state in [
            LspServerState::Running,
            LspServerState::Starting,
            LspServerState::Crashed,
        ] {
            assert_eq!(lsp_state_note(state), None);
        }
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
            mem_cached: 0,
            mem_buffers: 0,
            swap_total: 4_000_000_000,
            swap_used,
            load: LoadAverage {
                one: 0.0,
                five: 0.0,
                fifteen: 0.0,
            },
            cpu_count: 4,
            uptime_secs: 0,
            disk_total: 0,
            disk_available: 0,
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

    // --- mem_used_bytes / mem_used_pct (docs/spec-telemetry-detail.md) -------

    #[test]
    fn test_mem_used_bytes_subtracts_available_from_total() {
        assert_eq!(
            mem_used_bytes(16_000_000_000, 4_000_000_000),
            12_000_000_000
        );
    }

    #[test]
    fn test_mem_used_bytes_saturates_on_a_degenerate_sample() {
        // mem_available > mem_total should never happen, but must not underflow.
        assert_eq!(mem_used_bytes(1_000, 2_000), 0);
    }

    #[test]
    fn test_mem_used_pct_computes_used_ratio() {
        assert_eq!(mem_used_pct(16_000_000_000, 8_000_000_000), 50.0);
    }

    #[test]
    fn test_mem_used_pct_guards_against_zero_mem_total() {
        assert_eq!(mem_used_pct(0, 0), 0.0);
    }

    // --- disk-headroom segment (docs/spec-telemetry-detail.md) ---------------

    #[test]
    fn test_disk_used_pct_computes_used_ratio() {
        assert_eq!(disk_used_pct(500_000_000_000, 200_000_000_000), 60.0);
    }

    #[test]
    fn test_disk_used_pct_guards_against_zero_disk_total() {
        assert_eq!(disk_used_pct(0, 0), 0.0);
    }

    #[test]
    fn test_disk_used_pct_saturates_on_a_degenerate_sample() {
        // disk_available > disk_total should never happen, but must not underflow.
        assert_eq!(disk_used_pct(1_000, 2_000), 0.0);
    }

    #[test]
    fn test_disk_text_formats_used_percentage_rounded() {
        assert_eq!(disk_text(500_000_000_000, 200_000_000_000), "DISK 60%");
    }

    #[test]
    fn test_disk_text_guards_against_zero_disk_total() {
        assert_eq!(disk_text(0, 0), "DISK 0%");
    }

    // --- MemoryHistory ring buffer (docs/spec-telemetry-detail.md) -----------

    #[test]
    fn test_memory_history_push_accumulates_samples_oldest_first() {
        let mut history = MemoryHistory::new(5);
        history.push(10.0);
        history.push(20.0);
        history.push(30.0);
        assert_eq!(history.as_slice(), &[10.0, 20.0, 30.0]);
    }

    #[test]
    fn test_memory_history_push_evicts_oldest_once_at_capacity() {
        let mut history = MemoryHistory::new(3);
        for sample in [1.0, 2.0, 3.0, 4.0, 5.0] {
            history.push(sample);
        }
        assert_eq!(history.as_slice(), &[3.0, 4.0, 5.0]);
        assert_eq!(history.as_slice().len(), 3, "bounded to the capacity");
    }

    #[test]
    fn test_memory_history_new_clamps_zero_capacity_to_one() {
        let mut history = MemoryHistory::new(0);
        history.push(1.0);
        history.push(2.0);
        assert_eq!(history.as_slice(), &[2.0]);
    }

    #[test]
    fn test_memory_history_default_uses_the_documented_capacity() {
        let mut history = MemoryHistory::default();
        for i in 0..(MEMORY_HISTORY_CAPACITY + 10) {
            history.push(i as f32);
        }
        assert_eq!(history.as_slice().len(), MEMORY_HISTORY_CAPACITY);
    }

    // --- sparkline_points (docs/spec-telemetry-detail.md) --------------------

    #[test]
    fn test_sparkline_points_empty_history_yields_no_points() {
        assert!(sparkline_points(&[], 100.0, 20.0).is_empty());
    }

    #[test]
    fn test_sparkline_points_single_sample_maps_to_origin_x() {
        let points = sparkline_points(&[50.0], 100.0, 20.0);
        assert_eq!(points, vec![(0.0, 10.0)]);
    }

    #[test]
    fn test_sparkline_points_spreads_x_evenly_and_inverts_y_for_percentage() {
        let points = sparkline_points(&[0.0, 100.0], 100.0, 20.0);
        // 0% sits at the bottom (y = height); 100% sits at the top (y = 0);
        // x is spread from 0 to width across the two samples.
        assert_eq!(points, vec![(0.0, 20.0), (100.0, 0.0)]);
    }

    #[test]
    fn test_sparkline_points_clamps_out_of_range_values() {
        let points = sparkline_points(&[-10.0, 150.0], 10.0, 10.0);
        assert_eq!(points, vec![(0.0, 10.0), (10.0, 0.0)]);
    }

    // --- format_bytes_gb / format_uptime / format_load ------------------------

    #[test]
    fn test_format_bytes_gb_renders_one_decimal() {
        assert_eq!(format_bytes_gb(16_000_000_000), "14.9 GB");
    }

    #[test]
    fn test_format_bytes_gb_zero_bytes() {
        assert_eq!(format_bytes_gb(0), "0.0 GB");
    }

    #[test]
    fn test_format_uptime_under_an_hour_shows_minutes_only() {
        assert_eq!(format_uptime(15 * 60), "15m");
    }

    #[test]
    fn test_format_uptime_under_a_day_shows_hours_and_minutes() {
        assert_eq!(format_uptime(3 * 3600 + 15 * 60), "3h 15m");
    }

    #[test]
    fn test_format_uptime_over_a_day_shows_days_hours_and_minutes() {
        assert_eq!(format_uptime(2 * 86400 + 3 * 3600 + 15 * 60), "2d 3h 15m");
    }

    #[test]
    fn test_format_load_formats_two_decimals() {
        assert_eq!(
            format_load(LoadAverage {
                one: 1.5,
                five: 1.1234,
                fifteen: 0.9
            }),
            "1.50 / 1.12 / 0.90"
        );
    }
}
