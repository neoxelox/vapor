use std::time::Duration;

use vapor_shared::{ThrottleState, constants};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThermalPressure {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl ThermalPressure {
    fn label(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResourcePressure {
    #[default]
    Nominal,
    Elevated,
    High,
    Severe,
}

impl ResourcePressure {
    fn label(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Elevated => "elevated",
            Self::High => "high",
            Self::Severe => "severe",
        }
    }
}

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
    pub allow_reconcile: bool,
    pub allow_hashing: bool,
    pub allow_uploads: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThrottleInputs {
    pub on_battery: bool,
    pub low_power_mode: bool,
    pub thermal_pressure: ThermalPressure,
    pub system_cpu_load_percent: u8,
    pub vapor_cpu_load_percent: u8,
    pub disk_pressure: ResourcePressure,
    pub network_error_rate_percent: u8,
    pub network_throughput_kbps: Option<u32>,
    pub user_active: bool,
}

impl Default for ThrottleInputs {
    fn default() -> Self {
        Self {
            on_battery: false,
            low_power_mode: false,
            thermal_pressure: ThermalPressure::Nominal,
            system_cpu_load_percent: 10,
            vapor_cpu_load_percent: 2,
            disk_pressure: ResourcePressure::Nominal,
            network_error_rate_percent: 0,
            network_throughput_kbps: Some(10_000),
            user_active: false,
        }
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThrottleController {
    sample_interval: Duration,
}

impl Default for ThrottleController {
    fn default() -> Self {
        Self::new()
    }
}

impl ThrottleController {
    pub fn new() -> Self {
        Self {
            sample_interval: Duration::from_millis(
                constants::engine::THROTTLE_SAMPLE_INTERVAL_MILLIS,
            ),
        }
    }

    pub fn sample_interval(&self) -> Duration {
        self.sample_interval
    }

    pub fn evaluate(&self, inputs: ThrottleInputs) -> ThrottleDecision {
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

        ThrottleDecision {
            state: selected.state,
            cause: selected.cause,
            reason: self.describe_reason(selected.cause, inputs),
            caps: self.caps_for(selected.state),
        }
    }

    pub fn caps_for(&self, state: ThrottleState) -> ThrottleCaps {
        match state {
            ThrottleState::IdleDrain => ThrottleCaps {
                planner_workers: constants::engine::IDLE_DRAIN_PLANNER_WORKERS,
                hash_workers: constants::engine::IDLE_DRAIN_HASH_WORKERS,
                read_tokens: constants::engine::IDLE_DRAIN_READ_TOKENS,
                upload_concurrency: constants::engine::IDLE_DRAIN_UPLOAD_CONCURRENCY,
                allow_reconcile: true,
                allow_hashing: true,
                allow_uploads: true,
            },
            ThrottleState::Light => ThrottleCaps {
                planner_workers: constants::engine::LIGHT_PLANNER_WORKERS,
                hash_workers: constants::engine::LIGHT_HASH_WORKERS,
                read_tokens: constants::engine::LIGHT_READ_TOKENS,
                upload_concurrency: constants::engine::LIGHT_UPLOAD_CONCURRENCY,
                allow_reconcile: false,
                allow_hashing: true,
                allow_uploads: true,
            },
            ThrottleState::Throttled => ThrottleCaps {
                planner_workers: constants::engine::THROTTLED_PLANNER_WORKERS,
                hash_workers: constants::engine::THROTTLED_HASH_WORKERS,
                read_tokens: constants::engine::THROTTLED_READ_TOKENS,
                upload_concurrency: constants::engine::THROTTLED_UPLOAD_CONCURRENCY,
                allow_reconcile: false,
                allow_hashing: true,
                allow_uploads: true,
            },
            ThrottleState::Suspended => ThrottleCaps {
                planner_workers: 0,
                hash_workers: 0,
                read_tokens: 0,
                upload_concurrency: 0,
                allow_reconcile: false,
                allow_hashing: false,
                allow_uploads: false,
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
        let controller = ThrottleController::default();

        let decision = controller.evaluate(ThrottleInputs::default());

        assert_eq!(decision.state, ThrottleState::IdleDrain);
        assert_eq!(decision.cause, ThrottleCause::IdleReady);
        assert_eq!(decision.reason, "idle, plugged in, and cool");
        assert_eq!(decision.caps.planner_workers, 4);
        assert_eq!(decision.caps.hash_workers, 4);
        assert_eq!(decision.caps.read_tokens, 2);
        assert_eq!(decision.caps.upload_concurrency, 4);
        assert!(decision.caps.allow_reconcile);
        assert!(decision.caps.allow_hashing);
        assert!(decision.caps.allow_uploads);
    }

    #[test]
    fn battery_power_prevents_idle_drain() {
        let controller = ThrottleController::default();
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
        let controller = ThrottleController::default();
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
        let controller = ThrottleController::default();
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
        let controller = ThrottleController::default();
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
        let controller = ThrottleController::default();
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
        let controller = ThrottleController::default();
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
        let controller = ThrottleController::default();
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
        let controller = ThrottleController::default();
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
}
