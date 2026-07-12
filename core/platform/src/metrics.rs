//! `PlatformMetricsSampler` trait and the `StaticPlatformMetricsSampler`
//! fallback. The native sampler (mach2 / IOKit on macOS, PSI on Linux,
//! GetSystemTimes on Windows) is stubbed for now and forwards to the
//! static sampler; the full FFI bridges land incrementally.
//!
//! See `docs/architecture/platform-abstractions.md` §`PlatformMetricsSampler`.
//! The sample type is `vapor_shared::ThrottleInputs` — one shared struct
//! consumed directly by the daemon's throttle controller, so the platform
//! layer and the engine can never drift structurally.

use std::sync::Mutex;

pub use vapor_shared::ThrottleInputs;

pub trait PlatformMetricsSampler: Send + Sync {
    fn sample(&self) -> ThrottleInputs;
}

/// Sampler that returns a fixed `ThrottleInputs` every tick. Used by the
/// CLI / headless paths and by tests; also the current default on every
/// OS until the per-OS native bridges (mach2 / PSI / PDH) land.
#[derive(Debug)]
pub struct StaticPlatformMetricsSampler {
    snapshot: Mutex<ThrottleInputs>,
}

impl StaticPlatformMetricsSampler {
    pub fn new(snapshot: ThrottleInputs) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
        }
    }

    pub fn set(&self, snapshot: ThrottleInputs) {
        *self
            .snapshot
            .lock()
            .expect("StaticPlatformMetricsSampler mutex poisoned") = snapshot;
    }
}

impl Default for StaticPlatformMetricsSampler {
    fn default() -> Self {
        Self::new(ThrottleInputs::default())
    }
}

impl PlatformMetricsSampler for StaticPlatformMetricsSampler {
    fn sample(&self) -> ThrottleInputs {
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
/// static sampler; the trait surface is the stable seam.
#[derive(Debug, Default)]
pub struct NativePlatformMetricsSampler {
    fallback: StaticPlatformMetricsSampler,
}

impl NativePlatformMetricsSampler {
    pub fn for_current_host() -> Self {
        Self::default()
    }

    /// Whether real per-OS sampling is wired in. `false` while the sampler
    /// forwards to the static fallback, so callers can warn that
    /// throttle inputs are placeholders. Flip to `true` when the native
    /// bridge lands.
    pub fn has_native_sampling() -> bool {
        false
    }
}

impl PlatformMetricsSampler for NativePlatformMetricsSampler {
    fn sample(&self) -> ThrottleInputs {
        self.fallback.sample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_sampler_returns_seeded_snapshot() {
        let sampler = StaticPlatformMetricsSampler::new(ThrottleInputs {
            on_battery: true,
            ..Default::default()
        });
        assert!(sampler.sample().on_battery);
    }

    #[test]
    fn static_sampler_set_overrides_subsequent_samples() {
        let sampler = StaticPlatformMetricsSampler::default();
        assert!(!sampler.sample().on_battery);
        sampler.set(ThrottleInputs {
            on_battery: true,
            ..Default::default()
        });
        assert!(sampler.sample().on_battery);
    }

    #[test]
    fn sample_carries_thermal_and_disk_pressure_fields() {
        // Regression guard for the pre-fix drift where the platform
        // snapshot type was missing `thermal_pressure` / `disk_pressure`
        // despite claiming structural identity with the engine's inputs.
        let sampler = StaticPlatformMetricsSampler::new(ThrottleInputs {
            thermal_pressure: vapor_shared::ThermalPressure::Serious,
            disk_pressure: vapor_shared::ResourcePressure::High,
            ..Default::default()
        });
        let sample = sampler.sample();
        assert_eq!(
            sample.thermal_pressure,
            vapor_shared::ThermalPressure::Serious
        );
        assert_eq!(sample.disk_pressure, vapor_shared::ResourcePressure::High);
    }
}
