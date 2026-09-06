//! User-idle notifier trait, the fakes, and the per-OS native source.
//!
//! See `docs/architecture/platform-abstractions.md` §`IdleNotifier`.
//! macOS reads the HID idle clock (`macos.rs`). Linux and Windows are
//! not shipping surfaces yet; their `NativeIdleNotifier` reports zero
//! idle time, which keeps idle boost off rather than running boosted
//! ceilings on an unmeasured host.

use std::sync::Mutex;
use std::time::Duration;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::NativeIdleNotifier;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::NativeIdleNotifier;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::NativeIdleNotifier;

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

impl AlwaysIdleNotifier {
    pub const IDLE_FOR: Duration = Duration::from_secs(86_400);
}

impl IdleNotifier for AlwaysIdleNotifier {
    fn idle_for(&self) -> Duration {
        Self::IDLE_FOR
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

    /// With a window-server session the HID clock answers; without one
    /// (SSH, CI agents) the notifier reports the headless always-idle
    /// reading. Either way the value is finite and never panics.
    #[cfg(target_os = "macos")]
    #[test]
    fn native_notifier_reports_a_reading_for_the_current_session() {
        let notifier = NativeIdleNotifier::for_current_host();
        let idle_for = notifier.idle_for();
        if notifier.has_gui_session() {
            assert!(idle_for < AlwaysIdleNotifier::IDLE_FOR);
        } else {
            assert_eq!(idle_for, AlwaysIdleNotifier::IDLE_FOR);
        }
    }
}
