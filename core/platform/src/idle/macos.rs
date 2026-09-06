//! macOS `IdleNotifier` on the HID idle clock.
//!
//! `CGEventSourceSecondsSinceLastEventType` reports the time since the
//! last keyboard, pointer, or tablet event system-wide. It needs a
//! window-server session; a daemon started over SSH or by a CI agent has
//! none, so the notifier checks once at construction and reports the
//! headless always-idle reading in that case, matching the CLI default.
#![allow(unsafe_code)]

use std::time::Duration;

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionaryRef;

use super::{AlwaysIdleNotifier, IdleNotifier};

/// `kCGEventSourceStateHIDSystemState`.
const HID_SYSTEM_STATE: u32 = 1;
/// `kCGAnyInputEventType`.
const ANY_INPUT_EVENT: u32 = u32::MAX;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: u32, event_type: u32) -> f64;
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

#[derive(Debug)]
pub struct NativeIdleNotifier {
    gui_session: bool,
}

impl NativeIdleNotifier {
    pub fn for_current_host() -> Self {
        Self {
            gui_session: gui_session_available(),
        }
    }

    /// Whether the process runs inside a window-server session, which
    /// is what makes the HID idle clock meaningful.
    pub fn has_gui_session(&self) -> bool {
        self.gui_session
    }
}

impl IdleNotifier for NativeIdleNotifier {
    fn idle_for(&self) -> Duration {
        if !self.gui_session {
            return AlwaysIdleNotifier.idle_for();
        }
        // SAFETY: plain value-returning call with constant arguments.
        let seconds =
            unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) };
        if !seconds.is_finite() || seconds < 0.0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(seconds)
    }
}

fn gui_session_available() -> bool {
    // SAFETY: no arguments; returns NULL outside a login session,
    // otherwise a +1 dictionary we release immediately.
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    if session.is_null() {
        return false;
    }
    // SAFETY: non-null, owned by us (create rule).
    drop(unsafe { CFType::wrap_under_create_rule(session as CFTypeRef) });
    true
}
