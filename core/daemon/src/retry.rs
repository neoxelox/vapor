use std::time::{Duration, SystemTime};

use vapor_shared::constants;

// The failure taxonomy is a shared contract (providers classify their own
// failures with it). Re-exported so existing `crate::retry::…` paths keep
// working.
pub use vapor_shared::RetryFailureKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryDecision {
    pub failure_kind: RetryFailureKind,
    pub retryable: bool,
    pub delay: Option<Duration>,
    pub available_at: Option<SystemTime>,
    pub slowdown_until: Option<SystemTime>,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    transient_base_delay: Duration,
    rate_limit_base_delay: Duration,
    max_delay: Duration,
    jitter_percent: u8,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            transient_base_delay: Duration::from_millis(constants::engine::RETRY_BASE_DELAY_MILLIS),
            rate_limit_base_delay: Duration::from_millis(
                constants::engine::RETRY_RATE_LIMIT_BASE_DELAY_MILLIS,
            ),
            max_delay: Duration::from_millis(constants::engine::RETRY_MAX_DELAY_MILLIS),
            jitter_percent: constants::engine::RETRY_JITTER_PERCENT,
        }
    }
}

impl RetryPolicy {
    pub fn decide(
        &self,
        intent_id: i64,
        attempt_count: u32,
        failure_kind: RetryFailureKind,
        now: SystemTime,
    ) -> RetryDecision {
        let retry_attempt = attempt_count.max(1);
        match failure_kind {
            RetryFailureKind::Transient => {
                let base_delay =
                    self.capped_exponential_delay(self.transient_base_delay, retry_attempt);
                let delay = self.symmetric_jitter(base_delay, intent_id, retry_attempt);
                RetryDecision {
                    failure_kind,
                    retryable: true,
                    delay: Some(delay),
                    available_at: Some(now + delay),
                    slowdown_until: None,
                    reason: "transient failure",
                }
            }
            RetryFailureKind::RateLimited { retry_after } => {
                let exponential_delay =
                    self.capped_exponential_delay(self.rate_limit_base_delay, retry_attempt);
                let delay = match retry_after {
                    Some(retry_after) => std::cmp::max(exponential_delay, retry_after),
                    None => self.positive_jitter(exponential_delay, intent_id, retry_attempt),
                };
                let available_at = now + delay;
                RetryDecision {
                    failure_kind,
                    retryable: true,
                    delay: Some(delay),
                    available_at: Some(available_at),
                    slowdown_until: Some(available_at),
                    reason: "rate limited",
                }
            }
            RetryFailureKind::Authentication => RetryDecision {
                failure_kind,
                retryable: false,
                delay: None,
                available_at: None,
                slowdown_until: None,
                reason: "authentication required",
            },
            RetryFailureKind::Permanent => RetryDecision {
                failure_kind,
                retryable: false,
                delay: None,
                available_at: None,
                slowdown_until: None,
                reason: "permanent failure",
            },
        }
    }

    fn capped_exponential_delay(&self, base_delay: Duration, attempt_count: u32) -> Duration {
        let attempt_count = attempt_count.saturating_sub(1).min(20);
        let multiplier = 1u128 << attempt_count;
        let delay_millis = base_delay.as_millis().saturating_mul(multiplier);
        Duration::from_millis(delay_millis.min(self.max_delay.as_millis()) as u64)
    }

    fn symmetric_jitter(
        &self,
        base_delay: Duration,
        intent_id: i64,
        attempt_count: u32,
    ) -> Duration {
        let base_delay_ms = base_delay.as_millis() as i128;
        let jitter_span_ms = ((base_delay.as_millis() * self.jitter_percent as u128) / 100) as i128;
        if jitter_span_ms == 0 {
            return base_delay;
        }

        let seed = deterministic_jitter_seed(intent_id, attempt_count);
        let offset = (seed % ((jitter_span_ms * 2 + 1) as u64)) as i128 - jitter_span_ms;
        Duration::from_millis(
            (base_delay_ms + offset).clamp(1, self.max_delay.as_millis() as i128) as u64,
        )
    }

    fn positive_jitter(
        &self,
        base_delay: Duration,
        intent_id: i64,
        attempt_count: u32,
    ) -> Duration {
        let base_delay_ms = base_delay.as_millis();
        let jitter_span_ms = (base_delay.as_millis() * self.jitter_percent as u128) / 100;
        if jitter_span_ms == 0 {
            return base_delay;
        }

        let seed = deterministic_jitter_seed(intent_id, attempt_count);
        let extra = (seed % (jitter_span_ms as u64 + 1)) as u128;
        Duration::from_millis((base_delay_ms + extra).min(self.max_delay.as_millis()) as u64)
    }
}

fn deterministic_jitter_seed(intent_id: i64, attempt_count: u32) -> u64 {
    let mut seed = (intent_id as u64).wrapping_mul(0x9E37_79B1_85EB_CA87);
    seed ^= (attempt_count as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    seed ^= seed >> 33;
    seed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn transient_failures_use_exponential_backoff_with_symmetric_jitter() {
        let policy = RetryPolicy::default();
        let now = timestamp_ms(1_000);

        let first = policy.decide(11, 1, RetryFailureKind::Transient, now);
        let second = policy.decide(11, 2, RetryFailureKind::Transient, now);

        assert!(first.retryable);
        assert!(second.retryable);
        assert!(first.delay.unwrap() >= Duration::from_millis(1_600));
        assert!(first.delay.unwrap() <= Duration::from_millis(2_400));
        assert!(second.delay.unwrap() >= Duration::from_millis(3_200));
        assert!(second.delay.unwrap() <= Duration::from_millis(4_800));
        assert!(second.delay.unwrap() > first.delay.unwrap());
    }

    #[test]
    fn retry_jitter_is_deterministic_for_same_intent_and_attempt() {
        let policy = RetryPolicy::default();
        let now = timestamp_ms(1_000);

        let left = policy.decide(17, 3, RetryFailureKind::Transient, now);
        let right = policy.decide(17, 3, RetryFailureKind::Transient, now);
        let other = policy.decide(18, 3, RetryFailureKind::Transient, now);

        assert_eq!(left.delay, right.delay);
        assert_ne!(left.delay, other.delay);
    }

    #[test]
    fn rate_limited_failures_respect_retry_after_and_slow_down_more() {
        let policy = RetryPolicy::default();
        let now = timestamp_ms(1_000);
        let transient = policy.decide(7, 1, RetryFailureKind::Transient, now);
        let rate_limited = policy.decide(
            7,
            1,
            RetryFailureKind::RateLimited {
                retry_after: Some(Duration::from_secs(45)),
            },
            now,
        );

        assert!(rate_limited.retryable);
        assert_eq!(rate_limited.delay.unwrap(), Duration::from_secs(45));
        assert!(rate_limited.delay.unwrap() > transient.delay.unwrap());
        assert_eq!(rate_limited.available_at, rate_limited.slowdown_until);
    }

    #[test]
    fn backoff_is_capped_to_fifteen_minutes() {
        let policy = RetryPolicy::default();
        let now = timestamp_ms(1_000);

        let transient = policy.decide(5, 20, RetryFailureKind::Transient, now);
        let rate_limited = policy.decide(
            5,
            20,
            RetryFailureKind::RateLimited { retry_after: None },
            now,
        );

        assert!(transient.delay.unwrap() <= Duration::from_secs(15 * 60));
        assert!(rate_limited.delay.unwrap() <= Duration::from_secs(15 * 60));
    }

    #[test]
    fn explicit_retry_after_is_respected_even_when_longer_than_local_backoff_cap() {
        let policy = RetryPolicy::default();
        let now = timestamp_ms(1_000);

        let rate_limited = policy.decide(
            5,
            20,
            RetryFailureKind::RateLimited {
                retry_after: Some(Duration::from_secs(3_600)),
            },
            now,
        );

        assert_eq!(rate_limited.delay, Some(Duration::from_secs(3_600)));
        assert_eq!(
            rate_limited.available_at,
            Some(now + Duration::from_secs(3_600))
        );
    }

    #[test]
    fn auth_and_permanent_failures_do_not_retry() {
        let policy = RetryPolicy::default();
        let now = timestamp_ms(1_000);

        let auth = policy.decide(1, 1, RetryFailureKind::Authentication, now);
        let permanent = policy.decide(1, 1, RetryFailureKind::Permanent, now);

        assert!(!auth.retryable);
        assert!(auth.delay.is_none());
        assert!(!permanent.retryable);
        assert!(permanent.available_at.is_none());
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
