//! Throttle-input sampling abstraction for the daemon runtime.
//!
//! `MetricsSampler` is the seam the runtime tick path consumes to obtain
//! a fresh [`ThrottleInputs`] every sample interval. Wave 4's
//! `core/platform/metrics::PlatformMetricsSampler` will plug in OS-native
//! samplers behind this trait.
//!
//! Pre-platform-layer code uses [`StaticMetricsSampler`] so the runtime
//! exercises real sampler plumbing instead of `ThrottleInputs::default()`.
//! The sampler is also valuable for the headless `vapor` CLI and for
//! deterministic tests, which feed scripted [`ThrottleInputs`] sequences
//! through [`ScriptedMetricsSampler`].
//!
//! See `docs/tasks/core.md` C2-1.

use std::sync::Mutex;

use crate::throttle::ThrottleInputs;

/// Per-tick sampler returning fresh [`ThrottleInputs`] for the runtime.
pub trait MetricsSampler: Send + Sync {
    fn sample(&self) -> ThrottleInputs;
}

/// Returns a fixed [`ThrottleInputs`] every tick. Equivalent to a config-
/// driven sampler: the engine treats the value as the current ground truth
/// without consulting any OS-specific signal.
#[derive(Clone, Debug)]
pub struct StaticMetricsSampler {
    inputs: ThrottleInputs,
}

impl StaticMetricsSampler {
    pub fn new(inputs: ThrottleInputs) -> Self {
        Self { inputs }
    }

    pub fn inputs(&self) -> ThrottleInputs {
        self.inputs
    }
}

impl Default for StaticMetricsSampler {
    fn default() -> Self {
        Self::new(ThrottleInputs::default())
    }
}

impl MetricsSampler for StaticMetricsSampler {
    fn sample(&self) -> ThrottleInputs {
        self.inputs
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::throttle::ThermalPressure;

    #[test]
    fn static_sampler_returns_configured_inputs() {
        let inputs = ThrottleInputs {
            on_battery: true,
            ..ThrottleInputs::default()
        };
        let sampler = StaticMetricsSampler::new(inputs);
        let sampled = sampler.sample();
        assert!(sampled.on_battery);
        assert_eq!(sampler.inputs().system_cpu_load_percent, 10);
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
}
