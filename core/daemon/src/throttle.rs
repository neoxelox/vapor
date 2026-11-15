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

/// The Idle concurrency tier, derived once from the machine's
/// available parallelism: half the cores, clamped into
/// `[IDLE_CONCURRENCY_MIN, IDLE_CONCURRENCY_MAX]`. A
/// 16-core desktop drains at 8-wide while a small laptop keeps the
/// classic 4: the throttle ladder still collapses this whenever the
/// user is active, and `resourceLimits.concurrentTransfers` caps it
/// explicitly.
pub fn idle_concurrency() -> usize {
    static TIER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *TIER.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|cores| cores.get())
            .unwrap_or(constants::engine::IDLE_CONCURRENCY_MIN);
        (cores / 2).clamp(
            constants::engine::IDLE_CONCURRENCY_MIN,
            constants::engine::IDLE_CONCURRENCY_MAX,
        )
    })
}

/// Read tokens scale at half the tier (floor at the classic 2): they
/// bound concurrent disk reads (hashing + reconcile), which saturate
/// well before the transfer tier does.
pub(crate) fn idle_read_tokens() -> usize {
    (idle_concurrency() / 2).max(constants::engine::IDLE_READ_TOKENS)
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

/// Keeps the daemon's own CPU under its ceiling by measurement. Each
/// sample above the ceiling takes one worker from every concurrency
/// cap; each sample clearly below gives one back. The ceiling is then
/// a limit the daemon holds itself to, not only a scale for the caps.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuRegulator {
    steps_down: usize,
}

impl CpuRegulator {
    /// Feeds one sample of the daemon's CPU share. `widest_cap` is the
    /// largest concurrency cap before regulation; one worker of each
    /// kind always stays.
    pub fn observe(&mut self, own_cpu_percent: u8, ceiling_percent: u8, widest_cap: usize) {
        if own_cpu_percent > ceiling_percent {
            if !self.exhausted(widest_cap) {
                self.steps_down += 1;
            }
        } else if u16::from(own_cpu_percent) * 4 <= u16::from(ceiling_percent) * 3 {
            // Below three quarters of the ceiling: room to give back.
            // The band in between holds, so the caps do not flap.
            self.steps_down = self.steps_down.saturating_sub(1);
        }
        // The steps stay when a throttle state narrows the caps: the
        // workers come back one a sample when the state relaxes, not
        // all at once.
    }

    /// The cap after regulation. A zero stays zero.
    pub fn apply(&self, cap: usize) -> usize {
        if cap == 0 {
            0
        } else {
            cap.saturating_sub(self.steps_down).max(1)
        }
    }

    pub fn steps_down(&self) -> usize {
        self.steps_down
    }

    /// No worker is left to take: every cap is at one.
    pub fn exhausted(&self, widest_cap: usize) -> bool {
        self.steps_down >= widest_cap.saturating_sub(1)
    }
}

#[derive(Clone, Debug)]
pub struct ThrottleController {
    sample_interval: Duration,
    clock: Arc<dyn Clock>,
    last_state: Option<ThrottleState>,
    last_state_change_inst: Option<Instant>,
    /// The ceiling the daemon's own CPU is above although every
    /// concurrency cap is already at one; `None` while regulation
    /// keeps it under.
    vapor_cpu_ceiling_exceeded: Option<u8>,
    /// What the link carries in its slower direction; `None` while
    /// nothing is known about it.
    link_capacity_kbps: Option<u32>,
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
            vapor_cpu_ceiling_exceeded: None,
            link_capacity_kbps: None,
        }
    }

    /// The capacity of the link in its slower direction, from the
    /// daemon's own estimate.
    pub fn set_link_capacity_kbps(&mut self, kbps: Option<u32>) {
        self.link_capacity_kbps = kbps;
    }

    /// Tells the controller that the daemon's own CPU is above its
    /// ceiling with nothing left to regulate (`Some(ceiling)`), or back
    /// under it (`None`).
    pub fn set_vapor_cpu_ceiling_exceeded(&mut self, ceiling_percent: Option<u8>) {
        self.vapor_cpu_ceiling_exceeded = ceiling_percent;
    }

    pub fn sample_interval(&self) -> Duration {
        self.sample_interval
    }

    /// Returns the minimum dwell time the controller will hold the given
    /// state before allowing a transition.
    pub fn min_dwell_for(state: ThrottleState) -> Duration {
        match state {
            ThrottleState::Idle => Duration::ZERO,
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
            state: ThrottleState::Idle,
            cause: ThrottleCause::IdleReady,
            priority: 0,
        };

        self.consider(&mut selected, self.low_power_candidate(inputs));
        self.consider(&mut selected, self.thermal_candidate(inputs));
        self.consider(&mut selected, self.system_cpu_candidate(inputs));
        self.consider(&mut selected, self.vapor_cpu_candidate());
        self.consider(&mut selected, self.disk_candidate(inputs));
        self.consider(&mut selected, self.user_activity_candidate(inputs));
        self.consider(&mut selected, self.network_error_candidate(inputs));
        self.consider(&mut selected, self.link_capacity_candidate());
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
        // have triggered the transition: operators see why the
        // controller wanted to move and that the dwell window pinned it.
        // The held-state ↔ would-be-cause pairing is intentional.
        let cause = selected.cause;

        let mut caps = self.caps_for(next_state);
        if inputs.disk_pressure == ResourcePressure::Severe {
            // A full disk has no room for what comes down. Uploads and
            // everything else go on.
            caps.allow_downloads = false;
            caps.download_concurrency = 0;
        }
        let decision = ThrottleDecision {
            state: next_state,
            cause,
            reason: self.describe_reason(cause, inputs),
            caps,
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
            ThrottleState::Idle => ThrottleCaps {
                planner_workers: idle_concurrency(),
                hash_workers: idle_concurrency(),
                read_tokens: idle_read_tokens(),
                upload_concurrency: idle_concurrency(),
                download_concurrency: idle_concurrency(),
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
                allow_reconcile: true,
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
                allow_reconcile: true,
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

    /// A user can leave Low Power Mode on for good, so it slows sync
    /// down and never stops it.
    fn low_power_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        inputs.low_power_mode.then_some(ThrottleCandidate {
            state: ThrottleState::Throttled,
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
            >= constants::engine::THROTTLED_SYSTEM_CPU_PERCENT
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

    /// The measured ceiling normally holds the daemon's CPU by taking
    /// workers away. When none is left to take and the CPU is still
    /// above the ceiling, the state itself steps down.
    fn vapor_cpu_candidate(&self) -> Option<ThrottleCandidate> {
        self.vapor_cpu_ceiling_exceeded.map(|_| ThrottleCandidate {
            state: ThrottleState::Throttled,
            cause: ThrottleCause::VaporCpuLoad,
            priority: 85,
        })
    }

    fn disk_candidate(&self, inputs: ThrottleInputs) -> Option<ThrottleCandidate> {
        let state = match inputs.disk_pressure {
            ResourcePressure::Nominal => return None,
            ResourcePressure::Elevated => ThrottleState::Light,
            // Severe also holds downloads; see `evaluate`.
            ResourcePressure::High | ResourcePressure::Severe => ThrottleState::Throttled,
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

    fn link_capacity_candidate(&self) -> Option<ThrottleCandidate> {
        let capacity = self.link_capacity_kbps?;
        let state = if capacity <= constants::engine::THROTTLED_LINK_CAPACITY_KBPS {
            ThrottleState::Throttled
        } else if capacity <= constants::engine::LIGHT_LINK_CAPACITY_KBPS {
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
            ThrottleCause::VaporCpuLoad => match self.vapor_cpu_ceiling_exceeded {
                Some(ceiling) => format!(
                    "Vapor CPU load is {}%, above its ceiling of {ceiling}%",
                    inputs.vapor_cpu_load_percent
                ),
                None => format!("Vapor CPU load is {}%", inputs.vapor_cpu_load_percent),
            },
            ThrottleCause::DiskPressure if inputs.disk_pressure == ResourcePressure::Severe => {
                "the disk is almost full; downloads wait".to_string()
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
                "the network link carries {} kbit/s",
                self.link_capacity_kbps.unwrap_or_default()
            ),
            ThrottleCause::UserActivity => "user activity is active".to_string(),
        }
    }
}

fn throttle_state_rank(state: ThrottleState) -> u8 {
    match state {
        ThrottleState::Idle => 0,
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
    fn idle_plugged_cool_inputs_enter_idle_with_full_caps() {
        let mut controller = ThrottleController::default();

        let decision = controller.evaluate(ThrottleInputs::default());

        assert_eq!(decision.state, ThrottleState::Idle);
        assert_eq!(decision.cause, ThrottleCause::IdleReady);
        assert_eq!(decision.reason, "idle, plugged in, and cool");
        let tier = idle_concurrency();
        assert!(
            (constants::engine::IDLE_CONCURRENCY_MIN..=constants::engine::IDLE_CONCURRENCY_MAX)
                .contains(&tier),
            "the derived tier must respect its bounds, got {tier}"
        );
        assert_eq!(decision.caps.planner_workers, tier);
        assert_eq!(decision.caps.hash_workers, tier);
        assert_eq!(decision.caps.read_tokens, idle_read_tokens());
        assert_eq!(decision.caps.upload_concurrency, tier);
        assert_eq!(decision.caps.download_concurrency, tier);
        assert!(decision.caps.allow_reconcile);
        assert!(decision.caps.allow_hashing);
        assert!(decision.caps.allow_uploads);
    }

    #[test]
    fn battery_power_prevents_idle() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            on_battery: true,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Light);
        assert_eq!(decision.cause, ThrottleCause::OnBattery);
        assert_eq!(decision.reason, "running on battery power");
        assert_eq!(decision.caps.upload_concurrency, 2);
        assert!(
            decision.caps.allow_reconcile,
            "battery power slows a scan down and does not hold it"
        );
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
    fn low_power_mode_slows_sync_down_and_does_not_stop_it() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            low_power_mode: true,
            on_battery: true,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, ThrottleCause::LowPowerMode);
        assert_eq!(decision.reason, "low power mode is enabled");
        assert!(decision.caps.allow_hashing);
        assert!(decision.caps.allow_uploads);
        assert!(decision.caps.allow_downloads);
        assert!(decision.caps.allow_reconcile);
        assert_eq!(decision.caps.upload_concurrency, 1);
    }

    #[test]
    fn only_critical_thermal_pressure_suspends() {
        // Every other input, at its worst and all at once, slows sync
        // down. A device that is about to overheat is the one case
        // where the daemon stops.
        let mut controller = ThrottleController::default();
        let worst_without_heat = ThrottleInputs {
            on_battery: true,
            low_power_mode: true,
            thermal_pressure: ThermalPressure::Serious,
            system_cpu_load_percent: 100,
            vapor_cpu_load_percent: 100,
            disk_pressure: ResourcePressure::Severe,
            network_error_rate_percent: 100,
            user_active: true,
            ..ThrottleInputs::default()
        };
        controller.set_vapor_cpu_ceiling_exceeded(Some(15));
        controller.set_link_capacity_kbps(Some(1));
        let decision = controller.evaluate(worst_without_heat);
        assert_eq!(decision.state, ThrottleState::Throttled);
        assert!(decision.caps.allow_uploads);
        assert!(decision.caps.allow_reconcile);

        let decision = controller.evaluate(ThrottleInputs {
            thermal_pressure: ThermalPressure::Critical,
            ..ThrottleInputs::default()
        });
        assert_eq!(decision.state, ThrottleState::Suspended);
        assert_eq!(decision.cause, ThrottleCause::ThermalPressure);
        assert!(!decision.caps.allow_hashing);
        assert!(!decision.caps.allow_uploads);
        assert!(!decision.caps.allow_reconcile);
        assert_eq!(decision.caps.planner_workers, 0);
        assert_eq!(decision.caps.upload_concurrency, 0);
    }

    #[test]
    fn a_disk_that_is_almost_full_holds_downloads_only() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            disk_pressure: ResourcePressure::Severe,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, ThrottleCause::DiskPressure);
        assert_eq!(decision.reason, "the disk is almost full; downloads wait");
        assert!(!decision.caps.allow_downloads);
        assert_eq!(decision.caps.download_concurrency, 0);
        assert!(decision.caps.allow_uploads);
        assert_eq!(decision.caps.upload_concurrency, 1);
        assert!(decision.caps.allow_reconcile);

        let decision = controller.evaluate(ThrottleInputs {
            disk_pressure: ResourcePressure::High,
            ..ThrottleInputs::default()
        });
        assert!(decision.caps.allow_downloads);
    }

    #[test]
    fn system_cpu_at_its_highest_gives_throttled() {
        let mut controller = ThrottleController::default();
        let decision = controller.evaluate(ThrottleInputs {
            system_cpu_load_percent: 90,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, ThrottleCause::SystemCpuLoad);
        assert_eq!(decision.reason, "system CPU load is 90%");
    }

    #[test]
    fn own_cpu_beyond_regulation_gives_throttled_and_names_the_ceiling() {
        let mut controller = ThrottleController::default();
        let busy = ThrottleInputs {
            vapor_cpu_load_percent: 40,
            ..ThrottleInputs::default()
        };
        // Regulation still has workers to take: the state stays.
        assert_eq!(controller.evaluate(busy).state, ThrottleState::Idle);

        controller.set_vapor_cpu_ceiling_exceeded(Some(15));
        let decision = controller.evaluate(busy);
        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, ThrottleCause::VaporCpuLoad);
        assert_eq!(
            decision.reason,
            "Vapor CPU load is 40%, above its ceiling of 15%"
        );
    }

    #[test]
    fn the_cpu_regulator_takes_a_worker_above_the_ceiling_and_gives_it_back_below() {
        let mut regulator = CpuRegulator::default();
        assert_eq!(regulator.apply(4), 4);

        // Above the ceiling: one worker a sample, never the last one.
        for expected in [3, 2, 1, 1, 1] {
            regulator.observe(20, 15, 4);
            assert_eq!(regulator.apply(4), expected);
        }
        assert!(regulator.exhausted(4));
        assert_eq!(regulator.apply(0), 0, "a suspended zero stays zero");
        assert_eq!(regulator.apply(1), 1);

        // Between three quarters of the ceiling and the ceiling: hold.
        regulator.observe(13, 15, 4);
        assert_eq!(regulator.apply(4), 1);

        // Clearly below: one worker back a sample.
        for expected in [2, 3, 4, 4] {
            regulator.observe(5, 15, 4);
            assert_eq!(regulator.apply(4), expected);
        }
        assert!(!regulator.exhausted(4));

        // A throttle state narrows the caps: the steps stay, so the
        // workers come back one a sample after the state relaxes.
        for _ in 0..6 {
            regulator.observe(50, 15, 8);
        }
        regulator.observe(50, 15, 1);
        assert_eq!(regulator.steps_down(), 6);
        assert_eq!(regulator.apply(1), 1);
        regulator.observe(2, 15, 1);
        assert_eq!(regulator.apply(8), 3);
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
    fn a_slow_link_affects_the_state_only_when_its_capacity_is_known() {
        let mut controller = ThrottleController::default();
        let unknown = controller.evaluate(ThrottleInputs::default());
        controller.set_link_capacity_kbps(Some(100));
        let constrained = controller.evaluate(ThrottleInputs::default());

        assert_eq!(unknown.state, ThrottleState::Idle);
        assert_eq!(constrained.state, ThrottleState::Throttled);
        assert_eq!(constrained.cause, ThrottleCause::NetworkThroughput);
        assert_eq!(constrained.reason, "the network link carries 100 kbit/s");
    }

    #[test]
    fn min_dwell_durations_match_constants() {
        // Sanity guard so the dwell-window helper stays in sync with the
        // shared constants. Future tweaks must update both sides.
        assert_eq!(
            ThrottleController::min_dwell_for(ThrottleState::Idle),
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
        // to transition back to Idle.
        clock.advance(Duration::from_secs(3));
        let recovered = controller.evaluate(ThrottleInputs {
            system_cpu_load_percent: 5,
            ..ThrottleInputs::default()
        });
        assert_eq!(recovered.state, ThrottleState::Idle);
    }

    #[test]
    fn escalation_to_more_conservative_state_bypasses_the_dwell_window() {
        // Safety invariant: dwell only delays *relaxations*. A machine
        // that reaches critical thermal pressure (Suspended) one second after
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
            thermal_pressure: ThermalPressure::Critical,
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
        assert_eq!(recovered.state, ThrottleState::Idle);
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
            thermal_pressure: ThermalPressure::Critical,
            ..ThrottleInputs::default()
        });
        assert_eq!(decision.state, ThrottleState::Suspended);
    }
}
