//! User-idle notifier trait + per-OS native implementation.
//!
//! See `docs/architecture/platform-abstractions.md` §`IdleNotifier`. The
//! macOS bridge (`CGEventSourceSecondsSinceLastEventType`) is not
//! wired up yet; until then `NativeIdleNotifier` forwards to
//! [`AlwaysIdleNotifier`], which is also what the headless CLI uses.

use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserActivity {
    Active,
    Idle,
}

pub trait IdleNotifier: Send + Sync {
    /// How long the user has been idle. Returns `Duration::ZERO` when
    /// there is no signal available (or the user is currently active).
    fn idle_for(&self) -> Duration;

    /// Cheap derived view: `Active` when `idle_for` is below
    /// `idle_threshold`, otherwise `Idle`.
    fn user_activity(&self, idle_threshold: Duration) -> UserActivity {
        if self.idle_for() >= idle_threshold {
            UserActivity::Idle
        } else {
            UserActivity::Active
        }
    }
}

/// Reports the user as always-idle. Headless / CLI default.
#[derive(Debug, Default)]
pub struct AlwaysIdleNotifier;

impl IdleNotifier for AlwaysIdleNotifier {
    fn idle_for(&self) -> Duration {
        Duration::from_secs(86_400)
    }
}

/// Test fake whose `idle_for` reading is set explicitly.
#[derive(Debug, Default)]
pub struct ManualIdleNotifier {
    inner: Mutex<Duration>,
}

impl ManualIdleNotifier {
    pub fn new(idle_for: Duration) -> Self {
        Self {
            inner: Mutex::new(idle_for),
        }
    }

    pub fn set(&self, idle_for: Duration) {
        *self
            .inner
            .lock()
            .expect("ManualIdleNotifier mutex poisoned") = idle_for;
    }
}

impl IdleNotifier for ManualIdleNotifier {
    fn idle_for(&self) -> Duration {
        *self
            .inner
            .lock()
            .expect("ManualIdleNotifier mutex poisoned")
    }
}

/// Native idle notifier. Forwards to [`AlwaysIdleNotifier`] until the
/// per-OS HID bridges land; the trait surface is stable and the
/// native back-ends arrive incrementally.
#[derive(Debug, Default)]
pub struct NativeIdleNotifier;

impl NativeIdleNotifier {
    pub fn for_current_host() -> Self {
        Self
    }
}

impl IdleNotifier for NativeIdleNotifier {
    fn idle_for(&self) -> Duration {
        // No native HID idle bridge yet. Fail safe for device impact:
        // report zero idle time so the idle-boost gate is never satisfied
        // on unmeasured activity. The old always-idle stub ran at boosted
        // ceilings (50% CPU / 80% bandwidth) regardless of what the user
        // was doing — the opposite of the product's low-impact priority.
        // A real per-OS bridge (CGEventSource seconds-since-last-input /
        // IOHIDSystem HIDIdleTime) replaces this.
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_idle_notifier_classifies_user_as_idle_against_threshold() {
        let notifier = AlwaysIdleNotifier;
        assert_eq!(
            notifier.user_activity(Duration::from_secs(60)),
            UserActivity::Idle
        );
    }

    #[test]
    fn manual_idle_notifier_set_changes_classification() {
        let notifier = ManualIdleNotifier::new(Duration::from_secs(0));
        assert_eq!(
            notifier.user_activity(Duration::from_secs(30)),
            UserActivity::Active
        );
        notifier.set(Duration::from_secs(120));
        assert_eq!(
            notifier.user_activity(Duration::from_secs(30)),
            UserActivity::Idle
        );
    }
}
