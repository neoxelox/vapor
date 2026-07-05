//! `CrashLoopGuard` — pure-logic crash-loop backoff policy.
//!
//! This is the only implementation: the Swift copy retired with M2-2 /
//! C4-7, and every surface consumes this guard through the `vapor
//! service` CLI. Defaults: `base_delay = 2s`, `max_delay = 120s`,
//! `delay_starts_after_failures = 1`,
//! `max_consecutive_failures_before_pause = 5`, `failure_window = 600s`.
//!
//! The canonical schedule with the default policy — the contract locked
//! in by `core/lifecycle/tests/crash_loop_parity.rs`, per
//! `docs/operations/macos/launchagent-policy.md`:
//!
//! > crash 1 → restart immediately (`NoDelay`), crash 2 → 2 s,
//! > crash 3 → 4 s, crash 4 → 8 s, crash 5 → `Paused`.
//!
//! `delay_starts_after_failures = N` means the first N crashes within
//! the window restart without delay; crash N+1 gets `base_delay`,
//! doubling per crash after that.
//!
//! Closes `core.md` C4-2.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrashLoopPolicy {
    /// Sliding window inside which crashes accumulate. Crashes outside
    /// this window are pruned and do not contribute to backoff.
    pub failure_window: Duration,
    /// Initial backoff delay applied after the
    /// `delay_starts_after_failures`-th crash.
    pub base_delay: Duration,
    /// Upper bound on the per-crash backoff.
    pub max_delay: Duration,
    /// Number of free-passes before backoff kicks in. Default `1` means
    /// the first crash within the window is "no delay" and the second
    /// triggers `base_delay`.
    pub delay_starts_after_failures: u32,
    /// Number of consecutive crashes within `failure_window` after which
    /// the guard transitions to `Paused` and refuses to schedule any
    /// further restart until the user acknowledges.
    pub max_consecutive_failures_before_pause: u32,
}

impl CrashLoopPolicy {
    pub fn new(
        failure_window: Duration,
        base_delay: Duration,
        max_delay: Duration,
        delay_starts_after_failures: u32,
        max_consecutive_failures_before_pause: u32,
    ) -> Self {
        Self {
            failure_window,
            base_delay,
            max_delay,
            delay_starts_after_failures: delay_starts_after_failures.max(1),
            max_consecutive_failures_before_pause: max_consecutive_failures_before_pause.max(1),
        }
    }
}

impl Default for CrashLoopPolicy {
    fn default() -> Self {
        // Defaults match Swift's `CrashLoopPolicy.default` 1:1.
        Self::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(120),
            1,
            5,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrashLoopDecision {
    /// First crash within the window — restart immediately.
    NoDelay,
    /// Wait at least `Duration` before the next restart attempt.
    Backoff(Duration),
    /// Auto-restart is suspended until the user acknowledges via
    /// `acknowledge_and_resume`.
    Paused,
}

#[derive(Clone, Debug)]
pub struct CrashLoopGuard {
    policy: CrashLoopPolicy,
    failure_moments: Vec<Instant>,
    paused_until: Option<Instant>,
    paused_indefinitely: bool,
}

impl CrashLoopGuard {
    pub fn new(policy: CrashLoopPolicy) -> Self {
        Self {
            policy,
            failure_moments: Vec::new(),
            paused_until: None,
            paused_indefinitely: false,
        }
    }

    /// Rebuilds a guard from durably persisted state (M2-6).
    /// `last_crash_elapsed` is how long before `now` the most recent
    /// crash happened. Reconstruction replays `consecutive_crashes`
    /// registrations at that moment, so the backoff deadline and pause
    /// flag are derived by the same code path that produced them
    /// originally. All persisted crashes collapse onto the last crash
    /// instant — they age out of the failure window together, which is
    /// the conservative direction: a restart never shortens a backoff
    /// that was already in force.
    pub fn restore(
        policy: CrashLoopPolicy,
        consecutive_crashes: u32,
        last_crash_elapsed: Duration,
        paused_indefinitely: bool,
        now: Instant,
    ) -> Self {
        let mut guard = Self::new(policy);
        let crash_instant = now.checked_sub(last_crash_elapsed).unwrap_or(now);
        for _ in 0..consecutive_crashes {
            let _ = guard.register_crash(crash_instant);
        }
        // Trust the persisted pause flag even when the replay did not
        // reproduce it (e.g. the pause threshold was lowered between
        // builds): only user acknowledgement may clear a pause.
        if paused_indefinitely {
            guard.paused_indefinitely = true;
            guard.paused_until = None;
        }
        guard
    }

    pub fn policy(&self) -> CrashLoopPolicy {
        self.policy
    }

    pub fn is_paused_indefinitely(&self) -> bool {
        self.paused_indefinitely
    }

    /// Number of crashes currently inside the failure window at `now`.
    pub fn consecutive_crashes(&mut self, now: Instant) -> u32 {
        self.prune_failures(now);
        self.failure_moments.len() as u32
    }

    /// Records a crash at `now` and returns the resulting backoff
    /// decision. Mirrors `CrashLoopGuard.registerCrash(at:)` in Swift.
    pub fn register_crash(&mut self, now: Instant) -> CrashLoopDecision {
        self.prune_failures(now);
        self.failure_moments.push(now);

        if self.failure_moments.len() as u32 >= self.policy.max_consecutive_failures_before_pause {
            self.paused_indefinitely = true;
            self.paused_until = None;
            return CrashLoopDecision::Paused;
        }

        let count = self.failure_moments.len() as u32;
        if count <= self.policy.delay_starts_after_failures {
            return CrashLoopDecision::NoDelay;
        }

        let exponent = count - self.policy.delay_starts_after_failures;
        let exponent = exponent.saturating_sub(1);
        let delay = exponential_backoff(self.policy.base_delay, self.policy.max_delay, exponent);
        self.paused_until = Some(now + delay);
        CrashLoopDecision::Backoff(delay)
    }

    /// How much of the current backoff window remains at `now`. Returns
    /// `Duration::ZERO` when no backoff is active. Returns
    /// `Duration::MAX` when the guard is in `CrashLoopPaused`.
    pub fn remaining_delay(&mut self, now: Instant) -> Duration {
        if self.paused_indefinitely {
            return Duration::MAX;
        }

        let Some(paused_until) = self.paused_until else {
            return Duration::ZERO;
        };

        if now >= paused_until {
            self.paused_until = None;
            Duration::ZERO
        } else {
            paused_until - now
        }
    }

    /// User-driven exit from the paused state. Mirrors
    /// `acknowledgeAndResume()` in Swift.
    pub fn acknowledge_and_resume(&mut self) {
        self.paused_indefinitely = false;
        self.failure_moments.clear();
        self.paused_until = None;
    }

    pub fn reset(&mut self) {
        self.failure_moments.clear();
        self.paused_until = None;
        self.paused_indefinitely = false;
    }

    fn prune_failures(&mut self, now: Instant) {
        let oldest_allowed = now.checked_sub(self.policy.failure_window);
        let Some(oldest_allowed) = oldest_allowed else {
            // The clock hasn't advanced past the window yet (very early
            // boot). Treat every recorded crash as still in window.
            return;
        };
        self.failure_moments
            .retain(|moment| *moment >= oldest_allowed);
    }
}

fn exponential_backoff(base: Duration, max: Duration, exponent: u32) -> Duration {
    // Cap the exponent so we never overflow the multiplier. The default
    // policy lands at 2^4 = 16x at most before the pause kicks in, so
    // the cap is conservative.
    let capped_exponent = exponent.min(20);
    let factor: u64 = 1u64 << capped_exponent;
    let scaled = base.checked_mul(factor as u32).unwrap_or(max);
    if scaled > max { max } else { scaled }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_policy() -> CrashLoopPolicy {
        CrashLoopPolicy::new(
            Duration::from_secs(60),
            Duration::from_secs(2),
            Duration::from_secs(32),
            2,
            10,
        )
    }

    #[test]
    fn default_policy_matches_swift_baseline() {
        // C4-2 verbatim port: `CrashLoopPolicy.default` in Swift is
        // (600 s, 2 s, 120 s, 1, 5). Drift here means surfaces would
        // disagree on backoff, so guard the constants explicitly.
        let policy = CrashLoopPolicy::default();
        assert_eq!(policy.failure_window, Duration::from_secs(600));
        assert_eq!(policy.base_delay, Duration::from_secs(2));
        assert_eq!(policy.max_delay, Duration::from_secs(120));
        assert_eq!(policy.delay_starts_after_failures, 1);
        assert_eq!(policy.max_consecutive_failures_before_pause, 5);
    }

    #[test]
    fn first_crash_is_no_delay_then_subsequent_crashes_backoff_exponentially() {
        let mut guard = CrashLoopGuard::new(fixed_policy());
        let t0 = Instant::now();
        assert_eq!(guard.register_crash(t0), CrashLoopDecision::NoDelay);
        assert_eq!(
            guard.register_crash(t0 + Duration::from_secs(1)),
            CrashLoopDecision::NoDelay
        );
        assert_eq!(
            guard.register_crash(t0 + Duration::from_secs(2)),
            CrashLoopDecision::Backoff(Duration::from_secs(2))
        );
        assert_eq!(
            guard.register_crash(t0 + Duration::from_secs(3)),
            CrashLoopDecision::Backoff(Duration::from_secs(4))
        );
        assert_eq!(
            guard.register_crash(t0 + Duration::from_secs(4)),
            CrashLoopDecision::Backoff(Duration::from_secs(8))
        );
    }

    #[test]
    fn backoff_is_capped_at_max_delay() {
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(8),
            1,
            20,
        );
        let mut guard = CrashLoopGuard::new(policy);
        let t0 = Instant::now();
        guard.register_crash(t0); // NoDelay
        for step in 1..6 {
            let decision = guard.register_crash(t0 + Duration::from_secs(step));
            if let CrashLoopDecision::Backoff(delay) = decision {
                assert!(delay <= Duration::from_secs(8));
            }
        }
    }

    #[test]
    fn pause_engages_after_max_consecutive_failures() {
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(120),
            1,
            3,
        );
        let mut guard = CrashLoopGuard::new(policy);
        let t0 = Instant::now();
        let _ = guard.register_crash(t0);
        let _ = guard.register_crash(t0 + Duration::from_secs(1));
        let decision = guard.register_crash(t0 + Duration::from_secs(2));
        assert_eq!(decision, CrashLoopDecision::Paused);
        assert!(guard.is_paused_indefinitely());
        // While paused, `remaining_delay` is `Duration::MAX` (the Rust
        // analogue of Swift's `.infinity`).
        assert_eq!(
            guard.remaining_delay(t0 + Duration::from_secs(3_600)),
            Duration::MAX
        );
    }

    #[test]
    fn acknowledge_and_resume_clears_pause_and_failure_history() {
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(120),
            1,
            3,
        );
        let mut guard = CrashLoopGuard::new(policy);
        let t0 = Instant::now();
        let _ = guard.register_crash(t0);
        let _ = guard.register_crash(t0 + Duration::from_secs(1));
        let _ = guard.register_crash(t0 + Duration::from_secs(2));
        assert!(guard.is_paused_indefinitely());
        guard.acknowledge_and_resume();
        assert!(!guard.is_paused_indefinitely());
        assert_eq!(
            guard.remaining_delay(t0 + Duration::from_secs(3_600)),
            Duration::ZERO
        );
    }

    #[test]
    fn crashes_outside_failure_window_are_pruned() {
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(10),
            Duration::from_secs(2),
            Duration::from_secs(30),
            2,
            10,
        );
        let mut guard = CrashLoopGuard::new(policy);
        let t0 = Instant::now();
        assert_eq!(guard.register_crash(t0), CrashLoopDecision::NoDelay);
        assert_eq!(
            guard.register_crash(t0 + Duration::from_secs(1)),
            CrashLoopDecision::NoDelay
        );
        // 20 seconds later — both prior crashes are outside the 10 s
        // window. The new crash starts the count fresh.
        assert_eq!(
            guard.register_crash(t0 + Duration::from_secs(20)),
            CrashLoopDecision::NoDelay
        );
    }

    #[test]
    fn restore_replays_persisted_crashes_and_reinstates_backoff() {
        // 3 crashes with the fixed policy → Backoff(2 s) measured from
        // the last crash. Restoring 1 s after that crash must leave 1 s
        // of the window in force.
        let now = Instant::now();
        let mut guard =
            CrashLoopGuard::restore(fixed_policy(), 3, Duration::from_secs(1), false, now);
        assert_eq!(guard.consecutive_crashes(now), 3);
        assert_eq!(guard.remaining_delay(now), Duration::from_secs(1));
        assert_eq!(
            guard.remaining_delay(now + Duration::from_secs(2)),
            Duration::ZERO
        );
    }

    #[test]
    fn restore_with_elapsed_backoff_leaves_no_delay() {
        let now = Instant::now();
        let mut guard =
            CrashLoopGuard::restore(fixed_policy(), 3, Duration::from_secs(30), false, now);
        assert_eq!(guard.remaining_delay(now), Duration::ZERO);
    }

    #[test]
    fn restore_honors_persisted_pause_flag_even_without_replayed_pause() {
        let now = Instant::now();
        let guard = CrashLoopGuard::restore(
            fixed_policy(),
            2, // below the pause threshold of the policy
            Duration::from_secs(1),
            true,
            now,
        );
        assert!(guard.is_paused_indefinitely());
    }

    #[test]
    fn restore_reaches_pause_when_persisted_count_hits_the_threshold() {
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(120),
            1,
            3,
        );
        let guard =
            CrashLoopGuard::restore(policy, 3, Duration::from_secs(1), false, Instant::now());
        assert!(guard.is_paused_indefinitely());
    }

    #[test]
    fn restored_crashes_age_out_of_the_failure_window_together() {
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(10),
            Duration::from_secs(2),
            Duration::from_secs(30),
            2,
            10,
        );
        let now = Instant::now();
        let mut guard = CrashLoopGuard::restore(policy, 4, Duration::from_secs(1), false, now);
        assert_eq!(guard.consecutive_crashes(now), 4);
        // 15 s later the collapsed crash instant is outside the 10 s
        // window, so the whole history expires at once.
        assert_eq!(guard.consecutive_crashes(now + Duration::from_secs(15)), 0);
    }

    #[test]
    fn remaining_delay_drops_to_zero_after_backoff_window() {
        let mut guard = CrashLoopGuard::new(fixed_policy());
        let t0 = Instant::now();
        guard.register_crash(t0);
        guard.register_crash(t0 + Duration::from_secs(1));
        let _ = guard.register_crash(t0 + Duration::from_secs(2)); // Backoff(2)
        // Inside the window
        let remaining = guard.remaining_delay(t0 + Duration::from_secs(3));
        assert_eq!(remaining, Duration::from_secs(1));
        // Past the window
        let remaining = guard.remaining_delay(t0 + Duration::from_secs(10));
        assert_eq!(remaining, Duration::ZERO);
    }
}
