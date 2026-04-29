//! `PlatformMetricsSampler` trait and the `StaticPlatformMetricsSampler`
//! fallback. The native sampler (mach2 / IOKit on macOS, PSI on Linux,
//! GetSystemTimes on Windows) is stubbed for now and forwards to the
//! static sampler; full FFI lands incrementally as Wave 4 follow-ups.
//!
//! See `docs/architecture/platform-abstractions.md` §`PlatformMetricsSampler`.
//! `StaticPlatformMetricsSampler` mirrors `core/daemon::metrics::
//! StaticMetricsSampler` from C2-1 — it returns a fixed snapshot every
//! tick — but it lives here so the engine can substitute the platform
//! trait directly without taking a dependency on `core/daemon`.

use std::sync::Mutex;

/// Snapshot of throttle inputs returned by the platform sampler.
///
/// Mirrors the `ThrottleInputs` shape from
/// `core/daemon::throttle::ThrottleInputs`. Kept structurally identical
/// so the engine can convert without re-deriving anything; we duplicate
/// the type here to keep `core/platform` independent of `core/daemon`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThrottleInputsSnapshot {
    pub on_battery: bool,
    pub low_power_mode: bool,
    pub system_cpu_load_percent: u8,
    pub vapor_cpu_load_percent: u8,
    pub network_error_rate_percent: u8,
    pub network_throughput_kbps: Option<u32>,
    pub user_active: bool,
}

impl Default for ThrottleInputsSnapshot {
    fn default() -> Self {
        Self {
            on_battery: false,
            low_power_mode: false,
            system_cpu_load_percent: 10,
            vapor_cpu_load_percent: 2,
            network_error_rate_percent: 0,
            network_throughput_kbps: Some(10_000),
            user_active: false,
        }
    }
}

pub trait PlatformMetricsSampler: Send + Sync {
    fn sample(&self) -> ThrottleInputsSnapshot;
}

/// Sampler that returns a fixed `ThrottleInputs` every tick. Used by the
/// CLI / headless paths and by tests; also the current default on every
/// OS until the per-OS native bridges (mach2 / PSI / PDH) land.
#[derive(Debug)]
pub struct StaticPlatformMetricsSampler {
    snapshot: Mutex<ThrottleInputsSnapshot>,
}

impl StaticPlatformMetricsSampler {
    pub fn new(snapshot: ThrottleInputsSnapshot) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
        }
    }

    pub fn set(&self, snapshot: ThrottleInputsSnapshot) {
        *self
            .snapshot
            .lock()
            .expect("StaticPlatformMetricsSampler mutex poisoned") = snapshot;
    }
}

impl Default for StaticPlatformMetricsSampler {
    fn default() -> Self {
        Self::new(ThrottleInputsSnapshot::default())
    }
}

impl PlatformMetricsSampler for StaticPlatformMetricsSampler {
    fn sample(&self) -> ThrottleInputsSnapshot {
        *self
            .snapshot
            .lock()
            .expect("StaticPlatformMetricsSampler mutex poisoned")
    }
}

/// In-memory fake. Equivalent to [`StaticPlatformMetricsSampler`] but
/// exposed under the `InMemory*` naming so the trait catalog is
/// uniform.
pub type InMemoryPlatformMetricsSampler = StaticPlatformMetricsSampler;

/// Native sampler. Until the per-OS bridges land it forwards to the
/// static sampler; the trait surface is the stable seam Wave 4
/// commits to.
#[derive(Debug, Default)]
pub struct NativePlatformMetricsSampler {
    fallback: StaticPlatformMetricsSampler,
}

impl NativePlatformMetricsSampler {
    pub fn for_current_host() -> Self {
        Self::default()
    }
}

impl PlatformMetricsSampler for NativePlatformMetricsSampler {
    fn sample(&self) -> ThrottleInputsSnapshot {
        self.fallback.sample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_sampler_returns_seeded_snapshot() {
        let sampler = StaticPlatformMetricsSampler::new(ThrottleInputsSnapshot {
            on_battery: true,
            ..Default::default()
        });
        assert!(sampler.sample().on_battery);
    }

    #[test]
    fn static_sampler_set_overrides_subsequent_samples() {
        let sampler = StaticPlatformMetricsSampler::default();
        assert!(!sampler.sample().on_battery);
        sampler.set(ThrottleInputsSnapshot {
            on_battery: true,
            ..Default::default()
        });
        assert!(sampler.sample().on_battery);
    }
}
