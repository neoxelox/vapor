#![forbid(unsafe_code)]

use std::time::Duration;

pub mod config;
pub mod constants;
pub mod logging;
pub mod runtime_paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleState {
    IdleDrain,
    Light,
    Throttled,
    Suspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Starting,
    Running,
    Paused,
    Error,
}

/// Thermal pressure tiers reported by the platform metrics sampler and
/// consumed by the throttle controller. Lives in `vapor-shared` so the
/// platform layer and the engine speak one type instead of mirroring it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThermalPressure {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl ThermalPressure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        }
    }
}

/// Generic resource-pressure tiers (disk today; extendable) shared between
/// the platform sampler and the throttle controller.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResourcePressure {
    #[default]
    Nominal,
    Elevated,
    High,
    Severe,
}

impl ResourcePressure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Elevated => "elevated",
            Self::High => "high",
            Self::Severe => "severe",
        }
    }
}

/// One sample of every signal the throttle controller evaluates. Produced
/// by `core/platform`'s `PlatformMetricsSampler` implementations and
/// consumed by `core/daemon`'s throttle controller — a single shared type
/// so the two layers can never drift structurally.
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

/// Failure taxonomy shared by the retry policy, the durable queue, and the
/// provider trait. Providers classify their own failures with this enum so
/// the engine's retry machinery never has to parse error strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryFailureKind {
    Transient,
    RateLimited { retry_after: Option<Duration> },
    Authentication,
    Permanent,
}

impl RetryFailureKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::RateLimited { .. } => "rate_limited",
            Self::Authentication => "authentication",
            Self::Permanent => "permanent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub run_state: RunState,
    pub throttle_state: ThrottleState,
    pub reason: String,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            run_state: RunState::Starting,
            throttle_state: ThrottleState::Light,
            reason: "starting up".to_string(),
        }
    }
}

impl StatusSnapshot {
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_snapshot_starts_in_starting_state() {
        let snapshot = StatusSnapshot::default();
        assert_eq!(snapshot.run_state, RunState::Starting);
        assert_eq!(snapshot.throttle_state, ThrottleState::Light);
    }

    #[test]
    fn reason_builder_overrides_reason_text() {
        let snapshot = StatusSnapshot::default().with_reason("idle");
        assert_eq!(snapshot.reason, "idle");
    }

    #[test]
    fn pressure_labels_are_lowercase_identifiers() {
        assert_eq!(ThermalPressure::Critical.label(), "critical");
        assert_eq!(ResourcePressure::Severe.label(), "severe");
        assert_eq!(
            RetryFailureKind::RateLimited { retry_after: None }.label(),
            "rate_limited"
        );
    }
}
