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

use std::sync::Mutex;

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
}
