//! `PlatformMetricsSampler` trait, the static fallback, and the per-OS
//! native sampler.
//!
//! See `docs/architecture/platform-abstractions.md` §`PlatformMetricsSampler`.
//! The sample type is `vapor_shared::ThrottleInputs`, one shared struct
//! consumed directly by the daemon's throttle controller, so the platform
//! layer and the engine can never drift structurally.
//!
//! macOS samples real host signals (`macos.rs`). Linux and Windows are
//! not shipping surfaces yet; their `NativePlatformMetricsSampler`
//! returns the static default inputs and reports
//! `has_native_sampling() == false` so the daemon can say so in its log.

use std::sync::Mutex;

pub use vapor_shared::ThrottleInputs;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::NativePlatformMetricsSampler;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::NativePlatformMetricsSampler;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::NativePlatformMetricsSampler;

pub trait PlatformMetricsSampler: Send + Sync {
    fn sample(&self) -> ThrottleInputs;
}

/// Sampler that returns a fixed `ThrottleInputs` every tick. Used by the
/// CLI / headless paths, by tests, and as the native fallback on OSes
/// without a sampler yet.
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

/// Converts two cumulative CPU readings into whole-device percentages.
///
/// `system_ticks` are the kernel's per-state tick counters (user,
/// system, idle, nice) before and after the window; `process_cpu` is the
/// process's accumulated CPU time before and after; `wall` is the window
/// length and `cpus` the online core count, so the process figure is a
/// share of the whole machine like the system figure. Shared by every
/// native sampler so the arithmetic is tested once.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn cpu_percentages(
    system_ticks: (&[u32; 4], &[u32; 4]),
    process_cpu: (std::time::Duration, std::time::Duration),
    wall: std::time::Duration,
    cpus: u32,
) -> (u8, u8) {
    let (before, after) = system_ticks;
    let deltas: Vec<u64> = before
        .iter()
        .zip(after.iter())
        .map(|(b, a)| u64::from(a.wrapping_sub(*b)))
        .collect();
    let total: u64 = deltas.iter().sum();
    let idle = deltas[2];
    let system_percent = if total == 0 {
        0
    } else {
        ((total - idle) * 100).div_ceil(total).min(100) as u8
    };

    let (before, after) = process_cpu;
    let used = after.saturating_sub(before);
    let capacity = wall.as_secs_f64() * f64::from(cpus.max(1));
    let vapor_percent = if capacity <= 0.0 {
        0
    } else {
        (used.as_secs_f64() / capacity * 100.0)
            .ceil()
            .clamp(0.0, 100.0) as u8
    };
    (system_percent, vapor_percent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
    fn cpu_percentages_report_busy_share_and_process_share_of_whole_device() {
        // 1000 ticks elapsed, 250 idle: 75% busy. The process used 1s of
        // CPU over a 2s window on 4 cores: 12.5% of the machine, rounded up.
        let before = [0, 0, 0, 0];
        let after = [500, 250, 250, 0];
        let (system, vapor) = cpu_percentages(
            (&before, &after),
            (Duration::ZERO, Duration::from_secs(1)),
            Duration::from_secs(2),
            4,
        );
        assert_eq!(system, 75);
        assert_eq!(vapor, 13);
    }

    #[test]
    fn cpu_percentages_survive_counter_wrap_and_empty_windows() {
        let before = [u32::MAX - 5, 0, u32::MAX - 5, 0];
        let after = [5, 0, 5, 0];
        let (system, _) = cpu_percentages(
            (&before, &after),
            (Duration::ZERO, Duration::ZERO),
            Duration::from_secs(1),
            8,
        );
        assert_eq!(system, 50);
        let (system, vapor) = cpu_percentages(
            (&[1, 1, 1, 1], &[1, 1, 1, 1]),
            (Duration::from_secs(3), Duration::from_secs(1)),
            Duration::ZERO,
            0,
        );
        assert_eq!((system, vapor), (0, 0));
    }

    #[test]
    fn cpu_percentages_never_exceed_one_hundred() {
        let (system, vapor) = cpu_percentages(
            (&[0, 0, 0, 0], &[10, 10, 0, 10]),
            (Duration::ZERO, Duration::from_secs(60)),
            Duration::from_secs(1),
            1,
        );
        assert_eq!((system, vapor), (100, 100));
    }

    /// The native sampler must return well-formed inputs on the host
    /// that runs the suite; the values themselves depend on the machine.
    #[cfg(target_os = "macos")]
    #[test]
    fn native_sampler_reports_real_host_signals() {
        let sampler = NativePlatformMetricsSampler::for_current_host();
        assert!(NativePlatformMetricsSampler::has_native_sampling());
        let first = sampler.sample();
        assert!(first.system_cpu_load_percent <= 100);
        assert!(first.vapor_cpu_load_percent <= 100);
        assert!(first.device_memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(first.vapor_memory_bytes.is_some_and(|bytes| bytes > 0));
        // A second call inside the cache window returns the same reading.
        assert_eq!(sampler.sample(), first);
    }
}
