//! Bounded observation of an external process. Every wait has a hard
//! deadline on the wall clock and a named condition, so a timeout says
//! what never happened. This is not a timing assertion; the in-process
//! no-sleep rule of Tier 1 does not apply to watching a real daemon.

use std::thread;
use std::time::{Duration, Instant};

use crate::Failure;

pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Polls `probe` until it returns `true` or `timeout` elapses. The
/// deadline is measured on the clock, not in attempts, so a slow probe
/// (an IPC call against a wedged daemon) cannot stretch the wait.
pub fn wait_until(
    timeout: Duration,
    description: &str,
    mut probe: impl FnMut() -> bool,
) -> Result<(), Failure> {
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
