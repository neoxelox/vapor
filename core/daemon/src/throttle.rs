use std::sync::Arc;
use std::time::{Duration, Instant};

use vapor_shared::{ThrottleState, constants};

use crate::clock::{Clock, SystemClock};

// The sample types are shared contracts: `core/platform`'s metrics
// sampler produces them and the controller below consumes them.
// Re-exported here so existing `crate::throttle::…` paths keep working.
pub use vapor_shared::{ResourcePressure, ThermalPressure, ThrottleInputs};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThrottleCause {
    IdleReady,
    OnBattery,
    LowPowerMode,
    ThermalPressure,
    SystemCpuLoad,
    VaporCpuLoad,
    DiskPressure,
    NetworkErrors,
    NetworkThroughput,
    UserActivity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThrottleCaps {
    pub planner_workers: usize,
    pub hash_workers: usize,
    pub read_tokens: usize,
    pub upload_concurrency: usize,
    pub download_concurrency: usize,
    pub allow_reconcile: bool,
    pub allow_hashing: bool,
    pub allow_uploads: bool,
    pub allow_downloads: bool,
}

/// The IdleDrain concurrency tier, derived once from the machine's
/// available parallelism: half the cores, clamped into
/// `[IDLE_DRAIN_CONCURRENCY_MIN, IDLE_DRAIN_CONCURRENCY_MAX]`. A
/// 16-core desktop drains at 8-wide while a small laptop keeps the
/// classic 4 — the throttle ladder still collapses this whenever the
/// user is active, and `resourceLimits.maxConcurrentTransfers` caps it
/// explicitly.
pub fn idle_drain_concurrency() -> usize {
    static TIER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *TIER.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|cores| cores.get())
            .unwrap_or(constants::engine::IDLE_DRAIN_CONCURRENCY_MIN);
        (cores / 2).clamp(
            constants::engine::IDLE_DRAIN_CONCURRENCY_MIN,
            constants::engine::IDLE_DRAIN_CONCURRENCY_MAX,
        )
    })
}

/// Read tokens scale at half the tier (floor at the classic 2): they
/// bound concurrent disk reads (hashing + reconcile), which saturate
/// well before the transfer tier does.
pub(crate) fn idle_drain_read_tokens() -> usize {
    (idle_drain_concurrency() / 2).max(constants::engine::IDLE_DRAIN_READ_TOKENS)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThrottleDecision {
    pub state: ThrottleState,
    pub cause: ThrottleCause,
    pub reason: String,
    pub caps: ThrottleCaps,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThrottleCandidate {
    state: ThrottleState,
    cause: ThrottleCause,
    priority: u8,
}

#[derive(Clone, Debug)]
pub struct ThrottleController {
    sample_interval: Duration,
    clock: Arc<dyn Clock>,
    last_state: Option<ThrottleState>,
    last_state_change_inst: Option<Instant>,
}

impl Default for ThrottleController {
    fn default() -> Self {
        Self::new()
    }
}

impl ThrottleController {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            sample_interval: Duration::from_millis(
                constants::engine::THROTTLE_SAMPLE_INTERVAL_MILLIS,
            ),
            clock,
            last_state: None,
            last_state_change_inst: None,
        }
    }

    pub fn sample_interval(&self) -> Duration {
        self.sample_interval
    }

    /// Returns the minimum dwell time the controller will hold the given
    /// state before allowing a transition.
    pub fn min_dwell_for(state: ThrottleState) -> Duration {
        match state {
            ThrottleState::IdleDrain => Duration::ZERO,
            ThrottleState::Light => Duration::from_secs(constants::engine::MIN_DWELL_LIGHT_SECONDS),
            ThrottleState::Throttled => {
                Duration::from_secs(constants::engine::MIN_DWELL_THROTTLED_SECONDS)
            }
            ThrottleState::Suspended => {
                Duration::from_secs(constants::engine::MIN_DWELL_SUSPENDED_SECONDS)
            }
        }
    }

    pub fn evaluate(&mut self, inputs: ThrottleInputs) -> ThrottleDecision {
        let mut selected = ThrottleCandidate {
            state: ThrottleState::IdleDrain,
            cause: ThrottleCause::IdleReady,
            priority: 0,
        };

        self.consider(&mut selected, self.low_power_candidate(inputs));
        self.consider(&mut selected, self.thermal_candidate(inputs));
        self.consider(&mut selected, self.system_cpu_candidate(inputs));
        self.consider(&mut selected, self.vapor_cpu_candidate(inputs));
        self.consider(&mut selected, self.disk_candidate(inputs));
        self.consider(&mut selected, self.user_activity_candidate(inputs));
        self.consider(&mut selected, self.network_error_candidate(inputs));
        self.consider(&mut selected, self.network_throughput_candidate(inputs));
        self.consider(&mut selected, self.battery_candidate(inputs));

        // Hysteresis / min-dwell: if the previous decision is still inside
        // its dwell window, hold the existing state and reuse the cause we
        // recorded on entry. This stops oscillating CPU / network samples
        // from flipping the throttle state more than once per dwell window.
        // Two deliberate carve-outs:
        //
        // - The first transition (no prior state) is always honored so
        // boot still settles to the right tier immediately.
        // - Escalations (a candidate *more* conservative than the held
        // state) bypass the dwell entirely. Dwell exists to stop
        // oscillating samples from relaxing caps too eagerly; making a
        // thermally-critical or low-power machine wait out a dwell
        // window at full caps would invert the "defer under pressure"
        // invariant.
        let now_inst = self.clock.now();
        let next_state = if let (Some(prev_state), Some(changed_at)) =
            (self.last_state, self.last_state_change_inst)
        {
            let dwell = Self::min_dwell_for(prev_state);
            let within_dwell = now_inst.saturating_duration_since(changed_at) < dwell;
            let is_relaxation =
                throttle_state_rank(selected.state) < throttle_state_rank(prev_state);
            if within_dwell && is_relaxation {
                prev_state
            } else {
                selected.state
            }
        } else {
            selected.state
        };

        // The cause field is diagnostic, not behavioral. When hysteresis
        // holds the prior state we still report the cause that *would*
        // have triggered the transition — operators see why the
        // controller wanted to move and that the dwell window pinned it.
        // The held-state ↔ would-be-cause pairing is intentional.
        let cause = selected.cause;

        let decision = ThrottleDecision {
            state: next_state,
            cause,
            reason: self.describe_reason(cause, inputs),
            caps: self.caps_for(next_state),
        };

        // Record the transition only when we actually changed state.
        match self.last_state {
            Some(prev) if prev == decision.state => {}
            _ => {
                self.last_state_change_inst = Some(now_inst);
            }
        }
        self.last_state = Some(decision.state);

        decision
    }

    pub fn caps_for(&self, state: ThrottleState) -> ThrottleCaps {
        match state {
            ThrottleState::IdleDrain => ThrottleCaps {
                planner_workers: idle_drain_concurrency(),
                hash_workers: idle_drain_concurrency(),
                read_tokens: idle_drain_read_tokens(),
                upload_concurrency: idle_drain_concurrency(),
                download_concurrency: idle_drain_concurrency(),
                allow_reconcile: true,
                allow_hashing: true,
                allow_uploads: true,
                allow_downloads: true,
            },
            ThrottleState::Light => ThrottleCaps {
                planner_workers: constants::engine::LIGHT_PLANNER_WORKERS,
                hash_workers: constants::engine::LIGHT_HASH_WORKERS,
                read_tokens: constants::engine::LIGHT_READ_TOKENS,
                upload_concurrency: constants::engine::LIGHT_UPLOAD_CONCURRENCY,
                download_concurrency: constants::engine::LIGHT_DOWNLOAD_CONCURRENCY,
                allow_reconcile: false,
                allow_hashing: true,
                allow_uploads: true,
                allow_downloads: true,
            },
            ThrottleState::Throttled => ThrottleCaps {
                planner_workers: constants::engine::THROTTLED_PLANNER_WORKERS,
                hash_workers: constants::engine::THROTTLED_HASH_WORKERS,
                read_tokens: constants::engine::THROTTLED_READ_TOKENS,
                upload_concurrency: constants::engine::THROTTLED_UPLOAD_CONCURRENCY,
                download_concurrency: constants::engine::THROTTLED_DOWNLOAD_CONCURRENCY,
                allow_reconcile: false,
                allow_hashing: true,
                allow_uploads: true,
                allow_downloads: true,
            },
            ThrottleState::Suspended => ThrottleCaps {
                planner_workers: 0,
                hash_workers: 0,
                read_tokens: 0,
                upload_concurrency: 0,
                download_concurrency: 0,
                allow_reconcile: false,
                allow_hashing: false,
                allow_uploads: false,
                allow_downloads: false,
            },
        }
    }

    fn consider(&self, current: &mut ThrottleCandidate, candidate: Option<ThrottleCandidate>) {
        let Some(candidate) = candidate else {
            return;
        };

        if throttle_state_rank(candidate.state) > throttle_state_rank(current.state)
            || (candidate.state == current.state && candidate.priority > current.priority)
        {
            *current = candidate;
        }
    }

    fn low_power_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        inputs.low_power_mode.then_some(ThrottleCandidate {
            state: ThrottleState::Suspended,
            cause: ThrottleCause::LowPowerMode,
            priority: 100,
        })
    }

    fn thermal_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        let state = match inputs.thermal_pressure {
            ThermalPressure::Nominal => return None,
            ThermalPressure::Fair => ThrottleState::Light,
            ThermalPressure::Serious => ThrottleState::Throttled,
            ThermalPressure::Critical => ThrottleState::Suspended,
        };

        Some(ThrottleCandidate {
            state,
            cause: ThrottleCause::ThermalPressure,
            priority: 95,
        })
    }

    fn system_cpu_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        let state = if inputs.system_cpu_load_percent
            >= constants::engine::SUSPENDED_SYSTEM_CPU_PERCENT
        {
            ThrottleState::Suspended
        } else if inputs.system_cpu_load_percent >= constants::engine::THROTTLED_SYSTEM_CPU_PERCENT
        {
            ThrottleState::Throttled
        } else if inputs.system_cpu_load_percent >= constants::engine::LIGHT_SYSTEM_CPU_PERCENT {
            ThrottleState::Light
        } else {
            return None;
        };

        Some(ThrottleCandidate {
            state,
            cause: ThrottleCause::SystemCpuLoad,
            priority: 90,
        })
    }

    fn vapor_cpu_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        let state = if inputs.vapor_cpu_load_percent
            >= constants::engine::SUSPENDED_VAPOR_CPU_PERCENT
        {
            ThrottleState::Suspended
        } else if inputs.vapor_cpu_load_percent >= constants::engine::THROTTLED_VAPOR_CPU_PERCENT {
            ThrottleState::Throttled
        } else if inputs.vapor_cpu_load_percent >= constants::engine::LIGHT_VAPOR_CPU_PERCENT {
            ThrottleState::Light
        } else {
            return None;
        };

        Some(ThrottleCandidate {
            state,
            cause: ThrottleCause::VaporCpuLoad,
            priority: 85,
        })
    }

    fn disk_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        let state = match inputs.disk_pressure {
            ResourcePressure::Nominal => return None,
            ResourcePressure::Elevated => ThrottleState::Light,
            ResourcePressure::High => ThrottleState::Throttled,
            ResourcePressure::Severe => ThrottleState::Suspended,
        };

        Some(ThrottleCandidate {
            state,
            cause: ThrottleCause::DiskPressure,
            priority: 80,
        })
    }

    fn user_activity_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        inputs.user_active.then_some(ThrottleCandidate {
            state: ThrottleState::Throttled,
            cause: ThrottleCause::UserActivity,
            priority: 70,
        })
    }

    fn network_error_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        let state = if inputs.network_error_rate_percent
            >= constants::engine::THROTTLED_NETWORK_ERROR_RATE_PERCENT
        {
            ThrottleState::Throttled
        } else if inputs.network_error_rate_percent
            >= constants::engine::LIGHT_NETWORK_ERROR_RATE_PERCENT
        {
            ThrottleState::Light
        } else {
            return None;
        };

        Some(ThrottleCandidate {
            state,
            cause: ThrottleCause::NetworkErrors,
            priority: 60,
        })
    }

    fn network_throughput_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        let throughput = inputs.network_throughput_kbps?;
        let state = if throughput <= constants::engine::THROTTLED_NETWORK_THROUGHPUT_KBPS {
            ThrottleState::Throttled
        } else if throughput <= constants::engine::LIGHT_NETWORK_THROUGHPUT_KBPS {
            ThrottleState::Light
        } else {
            return None;
        };

        Some(ThrottleCandidate {
            state,
            cause: ThrottleCause::NetworkThroughput,
            priority: 50,
        })
    }

    fn battery_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        inputs.on_battery.then_some(ThrottleCandidate {
            state: ThrottleState::Light,
            cause: ThrottleCause::OnBattery,
            priority: 40,
        })
    }

    fn describe_reason(&self, cause: ThrottleCause, inputs: ThrottleInputs) -> String {
        match cause {
            ThrottleCause::IdleReady => "idle, plugged in, and cool".to_string(),
            ThrottleCause::OnBattery => "running on battery power".to_string(),
            ThrottleCause::LowPowerMode => "low power mode is enabled".to_string(),
            ThrottleCause::ThermalPressure => {
                format!("thermal pressure is {}", inputs.thermal_pressure.label())
            }
            ThrottleCause::SystemCpuLoad => {
                format!("system CPU load is {}%", inputs.system_cpu_load_percent)
            }
            ThrottleCause::VaporCpuLoad => {
                format!("Vapor CPU load is {}%", inputs.vapor_cpu_load_percent)
            }
            ThrottleCause::DiskPressure => {
                format!("disk pressure is {}", inputs.disk_pressure.label())
            }
            ThrottleCause::NetworkErrors => {
                format!(
                    "network error rate is {}%",
                    inputs.network_error_rate_percent
                )
            }
            ThrottleCause::NetworkThroughput => format!(
                "network throughput is {} kbps",
                inputs.network_throughput_kbps.unwrap_or_default()
            ),
            ThrottleCause::UserActivity => "user activity is active".to_string(),
        }
    }
}

fn throttle_state_rank(state: ThrottleState) -> u8 {
    match state {
        ThrottleState::IdleDrain => 0,
        ThrottleState::Light => 1,
        ThrottleState::Throttled => 2,
        ThrottleState::Suspended => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_samples_on_one_second_cadence() {
        let controller = ThrottleController::default();
        assert_eq!(controller.sample_interval(), Duration::from_secs(1));
    }

    #[test]
    fn idle_plugged_cool_inputs_enter_idle_drain_with_full_caps() {
        let mut controller = ThrottleController::default();

        let decision = controller.evaluate(ThrottleInputs::default());

        assert_eq!(decision.state, ThrottleState::IdleDrain);
        assert_eq!(decision.cause, ThrottleCause::IdleReady);
        assert_eq!(decision.reason, "idle, plugged in, and cool");
        let tier = idle_drain_concurrency();
        assert!(
            (constants::engine::IDLE_DRAIN_CONCURRENCY_MIN
                ..=constants::engine::IDLE_DRAIN_CONCURRENCY_MAX)
                .contains(&tier),
            "the derived tier must respect its bounds, got {tier}"
        );
        assert_eq!(decision.caps.planner_workers, tier);
        assert_eq!(decision.caps.hash_workers, tier);
        assert_eq!(decision.caps.read_tokens, idle_drain_read_tokens());
        assert_eq!(decision.caps.upload_concurrency, tier);
        assert_eq!(decision.caps.download_concurrency, tier);
        assert!(decision.caps.allow_reconcile);
        assert!(decision.caps.allow_hashing);
        assert!(decision.caps.allow_uploads);
    }

    #[test]
    fn battery_power_prevents_idle_drain() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            on_battery: true,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Light);
        assert_eq!(decision.cause, ThrottleCause::OnBattery);
        assert_eq!(decision.reason, "running on battery power");
        assert_eq!(decision.caps.upload_concurrency, 2);
        assert!(!decision.caps.allow_reconcile);
    }

    #[test]
    fn active_user_inputs_downshift_to_throttled() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            user_active: true,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, ThrottleCause::UserActivity);
        assert_eq!(decision.reason, "user activity is active");
        assert_eq!(decision.caps.planner_workers, 1);
        assert_eq!(decision.caps.upload_concurrency, 1);
    }

    #[test]
    fn fair_thermal_pressure_enters_light_state() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            thermal_pressure: ThermalPressure::Fair,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Light);
        assert_eq!(decision.cause, ThrottleCause::ThermalPressure);
        assert_eq!(decision.reason, "thermal pressure is fair");
    }

    #[test]
    fn serious_thermal_pressure_beats_battery_light_signal() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            on_battery: true,
            thermal_pressure: ThermalPressure::Serious,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, ThrottleCause::ThermalPressure);
        assert_eq!(decision.reason, "thermal pressure is serious");
    }

    #[test]
    fn low_power_mode_suspends_heavy_work() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            low_power_mode: true,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Suspended);
        assert_eq!(decision.cause, ThrottleCause::LowPowerMode);
        assert!(!decision.caps.allow_hashing);
        assert!(!decision.caps.allow_uploads);
        assert_eq!(decision.caps.planner_workers, 0);
        assert_eq!(decision.caps.upload_concurrency, 0);
    }

    #[test]
    fn severe_system_cpu_can_suspend_even_when_other_inputs_are_lower() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            user_active: true,
            system_cpu_load_percent: 90,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Suspended);
        assert_eq!(decision.cause, ThrottleCause::SystemCpuLoad);
        assert_eq!(decision.reason, "system CPU load is 90%");
    }

    #[test]
    fn network_errors_downshift_without_forcing_suspension() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            network_error_rate_percent: 30,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, ThrottleCause::NetworkErrors);
        assert_eq!(decision.reason, "network error rate is 30%");
    }

    #[test]
    fn network_throughput_only_affects_state_when_value_is_known() {
        let mut controller = ThrottleController::default();
        let unknown = controller.evaluate(ThrottleInputs {
            network_throughput_kbps: None,
            ..ThrottleInputs::default()
        });
        let constrained = controller.evaluate(ThrottleInputs {
            network_throughput_kbps: Some(100),
            ..ThrottleInputs::default()
        });

        assert_eq!(unknown.state, ThrottleState::IdleDrain);
        assert_eq!(constrained.state, ThrottleState::Throttled);
        assert_eq!(constrained.cause, ThrottleCause::NetworkThroughput);
        assert_eq!(constrained.reason, "network throughput is 100 kbps");
    }

    #[test]
    fn min_dwell_durations_match_constants() {
        // Sanity guard so the dwell-window helper stays in sync with the
        // shared constants. Future tweaks must update both sides.
        assert_eq!(
            ThrottleController::min_dwell_for(ThrottleState::IdleDrain),
            Duration::ZERO
        );
        assert_eq!(
            ThrottleController::min_dwell_for(ThrottleState::Light),
            Duration::from_secs(constants::engine::MIN_DWELL_LIGHT_SECONDS)
        );
        assert_eq!(
            ThrottleController::min_dwell_for(ThrottleState::Throttled),
            Duration::from_secs(constants::engine::MIN_DWELL_THROTTLED_SECONDS)
        );
        assert_eq!(
            ThrottleController::min_dwell_for(ThrottleState::Suspended),
            Duration::from_secs(constants::engine::MIN_DWELL_SUSPENDED_SECONDS)
        );
    }

    #[test]
    fn oscillating_cpu_samples_do_not_flip_state_more_than_once_per_min_dwell() {
        // Invariant: CPU samples that oscillate just above and below
        // the Light/Throttled thresholds must not flip the throttle
        // state on every sample. We drive the controller through 6 quick
        // samples (well under the 5 s dwell window for `Throttled`) that
        // alternate around the boundary; the recorded transitions must be
        // capped at one within the window.
        use crate::clock::ManualClock;

        let clock = Arc::new(ManualClock::at_now());
        let mut controller = ThrottleController::with_clock(clock.clone());

        // First evaluate puts us in Throttled (CPU ≥ 60).
        let throttled = controller.evaluate(ThrottleInputs {
            system_cpu_load_percent: 65,
            ..ThrottleInputs::default()
        });
        assert_eq!(throttled.state, ThrottleState::Throttled);

        // Oscillate: each subsequent sample alternates "back to idle" /
        // "still throttled" at sub-second cadence. The dwell window
        // (`MIN_DWELL_THROTTLED_SECONDS = 5 s`) means the controller must
        // hold `Throttled` for the entire burst.
        for step in 1..=6 {
            clock.advance(Duration::from_millis(500));
            let cpu_load = if step % 2 == 0 { 5 } else { 65 };
            let decision = controller.evaluate(ThrottleInputs {
                system_cpu_load_percent: cpu_load,
                ..ThrottleInputs::default()
            });
            assert_eq!(
                decision.state,
                ThrottleState::Throttled,
                "step {step}: throttle state must stay pinned during dwell window"
            );
        }

        // After the dwell elapses, the next "all clear" sample is allowed
        // to transition back to IdleDrain.
        clock.advance(Duration::from_secs(3));
        let recovered = controller.evaluate(ThrottleInputs {
            system_cpu_load_percent: 5,
            ..ThrottleInputs::default()
        });
        assert_eq!(recovered.state, ThrottleState::IdleDrain);
    }

    #[test]
    fn escalation_to_more_conservative_state_bypasses_the_dwell_window() {
        // Safety invariant: dwell only delays *relaxations*. A machine
        // that enters Low Power Mode (→ Suspended) one second after
        // landing in Light must suspend immediately, not keep hashing
        // and uploading for the remainder of the 5 s dwell.
        use crate::clock::ManualClock;

        let clock = Arc::new(ManualClock::at_now());
        let mut controller = ThrottleController::with_clock(clock.clone());

        let light = controller.evaluate(ThrottleInputs {
            on_battery: true,
            ..ThrottleInputs::default()
        });
        assert_eq!(light.state, ThrottleState::Light);

        clock.advance(Duration::from_secs(1));
        let suspended = controller.evaluate(ThrottleInputs {
            on_battery: true,
            low_power_mode: true,
            ..ThrottleInputs::default()
        });
        assert_eq!(
            suspended.state,
            ThrottleState::Suspended,
            "escalation must not wait out the Light dwell window"
        );

        // And the subsequent relaxation honors the Suspended dwell (1 s).
        let held = controller.evaluate(ThrottleInputs::default());
        assert_eq!(
            held.state,
            ThrottleState::Suspended,
            "relaxation out of Suspended still dwells"
        );
        clock.advance(Duration::from_secs(2));
        let recovered = controller.evaluate(ThrottleInputs::default());
        assert_eq!(recovered.state, ThrottleState::IdleDrain);
    }

    #[test]
    fn first_evaluation_after_construction_skips_dwell_so_boot_settles_quickly() {
        // The dwell only kicks in after we have a recorded prior state.
        // On boot, the very first decision must reflect the inputs
        // immediately so the engine starts in the right tier.
        use crate::clock::ManualClock;

        let clock = Arc::new(ManualClock::at_now());
        let mut controller = ThrottleController::with_clock(clock);
        let decision = controller.evaluate(ThrottleInputs {
            system_cpu_load_percent: 90,
            ..ThrottleInputs::default()
        });
        assert_eq!(decision.state, ThrottleState::Suspended);
    }
}
