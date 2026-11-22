//! Cross-thread control bits between the IPC server and the runtime
//! tick loop.
//!
//! IPC handler threads write into this struct via the
//! `set_*` methods. The runtime tick loop reads + clears them on each
//! tick via `take_*`. The pattern keeps the IPC layer decoupled from
//! the runtime: IPC never holds the runtime mutex, the runtime never
//! blocks on a channel.
//!
//! The first batch of control requests:
//!
//! - `pause` / `resume`: flip the run-state.
//! - `reconcile`: the user's on-demand sync: a whole-scope reconcile
//!   admitted under any throttle state but `Suspended`, plus a short
//!   boost that releases deferred work, so what the scan finds moves at
//!   once.

use std::sync::{Arc, Mutex};

use crate::runtime::TickWaker;

#[derive(Debug, Default)]
pub struct RuntimeControl {
    inner: Mutex<RuntimeControlInner>,
}

/// One of the three control requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlRequest {
    Pause,
    Resume,
    Reconcile,
}

#[derive(Debug, Default)]
struct RuntimeControlInner {
    /// `Some(true)` to flip the run-state to `Paused`; `Some(false)`
    /// to flip back to `Running`. `None` means no pending request.
    pause_request: Option<bool>,
    reconcile_pending: bool,
    /// Requests for one profile each, in the order they came. Only the
    /// daemon's own control holds any: the multi-profile runtime hands
    /// each one to the control of the profile it names.
    for_profile: Vec<(String, ControlRequest)>,
    /// Set by `DaemonRuntime::attach_control` so control requests wake
    /// the tick loop immediately instead of waiting out the sleep.
    waker: Option<Arc<TickWaker>>,
}

impl RuntimeControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wires the runtime's tick waker in. Called by
    /// `DaemonRuntime::attach_control`.
    pub(crate) fn set_waker(&self, waker: Arc<TickWaker>) {
        self.with_inner(|inner| inner.waker = Some(waker));
    }

    fn notify_waker(&self) {
        let waker = self.with_inner(|inner| inner.waker.clone());
        if let Some(waker) = waker {
            waker.notify();
        }
    }

    /// Mark "pause requested". Idempotent.
    pub fn request_pause(&self) {
        self.request_pause_or_resume(true);
    }

    /// Mark "resume requested". Idempotent.
    pub fn request_resume(&self) {
        self.request_pause_or_resume(false);
    }

    /// A pause or a resume for every profile replaces what was asked
    /// of single profiles before it. The reader applies this request
    /// first and the ones that name a profile after it, so the ones
    /// that are still in the list are the ones that came later.
    fn request_pause_or_resume(&self, pause: bool) {
        self.with_inner(|inner| {
            inner.pause_request = Some(pause);
            inner
                .for_profile
                .retain(|(_, request)| *request == ControlRequest::Reconcile);
        });
        self.notify_waker();
    }

    pub fn request_reconcile(&self) {
        self.with_inner(|inner| inner.reconcile_pending = true);
        self.notify_waker();
    }

    /// A request for the profile `profile` alone.
    pub fn request_for_profile(&self, profile: &str, request: ControlRequest) {
        self.with_inner(|inner| inner.for_profile.push((profile.to_string(), request)));
        self.notify_waker();
    }

    /// Returns + clears the requests that name a profile.
    pub fn take_profile_requests(&self) -> Vec<(String, ControlRequest)> {
        self.with_inner(|inner| std::mem::take(&mut inner.for_profile))
    }

    /// Returns + clears any pending pause/resume request.
    pub fn take_pause_request(&self) -> Option<bool> {
        self.with_inner(|inner| inner.pause_request.take())
    }

    /// Returns + clears any pending reconcile request.
    pub fn take_reconcile_request(&self) -> bool {
        self.with_inner(|inner| std::mem::take(&mut inner.reconcile_pending))
    }

    fn with_inner<R>(&self, body: impl FnOnce(&mut RuntimeControlInner) -> R) -> R {
        let mut guard = self.inner.lock().expect("RuntimeControl mutex poisoned");
        body(&mut guard)
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    #[test]
    fn a_request_for_every_profile_replaces_the_earlier_ones_for_one_profile() {
        let control = RuntimeControl::new();
        control.request_for_profile("work", ControlRequest::Pause);
        control.request_for_profile("work", ControlRequest::Reconcile);
        control.request_resume();
        control.request_for_profile("home", ControlRequest::Pause);

        assert_eq!(control.take_pause_request(), Some(false));
        assert_eq!(
            control.take_profile_requests(),
            vec![
                ("work".to_string(), ControlRequest::Reconcile),
                ("home".to_string(), ControlRequest::Pause),
            ],
            "the pause of work came before the resume of everything, and is gone"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_request_round_trips_once_and_clears_after_take() {
        let control = RuntimeControl::new();
        assert_eq!(control.take_pause_request(), None);
        control.request_pause();
        assert_eq!(control.take_pause_request(), Some(true));
        assert_eq!(control.take_pause_request(), None);
    }

    #[test]
    fn resume_overwrites_pause_request() {
        let control = RuntimeControl::new();
        control.request_pause();
        control.request_resume();
        assert_eq!(control.take_pause_request(), Some(false));
    }

    #[test]
    fn requests_for_a_profile_keep_their_order_and_clear_after_take() {
        let control = RuntimeControl::new();
        control.request_for_profile("work", ControlRequest::Pause);
        control.request_for_profile("home", ControlRequest::Reconcile);
        control.request_for_profile("work", ControlRequest::Resume);
        assert_eq!(
            control.take_profile_requests(),
            vec![
                ("work".to_string(), ControlRequest::Pause),
                ("home".to_string(), ControlRequest::Reconcile),
                ("work".to_string(), ControlRequest::Resume),
            ]
        );
        assert!(control.take_profile_requests().is_empty());
        // A request for one profile is not a request for all of them.
        assert_eq!(control.take_pause_request(), None);
        assert!(!control.take_reconcile_request());
    }

    #[test]
    fn reconcile_request_clears_after_take() {
        let control = RuntimeControl::new();
        control.request_reconcile();
        assert!(control.take_reconcile_request());
        assert!(!control.take_reconcile_request());
    }
}
