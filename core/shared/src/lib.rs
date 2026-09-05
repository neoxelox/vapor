#![forbid(unsafe_code)]

use std::time::Duration;

pub mod config;
pub mod constants;
pub mod device_id;
pub mod logging;
pub mod paths;
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
            // No measurement yet: report None so the bandwidth shaper uses
            // the assumed-link-capacity fallback (~100 Mbps) rather than a
            // hard-coded 10 Mbps placeholder that capped every transfer at
            // ~312 KB/s. A real per-OS throughput measurement, when it
            // lands, will supply a concrete value here.
            network_throughput_kbps: None,
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

/// Provider-neutral error taxonomy. Every provider classifies its
/// failures with this enum; the engine maps it onto [`RetryFailureKind`]
/// for retry scheduling and keeps the richer classification for
/// diagnostics and race resolution (`NotFound` and `PreconditionFailed`
/// carry meaning the retry policy alone does not need).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderErrorKind {
    /// Recoverable environment failure (network blip, EINTR, 5xx).
    Transient,
    /// The provider asked us to slow down (429; quota exhaustion).
    RateLimited { retry_after: Option<Duration> },
    /// Credentials are missing, expired beyond refresh, or revoked.
    Authentication,
    /// The remote object changed underneath the planned operation
    /// (etag/revision mismatch, concurrent writer). Retryable after
    /// re-planning against fresh remote state.
    PreconditionFailed,
    /// The remote object does not exist. Terminal for the operation as
    /// planned, but callers may treat it as convergence (deleting a
    /// file that is already gone is success, not failure).
    NotFound,
    /// The provider's cloud sync root itself is gone or unreachable
    /// (deleted out from under a running daemon, unmounted volume,
    /// remote folder removed). Self-healing: the engine blocks new work,
    /// re-ensures the root, and reconciles — individual intents must
    /// wait, not fail terminally.
    CloudRootUnavailable,
    /// Anything that will keep failing no matter how often we retry.
    Permanent,
}

impl ProviderErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::RateLimited { .. } => "rate_limited",
            Self::Authentication => "authentication",
            Self::PreconditionFailed => "precondition_failed",
            Self::NotFound => "not_found",
            Self::CloudRootUnavailable => "cloud_root_unavailable",
            Self::Permanent => "permanent",
        }
    }

    /// Collapses the provider taxonomy onto the retry taxonomy the durable
    /// queue schedules with. `PreconditionFailed` retries as transient
    /// (the re-lease re-plans against fresh remote state); `NotFound`
    /// finalizes as permanent unless the caller already resolved it as
    /// convergence.
    pub fn retry_classification(self) -> RetryFailureKind {
        match self {
            Self::Transient => RetryFailureKind::Transient,
            Self::RateLimited { retry_after } => RetryFailureKind::RateLimited { retry_after },
            Self::Authentication => RetryFailureKind::Authentication,
            Self::PreconditionFailed => RetryFailureKind::Transient,
            // Self-healing condition: the runtime blocks admission and
            // re-ensures the root, so the intent retries once sync
            // resumes instead of finalizing into failed_intents.
            Self::CloudRootUnavailable => RetryFailureKind::Transient,
            Self::NotFound | Self::Permanent => RetryFailureKind::Permanent,
        }
    }
}

/// Direction selector for a sync scope. `TwoWay` is the default
/// and the historical behavior; the one-way modes are strict mirrors and
/// opt-in per profile. Full design: `docs/architecture/sync-modes.md`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyncMode {
    #[default]
    TwoWay,
    PullOnly,
    PushOnly,
}

impl SyncMode {
    /// The wire/config value (`vapor.json` `syncMode` field).
    pub fn as_config_value(self) -> &'static str {
        match self {
            Self::TwoWay => crate::constants::sync_mode::TWO_WAY,
            Self::PullOnly => crate::constants::sync_mode::PULL_ONLY,
            Self::PushOnly => crate::constants::sync_mode::PUSH_ONLY,
        }
    }

    /// Parses a config value; `None` for unknown values so callers can
    /// surface an actionable error instead of silently defaulting a
    /// destructive mode selector.
    pub fn from_config_value(raw: &str) -> Option<Self> {
        match raw.trim() {
            v if v == crate::constants::sync_mode::TWO_WAY => Some(Self::TwoWay),
            v if v == crate::constants::sync_mode::PULL_ONLY => Some(Self::PullOnly),
            v if v == crate::constants::sync_mode::PUSH_ONLY => Some(Self::PushOnly),
            _ => None,
        }
    }

    /// Whether local changes may propagate to the cloud in this mode.
    pub fn allows_local_to_remote(self) -> bool {
        !matches!(self, Self::PullOnly)
    }

    /// Whether remote changes may propagate to local in this mode.
    pub fn allows_remote_to_local(self) -> bool {
        !matches!(self, Self::PushOnly)
    }

    /// One-way modes are strict mirrors: the subordinate side is driven to
    /// exactly match the source, permanently overwriting divergence.
    pub fn is_strict_mirror(self) -> bool {
        !matches!(self, Self::TwoWay)
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
