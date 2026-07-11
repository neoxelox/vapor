//! Optional advanced safeguards.
//!
//! Three small, self-contained protections the runtime consults on its
//! normal tick cadence:
//!
//! - [`ActiveCodingHeuristic`] — treats the user as actively
//!   working when code-class files churn rapidly, even on hosts where
//!   no permissioned HID-idle signal is available. The permissioned
//!   native signal stays the primary source (`MetricsSampler`
//!   `user_active`); this heuristic can only *add* activity, never
//!   clear it, so it strictly increases throttle caution.
//! - [`MassChangeGuard`] — a burst of local deletions above
//!   the configured rate looks like ransomware or an accidental
//!   recursive delete. Propagating it would faithfully replicate the
//!   damage to the cloud, so the guard pauses the daemon and raises a
//!   timeline alert instead; `vapor resume` is the explicit
//!   human-in-the-loop reset.
//! - [`intent_priority_rank`] — folder/path priority classes
//!   reuse the debounce classification so key config files and code
//!   flush ahead of lockfile noise when a single tick drains a mixed
//!   batch to the durable queue.
//!
//! The flush-boost half lives on `DaemonRuntime` (it touches
//! deferred-reconcile release and remote-poll cadence); the window
//! constant is shared from `constants::engine::FLUSH_BOOST_SECONDS`.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use vapor_shared::constants;

use crate::debounce::DebounceClass;

/// Rolling-window event counter: the shared mechanics behind both the
/// active-coding heuristic and the mass-deletion guard.
#[derive(Debug)]
struct RollingWindowCounter {
    window: Duration,
    /// Timestamps are pruned to the `[now - window, now]` band on every
    /// access. Dropping entries newer than `now` as well as older than the
    /// window keeps a backward wall-clock step (NTP correction) from
    /// leaving future-dated events inside the window, which would
    /// spuriously trip the guard.
    events: VecDeque<SystemTime>,
    cap: usize,
}

impl RollingWindowCounter {
    fn new(window: Duration, cap: usize) -> Self {
        Self {
            window,
            events: VecDeque::new(),
            cap,
        }
    }

    fn record(&mut self, now: SystemTime) {
        self.prune(now);
        if self.events.len() == self.cap {
            self.events.pop_front();
        }
        self.events.push_back(now);
    }

    fn count(&mut self, now: SystemTime) -> usize {
        self.prune(now);
        self.events.len()
    }

    fn prune(&mut self, now: SystemTime) {
        let cutoff = now
            .checked_sub(self.window)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        // Keep only the in-window band. `retain` (not a front pop loop)
        // because after a wall-clock rewind the deque is no longer sorted:
        // pre-rewind stamps are future-dated relative to `now` and must be
        // dropped even though they are not at the front.
        self.events
            .retain(|stamp| *stamp >= cutoff && *stamp <= now);
    }

    fn clear(&mut self) {
        self.events.clear();
    }
}

/// Heuristic active-coding detection.
///
/// Counts stabilized code/config-class events; at or above the
/// threshold inside the window the user is presumed to be actively
/// working, and the runtime ORs `user_active = true` into the throttle
/// inputs. Purely additive: it never overrides a positive native
/// activity signal with "idle".
#[derive(Debug)]
pub struct ActiveCodingHeuristic {
    counter: RollingWindowCounter,
    threshold: usize,
}

impl Default for ActiveCodingHeuristic {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(constants::engine::ACTIVE_CODING_WINDOW_SECONDS),
            constants::engine::ACTIVE_CODING_EVENT_THRESHOLD,
        )
    }
}

impl ActiveCodingHeuristic {
    pub fn new(window: Duration, threshold: usize) -> Self {
        // Cap the buffer just above the threshold: once tripped, older
        // history adds nothing, so memory stays O(threshold) no matter
        // how large the storm is.
        Self {
            counter: RollingWindowCounter::new(window, threshold.max(1).saturating_add(1)),
            threshold: threshold.max(1),
        }
    }

    /// Feed one stabilized event; only code-ish classes count.
    pub fn record_stabilized(&mut self, class: DebounceClass, now: SystemTime) {
        if matches!(class, DebounceClass::CodeText | DebounceClass::KeyConfig) {
            self.counter.record(now);
        }
    }

    pub fn is_active(&mut self, now: SystemTime) -> bool {
        self.counter.count(now) >= self.threshold
    }
}

/// Mass-change / ransomware guard.
///
/// Latches once tripped: the daemon stays paused (and the guard stays
/// reported) until an explicit `vapor resume`, which calls
/// [`MassChangeGuard::reset`]. Only *local* stabilized deletions that
/// survived self-write-echo suppression count — remote deletions the
/// engine applies locally are its own doing and never trip the guard.
#[derive(Debug)]
pub struct MassChangeGuard {
    counter: RollingWindowCounter,
    threshold: usize,
    tripped: bool,
}

impl Default for MassChangeGuard {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(constants::engine::MASS_DELETE_WINDOW_SECONDS),
            constants::engine::MASS_DELETE_THRESHOLD,
        )
    }
}

impl MassChangeGuard {
    pub fn new(window: Duration, threshold: usize) -> Self {
        Self {
            counter: RollingWindowCounter::new(window, threshold.max(1)),
            threshold: threshold.max(1),
            tripped: false,
        }
    }

    /// Records one local deletion; returns `true` exactly once, on the
    /// record that trips the guard (the caller pauses + alerts on that
    /// edge, not on every subsequent deletion).
    pub fn record_delete(&mut self, now: SystemTime) -> bool {
        self.counter.record(now);
        if self.tripped {
            return false;
        }
        if self.counter.count(now) >= self.threshold {
            self.tripped = true;
            return true;
        }
        false
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    /// Explicit human reset (`vapor resume`); the window restarts empty
    /// so an ongoing storm re-trips on fresh evidence only.
    pub fn reset(&mut self) {
        self.tripped = false;
        self.counter.clear();
    }
}

/// Folder/path priority classes for durable-queue flush order.
///
/// Lower rank flushes first. Reuses the debounce classification (the
/// same signal that already encodes "how urgent is a change to this
/// path"): key config files carry user intent, code/text is active
/// work, lockfiles are derived noise.
pub fn intent_priority_rank(class: DebounceClass) -> u8 {
    match class {
        DebounceClass::KeyConfig => 0,
        DebounceClass::CodeText => 1,
        DebounceClass::Other => 2,
        DebounceClass::Lockfile => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn coding_heuristic_trips_at_threshold_inside_window() {
        let mut heuristic = ActiveCodingHeuristic::new(Duration::from_secs(60), 3);
        heuristic.record_stabilized(DebounceClass::CodeText, at(100));
        heuristic.record_stabilized(DebounceClass::CodeText, at(110));
        assert!(!heuristic.is_active(at(110)));
        heuristic.record_stabilized(DebounceClass::KeyConfig, at(120));
        assert!(heuristic.is_active(at(120)));
    }

    #[test]
    fn coding_heuristic_ignores_non_code_classes() {
        let mut heuristic = ActiveCodingHeuristic::new(Duration::from_secs(60), 1);
        heuristic.record_stabilized(DebounceClass::Lockfile, at(100));
        heuristic.record_stabilized(DebounceClass::Other, at(100));
        assert!(!heuristic.is_active(at(100)));
    }

    #[test]
    fn coding_heuristic_decays_once_events_age_out_of_the_window() {
        let mut heuristic = ActiveCodingHeuristic::new(Duration::from_secs(60), 2);
        heuristic.record_stabilized(DebounceClass::CodeText, at(100));
        heuristic.record_stabilized(DebounceClass::CodeText, at(101));
        assert!(heuristic.is_active(at(101)));
        // 100/101 fall out of the 60s window by t=162.
        assert!(!heuristic.is_active(at(162)));
    }

    #[test]
    fn coding_heuristic_survives_wall_clock_rewind_without_tripping() {
        let mut heuristic = ActiveCodingHeuristic::new(Duration::from_secs(60), 3);
        heuristic.record_stabilized(DebounceClass::CodeText, at(100));
        heuristic.record_stabilized(DebounceClass::CodeText, at(100));
        // Clock rewinds before the window start: recorded events prune
        // (they are "in the future"), which fails safe to inactive.
        assert!(!heuristic.is_active(at(10)));
    }

    #[test]
    fn rolling_window_drops_future_dated_events_after_a_wall_clock_rewind() {
        // Threshold 3: two deletes recorded far in the future would remain
        // "inside the window" of a rewound clock without the fix, so a lone
        // post-rewind delete would spuriously trip the guard.
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 3);
        assert!(!guard.record_delete(at(100_000)));
        assert!(!guard.record_delete(at(100_001)));
        // NTP steps the clock back well before those events.
        assert!(
            !guard.record_delete(at(10)),
            "future-dated events must be pruned, not counted after a rewind"
        );
        assert!(!guard.is_tripped());
    }

    #[test]
    fn mass_change_guard_trips_exactly_once_and_latches() {
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 3);
        assert!(!guard.record_delete(at(100)));
        assert!(!guard.record_delete(at(101)));
        assert!(guard.record_delete(at(102)), "third delete trips");
        assert!(guard.is_tripped());
        // Latched: further deletions report no new edge.
        assert!(!guard.record_delete(at(103)));
        assert!(guard.is_tripped());
    }

    #[test]
    fn mass_change_guard_does_not_trip_on_slow_deletions() {
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 3);
        assert!(!guard.record_delete(at(100)));
        assert!(!guard.record_delete(at(200)));
        assert!(!guard.record_delete(at(300)));
        assert!(!guard.is_tripped(), "one delete per 100s is normal use");
    }

    #[test]
    fn mass_change_guard_reset_rearms_with_an_empty_window() {
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 2);
        guard.record_delete(at(100));
        assert!(guard.record_delete(at(101)));
        guard.reset();
        assert!(!guard.is_tripped());
        // Old evidence is gone; a single fresh delete does not re-trip.
        assert!(!guard.record_delete(at(102)));
        // But a fresh storm does.
        assert!(guard.record_delete(at(103)));
    }

    #[test]
    fn priority_ranks_order_config_before_code_before_noise() {
        assert!(
            intent_priority_rank(DebounceClass::KeyConfig)
                < intent_priority_rank(DebounceClass::CodeText)
        );
        assert!(
            intent_priority_rank(DebounceClass::CodeText)
                < intent_priority_rank(DebounceClass::Other)
        );
        assert!(
            intent_priority_rank(DebounceClass::Other)
                < intent_priority_rank(DebounceClass::Lockfile)
        );
    }
}
