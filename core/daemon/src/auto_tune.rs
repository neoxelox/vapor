//! Auto-tuning loop.
//!
//! Every `AUTO_TUNE_INTERVAL_SECONDS` (inside the documented 60-120s
//! window) the tuner makes at most ONE small change to its single
//! knob — the per-tick transfer step budget — following the priority
//! order *impact → rate-limit avoidance → latency*:
//!
//! - An active rate-limit slowdown halves the step (rate-limit
//!   avoidance beats throughput).
//! - A deep queue with no slowdown grows the step by 25% (latency).
//! - Anything else holds.
//!
//! Every change records the cycle's completion throughput; if the next
//! cycle completes fewer intents than the one before the change, the
//! change rolls back and the tuner holds for a cooldown cycle
//! (hysteresis + rollback-on-regression). Throughput — not absolute
//! queue depth — is the regression signal: depth is dominated by
//! exogenous ingest, and judging on it would roll back every increase
//! exactly when a deep queue needs it most. The knob is bounded to
//! 50%–200% of the compiled default and the bandwidth shaper still
//! caps actual bytes, so tuning can never exceed the user's effective
//! ceilings.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use vapor_shared::constants;

use crate::logging;

/// Queue depth above which the tuner considers throughput growth.
const DEEP_QUEUE_THRESHOLD: u64 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LastChange {
    None,
    /// A change was made; holds the pre-change cycle's completed-intent
    /// count for the regression check, and whether it was an increase.
    Pending {
        completed_before: u64,
        increased: bool,
    },
    /// Rolled back last cycle; hold one full cycle before touching the
    /// knob again.
    Cooldown,
}

pub struct AutoTuner {
    /// The knob: per-step transfer budget in bytes, shared with
    /// every profile runtime's executor.
    step_bytes: Arc<AtomicU64>,
    interval: Duration,
    last_cycle: Option<Instant>,
    last_change: LastChange,
    /// Intents completed since the current cycle window opened —
    /// the drain-rate signal the regression check judges on.
    completed_in_window: u64,
}

impl AutoTuner {
    pub fn new(step_bytes: Arc<AtomicU64>) -> Self {
        Self {
            step_bytes,
            interval: Duration::from_secs(constants::engine::AUTO_TUNE_INTERVAL_SECONDS),
            last_cycle: None,
            last_change: LastChange::None,
            completed_in_window: 0,
        }
    }

    #[cfg(test)]
    fn with_interval(step_bytes: Arc<AtomicU64>, interval: Duration) -> Self {
        Self {
            step_bytes,
            interval,
            last_cycle: None,
            last_change: LastChange::None,
            completed_in_window: 0,
        }
    }

    fn bounds() -> (u64, u64) {
        let base = constants::engine::TRANSFER_STAGE_STEP_BYTES;
        (
            base * constants::engine::AUTO_TUNE_MIN_STEP_PERCENT / 100,
            base * constants::engine::AUTO_TUNE_MAX_STEP_PERCENT / 100,
        )
    }

    /// Whether the next `evaluate` call would run a tuning cycle. Lets
    /// the caller skip gathering the (SQL-backed) queue-depth signal on
    /// the ~360 ticks per cycle where evaluate would ignore it.
    pub fn cycle_due(&self, now_inst: Instant) -> bool {
        self.last_cycle
            .map(|last| now_inst.saturating_duration_since(last) >= self.interval)
            .unwrap_or(true)
    }

    /// Accumulates completed-intent counts from every tick; the cycle
    /// window drains on each evaluation.
    pub fn record_completions(&mut self, completed: u64) {
        self.completed_in_window = self.completed_in_window.saturating_add(completed);
    }

    /// One evaluation; call when `cycle_due`. `rate_limited` = any
    /// profile currently in a retry slowdown window;
    /// `total_queue_depth` = durable depth across profiles.
    pub fn evaluate(&mut self, rate_limited: bool, total_queue_depth: u64, now_inst: Instant) {
        if !self.cycle_due(now_inst) {
            return;
        }
        self.last_cycle = Some(now_inst);
        let completed_this_cycle = std::mem::take(&mut self.completed_in_window);

        let (min_step, max_step) = Self::bounds();
        let current = self.step_bytes.load(Ordering::Relaxed);

        // Regression check for the previous cycle's change: judged on
        // drain rate. Queue depth is confounded by ingest — a deep queue
        // getting deeper during a burst says nothing about whether the
        // step increase helped — but completed-per-cycle falling after a
        // speed-up does.
        match self.last_change {
            LastChange::Pending {
                completed_before,
                increased,
            } => {
                if increased && completed_this_cycle < completed_before {
                    let reverted = (current * 100 / 125).clamp(min_step, max_step);
                    self.step_bytes.store(reverted, Ordering::Relaxed);
                    self.last_change = LastChange::Cooldown;
                    logging::info(
                        "Auto-tuner rolled back a step increase after throughput regression",
                        &[
                            ("previous_step_bytes", current.to_string()),
                            ("reverted_step_bytes", reverted.to_string()),
                            ("completed_before", completed_before.to_string()),
                            ("completed_after", completed_this_cycle.to_string()),
                        ],
                    );
                    return;
                }
                self.last_change = LastChange::None;
            }
            LastChange::Cooldown => {
                self.last_change = LastChange::None;
                return;
            }
            LastChange::None => {}
        }

        // One small change per cycle, priority-ordered.
        if rate_limited {
            let lowered = (current / 2).clamp(min_step, max_step);
            if lowered != current {
                self.step_bytes.store(lowered, Ordering::Relaxed);
                self.last_change = LastChange::Pending {
                    completed_before: completed_this_cycle,
                    increased: false,
                };
                logging::info(
                    "Auto-tuner lowered the transfer step under rate limiting",
                    &[("step_bytes", lowered.to_string())],
                );
            }
        } else if total_queue_depth > DEEP_QUEUE_THRESHOLD {
            let raised = (current * 125 / 100).clamp(min_step, max_step);
            if raised != current {
                self.step_bytes.store(raised, Ordering::Relaxed);
                self.last_change = LastChange::Pending {
                    completed_before: completed_this_cycle,
                    increased: true,
                };
                logging::info(
                    "Auto-tuner raised the transfer step to drain a deep queue",
                    &[("step_bytes", raised.to_string())],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuner() -> (AutoTuner, Arc<AtomicU64>) {
        let step = Arc::new(AtomicU64::new(constants::engine::TRANSFER_STAGE_STEP_BYTES));
        (
            AutoTuner::with_interval(step.clone(), Duration::from_secs(90)),
            step,
        )
    }

    #[test]
    fn rate_limiting_halves_the_step_down_to_the_floor() {
        let (mut tuner, step) = tuner();
        let base = constants::engine::TRANSFER_STAGE_STEP_BYTES;
        let t0 = Instant::now();

        tuner.evaluate(true, 0, t0);
        assert_eq!(step.load(Ordering::Relaxed), base / 2);

        // Next cycle still rate-limited: clamped at the 50% floor.
        tuner.evaluate(true, 0, t0 + Duration::from_secs(91));
        assert_eq!(step.load(Ordering::Relaxed), base / 2, "floor holds");
    }

    #[test]
    fn deep_queue_grows_the_step_by_one_increment_per_cycle() {
        let (mut tuner, step) = tuner();
        let base = constants::engine::TRANSFER_STAGE_STEP_BYTES;
        let t0 = Instant::now();

        tuner.evaluate(false, 500, t0);
        assert_eq!(step.load(Ordering::Relaxed), base * 125 / 100);

        // Within the same cadence window nothing changes (one change
        // per cycle).
        tuner.evaluate(false, 500, t0 + Duration::from_secs(10));
        assert_eq!(step.load(Ordering::Relaxed), base * 125 / 100);
    }

    #[test]
    fn throughput_regression_after_an_increase_rolls_back_and_cools_down() {
        let (mut tuner, step) = tuner();
        let base = constants::engine::TRANSFER_STAGE_STEP_BYTES;
        let t0 = Instant::now();

        tuner.record_completions(100);
        tuner.evaluate(false, 500, t0);
        let raised = step.load(Ordering::Relaxed);
        assert!(raised > base);

        // Fewer intents completed after the increase: the speed-up hurt
        // throughput. Rollback.
        tuner.record_completions(40);
        tuner.evaluate(false, 900, t0 + Duration::from_secs(91));
        assert!(step.load(Ordering::Relaxed) < raised, "rolled back");

        // Cooldown cycle: even a deep queue does not retrigger a change.
        let after_rollback = step.load(Ordering::Relaxed);
        tuner.evaluate(false, 2_000, t0 + Duration::from_secs(182));
        assert_eq!(step.load(Ordering::Relaxed), after_rollback, "cooldown");

        // After the cooldown, tuning resumes.
        tuner.evaluate(false, 2_000, t0 + Duration::from_secs(273));
        assert!(step.load(Ordering::Relaxed) > after_rollback);
    }

    #[test]
    fn improvement_after_an_increase_keeps_the_change() {
        let (mut tuner, step) = tuner();
        let t0 = Instant::now();
        tuner.record_completions(100);
        tuner.evaluate(false, 500, t0);
        let raised = step.load(Ordering::Relaxed);

        // Throughput held up: the change sticks and can grow again.
        tuner.record_completions(150);
        tuner.evaluate(false, 200, t0 + Duration::from_secs(91));
        assert!(step.load(Ordering::Relaxed) >= raised);
    }

    #[test]
    fn increase_sticks_when_ingest_outpaces_drain_but_throughput_improved() {
        // The scenario the depth-based check got wrong: a sustained
        // ingest keeps the queue growing no matter what the tuner does.
        // Depth after the increase is WORSE, but the drain rate improved
        // — the increase must stick.
        let (mut tuner, step) = tuner();
        let t0 = Instant::now();

        tuner.record_completions(100);
        tuner.evaluate(false, 5_000, t0);
        let raised = step.load(Ordering::Relaxed);

        tuner.record_completions(160);
        tuner.evaluate(false, 9_000, t0 + Duration::from_secs(91));
        assert!(
            step.load(Ordering::Relaxed) >= raised,
            "a deeper queue with better throughput must not roll back"
        );
    }

    #[test]
    fn the_step_is_bounded_at_both_ends() {
        let (mut tuner, step) = tuner();
        let base = constants::engine::TRANSFER_STAGE_STEP_BYTES;
        let mut at = Instant::now();
        for _ in 0..10 {
            at += Duration::from_secs(91);
            tuner.evaluate(false, 10_000, at);
        }
        assert!(step.load(Ordering::Relaxed) <= base * 2, "200% ceiling");
        for _ in 0..10 {
            at += Duration::from_secs(91);
            tuner.evaluate(true, 10_000, at);
        }
        assert!(step.load(Ordering::Relaxed) >= base / 2, "50% floor");
    }
}
