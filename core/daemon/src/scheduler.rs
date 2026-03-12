use std::collections::BTreeMap;
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
        self.intents
            .values()
            .filter(|record| record.state == ScheduledIntentState::Pending)
            .count()
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
        self.claim_next_matching(|_| true)
    }

    pub fn claim_next_reconcile(&mut self) -> Option<ClaimedIntent> {
        self.claim_next_matching(|record| record.kind == PendingIntentKind::ReconcileSubtree)
    }

    fn claim_next_matching(
        &mut self,
        mut predicate: impl FnMut(&ScheduledIntentRecord) -> bool,
    ) -> Option<ClaimedIntent> {
        let path = self
            .intents
            .iter()
            .filter(|(_, record)| {
                record.state == ScheduledIntentState::Pending && predicate(record)
            })
            .min_by(|(left_path, left_record), (right_path, right_record)| {
                left_record
                    .queue_sequence
                    .cmp(&right_record.queue_sequence)
                    .then_with(|| left_path.cmp(right_path))
            })
            .map(|(path, _)| path.clone())?;

        let record = self
            .intents
            .get_mut(&path)
            .expect("scheduled intent disappeared before claim");
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
            let record = self
                .intents
                .get_mut(path)
                .expect("scheduled intent disappeared before completion");
            record.state = ScheduledIntentState::Pending;
            record.dirty = false;
            record.replay_count = replay_count + 1;
            record.queue_sequence = sequence;
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
        if let Some(record) = self.intents.get_mut(&path) {
            record.kind = kind;
            record.first_observed_at = earliest_time(record.first_observed_at, first_observed_at);
            record.last_observed_at = latest_time(record.last_observed_at, last_observed_at);
            record.burst_count = record.burst_count.saturating_add(burst_count);
            if record.state == ScheduledIntentState::Running {
                record.dirty = true;
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
        self.intents.insert(path.clone(), record);

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

fn intent_kind_for_stabilized_event(event: &StabilizedEvent) -> PendingIntentKind {
    if event.last_event_kind == FsEventKind::Removed {
        PendingIntentKind::Delete
    } else if event.flags.renamed {
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
        let path = PathBuf::from("/tmp/vapor-root/src/renamed.rs");
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
