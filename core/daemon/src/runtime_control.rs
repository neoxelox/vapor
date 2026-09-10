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
//! - `pause` / `resume` — flip the run-state.
//! - `flush_now` — informational nudge; the runtime drains
//!   opportunistically already.
//! - `reconcile` — schedule a fresh whole-scope reconcile against the
//!   local sync directory.
//! - `sync_now` — the user's on-demand sync: a whole-scope reconcile
//!   admitted under any throttle state but `Suspended`, plus the flush
//!   boost, so what the scan finds moves at once.

use std::sync::{Arc, Mutex};

use crate::runtime::TickWaker;

#[derive(Debug, Default)]
pub struct RuntimeControl {
    inner: Mutex<RuntimeControlInner>,
}

#[derive(Debug, Default)]
struct RuntimeControlInner {
    /// `Some(true)` to flip the run-state to `Paused`; `Some(false)`
    /// to flip back to `Running`. `None` means no pending request.
    pause_request: Option<bool>,
    flush_pending: bool,
    reconcile_pending: bool,
    sync_now_pending: bool,
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
        self.with_inner(|inner| inner.pause_request = Some(true));
        self.notify_waker();
    }

    /// Mark "resume requested". Idempotent.
    pub fn request_resume(&self) {
        self.with_inner(|inner| inner.pause_request = Some(false));
        self.notify_waker();
    }

    pub fn request_flush(&self) {
        self.with_inner(|inner| inner.flush_pending = true);
        self.notify_waker();
    }

    pub fn request_reconcile(&self) {
        self.with_inner(|inner| inner.reconcile_pending = true);
        self.notify_waker();
    }

    pub fn request_sync_now(&self) {
        self.with_inner(|inner| inner.sync_now_pending = true);
        self.notify_waker();
    }

    /// Returns + clears any pending pause/resume request.
    pub fn take_pause_request(&self) -> Option<bool> {
        self.with_inner(|inner| inner.pause_request.take())
    }

    /// Returns + clears any pending flush request.
    pub fn take_flush_request(&self) -> bool {
        self.with_inner(|inner| std::mem::take(&mut inner.flush_pending))
    }

    /// Returns + clears any pending reconcile request.
    pub fn take_reconcile_request(&self) -> bool {
        self.with_inner(|inner| std::mem::take(&mut inner.reconcile_pending))
    }

    /// Returns + clears any pending sync-now request.
    pub fn take_sync_now_request(&self) -> bool {
        self.with_inner(|inner| std::mem::take(&mut inner.sync_now_pending))
    }

    fn with_inner<R>(&self, body: impl FnOnce(&mut RuntimeControlInner) -> R) -> R {
        let mut guard = self.inner.lock().expect("RuntimeControl mutex poisoned");
        body(&mut guard)
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
    fn flush_request_clears_after_take() {
        let control = RuntimeControl::new();
        control.request_flush();
        assert!(control.take_flush_request());
        assert!(!control.take_flush_request());
    }

    #[test]
    fn reconcile_request_clears_after_take() {
        let control = RuntimeControl::new();
        control.request_reconcile();
        assert!(control.take_reconcile_request());
        assert!(!control.take_reconcile_request());
    }
}
