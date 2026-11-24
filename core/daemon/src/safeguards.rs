//! Optional advanced safeguards.
//!
//! Two small, self-contained protections the runtime consults on its
//! normal tick cadence:
//!
//! - [`MassChangeGuard`]: a burst of deletions above the configured
//!   rate looks like ransomware or an accidental recursive delete.
//!   Propagating it would copy the damage to the other side, so the
//!   guard holds the whole batch behind a `mass-delete` decision. The
//!   rest of the profile keeps syncing, and the user's answer lets the
//!   deletions through or puts the files back.
//! - [`intent_priority_rank`]: folder/path priority classes reuse the
//!   debounce classification so key config files and code flush ahead
//!   of lockfile noise when a single tick drains a mixed batch to the
//!   durable queue.
//!
//! The reconcile boost lives on `DaemonRuntime` (it touches
//! deferred-reconcile release and remote-poll cadence); its window is
//! `constants::engine::RECONCILE_BOOST_SECONDS`.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use vapor_shared::constants;

use crate::debounce::DebounceClass;

/// Rolling-window event counter: the mechanics of the mass-delete
/// guard.
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

/// Resolved mass-delete guard tuning: the `safeguards` config group
/// with floors applied. Configurable because a workflow that
/// legitimately unlinks many files (`rm -rf` of large trees, build
/// cleans) may need a higher threshold; the defaults stay conservative:
/// this guard is the ransomware / bulk-mistake backstop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MassDeleteGuardSettings {
    pub enabled: bool,
    /// Absolute count inside the window that always holds.
    pub threshold: usize,
    pub window: Duration,
    /// Share of the synced files (percent) inside the window that holds
    /// too, so a small tree is protected before the absolute count.
    pub ratio_percent: u8,
}

impl Default for MassDeleteGuardSettings {
    fn default() -> Self {
        Self {
            enabled: constants::safeguards::DEFAULT_MASS_DELETE_ENABLED,
            threshold: constants::safeguards::DEFAULT_MASS_DELETE_THRESHOLD,
            window: Duration::from_secs(constants::safeguards::DEFAULT_MASS_DELETE_WINDOW_SECONDS),
            ratio_percent: constants::safeguards::DEFAULT_MASS_DELETE_RATIO_PERCENT,
        }
    }
}

impl MassDeleteGuardSettings {
    /// Resolves the configured values, clamping to the floors with a
    /// logged warning (a too-low threshold would trip on ordinary work
    /// and train users to blind-approve the guard).
    pub fn resolve(config: &vapor_shared::config::VaporConfig) -> Self {
        let configured = &config.safeguards.mass_delete;
        let threshold = usize::try_from(configured.threshold).unwrap_or(usize::MAX);
        let clamped_threshold = threshold.max(constants::safeguards::MIN_MASS_DELETE_THRESHOLD);
        let clamped_window = configured
            .window_seconds
            .max(constants::safeguards::MIN_MASS_DELETE_WINDOW_SECONDS);
        if clamped_threshold != threshold || clamped_window != configured.window_seconds {
            crate::logging::warning(
                "Clamped safeguards.massDelete values to their floors",
                &[
                    ("configured_threshold", threshold.to_string()),
                    ("effective_threshold", clamped_threshold.to_string()),
                    (
                        "configured_window_seconds",
                        configured.window_seconds.to_string(),
                    ),
                    ("effective_window_seconds", clamped_window.to_string()),
                ],
            );
        }
        if !configured.enabled {
            crate::logging::warning(
                "The mass-delete guard is disabled by configuration; bulk deletes will replicate without a decision",
                &[],
            );
        }
        Self {
            enabled: configured.enabled,
            threshold: clamped_threshold,
            window: Duration::from_secs(clamped_window),
            ratio_percent: configured.ratio_percent.min(100),
        }
    }
}

/// Mass-change / ransomware guard.
///
/// Counts deletions in both directions (a local delete about to remove
/// a cloud object, a cloud delete about to remove a local file) at the
/// moment the executor would make them irreversible. Once tripped it
/// stays tripped, and every further deletion inside the window joins
/// the held batch, until the decision that holds the batch is answered
/// and the guard is reset. Deletions that turn out to be no-ops (the
/// other side is already gone) never reach the guard.
#[derive(Debug)]
pub struct MassChangeGuard {
    counter: RollingWindowCounter,
    threshold: usize,
    ratio_percent: u8,
    tripped: bool,
}

impl Default for MassChangeGuard {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(constants::safeguards::DEFAULT_MASS_DELETE_WINDOW_SECONDS),
            constants::safeguards::DEFAULT_MASS_DELETE_THRESHOLD,
        )
    }
}

impl MassChangeGuard {
    pub fn new(window: Duration, threshold: usize) -> Self {
        Self::with_ratio(
            window,
            threshold,
            constants::safeguards::DEFAULT_MASS_DELETE_RATIO_PERCENT,
        )
    }

    pub fn with_ratio(window: Duration, threshold: usize, ratio_percent: u8) -> Self {
        Self {
            counter: RollingWindowCounter::new(window, threshold.max(1)),
            threshold: threshold.max(1),
            ratio_percent: ratio_percent.min(100),
            tripped: false,
        }
    }

    /// Records one deletion about to become irreversible; returns
    /// `true` when it must be held: on the record that trips the guard
    /// and on every record while the guard stays tripped.
    /// `synced_files` is the size of the synced tree the ratio rule
    /// measures against. `queued_behind` is the number of deletions in
    /// the same direction still waiting in the queue: a burst is judged
    /// as a whole, so the first deletion of a large batch is held
    /// before any of it lands, instead of the batch propagating up to
    /// the threshold and only the tail being held.
    pub fn record_delete(
        &mut self,
        now: SystemTime,
        synced_files: usize,
        queued_behind: usize,
    ) -> bool {
        self.counter.record(now);
        if self.tripped {
            return true;
        }
        let count = self.counter.count(now).saturating_add(queued_behind);
        let over_threshold = count >= self.threshold;
        let over_ratio = self.ratio_percent > 0
            && count >= constants::safeguards::MIN_MASS_DELETE_RATIO_COUNT
            && count.saturating_mul(100)
                >= synced_files.saturating_mul(usize::from(self.ratio_percent));
        if over_threshold || over_ratio {
            self.tripped = true;
            return true;
        }
        false
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    /// Records one deletion that is held because the question of an
    /// earlier burst is still open. The guard counts as tripped until
    /// that question is answered, also in a daemon that started after
    /// the question was asked.
    pub fn hold_while_open(&mut self, now: SystemTime) {
        self.counter.record(now);
        self.tripped = true;
    }

    /// The count inside the window right now.
    pub fn count(&mut self, now: SystemTime) -> usize {
        self.counter.count(now)
    }

    /// Reset after the holding decision is answered; the window
    /// restarts empty so an ongoing storm re-trips on fresh evidence
    /// only.
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
        DebounceClass::Document => 2,
        DebounceClass::Other => 3,
        DebounceClass::Lockfile => 4,
    }
}

/// Where a durable queue intent came from, for lease-priority
/// purposes. Reconcile-walk backlog ranks below every fresh intent so
/// a whole-scope reconcile of a large tree can never starve a file the
/// user just edited: background convergence yields to foreground
/// changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntentSource {
    /// Debounced local edits, remote-change polls, conflict follow-ups.
    Fresh,
    /// Comparison-walk output of a whole-scope/subtree reconcile.
    ReconcileBacklog,
}

/// Rank for `ReconcileSubtree` control intents: first. Leasing one is
/// cheap (it only hands the walk to the reconcile controller, which
/// itself defers until Idle) and the startup reconstruction
/// barrier depends on the whole-scope reconcile leasing ahead of any
/// recovered file intents.
pub const RECONCILE_INTENT_PRIORITY_RANK: u8 = 0;

/// Fresh file intents rank `1 + class` (1..=5), preserving the
/// key-config-before-lockfile order within fresh work.
pub const FRESH_INTENT_PRIORITY_RANK_BASE: u8 = 1;

/// Reconcile-backlog file intents rank below every fresh class.
pub const BACKLOG_INTENT_PRIORITY_RANK: u8 = 6;

/// Durable lease-order priority for one queue row (lower leases
/// first): reconcile control intents, then fresh file intents by the
/// path's debounce class, then reconcile-backlog file intents.
pub fn durable_intent_priority_rank(
    path: &std::path::Path,
    kind: crate::event_intents::PendingIntentKind,
    source: IntentSource,
) -> u8 {
    if kind == crate::event_intents::PendingIntentKind::ReconcileSubtree {
        return RECONCILE_INTENT_PRIORITY_RANK;
    }
    if source == IntentSource::ReconcileBacklog {
        return BACKLOG_INTENT_PRIORITY_RANK;
    }
    FRESH_INTENT_PRIORITY_RANK_BASE
        + intent_priority_rank(
            crate::debounce::DebounceWindows::default()
                .classify_path(path)
                .0,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn rolling_window_drops_future_dated_events_after_a_wall_clock_rewind() {
        // Threshold 3: two deletes recorded far in the future would remain
        // "inside the window" of a rewound clock without the fix, so a lone
        // post-rewind delete would spuriously trip the guard.
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 3);
        assert!(!guard.record_delete(at(100_000), 1_000_000, 0));
        assert!(!guard.record_delete(at(100_001), 1_000_000, 0));
        // NTP steps the clock back well before those events.
        assert!(
            !guard.record_delete(at(10), 1_000_000, 0),
            "future-dated events must be pruned, not counted after a rewind"
        );
        assert!(!guard.is_tripped());
    }

    #[test]
    fn mass_change_guard_trips_and_holds_every_deletion_while_tripped() {
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 3);
        assert!(!guard.record_delete(at(100), 1_000_000, 0));
        assert!(!guard.record_delete(at(101), 1_000_000, 0));
        assert!(
            guard.record_delete(at(102), 1_000_000, 0),
            "third delete trips"
        );
        assert!(guard.is_tripped());
        // Latched: further deletions join the held batch.
        assert!(guard.record_delete(at(103), 1_000_000, 0));
        assert!(guard.is_tripped());
    }

    #[test]
    fn mass_change_guard_judges_a_queued_batch_as_a_whole() {
        // Twelve deletions of a forty-file tree land in the queue at
        // once: the first one is held, none of them propagates.
        let mut guard = MassChangeGuard::with_ratio(Duration::from_secs(60), 1_000, 25);
        assert!(
            guard.record_delete(at(100), 40, 11),
            "the first of twelve is held"
        );
        // A lone deletion with nothing behind it is not a burst.
        let mut lone = MassChangeGuard::with_ratio(Duration::from_secs(60), 1_000, 25);
        assert!(!lone.record_delete(at(100), 40, 0));
        // The absolute threshold counts the queue too.
        let mut big = MassChangeGuard::with_ratio(Duration::from_secs(60), 1_000, 25);
        assert!(big.record_delete(at(100), 2_000_000, 999));
    }

    #[test]
    fn mass_change_guard_ratio_rule_protects_a_small_tree() {
        // 40 synced files: a quarter is 10 deletions, exactly the floor
        // the ratio rule never goes under.
        let mut guard = MassChangeGuard::with_ratio(Duration::from_secs(60), 1_000, 25);
        for second in 0..9 {
            assert!(
                !guard.record_delete(at(100 + second), 40, 0),
                "delete {second}"
            );
        }
        assert!(
            guard.record_delete(at(110), 40, 0),
            "the tenth deletion is a quarter of 40"
        );
        // A big tree needs the absolute threshold.
        let mut big = MassChangeGuard::with_ratio(Duration::from_secs(60), 1_000, 25);
        for second in 0..200 {
            assert!(!big.record_delete(at(100 + second), 2_000_000, 0));
        }
    }

    #[test]
    fn mass_change_guard_does_not_trip_on_slow_deletions() {
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 3);
        assert!(!guard.record_delete(at(100), 1_000_000, 0));
        assert!(!guard.record_delete(at(200), 1_000_000, 0));
        assert!(!guard.record_delete(at(300), 1_000_000, 0));
        assert!(!guard.is_tripped(), "one delete per 100s is normal use");
    }

    #[test]
    fn mass_change_guard_reset_rearms_with_an_empty_window() {
        let mut guard = MassChangeGuard::new(Duration::from_secs(60), 2);
        guard.record_delete(at(100), 1_000_000, 0);
        assert!(guard.record_delete(at(101), 1_000_000, 0));
        guard.reset();
        assert!(!guard.is_tripped());
        // Old evidence is gone; a single fresh delete does not re-trip.
        assert!(!guard.record_delete(at(102), 1_000_000, 0));
        // But a fresh storm does.
        assert!(guard.record_delete(at(103), 1_000_000, 0));
    }

    #[test]
    fn mass_delete_settings_resolve_clamps_floors_and_honors_disable() {
        let mut config = vapor_shared::config::VaporConfig::default();
        let resolved = MassDeleteGuardSettings::resolve(&config);
        assert!(resolved.enabled);
        assert_eq!(
            resolved.threshold,
            constants::safeguards::DEFAULT_MASS_DELETE_THRESHOLD
        );
        assert_eq!(
            resolved.window,
            Duration::from_secs(constants::safeguards::DEFAULT_MASS_DELETE_WINDOW_SECONDS)
        );

        // A threshold/window below the floors clamps up: a too-low
        // threshold would trip the guard on ordinary work.
        config.safeguards.mass_delete.threshold = 1;
        config.safeguards.mass_delete.window_seconds = 1;
        let clamped = MassDeleteGuardSettings::resolve(&config);
        assert_eq!(
            clamped.threshold,
            constants::safeguards::MIN_MASS_DELETE_THRESHOLD
        );
        assert_eq!(
            clamped.window,
            Duration::from_secs(constants::safeguards::MIN_MASS_DELETE_WINDOW_SECONDS)
        );

        // Raising the threshold for bulk-deletion workflows passes
        // through, and the explicit opt-out resolves as disabled.
        config.safeguards.mass_delete.threshold = 5_000;
        config.safeguards.mass_delete.enabled = false;
        let raised = MassDeleteGuardSettings::resolve(&config);
        assert_eq!(raised.threshold, 5_000);
        assert!(!raised.enabled);
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
