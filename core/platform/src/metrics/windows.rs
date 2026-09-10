//! Windows `PlatformMetricsSampler` stub: static default inputs until
//! the GetSystemTimes / power-status bridge lands with the Windows surface.

use super::{PlatformMetricsSampler, StaticPlatformMetricsSampler, ThrottleInputs};

#[derive(Debug, Default)]
pub struct NativePlatformMetricsSampler {
    fallback: StaticPlatformMetricsSampler,
}

impl NativePlatformMetricsSampler {
    pub fn for_current_host() -> Self {
        Self::default()
    }

    /// `false`: every input is a placeholder on this OS.
    pub fn has_native_sampling() -> bool {
        false
    }

    /// The inputs this host feeds the throttle, for logs and doctor.
    pub fn input_sources() -> &'static str {
        "none; every input is a placeholder on Windows"
    }
}

impl PlatformMetricsSampler for NativePlatformMetricsSampler {
    fn sample(&self) -> ThrottleInputs {
        self.fallback.sample()
    }
}
