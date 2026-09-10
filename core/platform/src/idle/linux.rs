//! Linux `IdleNotifier`.
//!
//! Idle time on a Linux desktop lives with the display server, and
//! reading it means an X11 screensaver query or a Wayland idle-notify
//! protocol, each a client library of its own. Neither is wired yet.
//! What this notifier knows is whether a graphical session exists at
//! all (`DISPLAY` or `WAYLAND_DISPLAY`): without one, a server or a
//! container, the host is idle by definition and idle boost may run;
//! with one, the reading is zero, so idle boost stays off rather than
//! running boosted ceilings while someone may be typing.

use std::time::Duration;

use super::{AlwaysIdleNotifier, IdleNotifier};

#[derive(Debug)]
pub struct NativeIdleNotifier {
    gui_session: bool,
}

impl Default for NativeIdleNotifier {
    fn default() -> Self {
        Self::for_current_host()
    }
}

impl NativeIdleNotifier {
    pub fn for_current_host() -> Self {
        let gui_session = ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
        Self { gui_session }
    }

    /// Whether a display server session exists, which is what would
    /// make a user-idle reading meaningful.
    pub fn has_gui_session(&self) -> bool {
        self.gui_session
    }
}

impl IdleNotifier for NativeIdleNotifier {
    fn idle_for(&self) -> Duration {
        if self.gui_session {
            Duration::ZERO
        } else {
            AlwaysIdleNotifier.idle_for()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_headless_host_is_always_idle_and_a_desktop_is_never_boosted() {
        let headless = NativeIdleNotifier { gui_session: false };
        assert_eq!(headless.idle_for(), AlwaysIdleNotifier::IDLE_FOR);
        let desktop = NativeIdleNotifier { gui_session: true };
        assert_eq!(desktop.idle_for(), Duration::ZERO);
        assert!(desktop.has_gui_session());
    }
}
