//! The bandwidth ceiling: a share of the link, and the measurement of
//! the link it is a share of.
//!
//! `resourceLimits.bandwidthPercent` is a share of what the link can
//! carry, so the daemon has to know that figure. It never assumes one.
//! Each direction starts from the speed the OS reports for the network
//! interface, which is an upper bound (a fast LAN can sit behind a slow
//! uplink). From the first transfer on the daemon measures. A step that
//! ran without the limiter moved its bytes at the speed of the link:
//! the highest rate such a step reached in the last minutes is the
//! capacity. A step that the limiter sized is short, and most of the
//! time of a short step is the round trip to the provider, so it reads
//! low: it can raise the estimate and never lowers it. When no step
//! measured the link for that long, one step runs without the limiter
//! as a probe, sized to last about a second on the link as it was last
//! measured. A faster link gives a higher rate with no setting to
//! change.
//!
//! Uploads and downloads have a token bucket each, because the two
//! directions of a link seldom carry the same.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vapor_providers::BandwidthShaper;
use vapor_shared::constants;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferDirection {
    Upload,
    Download,
}

/// What the rate of one transfer step says about the link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepReading {
    /// The step ran without the limiter and did nothing but move bytes:
    /// its rate is the link's.
    Link,
    /// The limiter sized the step, or the step also finished the
    /// transfer (a last request, a commit): its rate is a lower bound.
    AtLeast,
}

/// What one direction of the link is known to carry.
#[derive(Debug, Default)]
struct DirectionEstimate {
    /// Rates of the steps large enough to measure, newest last, in
    /// bytes per second.
    samples: VecDeque<(Instant, u64)>,
    /// The last capacity the samples gave, kept after they age out.
    last_measured: Option<u64>,
    last_probe_at: Option<Instant>,
}

impl DirectionEstimate {
    fn prune(&mut self, now: Instant) {
        let window = Duration::from_secs(constants::engine::LINK_CAPACITY_WINDOW_SECONDS);
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > window)
        {
            self.samples.pop_front();
        }
    }

    fn fresh_capacity(&mut self, now: Instant) -> Option<u64> {
        self.prune(now);
        self.samples.iter().map(|(_, rate)| *rate).max()
    }
}

#[derive(Debug, Default)]
struct LinkState {
    upload: DirectionEstimate,
    download: DirectionEstimate,
    /// Interface speed from the OS, in bytes per second.
    os_reported: Option<u64>,
    /// A capacity fixed from outside (the static throttle inputs of the
    /// test harnesses): nothing is measured and nothing is probed.
    pinned: Option<u64>,
    /// Bytes moved by recent steps in both directions, for the usage
    /// figure in the status.
    moved: VecDeque<(Instant, u64)>,
}

impl LinkState {
    fn direction(&mut self, direction: TransferDirection) -> &mut DirectionEstimate {
        match direction {
            TransferDirection::Upload => &mut self.upload,
            TransferDirection::Download => &mut self.download,
        }
    }
}

/// The daemon-wide bandwidth state every transfer step consults: the
/// two token buckets and the capacity estimate that sets their rates.
#[derive(Debug)]
pub struct TransferBandwidth {
    upload: Mutex<BandwidthShaper>,
    download: Mutex<BandwidthShaper>,
    link: Mutex<LinkState>,
}

impl Default for TransferBandwidth {
    fn default() -> Self {
        Self {
            upload: Mutex::new(BandwidthShaper::unlimited()),
            download: Mutex::new(BandwidthShaper::unlimited()),
            link: Mutex::new(LinkState::default()),
        }
    }
}

impl TransferBandwidth {
    pub fn shaper(
        &self,
        direction: TransferDirection,
    ) -> std::sync::MutexGuard<'_, BandwidthShaper> {
        let shaper = match direction {
            TransferDirection::Upload => &self.upload,
            TransferDirection::Download => &self.download,
        };
        shaper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn link(&self) -> std::sync::MutexGuard<'_, LinkState> {
        self.link
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The interface speed the OS reports, or `None` when it reports
    /// none. Used until the first measurement of a direction.
    pub fn set_os_reported_kbps(&self, kbps: Option<u32>) {
        self.link().os_reported = kbps.map(kbps_to_bytes_per_second);
    }

    /// Fixes the capacity: no measurement, no probe. For harnesses
    /// that need the same rate on every host.
    pub fn pin_capacity_kbps(&self, kbps: u32) {
        self.link().pinned = Some(kbps_to_bytes_per_second(kbps));
    }

    /// What the link carries in `direction`, in bytes per second: the
    /// pinned figure, else the highest measured rate of the last
    /// minutes, else the last measurement, else the OS figure. `None`
    /// when nothing is known yet: the first transfer then runs with no
    /// limit and is the first measurement.
    pub fn capacity_bytes_per_second(
        &self,
        direction: TransferDirection,
        now: Instant,
    ) -> Option<u64> {
        let mut link = self.link();
        if let Some(pinned) = link.pinned {
            return Some(pinned);
        }
        let os_reported = link.os_reported;
        let estimate = link.direction(direction);
        if let Some(fresh) = estimate.fresh_capacity(now) {
            estimate.last_measured = Some(fresh);
        }
        estimate.last_measured.or(os_reported)
    }

    /// Sets each bucket to `percent` of its direction's capacity. A
    /// direction with no known capacity has no limit.
    pub fn apply_ceiling(&self, percent: u8, now: Instant) {
        for direction in [TransferDirection::Upload, TransferDirection::Download] {
            let rate = self
                .capacity_bytes_per_second(direction, now)
                .map(|capacity| (capacity * u64::from(percent) / 100).max(1));
            self.shaper(direction).set_rate(rate);
        }
    }

    /// Whether the next step in `direction` may run without the limiter
    /// to measure the link: the estimate has no measurement younger
    /// than the window, and no probe ran inside the window either. One
    /// caller gets `true`; the others keep to the bucket.
    pub fn take_probe(&self, direction: TransferDirection, now: Instant) -> bool {
        let mut link = self.link();
        if link.pinned.is_some() {
            return false;
        }
        let estimate = link.direction(direction);
        if estimate.fresh_capacity(now).is_some() {
            return false;
        }
        let window = Duration::from_secs(constants::engine::LINK_CAPACITY_WINDOW_SECONDS);
        if estimate
            .last_probe_at
            .is_some_and(|at| now.saturating_duration_since(at) < window)
        {
            return false;
        }
        estimate.last_probe_at = Some(now);
        true
    }

    /// How many bytes the probe step may move: about a second on the
    /// link as it was last measured, so the round trip to the provider
    /// is a small part of what the probe times. Never less than
    /// `LINK_CAPACITY_PROBE_BYTES`, which is enough on a slow link.
    pub fn probe_bytes(&self, direction: TransferDirection, now: Instant) -> u64 {
        self.capacity_bytes_per_second(direction, now)
            .map_or(u64::MAX, |capacity| {
                capacity.max(constants::engine::LINK_CAPACITY_PROBE_BYTES)
            })
    }

    /// Records one finished transfer step. A step that moved enough
    /// bytes over enough time says something about the link: its rate
    /// is the link's, or a lower bound of it (see [`StepReading`]).
    pub fn record_step(
        &self,
        direction: TransferDirection,
        bytes: u64,
        elapsed: Duration,
        now: Instant,
        reading: StepReading,
    ) {
        if bytes == 0 {
            return;
        }
        let mut link = self.link();
        link.moved.push_back((now, bytes));
        let usage_window = Duration::from_secs(constants::engine::BANDWIDTH_USAGE_WINDOW_SECONDS);
        while link
            .moved
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > usage_window)
        {
            link.moved.pop_front();
        }
        if link.pinned.is_some()
            || bytes < constants::engine::LINK_CAPACITY_SAMPLE_MIN_BYTES
            || elapsed < Duration::from_millis(constants::engine::LINK_CAPACITY_SAMPLE_MIN_MILLIS)
        {
            return;
        }
        let rate = ((bytes as f64 / elapsed.as_secs_f64()) as u64).max(1);
        let os_reported = link.os_reported;
        let estimate = link.direction(direction);
        if reading == StepReading::AtLeast {
            // A lower bound says something only when it is above what
            // is known. Kept otherwise, it would become the estimate
            // when the real measurements age out, and the ceiling, a
            // share of the estimate, would fall step by step.
            let known = estimate
                .fresh_capacity(now)
                .or(estimate.last_measured)
                .or(os_reported);
            if known.is_some_and(|known| rate <= known) {
                return;
            }
        }
        estimate.samples.push_back((now, rate));
        if estimate.samples.len() > constants::engine::LINK_CAPACITY_SAMPLES_MAX {
            estimate.samples.pop_front();
        }
        estimate.last_measured = estimate.fresh_capacity(now);
    }

    /// What the daemon moved over the last seconds, both directions
    /// together, in kbit/s.
    pub fn used_kbps(&self, now: Instant) -> u64 {
        let link = self.link();
        let window = Duration::from_secs(constants::engine::BANDWIDTH_USAGE_WINDOW_SECONDS);
        let bytes: u64 = link
            .moved
            .iter()
            .filter(|(at, _)| now.saturating_duration_since(*at) <= window)
            .map(|(_, bytes)| *bytes)
            .sum();
        bytes * 8 / 1_000 / window.as_secs().max(1)
    }
}

fn kbps_to_bytes_per_second(kbps: u32) -> u64 {
    u64::from(kbps) * 1_000 / 8
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn nothing_known_means_no_limit_and_the_first_step_measures() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, t0),
            None
        );
        bandwidth.apply_ceiling(25, t0);
        assert_eq!(bandwidth.shaper(TransferDirection::Upload).rate(), None);

        // 4 MiB in one second: the link carries 4 MiB each second.
        bandwidth.record_step(
            TransferDirection::Upload,
            4 * MIB,
            Duration::from_secs(1),
            t0,
            StepReading::Link,
        );
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, t0),
            Some(4 * MIB)
        );
        bandwidth.apply_ceiling(25, t0);
        assert_eq!(
            bandwidth.shaper(TransferDirection::Upload).rate(),
            Some(MIB)
        );
        // The other direction was not measured and has no OS figure.
        assert_eq!(bandwidth.shaper(TransferDirection::Download).rate(), None);
    }

    #[test]
    fn the_os_figure_is_the_start_and_a_measurement_replaces_it() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        // 1 Gbit/s Ethernet behind a 20 Mbit/s uplink.
        bandwidth.set_os_reported_kbps(Some(1_000_000));
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, t0),
            Some(125_000_000)
        );
        bandwidth.record_step(
            TransferDirection::Upload,
            5 * MIB,
            Duration::from_secs(2),
            t0,
            StepReading::Link,
        );
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, t0),
            Some(5 * MIB / 2),
            "the measured uplink, not the interface speed"
        );
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Download, t0),
            Some(125_000_000),
            "an unmeasured direction keeps the OS figure"
        );
    }

    #[test]
    fn capacity_is_the_highest_rate_of_the_window_and_follows_a_faster_link() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        let window = Duration::from_secs(constants::engine::LINK_CAPACITY_WINDOW_SECONDS);
        for (offset, rate) in [(0, 2 * MIB), (10, 6 * MIB), (20, 3 * MIB)] {
            bandwidth.record_step(
                TransferDirection::Download,
                rate,
                Duration::from_secs(1),
                t0 + Duration::from_secs(offset),
                StepReading::Link,
            );
        }
        let at = t0 + Duration::from_secs(30);
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Download, at),
            Some(6 * MIB)
        );

        // The fast sample ages out; the device moved to a slower link.
        let later = t0 + Duration::from_secs(15) + window;
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Download, later),
            Some(3 * MIB)
        );
        // A faster link shows up with the next step.
        bandwidth.record_step(
            TransferDirection::Download,
            40 * MIB,
            Duration::from_secs(1),
            later,
            StepReading::Link,
        );
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Download, later),
            Some(40 * MIB)
        );
    }

    #[test]
    fn steps_too_small_or_too_short_are_not_measurements() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        bandwidth.record_step(
            TransferDirection::Upload,
            4_096,
            Duration::from_secs(1),
            t0,
            StepReading::Link,
        );
        bandwidth.record_step(
            TransferDirection::Upload,
            8 * MIB,
            Duration::from_micros(200),
            t0,
            StepReading::Link,
        );
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, t0),
            None
        );
    }

    #[test]
    fn an_old_estimate_gets_one_probe_a_window() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        let window = Duration::from_secs(constants::engine::LINK_CAPACITY_WINDOW_SECONDS);
        assert!(
            bandwidth.take_probe(TransferDirection::Upload, t0),
            "nothing measured yet"
        );
        assert!(
            !bandwidth.take_probe(TransferDirection::Upload, t0),
            "one probe, not one for each concurrent transfer"
        );
        assert!(bandwidth.take_probe(TransferDirection::Download, t0));

        bandwidth.record_step(
            TransferDirection::Upload,
            2 * MIB,
            Duration::from_secs(1),
            t0 + window,
            StepReading::Link,
        );
        assert!(
            !bandwidth.take_probe(
                TransferDirection::Upload,
                t0 + window + Duration::from_secs(1)
            ),
            "a fresh measurement needs no probe"
        );
        // The measurement ages out and the last probe is a window old.
        let stale = t0 + window * 2 + Duration::from_secs(2);
        assert!(bandwidth.take_probe(TransferDirection::Upload, stale));
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, stale),
            Some(2 * MIB),
            "the old figure holds the limit until the probe lands"
        );
    }

    #[test]
    fn a_pinned_capacity_is_never_measured_or_probed() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        bandwidth.pin_capacity_kbps(100_000);
        bandwidth.record_step(
            TransferDirection::Upload,
            64 * MIB,
            Duration::from_secs(1),
            t0,
            StepReading::Link,
        );
        assert!(!bandwidth.take_probe(TransferDirection::Upload, t0));
        bandwidth.apply_ceiling(25, t0);
        for direction in [TransferDirection::Upload, TransferDirection::Download] {
            assert_eq!(bandwidth.shaper(direction).rate(), Some(3_125_000));
        }
    }

    #[test]
    fn usage_is_what_moved_over_the_last_seconds() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        let window = constants::engine::BANDWIDTH_USAGE_WINDOW_SECONDS;
        assert_eq!(bandwidth.used_kbps(t0), 0);
        bandwidth.record_step(
            TransferDirection::Upload,
            window * 125_000,
            Duration::from_millis(1),
            t0,
            StepReading::Link,
        );
        bandwidth.record_step(
            TransferDirection::Download,
            window * 125_000,
            Duration::from_millis(1),
            t0,
            StepReading::Link,
        );
        // 250 000 bytes each second over the window: 2 000 kbit/s.
        assert_eq!(bandwidth.used_kbps(t0), 2_000);
        assert_eq!(
            bandwidth.used_kbps(t0 + Duration::from_secs(window + 1)),
            0,
            "an idle daemon uses nothing"
        );
    }

    #[test]
    fn a_step_that_the_limiter_sized_raises_the_estimate_and_never_lowers_it() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        let window = Duration::from_secs(constants::engine::LINK_CAPACITY_WINDOW_SECONDS);
        // The probe: 8 MiB each second.
        bandwidth.record_step(
            TransferDirection::Upload,
            8 * MIB,
            Duration::from_secs(1),
            t0,
            StepReading::Link,
        );
        // Under a 25 % ceiling the steps are small, and the round trip
        // is most of their time: each reads far below the link.
        for second in 1..=(window.as_secs() + 60) {
            bandwidth.record_step(
                TransferDirection::Upload,
                MIB / 2,
                Duration::from_millis(400),
                t0 + Duration::from_secs(second),
                StepReading::AtLeast,
            );
        }
        let later = t0 + window + Duration::from_secs(61);
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, later),
            Some(8 * MIB),
            "the low readings did not become the estimate when the probe aged out"
        );
        assert!(
            bandwidth.take_probe(TransferDirection::Upload, later),
            "and they do not stand in for the next probe"
        );

        // A limited step that moved faster than the estimate: the link
        // carries at least that.
        bandwidth.record_step(
            TransferDirection::Upload,
            3 * MIB,
            Duration::from_millis(250),
            later,
            StepReading::AtLeast,
        );
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, later),
            Some(12 * MIB)
        );

        // A probe that reads lower is a slower link, once the old
        // readings are out of the window.
        let much_later = later + window + Duration::from_secs(1);
        bandwidth.record_step(
            TransferDirection::Upload,
            2 * MIB,
            Duration::from_secs(1),
            much_later,
            StepReading::Link,
        );
        assert_eq!(
            bandwidth.capacity_bytes_per_second(TransferDirection::Upload, much_later),
            Some(2 * MIB)
        );
    }

    #[test]
    fn the_probe_lasts_about_a_second_on_the_link_as_last_measured() {
        let bandwidth = TransferBandwidth::default();
        let t0 = Instant::now();
        assert_eq!(
            bandwidth.probe_bytes(TransferDirection::Download, t0),
            u64::MAX,
            "nothing known: the step's own size bounds it"
        );
        bandwidth.record_step(
            TransferDirection::Download,
            12 * MIB,
            Duration::from_secs(1),
            t0,
            StepReading::Link,
        );
        assert_eq!(
            bandwidth.probe_bytes(TransferDirection::Download, t0),
            12 * MIB
        );

        let slow = TransferBandwidth::default();
        slow.record_step(
            TransferDirection::Download,
            200 * 1024,
            Duration::from_secs(2),
            t0,
            StepReading::Link,
        );
        assert_eq!(
            slow.probe_bytes(TransferDirection::Download, t0),
            constants::engine::LINK_CAPACITY_PROBE_BYTES
        );
    }
}
