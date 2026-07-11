//! Injectable clock abstraction for the daemon runtime.
//!
//! The runtime tick path measures *elapsed* time for tick cadence, slice
//! budgets, throttle sampling, and staged-executor timing. Those checks
//! must stay correct under wall-clock rewinds (DST, NTP corrections,
//! manual `date` command). [`Instant`] is the right primitive for that
//! because it is monotonic; [`SystemTime`] is reserved for durable fields
//! that cross processes (queue rows, retry slowdown markers).
//!
//! [`Clock`] exposes both views so a single injection seam serves every
//! consumer. Production code uses [`SystemClock`]; tests use
//! [`ManualClock`] to advance time deterministically without sleeping.
//!

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// Source of monotonic and wall-clock time for the daemon runtime.
///
/// Production wires [`SystemClock`]. Tests substitute [`ManualClock`] to
/// drive `Instant` and `SystemTime` independently — useful for asserting
/// that elapsed-time checks ignore wall-clock rewinds.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Monotonic clock reading. Used for elapsed-time decisions.
    fn now(&self) -> Instant;
    /// Wall-clock reading. Used for durable/cross-process timestamps.
    fn now_system(&self) -> SystemTime;
}

/// Default production clock: forwards directly to `Instant::now()` and
/// `SystemTime::now()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn now_system(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Test-only clock whose [`Instant`] and [`SystemTime`] readings advance
/// only when [`ManualClock::advance`] / [`ManualClock::advance_system`]
/// are called explicitly. Each axis is independent so tests can rewind
/// the wall clock without rewinding the monotonic clock (and vice
/// versa), which is exactly the property elapsed-time invariants must
/// guard against.
#[derive(Debug)]
pub struct ManualClock {
    inner: Mutex<ManualClockInner>,
}

#[derive(Debug)]
struct ManualClockInner {
    instant: Instant,
    system: SystemTime,
}

impl ManualClock {
    pub fn new(start_instant: Instant, start_system: SystemTime) -> Self {
        Self {
            inner: Mutex::new(ManualClockInner {
                instant: start_instant,
                system: start_system,
            }),
        }
    }

    /// Convenience constructor that anchors both axes at the current
    /// process time.
    pub fn at_now() -> Self {
        Self::new(Instant::now(), SystemTime::now())
    }

    /// Advance the monotonic axis by `delta`. The wall clock is not
    /// touched — callers stage rewinds via [`set_system`].
    pub fn advance(&self, delta: Duration) {
        let mut inner = self.inner.lock().expect("ManualClock poisoned");
        inner.instant += delta;
    }

    /// Advance the wall-clock axis by `delta`. The monotonic axis is not
    /// touched.
    pub fn advance_system(&self, delta: Duration) {
        let mut inner = self.inner.lock().expect("ManualClock poisoned");
        inner.system += delta;
    }

    /// Replace the wall-clock reading. Useful for asserting that the
    /// elapsed-time pipeline tolerates an arbitrary wall-clock rewind
    /// (e.g. NTP correction running the clock backwards).
    pub fn set_system(&self, system: SystemTime) {
        let mut inner = self.inner.lock().expect("ManualClock poisoned");
        inner.system = system;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.inner.lock().expect("ManualClock poisoned").instant
    }

    fn now_system(&self) -> SystemTime {
        self.inner.lock().expect("ManualClock poisoned").system
    }
}

/// Convenience type alias for shared clock references that the engine
/// passes around.
pub type SharedClock = Arc<dyn Clock>;

/// Construct a [`SystemClock`]-backed [`SharedClock`] for production use.
pub fn system_clock() -> SharedClock {
    Arc::new(SystemClock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_returns_anchored_readings() {
        let start_instant = Instant::now();
        let start_system = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = ManualClock::new(start_instant, start_system);

        assert_eq!(clock.now(), start_instant);
        assert_eq!(clock.now_system(), start_system);
    }

    #[test]
    fn manual_clock_axes_advance_independently() {
        let clock = ManualClock::at_now();
        let baseline_instant = clock.now();
        let baseline_system = clock.now_system();

        clock.advance(Duration::from_secs(5));
        // Wall clock unchanged after monotonic-only advance.
        assert_eq!(clock.now_system(), baseline_system);
        assert_eq!(clock.now() - baseline_instant, Duration::from_secs(5));

        clock.advance_system(Duration::from_secs(60));
        // Monotonic clock unchanged after wall-only advance.
        assert_eq!(clock.now() - baseline_instant, Duration::from_secs(5));
        assert_eq!(
            clock
                .now_system()
                .duration_since(baseline_system)
                .expect("forward wall-clock advance"),
            Duration::from_secs(60),
        );
    }

    #[test]
    fn manual_clock_supports_wall_clock_rewind_without_touching_monotonic_axis() {
        // Critical invariant: a wall-clock rewind must not affect
        // monotonic readings. Code that gates on `clock.now()` for
        // elapsed-time checks therefore stays stable under DST / NTP
        // corrections / `date -s` style time travel.
        let clock = ManualClock::at_now();
        let monotonic_before = clock.now();

        clock.set_system(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let monotonic_after = clock.now();

        assert_eq!(monotonic_before, monotonic_after);
    }

    #[test]
    fn system_clock_produces_strictly_monotonic_instants() {
        let clock = SystemClock;
        let first = clock.now();
        let second = clock.now();
        // Strict monotonicity is the std::time::Instant guarantee; the
        // assertion is a regression guard against accidental subs that
        // could feed a non-monotonic source through the trait.
        assert!(second >= first);
    }
}
