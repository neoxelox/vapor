//! Provider-neutral bandwidth shaper (C8-38).
//!
//! A bytes-per-second token bucket shared across every upload and
//! download session in the daemon. The engine sets the rate each tick
//! to `bandwidthPercent` of the measured link capacity (or the assumed
//! capacity until a measurement exists), so non-Vapor traffic retains
//! at least `100 - bandwidthPercent` of the link by construction.

use std::time::Instant;

#[derive(Debug)]
pub struct BandwidthShaper {
    /// `None` = unlimited (no ceiling configured).
    rate_bytes_per_sec: Option<u64>,
    tokens: f64,
    last_refill: Option<Instant>,
}

impl BandwidthShaper {
    pub fn unlimited() -> Self {
        Self {
            rate_bytes_per_sec: None,
            tokens: 0.0,
            last_refill: None,
        }
    }

    pub fn with_rate(rate_bytes_per_sec: u64) -> Self {
        Self {
            rate_bytes_per_sec: Some(rate_bytes_per_sec.max(1)),
            tokens: 0.0,
            last_refill: None,
        }
    }

    /// Updates the sustained rate; the bucket keeps its accumulated
    /// tokens (bounded by the new one-second capacity).
    pub fn set_rate(&mut self, rate_bytes_per_sec: Option<u64>) {
        self.rate_bytes_per_sec = rate_bytes_per_sec.map(|rate| rate.max(1));
        if let Some(rate) = self.rate_bytes_per_sec {
            self.tokens = self.tokens.min(rate as f64);
        }
    }

    pub fn rate(&self) -> Option<u64> {
        self.rate_bytes_per_sec
    }

    /// Grants up to `requested` bytes for one transfer step. The bucket
    /// holds at most one second of rate so idle periods cannot bank an
    /// unbounded burst.
    pub fn budget(&mut self, requested: u64, now: Instant) -> u64 {
        let Some(rate) = self.rate_bytes_per_sec else {
            return requested;
        };
        let elapsed = self
            .last_refill
            .map(|last| now.saturating_duration_since(last).as_secs_f64())
            .unwrap_or(1.0);
        self.last_refill = Some(now);
        self.tokens = (self.tokens + elapsed * rate as f64).min(rate as f64);

        let granted = (self.tokens.floor() as u64).min(requested);
        self.tokens -= granted as f64;
        granted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn unlimited_shaper_grants_everything() {
        let mut shaper = BandwidthShaper::unlimited();
        assert_eq!(shaper.budget(10_000_000, Instant::now()), 10_000_000);
    }

    #[test]
    fn rate_bounds_sustained_throughput() {
        let mut shaper = BandwidthShaper::with_rate(1_000);
        let t0 = Instant::now();
        // First call grants up to one second of tokens.
        let first = shaper.budget(10_000, t0);
        assert_eq!(first, 1_000);
        // Immediately after, the bucket is empty.
        assert_eq!(shaper.budget(10_000, t0), 0);
        // Half a second later, half the rate refilled.
        let granted = shaper.budget(10_000, t0 + Duration::from_millis(500));
        assert!(
            (450..=550).contains(&granted),
            "≈500 bytes must refill in 500ms, got {granted}"
        );
    }

    #[test]
    fn bucket_never_banks_more_than_one_second() {
        let mut shaper = BandwidthShaper::with_rate(1_000);
        let t0 = Instant::now();
        let _ = shaper.budget(1, t0);
        // A long idle period must not accumulate a burst.
        let granted = shaper.budget(1_000_000, t0 + Duration::from_secs(60));
        assert!(granted <= 1_000, "burst capped at 1s of rate: {granted}");
    }

    #[test]
    fn lowering_the_rate_clamps_banked_tokens() {
        let mut shaper = BandwidthShaper::with_rate(1_000_000);
        let t0 = Instant::now();
        let _ = shaper.budget(1, t0); // prime refill timestamp
        shaper.set_rate(Some(100));
        let granted = shaper.budget(1_000_000, t0 + Duration::from_secs(1));
        assert!(granted <= 100, "new rate applies immediately: {granted}");
    }

    #[test]
    fn removing_the_rate_returns_to_unlimited() {
        let mut shaper = BandwidthShaper::with_rate(10);
        shaper.set_rate(None);
        assert_eq!(shaper.budget(5_000, Instant::now()), 5_000);
    }
}
