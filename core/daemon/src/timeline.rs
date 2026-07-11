//! Bounded in-memory daemon activity timeline.
//!
//! One buffer serves the whole daemon: every profile runtime appends
//! noteworthy events (state transitions, conflicts, terminal failures,
//! mirror actions, reconcile completions, feed-cursor expiry) and the
//! IPC `Timeline` endpoint reads a snapshot. The buffer is bounded by
//! the `timelineLimit` config key (default 1000): when full, the
//! oldest entries fall off — diagnostics favor recency.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use vapor_shared::constants;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineEventRecord {
    /// Monotonic per-buffer sequence: readers can detect truncation by
    /// gaps after the first entry.
    pub sequence: u64,
    pub timestamp: SystemTime,
    /// Stable machine-readable kind (`run_state`, `throttle`,
    /// `conflict`, `intent_failed`, `mirror`, `reconcile`, `feed`,
    /// `profile`).
    pub kind: String,
    pub profile_id: String,
    pub message: String,
}

#[derive(Debug)]
struct TimelineInner {
    entries: VecDeque<TimelineEventRecord>,
    next_sequence: u64,
    max_entries: usize,
}

#[derive(Debug)]
pub struct TimelineBuffer {
    inner: Mutex<TimelineInner>,
}

impl TimelineBuffer {
    pub fn new(max_entries: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(TimelineInner {
                entries: VecDeque::new(),
                next_sequence: 1,
                max_entries: max_entries.max(1),
            }),
        })
    }

    /// Buffer size from the config value, clamped to a sane floor so a
    /// misconfigured `0` cannot silently disable diagnostics.
    pub fn from_config_limit(limit: i64) -> Arc<Self> {
        let max_entries = usize::try_from(limit)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(constants::config::DEFAULT_TIMELINE_LIMIT as usize);
        Self::new(max_entries)
    }

    pub fn push(
        &self,
        kind: impl Into<String>,
        profile_id: impl Into<String>,
        message: impl Into<String>,
        now: SystemTime,
    ) {
        let mut inner = self.lock();
        let sequence = inner.next_sequence;
        inner.next_sequence += 1;
        inner.entries.push_back(TimelineEventRecord {
            sequence,
            timestamp: now,
            kind: kind.into(),
            profile_id: profile_id.into(),
            message: message.into(),
        });
        while inner.entries.len() > inner.max_entries {
            inner.entries.pop_front();
        }
    }

    /// Oldest-to-newest snapshot, optionally capped to the newest
    /// `limit` entries.
    pub fn snapshot(&self, limit: Option<usize>) -> Vec<TimelineEventRecord> {
        let inner = self.lock();
        let take_from = match limit {
            Some(limit) if limit < inner.entries.len() => inner.entries.len() - limit,
            _ => 0,
        };
        inner.entries.iter().skip(take_from).cloned().collect()
    }

    /// Trims capacity under memory pressure. The documented
    /// floor keeps the timeline useful even when squeezed.
    pub fn set_max_entries(&self, max_entries: usize) {
        let mut inner = self.lock();
        inner.max_entries = max_entries.clamp(64, usize::MAX);
        while inner.entries.len() > inner.max_entries {
            inner.entries.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().entries.is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TimelineInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ts(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn entries_are_ordered_and_sequenced() {
        let buffer = TimelineBuffer::new(10);
        buffer.push("run_state", "default", "Running", ts(1));
        buffer.push("throttle", "default", "IdleDrain", ts(2));

        let entries = buffer.snapshot(None);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].sequence < entries[1].sequence);
        assert!(entries[0].timestamp <= entries[1].timestamp);
        assert_eq!(entries[0].kind, "run_state");
    }

    #[test]
    fn buffer_truncates_oldest_when_full() {
        let buffer = TimelineBuffer::new(3);
        for index in 0..5 {
            buffer.push("k", "p", format!("event-{index}"), ts(index));
        }
        let entries = buffer.snapshot(None);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].message, "event-2", "oldest entries fall off");
        // Sequence gap reveals the truncation.
        assert_eq!(entries[0].sequence, 3);
    }

    #[test]
    fn snapshot_limit_returns_the_newest_entries() {
        let buffer = TimelineBuffer::new(10);
        for index in 0..6 {
            buffer.push("k", "p", format!("event-{index}"), ts(index));
        }
        let entries = buffer.snapshot(Some(2));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].message, "event-5");
    }

    #[test]
    fn shrinking_capacity_trims_immediately_but_respects_the_floor() {
        let buffer = TimelineBuffer::new(1_000);
        for index in 0..200 {
            buffer.push("k", "p", format!("event-{index}"), ts(index));
        }
        buffer.set_max_entries(1);
        assert_eq!(buffer.len(), 64, "the floor keeps diagnostics usable");
    }

    #[test]
    fn config_limit_zero_falls_back_to_the_default() {
        let buffer = TimelineBuffer::from_config_limit(0);
        for index in 0..10 {
            buffer.push("k", "p", format!("event-{index}"), ts(index));
        }
        assert_eq!(buffer.len(), 10);
    }
}
