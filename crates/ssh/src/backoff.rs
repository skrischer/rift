//! Jittered, capped exponential backoff for reconnect loops.
//!
//! The single backoff policy behind every reconnect engine
//! (`docs/spec-connection-robustness.md`): delays grow exponentially from
//! [`BASE_DELAY`] and saturate at [`MAX_DELAY`], with a small additive jitter
//! so independent retry loops never synchronize. Pure arithmetic lives in
//! free functions so the policy is testable without sleeping.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// First retry delay; doubles on every subsequent attempt.
const BASE_DELAY: Duration = Duration::from_millis(500);

/// Backoff ceiling (spec decision 2026-07-05: 30s cap) — growth saturates
/// here, so a long outage retries steadily instead of never.
const MAX_DELAY: Duration = Duration::from_secs(30);

/// Capped exponential backoff with additive jitter.
///
/// [`ReconnectBackoff::next_delay`] yields the wait before the *next* attempt
/// and advances the schedule; [`ReconnectBackoff::reset`] restarts it after a
/// successful reconnect so the next outage begins at [`BASE_DELAY`] again.
#[derive(Debug, Default)]
pub struct ReconnectBackoff {
    attempt: u32,
}

impl ReconnectBackoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// The delay to sleep before the next attempt: capped exponential growth
    /// plus jitter. Advances the internal attempt counter.
    pub fn next_delay(&mut self) -> Duration {
        let base = base_delay(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        jittered(base, entropy_seed())
    }

    /// Restart the schedule at [`BASE_DELAY`] (call after a successful
    /// reconnect, so the next outage backs off from the start again).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// The unjittered delay for the zero-based `attempt`: `BASE_DELAY * 2^attempt`,
/// saturating at [`MAX_DELAY`]. The shift is clamped so a large attempt count
/// can never overflow.
fn base_delay(attempt: u32) -> Duration {
    // 500ms << 6 = 32s already exceeds the cap, so clamping the exponent both
    // avoids overflow and changes nothing below the ceiling.
    let exp = attempt.min(6);
    BASE_DELAY.saturating_mul(1u32 << exp).min(MAX_DELAY)
}

/// Add seed-derived jitter of up to +25% of `base` — enough spread to keep
/// independent reconnect loops from retrying in lockstep, without a `rand`
/// dependency.
fn jittered(base: Duration, seed: u64) -> Duration {
    let span_ms = (base.as_millis() as u64 / 4).max(1);
    base + Duration::from_millis(seed % span_ms)
}

/// A cheap per-call jitter seed: the subsecond nanos of the wall clock. Not
/// cryptographic — it only has to differ between concurrent retry loops.
fn entropy_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_delay_first_attempt_returns_base() {
        assert_eq!(base_delay(0), BASE_DELAY);
    }

    #[test]
    fn test_base_delay_growth_doubles_per_attempt() {
        assert_eq!(base_delay(1), Duration::from_secs(1));
        assert_eq!(base_delay(2), Duration::from_secs(2));
        assert_eq!(base_delay(3), Duration::from_secs(4));
        assert_eq!(base_delay(4), Duration::from_secs(8));
        assert_eq!(base_delay(5), Duration::from_secs(16));
    }

    #[test]
    fn test_base_delay_large_attempt_saturates_at_cap() {
        assert_eq!(base_delay(6), MAX_DELAY);
        assert_eq!(base_delay(7), MAX_DELAY);
        assert_eq!(base_delay(u32::MAX), MAX_DELAY);
    }

    #[test]
    fn test_jittered_seed_zero_returns_base() {
        assert_eq!(jittered(BASE_DELAY, 0), BASE_DELAY);
    }

    #[test]
    fn test_jittered_any_seed_stays_within_quarter_of_base() {
        for seed in [1, 124, 125, 126, 999_999_937, u64::MAX] {
            let delay = jittered(BASE_DELAY, seed);
            assert!(delay >= BASE_DELAY, "seed {seed}: {delay:?} below base");
            assert!(
                delay < BASE_DELAY + BASE_DELAY / 4,
                "seed {seed}: {delay:?} exceeds base + 25%"
            );
        }
    }

    #[test]
    fn test_jittered_submillisecond_base_does_not_divide_by_zero() {
        let base = Duration::from_millis(2);
        let delay = jittered(base, u64::MAX);
        assert!(delay >= base);
        assert!(delay <= base + Duration::from_millis(1));
    }

    #[test]
    fn test_next_delay_advances_schedule_and_reset_restarts_it() {
        let mut backoff = ReconnectBackoff::new();
        let first = backoff.next_delay();
        let second = backoff.next_delay();
        assert!(first < Duration::from_secs(1), "first delay {first:?}");
        assert!(second >= Duration::from_secs(1), "second delay {second:?}");

        backoff.reset();
        let restarted = backoff.next_delay();
        assert!(
            restarted < Duration::from_secs(1),
            "post-reset delay {restarted:?}"
        );
    }

    #[test]
    fn test_next_delay_never_exceeds_cap_plus_jitter() {
        let mut backoff = ReconnectBackoff::new();
        for _ in 0..64 {
            let delay = backoff.next_delay();
            assert!(delay < MAX_DELAY + MAX_DELAY / 4, "delay {delay:?}");
        }
    }
}
