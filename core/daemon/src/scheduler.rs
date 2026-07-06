use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::debounce::StabilizedEvent;
use crate::event_intents::{PendingIntentKind, PendingIntentRecord};
use crate::fs_events::FsEventKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduledIntentState {
    Pending,
    Running,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledIntentRecord {
    pub path: PathBuf,
    pub kind: PendingIntentKind,
    pub state: ScheduledIntentState,
    pub first_observed_at: SystemTime,
    pub last_observed_at: SystemTime,
    pub burst_count: usize,
    pub dirty: bool,
    pub replay_count: usize,
    queue_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedIntent {
    pub path: PathBuf,
    pub kind: PendingIntentKind,
    pub first_observed_at: SystemTime,
    pub last_observed_at: SystemTime,
    pub burst_count: usize,
    pub replay_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionDisposition {
    Removed,
    RequeuedDirty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerUpdate {
    pub path: PathBuf,
    pub kind: PendingIntentKind,
    pub state: ScheduledIntentState,
    pub dirty: bool,
    pub replay_count: usize,
}

#[derive(Debug, Default)]
pub struct KeyedSupersedingScheduler {
    intents: BTreeMap<PathBuf, ScheduledIntentRecord>,
    pending_index: BTreeSet<(u64, PathBuf)>,
    pending_reconcile_index: BTreeSet<(u64, PathBuf)>,
    next_sequence: u64,
}

impl KeyedSupersedingScheduler {
    pub fn len(&self) -> usize {
        self.intents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }

    pub fn pending_count(&self) -> usize {
        self.pending_index.len()
    }

    pub fn running_count(&self) -> usize {
        self.intents
            .values()
            .filter(|record| record.state == ScheduledIntentState::Running)
            .count()
    }

    pub fn scheduled_intent(&self, path: &Path) -> Option<&ScheduledIntentRecord> {
        self.intents.get(path)
    }

    pub fn upsert_stabilized_event(&mut self, event: StabilizedEvent) -> SchedulerUpdate {
        let kind = intent_kind_for_stabilized_event(&event);
        self.upsert_stabilized_event_as(event, kind)
    }

    /// Upserts with a caller-computed kind, so the runtime's safeguard
    /// taps and the scheduled intent are guaranteed to agree on one
    /// classification (the existence probe must not run twice).
    pub(crate) fn upsert_stabilized_event_as(
        &mut self,
        event: StabilizedEvent,
        kind: PendingIntentKind,
    ) -> SchedulerUpdate {
        self.upsert_intent_with_metadata(
            event.path,
            kind,
            event.first_observed_at,
            event.last_observed_at,
            event.burst_count,
        )
    }

    pub fn upsert_intent(
        &mut self,
        path: impl Into<PathBuf>,
        kind: PendingIntentKind,
        observed_at: SystemTime,
    ) -> SchedulerUpdate {
        self.upsert_intent_with_metadata(path.into(), kind, observed_at, observed_at, 1)
    }

    pub fn upsert_pending_intent_record(&mut self, record: PendingIntentRecord) -> SchedulerUpdate {
        self.upsert_intent_with_metadata(
            record.path,
            record.kind,
            record.observed_at,
            record.observed_at,
            1,
        )
    }

    pub fn claim_next(&mut self) -> Option<ClaimedIntent> {
        let (_, path) = self.pending_index.iter().next()?.clone();
        self.claim_path(path.as_path())
    }

    pub fn claim_next_reconcile(&mut self) -> Option<ClaimedIntent> {
        let (_, path) = self.pending_reconcile_index.iter().next()?.clone();
        self.claim_path(path.as_path())
    }

    pub fn discard_pending(&mut self, path: &Path) -> bool {
        let Some(record) = self.intents.get(path) else {
            return false;
        };
        if record.state != ScheduledIntentState::Pending {
            return false;
        }
        let queue_key = (record.queue_sequence, record.path.clone());
        self.pending_index.remove(&queue_key);
        self.pending_reconcile_index.remove(&queue_key);
        self.intents.remove(path);
        true
    }

    fn claim_path(&mut self, path: &Path) -> Option<ClaimedIntent> {
        let queue_key = {
            let record = self.intents.get(path)?;
            (record.queue_sequence, record.path.clone())
        };
        self.pending_index.remove(&queue_key);
        self.pending_reconcile_index.remove(&queue_key);
        let record = self.intents.get_mut(path)?;
        record.state = ScheduledIntentState::Running;
        record.dirty = false;

        Some(ClaimedIntent {
            path: record.path.clone(),
            kind: record.kind,
            first_observed_at: record.first_observed_at,
            last_observed_at: record.last_observed_at,
            burst_count: record.burst_count,
            replay_count: record.replay_count,
        })
    }

    pub fn complete_running(&mut self, path: &Path) -> Option<CompletionDisposition> {
        let (dirty, replay_count) = {
            let record = self.intents.get(path)?;
            if record.state != ScheduledIntentState::Running {
                return None;
            }
            (record.dirty, record.replay_count)
        };

        if dirty {
            let sequence = self.take_next_sequence();
            let (queue_key, should_index_reconcile) = {
                let record = self
                    .intents
                    .get_mut(path)
                    .expect("scheduled intent disappeared before completion");
                record.state = ScheduledIntentState::Pending;
                record.dirty = false;
                record.replay_count = replay_count + 1;
                record.queue_sequence = sequence;
                (
                    (record.queue_sequence, record.path.clone()),
                    record.kind == PendingIntentKind::ReconcileSubtree,
                )
            };
            self.pending_index.insert(queue_key.clone());
            if should_index_reconcile {
                self.pending_reconcile_index.insert(queue_key);
            }
            return Some(CompletionDisposition::RequeuedDirty);
        }

        self.intents.remove(path);
        Some(CompletionDisposition::Removed)
    }

    fn upsert_intent_with_metadata(
        &mut self,
        path: PathBuf,
        kind: PendingIntentKind,
        first_observed_at: SystemTime,
        last_observed_at: SystemTime,
        burst_count: usize,
    ) -> SchedulerUpdate {
        if self.intents.contains_key(&path) {
            let (was_pending, old_queue_key) = {
                let record = self
                    .intents
                    .get(&path)
                    .expect("scheduled intent disappeared");
                (
                    record.state == ScheduledIntentState::Pending,
                    (record.queue_sequence, record.path.clone()),
                )
            };
            if was_pending {
                self.pending_index.remove(&old_queue_key);
                self.pending_reconcile_index.remove(&old_queue_key);
            }

            let record = self
                .intents
                .get_mut(&path)
                .expect("scheduled intent disappeared");
            // Latest-wins for per-path work — except a pending subtree
            // reconcile, which subsumes any per-path action for the same
            // path. Downgrading a reconcile to (say) an Upload would leave
            // the compacted-subtree boundary waiting on a reconcile intent
            // that no longer exists.
            record.kind = if record.kind == PendingIntentKind::ReconcileSubtree {
                PendingIntentKind::ReconcileSubtree
            } else {
                kind
            };
            record.first_observed_at = earliest_time(record.first_observed_at, first_observed_at);
            record.last_observed_at = latest_time(record.last_observed_at, last_observed_at);
            record.burst_count = record.burst_count.saturating_add(burst_count);
            if record.state == ScheduledIntentState::Running {
                record.dirty = true;
            } else {
                let queue_key = (record.queue_sequence, record.path.clone());
                self.pending_index.insert(queue_key.clone());
                if record.kind == PendingIntentKind::ReconcileSubtree {
                    self.pending_reconcile_index.insert(queue_key);
                }
            }

            return SchedulerUpdate {
                path,
                kind: record.kind,
                state: record.state,
                dirty: record.dirty,
                replay_count: record.replay_count,
            };
        }

        let record = ScheduledIntentRecord {
            path: path.clone(),
            kind,
            state: ScheduledIntentState::Pending,
            first_observed_at,
            last_observed_at,
            burst_count,
            dirty: false,
            replay_count: 0,
            queue_sequence: self.take_next_sequence(),
        };
        let queue_key = (record.queue_sequence, path.clone());
        self.intents.insert(path.clone(), record);
        self.pending_index.insert(queue_key.clone());
        if kind == PendingIntentKind::ReconcileSubtree {
            self.pending_reconcile_index.insert(queue_key);
        }

        SchedulerUpdate {
            path,
            kind,
            state: ScheduledIntentState::Pending,
            dirty: false,
            replay_count: 0,
        }
    }

    fn take_next_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        sequence
    }
}

/// Maps a stabilized event onto the intent that converges it. Deletion
/// is decided by ground truth, not event order: real fs-watch backends
/// (FSEvents especially) coalesce and split per-path flags, so the
/// *last* event delivered for an unlinked file is frequently a
/// write-kind — trusting it turned local deletions into upload plans
/// that no-op'd as "vanished before upload" and never removed the
/// remote copy. When the burst carried a removal or rename and the
/// path is gone at stabilization time (runtime thread — never the
/// callback), the local truth is "deleted" regardless of which
/// fragment notify delivered last.
pub(crate) fn intent_kind_for_stabilized_event(event: &StabilizedEvent) -> PendingIntentKind {
    let removal_shaped =
        event.flags.removed || event.flags.renamed || event.last_event_kind == FsEventKind::Removed;
    if removal_shaped && std::fs::symlink_metadata(&event.path).is_err() {
        return PendingIntentKind::Delete;
    }
    if event.flags.renamed {
        PendingIntentKind::Rename
    } else {
        PendingIntentKind::Upload
    }
}

fn earliest_time(left: SystemTime, right: SystemTime) -> SystemTime {
    if left <= right { left } else { right }
}

fn latest_time(left: SystemTime, right: SystemTime) -> SystemTime {
    if left >= right { left } else { right }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debounce::{DebounceClass, DebounceLoop};
    use crate::event_intents::{BoundedEventIntentMaps, EventIntentLimits, PendingEventFlags};
    use crate::fs_events::FsEventRecord;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    #[test]
    fn stabilized_create_or_modify_becomes_upload_intent() {
        let path = PathBuf::from("/tmp/vapor-root/src/main.rs");
        let mut scheduler = KeyedSupersedingScheduler::default();

        let update = scheduler.upsert_stabilized_event(stabilized_event(
            path.clone(),
            FsEventKind::Modified,
            PendingEventFlags {
                modified: true,
                ..PendingEventFlags::default()
            },
            1,
            2,
            2,
        ));

        assert_eq!(update.kind, PendingIntentKind::Upload);
        assert_eq!(scheduler.pending_count(), 1);
        assert_eq!(
            scheduler.scheduled_intent(&path).unwrap().kind,
            PendingIntentKind::Upload
        );
    }

    #[test]
    fn delete_supersedes_older_upload_for_same_path() {
        let path = PathBuf::from("/tmp/vapor-root/src/main.rs");
        let mut scheduler = KeyedSupersedingScheduler::default();

        scheduler.upsert_stabilized_event(stabilized_event(
            path.clone(),
            FsEventKind::Created,
            PendingEventFlags {
                created: true,
                ..PendingEventFlags::default()
            },
            1,
            2,
            1,
        ));
        let update = scheduler.upsert_stabilized_event(stabilized_event(
            path.clone(),
            FsEventKind::Removed,
            PendingEventFlags {
                created: true,
                removed: true,
                ..PendingEventFlags::default()
            },
            3,
            4,
            1,
        ));

        assert_eq!(update.kind, PendingIntentKind::Delete);
        let record = scheduler.scheduled_intent(&path).unwrap();
        assert_eq!(record.kind, PendingIntentKind::Delete);
        assert_eq!(record.first_observed_at, timestamp(1));
        assert_eq!(record.last_observed_at, timestamp(4));
        assert_eq!(record.burst_count, 2);
    }

    #[test]
    fn rename_is_preserved_when_followed_by_modify() {
        // Destination side of a rename: the file exists at
        // stabilization, so the rename classification survives a
        // trailing modify fragment. (A rename-flagged path that is
        // *gone* is the source side and maps to Delete — covered by
        // `deletion_is_decided_by_ground_truth_not_event_order`.)
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("renamed.rs");
        std::fs::write(&path, b"fn main() {}").expect("seed");
        let mut scheduler = KeyedSupersedingScheduler::default();

        let update = scheduler.upsert_stabilized_event(stabilized_event(
            path.clone(),
            FsEventKind::Modified,
            PendingEventFlags {
                renamed: true,
                modified: true,
                ..PendingEventFlags::default()
            },
            1,
            2,
            3,
        ));

        assert_eq!(update.kind, PendingIntentKind::Rename);
        assert_eq!(
            scheduler.scheduled_intent(&path).unwrap().kind,
            PendingIntentKind::Rename
        );
    }

    #[test]
    fn claim_next_returns_oldest_pending_intent() {
        let first = PathBuf::from("/tmp/vapor-root/a.txt");
        let second = PathBuf::from("/tmp/vapor-root/b.txt");
        let mut scheduler = KeyedSupersedingScheduler::default();

        scheduler.upsert_intent(first.clone(), PendingIntentKind::Upload, timestamp(1));
        scheduler.upsert_intent(second.clone(), PendingIntentKind::Delete, timestamp(2));

        let claimed = scheduler.claim_next().unwrap();
        assert_eq!(claimed.path, first);
        assert_eq!(claimed.kind, PendingIntentKind::Upload);
        assert_eq!(scheduler.pending_count(), 1);
        assert_eq!(scheduler.running_count(), 1);
    }

    #[test]
    fn claim_next_reconcile_ignores_non_reconcile_work() {
        let upload_path = PathBuf::from("/tmp/vapor-root/a.txt");
        let reconcile_path = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();

        scheduler.upsert_intent(upload_path, PendingIntentKind::Upload, timestamp(1));
        scheduler.upsert_intent(
            reconcile_path.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(2),
        );

        let claimed = scheduler.claim_next_reconcile().unwrap();
        assert_eq!(claimed.path, reconcile_path);
        assert_eq!(claimed.kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(scheduler.pending_count(), 1);
    }

    #[test]
    fn new_change_while_running_marks_intent_dirty_and_requeues_on_completion() {
        let path = PathBuf::from("/tmp/vapor-root/a.txt");
        let mut scheduler = KeyedSupersedingScheduler::default();

        scheduler.upsert_intent(path.clone(), PendingIntentKind::Upload, timestamp(1));
        let claimed = scheduler.claim_next().unwrap();
        assert_eq!(claimed.path, path);

        let update = scheduler.upsert_intent(path.clone(), PendingIntentKind::Delete, timestamp(2));
        assert_eq!(update.state, ScheduledIntentState::Running);
        assert!(update.dirty);

        let disposition = scheduler.complete_running(&path).unwrap();
        assert_eq!(disposition, CompletionDisposition::RequeuedDirty);

        let record = scheduler.scheduled_intent(&path).unwrap();
        assert_eq!(record.state, ScheduledIntentState::Pending);
        assert_eq!(record.kind, PendingIntentKind::Delete);
        assert_eq!(record.replay_count, 1);
        assert!(!record.dirty);
    }

    #[test]
    fn completing_clean_running_intent_removes_it() {
        let path = PathBuf::from("/tmp/vapor-root/a.txt");
        let mut scheduler = KeyedSupersedingScheduler::default();

        scheduler.upsert_intent(path.clone(), PendingIntentKind::Upload, timestamp(1));
        scheduler.claim_next().unwrap();

        let disposition = scheduler.complete_running(&path).unwrap();
        assert_eq!(disposition, CompletionDisposition::Removed);
        assert!(scheduler.scheduled_intent(&path).is_none());
        assert_eq!(scheduler.len(), 0);
    }

    #[test]
    fn explicit_reconcile_intents_can_be_queued() {
        let path = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();

        let update = scheduler.upsert_intent(
            path.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(5),
        );

        assert_eq!(update.kind, PendingIntentKind::ReconcileSubtree);
        let claimed = scheduler.claim_next().unwrap();
        assert_eq!(claimed.kind, PendingIntentKind::ReconcileSubtree);
    }

    #[test]
    fn pending_reconcile_is_not_downgraded_by_a_later_per_path_event() {
        // A plain fs event on a compacted subtree's root (e.g. a mkdir or
        // attribute change) must not replace the pending reconcile with an
        // Upload — the compaction boundary is waiting on that reconcile.
        let root = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();

        scheduler.upsert_intent(
            root.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(1),
        );
        let update = scheduler.upsert_intent(root.clone(), PendingIntentKind::Upload, timestamp(2));

        assert_eq!(update.kind, PendingIntentKind::ReconcileSubtree);
        let claimed = scheduler.claim_next_reconcile().expect("reconcile intent");
        assert_eq!(claimed.path, root);
        assert_eq!(claimed.kind, PendingIntentKind::ReconcileSubtree);
    }

    #[test]
    fn pending_intent_records_can_flow_into_scheduler() {
        let path = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();

        let update = scheduler.upsert_pending_intent_record(PendingIntentRecord {
            path: path.clone(),
            kind: PendingIntentKind::ReconcileSubtree,
            observed_at: timestamp(5),
        });

        assert_eq!(update.kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(
            scheduler.scheduled_intent(&path).unwrap().kind,
            PendingIntentKind::ReconcileSubtree
        );
    }

    #[test]
    fn debounced_events_flow_into_latest_wins_scheduler() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("src/main.rs");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root, EventIntentLimits::new(20, 20));
        let mut debounce = DebounceLoop::default();
        let mut scheduler = KeyedSupersedingScheduler::default();

        maps.record_event(FsEventRecord {
            path: path.clone(),
            kind: FsEventKind::Modified,
            observed_at: timestamp(0),
        });
        maps.record_event(FsEventRecord {
            path: path.clone(),
            kind: FsEventKind::Removed,
            observed_at: timestamp(10),
        });

        for event in debounce.run_tick(&mut maps, timestamp(1_250)) {
            scheduler.upsert_stabilized_event(event);
        }

        let record = scheduler.scheduled_intent(&path).unwrap();
        assert_eq!(record.kind, PendingIntentKind::Delete);
        assert_eq!(scheduler.pending_count(), 1);
    }

    #[test]
    fn compacted_reconcile_intents_can_flow_from_event_maps_into_scheduler() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let project_root = watch_root.join("project");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root, EventIntentLimits::new(10, 1));
        let mut scheduler = KeyedSupersedingScheduler::default();

        maps.record_event(FsEventRecord {
            path: project_root.join("a.txt"),
            kind: FsEventKind::Modified,
            observed_at: timestamp(0),
        });
        maps.record_event(FsEventRecord {
            path: project_root.join("b.txt"),
            kind: FsEventKind::Modified,
            observed_at: timestamp(1),
        });

        for intent in maps.drain_pending_intents() {
            scheduler.upsert_pending_intent_record(intent);
        }

        let record = scheduler.scheduled_intent(&project_root).unwrap();
        assert_eq!(record.kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(scheduler.pending_count(), 1);
    }

    #[test]
    fn scheduler_superseding_regression_stays_under_guardrail() {
        let path = PathBuf::from("/tmp/vapor-root/src/main.rs");
        let mut scheduler = KeyedSupersedingScheduler::default();

        let start = Instant::now();
        for index in 0..50_000 {
            scheduler.upsert_stabilized_event(stabilized_event(
                path.clone(),
                if index % 7 == 0 {
                    FsEventKind::Removed
                } else {
                    FsEventKind::Modified
                },
                PendingEventFlags {
                    modified: true,
                    removed: index % 7 == 0,
                    ..PendingEventFlags::default()
                },
                index as u64,
                index as u64 + 1,
                1,
            ));
        }
        let elapsed = start.elapsed();

        assert_eq!(scheduler.len(), 1);
        assert!(
            elapsed < Duration::from_secs(2),
            "scheduler superseding took {:?}, expected < 2s",
            elapsed
        );
    }

    #[test]
    fn deletion_is_decided_by_ground_truth_not_event_order() {
        // The real-watcher shape that used to lose deletions: FSEvents
        // splits/coalesces per-path flags, so an unlinked file's last
        // delivered fragment is often a write-kind. The burst carries
        // `removed`, the path is gone — the intent must be Delete no
        // matter which fragment arrived last.
        let temp = tempfile::TempDir::new().expect("temp");
        let vanished = temp.path().join("deleted.txt");
        let mut scheduler = KeyedSupersedingScheduler::default();
        let update = scheduler.upsert_stabilized_event(stabilized_event(
            vanished,
            FsEventKind::Modified, // trailing write fragment
            PendingEventFlags {
                removed: true,
                modified: true,
                ..PendingEventFlags::default()
            },
            1,
            2,
            2,
        ));
        assert_eq!(update.kind, PendingIntentKind::Delete);

        // The inverse race: a Removed fragment arrived last but the
        // file exists again (delete + recreate inside one quiet
        // window). Ground truth says upload the survivor — the old
        // last-kind rule would have deleted the remote copy.
        let recreated = temp.path().join("recreated.txt");
        std::fs::write(&recreated, b"back again").expect("seed");
        let update = scheduler.upsert_stabilized_event(stabilized_event(
            recreated,
            FsEventKind::Removed,
            PendingEventFlags {
                removed: true,
                created: true,
                ..PendingEventFlags::default()
            },
            3,
            4,
            2,
        ));
        assert_eq!(update.kind, PendingIntentKind::Upload);
    }

    fn stabilized_event(
        path: PathBuf,
        last_event_kind: FsEventKind,
        flags: PendingEventFlags,
        first_millis: u64,
        last_millis: u64,
        burst_count: usize,
    ) -> StabilizedEvent {
        StabilizedEvent {
            path,
            first_observed_at: timestamp(first_millis),
            last_observed_at: timestamp(last_millis),
            last_event_kind,
            flags,
            burst_count,
            debounce_class: DebounceClass::CodeText,
            quiet_window: Duration::from_millis(1_200),
        }
    }

    fn timestamp(milliseconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
