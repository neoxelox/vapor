//! Throttle-input sampling for the daemon runtime.
//!
//! The trait is `core/platform`'s [`PlatformMetricsSampler`], consumed
//! directly (the engine already depends on `core/platform`; a duplicate
//! engine-side trait bought nothing but drift). The runtime tick path
//! samples fresh [`ThrottleInputs`] from it every sample interval.
//!
//! Production wires [`NativePlatformMetricsSampler`] (per-OS FFI lands
//! incrementally; it currently forwards static idle inputs). Tests use
//! [`StaticMetricsSampler`] for fixed inputs or [`ScriptedMetricsSampler`]
//! to walk a deterministic sequence.
//!

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::throttle::ThrottleInputs;

pub use vapor_platform::{
    NativePlatformMetricsSampler, PlatformMetricsSampler as MetricsSampler,
    StaticPlatformMetricsSampler as StaticMetricsSampler,
};

/// Test sampler that walks through a scripted sequence of [`ThrottleInputs`].
/// After the sequence is exhausted, the sampler keeps returning the last
/// value (so a finite script does not poison long tick loops).
pub struct ScriptedMetricsSampler {
    state: Mutex<ScriptState>,
}

struct ScriptState {
    sequence: Vec<ThrottleInputs>,
    cursor: usize,
}

impl ScriptedMetricsSampler {
    pub fn new(sequence: Vec<ThrottleInputs>) -> Self {
        assert!(
            !sequence.is_empty(),
            "ScriptedMetricsSampler requires at least one element"
        );
        Self {
            state: Mutex::new(ScriptState {
                sequence,
                cursor: 0,
            }),
        }
    }
}

impl MetricsSampler for ScriptedMetricsSampler {
    fn sample(&self) -> ThrottleInputs {
        let mut state = self.state.lock().expect("ScriptedMetricsSampler poisoned");
        let index = state.cursor.min(state.sequence.len() - 1);
        let value = state.sequence[index];
        if state.cursor < state.sequence.len() - 1 {
            state.cursor += 1;
        }
        value
    }
}

/// The document a `file:` throttle-input source reads.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ThrottleInputsFile {
    pub inputs: ThrottleInputs,
    /// How long the user has been idle, as the idle notifier should
    /// report it.
    pub idle_seconds: u64,
}

impl ThrottleInputsFile {
    pub fn read(path: &Path) -> Option<Self> {
        let contents = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&contents).ok()
    }
}

/// Samples throttle inputs from a JSON file on every call. A missing
/// or malformed file samples as the static defaults, so a driver that
/// has not written yet, or a document mid-write, never wedges the
/// daemon.
#[derive(Debug)]
pub struct FileMetricsSampler {
    path: PathBuf,
}

impl FileMetricsSampler {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl MetricsSampler for FileMetricsSampler {
    fn sample(&self) -> ThrottleInputs {
        ThrottleInputsFile::read(&self.path)
            .map(|document| document.inputs)
            .unwrap_or_default()
    }
}

/// Idle notifier that reads `idle_seconds` from the same file.
#[derive(Debug)]
pub struct FileIdleNotifier {
    path: PathBuf,
}

impl FileIdleNotifier {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl vapor_platform::IdleNotifier for FileIdleNotifier {
    fn idle_for(&self) -> Duration {
        ThrottleInputsFile::read(&self.path)
            .map(|document| Duration::from_secs(document.idle_seconds))
            .unwrap_or(Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::throttle::ThermalPressure;
    use vapor_platform::IdleNotifier;

    #[test]
    fn static_sampler_returns_configured_inputs() {
        let inputs = ThrottleInputs {
            on_battery: true,
            ..ThrottleInputs::default()
        };
        let sampler = StaticMetricsSampler::new(inputs);
        let sampled = sampler.sample();
        assert!(sampled.on_battery);
        assert_eq!(sampled.system_cpu_load_percent, 10);
    }

    #[test]
    fn static_sampler_default_returns_throttle_default_inputs() {
        let sampler = StaticMetricsSampler::default();
        assert_eq!(sampler.sample(), ThrottleInputs::default());
    }

    #[test]
    fn scripted_sampler_advances_through_sequence_then_clamps_to_last() {
        let first = ThrottleInputs::default();
        let second = ThrottleInputs {
            thermal_pressure: ThermalPressure::Fair,
            ..ThrottleInputs::default()
        };
        let sampler = ScriptedMetricsSampler::new(vec![first, second]);

        assert_eq!(sampler.sample(), first);
        assert_eq!(sampler.sample(), second);
        // Sequence exhausted: subsequent samples clamp to the last value.
        assert_eq!(sampler.sample(), second);
        assert_eq!(sampler.sample(), second);
    }

    #[test]
    #[should_panic(expected = "ScriptedMetricsSampler requires at least one element")]
    fn scripted_sampler_rejects_empty_sequence() {
        let _sampler = ScriptedMetricsSampler::new(Vec::new());
    }
    #[test]
    fn file_sampler_reads_the_document_on_every_sample_and_defaults_when_absent() {
        let dir = tempfile::TempDir::new().expect("dir");
        let path = dir.path().join("inputs.json");
        let sampler = FileMetricsSampler::new(&path);
        let idle = FileIdleNotifier::new(&path);
        assert_eq!(sampler.sample(), ThrottleInputs::default());
        assert_eq!(idle.idle_for(), Duration::ZERO);

        let document = ThrottleInputsFile {
            inputs: ThrottleInputs {
                on_battery: true,
                system_cpu_load_percent: 90,
                user_active: true,
                ..ThrottleInputs::default()
            },
            idle_seconds: 42,
        };
        std::fs::write(&path, serde_json::to_string(&document).expect("json")).expect("write");
        let sampled = sampler.sample();
        assert!(sampled.on_battery && sampled.user_active);
        assert_eq!(sampled.system_cpu_load_percent, 90);
        assert_eq!(idle.idle_for(), Duration::from_secs(42));

        // A half-written document never wedges the daemon.
        std::fs::write(&path, "{\"inputs\": {").expect("write");
        assert_eq!(sampler.sample(), ThrottleInputs::default());
    }
}
