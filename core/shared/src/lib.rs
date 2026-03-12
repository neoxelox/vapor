#![forbid(unsafe_code)]

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
}
