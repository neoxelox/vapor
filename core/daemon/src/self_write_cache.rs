//! Loop-prevention self-write cache.
//!
//! Bidirectional sync must not react to its own writes: an upload echoes
//! back through the remote changes feed, and a downloaded file echoes
//! back through the local fs watcher. The daemon keeps two instances of
//! this cache — one keyed by remote path (suppressing feed echoes of
//! uploads / remote deletes) and one keyed by local path (suppressing
//! watcher echoes of downloads / local applies).
//!
//! Correlation follows `data-flow.md §Loop prevention`: the op-id tag is
//! the primary correlator; content hash is the fallback (with a cheap
//! size pre-check so the fallback never hashes a file that obviously
//! diverged). Eviction is LRU-on-insert; TTL expiry runs on the same 1s
//! cadence as throttle sampling. Bounds come from
//! `constants::self_write_cache` and may be tightened under memory
//! pressure but never below the documented floors —
//! loop-prevention is a safety guarantee, not an opportunistic feature.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use vapor_shared::constants;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelfWriteKind {
    /// The daemon wrote content at this path (upload or local apply).
    Write,
    /// The daemon deleted this path (remote delete or local apply).
    Delete,
}

#[derive(Clone, Debug)]
struct SelfWriteRecord {
    kind: SelfWriteKind,
    op_id: Option<String>,
    content_hash: Option<String>,
    size_bytes: Option<u64>,
    modified_at: Option<SystemTime>,
    expires_at: SystemTime,
}

#[derive(Debug)]
pub struct SelfWriteCache {
    entries: BTreeMap<String, SelfWriteRecord>,
    insertion_order: VecDeque<String>,
    ttl: Duration,
    max_entries: usize,
    recorded_count: u64,
    suppressed_count: u64,
}

impl Default for SelfWriteCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfWriteCache {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            insertion_order: VecDeque::new(),
            ttl: Duration::from_millis(constants::self_write_cache::DEFAULT_TTL_MILLIS),
            max_entries: constants::self_write_cache::MAX_ENTRIES,
            recorded_count: 0,
            suppressed_count: 0,
        }
    }

    /// Adjusts TTL and capacity (memory-pressure reaction).
    /// Values are clamped to the documented floors so loop prevention
    /// never degrades below its safety guarantee.
    pub fn set_bounds(&mut self, ttl: Duration, max_entries: usize) {
        let floor_ttl = Duration::from_millis(constants::self_write_cache::MIN_TTL_MILLIS);
        let ceiling_ttl = Duration::from_millis(constants::self_write_cache::DEFAULT_TTL_MILLIS);
        self.ttl = ttl.clamp(floor_ttl, ceiling_ttl);
        self.max_entries = max_entries.clamp(
            constants::self_write_cache::MIN_ENTRIES,
            constants::self_write_cache::MAX_ENTRIES,
        );
        while self.entries.len() > self.max_entries {
            self.evict_oldest();
        }
    }

    pub fn record_write(
        &mut self,
        key: impl Into<String>,
        op_id: Option<String>,
        content_hash: Option<String>,
        size_bytes: Option<u64>,
        modified_at: Option<SystemTime>,
        now: SystemTime,
    ) {
        self.insert(
            key.into(),
            SelfWriteRecord {
                kind: SelfWriteKind::Write,
                op_id,
                content_hash,
                size_bytes,
                modified_at,
                expires_at: now + self.ttl,
            },
        );
    }

    pub fn record_delete(&mut self, key: impl Into<String>, now: SystemTime) {
        self.insert(
            key.into(),
            SelfWriteRecord {
                kind: SelfWriteKind::Delete,
                op_id: None,
                content_hash: None,
                size_bytes: None,
                modified_at: None,
                expires_at: now + self.ttl,
            },
        );
    }

    /// Whether an observed write-shaped change at `key` is an echo of a
    /// write the daemon performed. Op-id equality is the primary
    /// correlator; content hash the fallback. A `None` observation on
    /// both correlators never matches — suppression must be positive
    /// evidence, not absence of evidence.
    pub fn matches_write(
        &mut self,
        key: &str,
        observed_op_id: Option<&str>,
        observed_content_hash: Option<&str>,
        now: SystemTime,
    ) -> bool {
        self.matches_write_checked(key, observed_op_id, observed_content_hash, None, None, now)
    }

    /// [`Self::matches_write`] with the observed object's size and
    /// mtime. An op-id tag survives an in-place edit (an append, a
    /// same-size rewrite), so the op-id alone says the object was once
    /// ours, not that it still holds what we wrote: when the record
    /// and the observation both carry a size or an mtime, they have to
    /// agree, at the millisecond the sync index keeps.
    pub fn matches_write_checked(
        &mut self,
        key: &str,
        observed_op_id: Option<&str>,
        observed_content_hash: Option<&str>,
        observed_size: Option<u64>,
        observed_modified_at: Option<SystemTime>,
        now: SystemTime,
    ) -> bool {
        let Some(record) = self.live_record(key, now) else {
            return false;
        };
        if record.kind != SelfWriteKind::Write {
            return false;
        }

        let size_agrees = match (record.size_bytes, observed_size) {
            (Some(recorded), Some(observed)) => recorded == observed,
            _ => true,
        };
        let mtime_agrees = match (record.modified_at, observed_modified_at) {
            (Some(recorded), Some(observed)) => same_millisecond(recorded, observed),
            _ => true,
        };
        let op_id_match = match (record.op_id.as_deref(), observed_op_id) {
            (Some(recorded), Some(observed)) => recorded == observed && size_agrees && mtime_agrees,
            _ => false,
        };
        let hash_match = match (record.content_hash.as_deref(), observed_content_hash) {
            (Some(recorded), Some(observed)) => recorded == observed,
            _ => false,
        };
        let matched = op_id_match || hash_match;
        if matched {
            self.suppressed_count += 1;
        }
        matched
    }

    /// The expected size of the daemon's own write at `key`, when a
    /// live write record exists. Callers use it as the cheap pre-check
    /// before paying for a content hash in the fallback path.
    pub fn expected_write_size(&self, key: &str, now: SystemTime) -> Option<u64> {
        let record = self.entries.get(key)?;
        if record.expires_at <= now || record.kind != SelfWriteKind::Write {
            return None;
        }
        record.size_bytes
    }

    /// Whether a live write record exists for `key` at all (used to
    /// decide whether the hash fallback is worth computing).
    pub fn has_write_record(&self, key: &str, now: SystemTime) -> bool {
        self.entries
            .get(key)
            .is_some_and(|record| record.expires_at > now && record.kind == SelfWriteKind::Write)
    }

    /// Whether an observed deletion at `key` is an echo of a delete the
    /// daemon performed. Deletes correlate by path + recency: the
    /// deleted object no longer exists, so there is nothing to read an
    /// op-id from.
    pub fn matches_delete(&mut self, key: &str, now: SystemTime) -> bool {
        let Some(record) = self.live_record(key, now) else {
            return false;
        };
        let matched = record.kind == SelfWriteKind::Delete;
        if matched {
            self.suppressed_count += 1;
        }
        matched
    }

    /// TTL sweep; runs on the 1s throttle-sampling cadence.
    pub fn purge_expired(&mut self, now: SystemTime) {
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, record)| record.expires_at <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.entries.remove(&key);
        }
        self.insertion_order
            .retain(|key| self.entries.contains_key(key));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn recorded_count(&self) -> u64 {
        self.recorded_count
    }

    /// Total suppressions performed — surfaced in diagnostics so loop
    /// prevention is observable (`data-flow.md §Loop prevention`).
    pub fn suppressed_count(&self) -> u64 {
        self.suppressed_count
    }

    fn live_record(&self, key: &str, now: SystemTime) -> Option<&SelfWriteRecord> {
        self.entries
            .get(key)
            .filter(|record| record.expires_at > now)
    }

    fn insert(&mut self, key: String, record: SelfWriteRecord) {
        self.recorded_count += 1;
        if self.entries.insert(key.clone(), record).is_none() {
            self.insertion_order.push_back(key);
        } else {
            // Refresh recency for LRU-on-insert semantics.
            self.insertion_order.retain(|existing| existing != &key);
            self.insertion_order.push_back(key);
        }
        while self.entries.len() > self.max_entries {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some(oldest) = self.insertion_order.pop_front() {
            self.entries.remove(&oldest);
        }
    }
}

/// Mtimes compare at the millisecond, the precision the sync index
/// stores; a raw comparison would fail on the nanoseconds a
/// filesystem keeps and a serialized record drops.
fn same_millisecond(left: SystemTime, right: SystemTime) -> bool {
    let millis = |time: SystemTime| {
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .ok()
    };
    millis(left).is_some() && millis(left) == millis(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(1_750_000_000_000 + ms)
    }

    #[test]
    fn op_id_is_the_primary_write_correlator() {
        let mut cache = SelfWriteCache::new();
        cache.record_write(
            "docs/a.txt",
            Some("op-1".into()),
            None,
            Some(10),
            None,
            ts(0),
        );

        assert!(cache.matches_write("docs/a.txt", Some("op-1"), None, ts(100)));
        assert!(!cache.matches_write("docs/a.txt", Some("op-other"), None, ts(100)));
        assert_eq!(cache.suppressed_count(), 1);
    }

    #[test]
    fn content_hash_is_the_fallback_correlator() {
        let mut cache = SelfWriteCache::new();
        cache.record_write(
            "docs/a.txt",
            Some("op-1".into()),
            Some("hash-x".into()),
            None,
            None,
            ts(0),
        );

        // Third-party tool rewrote the same bytes: no op-id, same hash.
        assert!(cache.matches_write("docs/a.txt", None, Some("hash-x"), ts(100)));
        assert!(!cache.matches_write("docs/a.txt", None, Some("hash-y"), ts(100)));
    }

    #[test]
    fn op_id_match_needs_the_size_and_mtime_to_agree() {
        let mut cache = SelfWriteCache::new();
        cache.record_write(
            "docs/a.txt",
            Some("op-1".into()),
            None,
            Some(10),
            Some(ts(0)),
            ts(0),
        );
        assert!(cache.matches_write_checked(
            "docs/a.txt",
            Some("op-1"),
            None,
            Some(10),
            Some(ts(0)),
            ts(100)
        ));
        // An append kept the tag and grew the object.
        assert!(!cache.matches_write_checked(
            "docs/a.txt",
            Some("op-1"),
            None,
            Some(14),
            Some(ts(0)),
            ts(100)
        ));
        // A same-size rewrite kept the tag and moved the mtime.
        assert!(!cache.matches_write_checked(
            "docs/a.txt",
            Some("op-1"),
            None,
            Some(10),
            Some(ts(50)),
            ts(100)
        ));
        // A feed without metadata still matches on the op-id alone.
        assert!(cache.matches_write_checked("docs/a.txt", Some("op-1"), None, None, None, ts(100)));
    }

    #[test]
    fn absence_of_evidence_never_suppresses() {
        let mut cache = SelfWriteCache::new();
        cache.record_write(
            "docs/a.txt",
            Some("op-1".into()),
            Some("hash-x".into()),
            None,
            None,
            ts(0),
        );
        assert!(!cache.matches_write("docs/a.txt", None, None, ts(100)));
        assert_eq!(cache.suppressed_count(), 0);
    }

    #[test]
    fn deletes_match_by_path_within_ttl() {
        let mut cache = SelfWriteCache::new();
        cache.record_delete("docs/gone.txt", ts(0));
        assert!(cache.matches_delete("docs/gone.txt", ts(100)));
        assert!(!cache.matches_delete("docs/other.txt", ts(100)));
        // A write record must not suppress a delete observation.
        cache.record_write("docs/w.txt", Some("op".into()), None, None, None, ts(0));
        assert!(!cache.matches_delete("docs/w.txt", ts(100)));
    }

    #[test]
    fn ttl_expiry_stops_suppression() {
        let mut cache = SelfWriteCache::new();
        cache.record_write("docs/a.txt", Some("op-1".into()), None, None, None, ts(0));

        let past_ttl = ts(constants::self_write_cache::DEFAULT_TTL_MILLIS + 1);
        assert!(!cache.matches_write("docs/a.txt", Some("op-1"), None, past_ttl));

        cache.purge_expired(past_ttl);
        assert!(cache.is_empty());
    }

    #[test]
    fn lru_eviction_keeps_the_cache_bounded() {
        let mut cache = SelfWriteCache::new();
        cache.set_bounds(
            Duration::from_millis(constants::self_write_cache::DEFAULT_TTL_MILLIS),
            constants::self_write_cache::MIN_ENTRIES,
        );
        for index in 0..(constants::self_write_cache::MIN_ENTRIES + 5) {
            cache.record_write(
                format!("f-{index}"),
                Some(format!("op-{index}")),
                None,
                None,
                None,
                ts(0),
            );
        }
        assert_eq!(cache.len(), constants::self_write_cache::MIN_ENTRIES);
        // The oldest entries were evicted.
        assert!(!cache.matches_write("f-0", Some("op-0"), None, ts(1)));
        let newest = format!("f-{}", constants::self_write_cache::MIN_ENTRIES + 4);
        let newest_op = format!("op-{}", constants::self_write_cache::MIN_ENTRIES + 4);
        assert!(cache.matches_write(&newest, Some(&newest_op), None, ts(1)));
    }

    #[test]
    fn bounds_clamp_to_documented_floors() {
        let mut cache = SelfWriteCache::new();
        cache.set_bounds(Duration::from_millis(1), 1);
        cache.record_write("a", Some("op".into()), None, None, None, ts(0));
        // Even with an aggressive request, the floor TTL keeps the
        // record alive within MIN_TTL_MILLIS.
        assert!(cache.matches_write(
            "a",
            Some("op"),
            None,
            ts(constants::self_write_cache::MIN_TTL_MILLIS - 1)
        ));
    }

    #[test]
    fn size_precheck_helper_reports_expected_write_size() {
        let mut cache = SelfWriteCache::new();
        cache.record_write(
            "docs/a.txt",
            Some("op".into()),
            None,
            Some(1234),
            None,
            ts(0),
        );
        assert_eq!(cache.expected_write_size("docs/a.txt", ts(1)), Some(1234));
        assert!(cache.has_write_record("docs/a.txt", ts(1)));
        assert_eq!(cache.expected_write_size("docs/missing.txt", ts(1)), None);
    }
}
