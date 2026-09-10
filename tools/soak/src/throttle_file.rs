//! Scripts the daemon's throttle inputs through the `file:` source.
//! The driver rewrites the document; the daemon re-reads it on every
//! sample, so a walk through the throttle states is a sequence of
//! writes with time in between.

use std::path::{Path, PathBuf};

use vapor_shared::{ThermalPressure, ThrottleInputs};

use crate::Failure;

/// A named point on the throttle walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Station {
    /// Idle, plugged in, cool: the daemon drains at full width.
    Idle,
    /// On battery: `Light`.
    Battery,
    /// The user is active: `Throttled`.
    UserActive,
    /// Severe system load and thermal pressure: `Suspended`.
    Overloaded,
}

impl Station {
    pub fn label(self) -> &'static str {
        match self {
            Station::Idle => "idle",
            Station::Battery => "battery",
            Station::UserActive => "user-active",
            Station::Overloaded => "overloaded",
        }
    }

    pub fn inputs(self) -> (ThrottleInputs, u64) {
        match self {
            Station::Idle => (ThrottleInputs::default(), 600),
            Station::Battery => (
                ThrottleInputs {
                    on_battery: true,
                    ..ThrottleInputs::default()
                },
                600,
            ),
            Station::UserActive => (
                ThrottleInputs {
                    user_active: true,
                    system_cpu_load_percent: 45,
                    ..ThrottleInputs::default()
                },
                0,
            ),
            Station::Overloaded => (
                ThrottleInputs {
                    user_active: true,
                    system_cpu_load_percent: 95,
                    thermal_pressure: ThermalPressure::Critical,
                    ..ThrottleInputs::default()
                },
                0,
            ),
        }
    }

    pub const WALK: &'static [Station] = &[
        Station::Idle,
        Station::Battery,
        Station::UserActive,
        Station::Overloaded,
        Station::UserActive,
        Station::Idle,
    ];
}

#[derive(Clone, Debug)]
pub struct ThrottleScript {
    pub path: PathBuf,
}

impl ThrottleScript {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// The value for `VAPOR_THROTTLE_INPUTS`.
    pub fn env_value(&self) -> String {
        format!(
            "{}{}",
            vapor_shared::constants::engine::THROTTLE_INPUTS_FILE_PREFIX,
            self.path.display()
        )
    }

    pub fn set(&self, station: Station) -> Result<(), Failure> {
        let (inputs, idle_seconds) = station.inputs();
        let document = serde_json::json!({
            "inputs": inputs,
            "idle_seconds": idle_seconds,
        });
        // Write through a temp file so the daemon never reads a partial
        // document (it would fall back to defaults for one sample).
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_string_pretty(&document)?)?;
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }
}
