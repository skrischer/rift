//! Working/idle classifier for a non-shell Busy pane's per-pane CPU
//! (`docs/spec-agent-activity.md`, issue #953): a threshold + hysteresis
//! (minimum dwell) state machine over the daemon's on-demand `PaneMetrics`
//! CPU samples (`docs/spec-pane-attribution.md`, #880/#881). Pure and
//! `gpui`-free so the threshold/hysteresis behavior is unit-tested directly
//! against a bare CPU sample series; `workspace.rs` wires one instance per
//! pane and drives [`rift_terminal::PaneView::set_work_state`] (via
//! `SessionView::set_pane_work_state`) from its output.
//!
//! Strictly agent-agnostic: the only input is a pane's `/proc` subtree CPU
//! percentage — no output parsing, no capture-pane content, no agent
//! detection.

use rift_terminal::WorkState;

/// CPU% (0.0-100.0 times the core count, [`rift_protocol::PaneMetric::cpu`]'s
/// scale) at/above which a pane's subtree reads as actively working; below
/// this it reads idle-awaiting-input. `docs/spec-agent-activity.md` leaves
/// the concrete numbers unpinned ("tuned to avoid flicker at the boundary"),
/// so this is an implementation default — tunable later without a spec
/// change, mirroring the `docs/archive/spec-memory-pressure.md` threshold
/// precedent. 5% of a core comfortably separates an agent idling at its
/// input prompt (~0%) from any real computation.
const WORKING_CPU_THRESHOLD: f32 = 5.0;

/// Consecutive `PaneMetrics` samples a pane's CPU must sit on the other side
/// of [`WORKING_CPU_THRESHOLD`] before [`CpuWorkClassifier::observe`] flips
/// its classified state — the hysteresis/minimum-dwell that keeps a pane
/// hovering at the boundary from flickering every ~2s sampler tick
/// (`PANE_METRICS_INTERVAL` in `crates/daemon/src/lib.rs`). A single noisy
/// sample never flips the state; two in a row (~4s, the settling window)
/// does.
const SETTLING_SAMPLES: u8 = 2;

/// Per-pane working/idle classifier state, fed one CPU sample per
/// `PaneMetrics` push. Defaults to [`WorkState::Working`], matching
/// [`WorkState::default`] — a freshly-busy pane, or one this classifier has
/// never observed a sample for, reads working until real samples say
/// otherwise (today's pre-#953 behavior).
#[derive(Debug, Clone, Copy)]
pub struct CpuWorkClassifier {
    state: WorkState,
    /// Consecutive samples observed on the side opposite `state`, reset to 0
    /// on any sample that agrees with `state`.
    pending_streak: u8,
}

impl Default for CpuWorkClassifier {
    fn default() -> Self {
        Self {
            state: WorkState::Working,
            pending_streak: 0,
        }
    }
}

impl CpuWorkClassifier {
    /// Feed one CPU sample and return the classifier's current state.
    /// `state` only flips once [`SETTLING_SAMPLES`] consecutive samples land
    /// on the other side of [`WORKING_CPU_THRESHOLD`] — a single sample
    /// crossing the boundary is recorded but never flips the reported state
    /// on its own, so a momentary dip/spike does not flicker the
    /// classification.
    pub fn observe(&mut self, cpu: f32) -> WorkState {
        let candidate = if cpu >= WORKING_CPU_THRESHOLD {
            WorkState::Working
        } else {
            WorkState::Idle
        };
        if candidate == self.state {
            self.pending_streak = 0;
        } else {
            self.pending_streak = self.pending_streak.saturating_add(1);
            if self.pending_streak >= SETTLING_SAMPLES {
                self.state = candidate;
                self.pending_streak = 0;
            }
        }
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_state_is_working() {
        assert_eq!(CpuWorkClassifier::default().state, WorkState::Working);
    }

    #[test]
    fn test_observe_sustained_high_cpu_stays_working() {
        let mut classifier = CpuWorkClassifier::default();
        for cpu in [80.0, 95.0, 60.0, 12.0] {
            assert_eq!(classifier.observe(cpu), WorkState::Working);
        }
    }

    #[test]
    fn test_observe_low_cpu_flips_to_idle_only_after_settling_samples() {
        let mut classifier = CpuWorkClassifier::default();
        // Fewer than SETTLING_SAMPLES low samples: still Working.
        assert_eq!(classifier.observe(0.0), WorkState::Working);
        // The settling-th consecutive low sample flips it.
        assert_eq!(classifier.observe(0.5), WorkState::Idle);
    }

    #[test]
    fn test_observe_single_low_dip_does_not_flicker() {
        let mut classifier = CpuWorkClassifier::default();
        assert_eq!(classifier.observe(90.0), WorkState::Working);
        // One low sample under the settling window...
        assert_eq!(classifier.observe(0.0), WorkState::Working);
        // ...followed by a high sample resets the streak: never flips.
        assert_eq!(classifier.observe(90.0), WorkState::Working);
        assert_eq!(classifier.observe(0.0), WorkState::Working);
    }

    #[test]
    fn test_observe_idle_to_working_requires_settling_samples() {
        let mut classifier = CpuWorkClassifier::default();
        classifier.observe(0.0);
        assert_eq!(classifier.observe(0.0), WorkState::Idle);
        // Fewer than SETTLING_SAMPLES high samples: still Idle.
        assert_eq!(classifier.observe(50.0), WorkState::Idle);
        // The settling-th consecutive high sample flips it back.
        assert_eq!(classifier.observe(50.0), WorkState::Working);
    }

    #[test]
    fn test_observe_boundary_value_counts_as_working() {
        let mut classifier = CpuWorkClassifier::default();
        // Exactly at the threshold is inclusive on the working side.
        assert_eq!(
            classifier.observe(WORKING_CPU_THRESHOLD),
            WorkState::Working
        );
    }

    #[test]
    fn test_observe_just_below_threshold_settles_to_idle() {
        let mut classifier = CpuWorkClassifier::default();
        classifier.observe(WORKING_CPU_THRESHOLD - 0.01);
        assert_eq!(
            classifier.observe(WORKING_CPU_THRESHOLD - 0.01),
            WorkState::Idle
        );
    }
}
