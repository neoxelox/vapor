//! Linux `IdleNotifier` stub. Reports zero idle time so idle boost never
//! engages on an unmeasured host; the X11 / Wayland bridge lands with
//! the Linux surface.

use std::time::Duration;

use super::IdleNotifier;

#[derive(Debug, Default)]
pub struct NativeIdleNotifier;

impl NativeIdleNotifier {
    pub fn for_current_host() -> Self {
        Self
    }

    pub fn has_gui_session(&self) -> bool {
        false
    }
}

impl IdleNotifier for NativeIdleNotifier {
    fn idle_for(&self) -> Duration {
        Duration::ZERO
    }
}
