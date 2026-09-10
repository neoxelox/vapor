//! Bounded observation of an external process. Every wait has a hard
//! deadline on the wall clock and a named condition, so a timeout says
//! what never happened. This is not a timing assertion; the in-process
//! no-sleep rule of Tier 1 does not apply to watching a real daemon.

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::Failure;

pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How much longer every deadline gets, in percent of the scenario's
/// own budget. A run that shares the host between several sandboxes
/// sets this above 100: each daemon is then a program on a slower
/// computer, and a wait that would have been a timeout on a quiet
/// host is not a finding on a loaded one. Hold windows are not scaled;
/// they assert product semantics, not host speed.
static DEADLINE_SCALE_PERCENT: AtomicU32 = AtomicU32::new(100);

pub fn set_deadline_scale_percent(percent: u32) {
    DEADLINE_SCALE_PERCENT.store(percent.max(100), Ordering::Relaxed);
}

pub fn deadline_scale_percent() -> u32 {
    DEADLINE_SCALE_PERCENT.load(Ordering::Relaxed)
}

fn scaled(timeout: Duration) -> Duration {
    timeout.mul_f64(f64::from(deadline_scale_percent()) / 100.0)
}

/// Polls `probe` until it returns `true` or `timeout` elapses. The
/// deadline is measured on the clock, not in attempts, so a slow probe
/// (an IPC call against a wedged daemon) cannot stretch the wait.
pub fn wait_until(
    timeout: Duration,
    description: &str,
    mut probe: impl FnMut() -> bool,
) -> Result<(), Failure> {
    let timeout = scaled(timeout);
    let deadline = Instant::now() + timeout;
    loop {
        if probe() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Failure::new(format!(
                "timed out after {}s waiting for: {description}",
                timeout.as_secs()
            )));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Like [`wait_until`] but the probe yields a value once it is
/// satisfied; returns that value.
pub fn wait_for<T>(
    timeout: Duration,
    description: &str,
    mut probe: impl FnMut() -> Option<T>,
) -> Result<T, Failure> {
    let timeout = scaled(timeout);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(Failure::new(format!(
                "timed out after {}s waiting for: {description}",
                timeout.as_secs()
            )));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Asserts that `probe` stays `true` for the whole `hold` window.
/// Used to prove a daemon keeps running, a file stays absent, and the
/// like, without a bare sleep: the probe is checked on every poll.
pub fn hold_for(
    hold: Duration,
    description: &str,
    mut probe: impl FnMut() -> bool,
) -> Result<(), Failure> {
    let deadline = Instant::now() + hold;
    loop {
        if !probe() {
            return Err(Failure::new(format!(
                "condition did not hold for {}s: {description}",
                hold.as_secs()
            )));
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        thread::sleep(POLL_INTERVAL);
    }
}
