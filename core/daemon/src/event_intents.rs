use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use vapor_shared::constants;

use crate::fs_events::{FsEventErrorRecord, FsEventKind, FsEventRecord, FsEventRecording};
use crate::logging;
use crate::storm::{DeferredReconcileRecord, StormDetector, StormReason, StormThresholds};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventIntentLimits {
    pub max_pending_paths: usize,
    pub max_pending_paths_per_subtree: usize,
}

impl EventIntentLimits {
    pub const fn new(max_pending_paths: usize, max_pending_paths_per_subtree: usize) -> Self {
        Self {
            max_pending_paths,
            max_pending_paths_per_subtree,
        }
    }
}

impl Default for EventIntentLimits {
    fn default() -> Self {
        Self::new(
            constants::engine::MAX_IN_MEMORY_PENDING_PATHS,
            constants::engine::MAX_IN_MEMORY_PENDING_PATHS_PER_SUBTREE,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PendingEventFlags {
    pub created: bool,
    pub modified: bool,
    pub removed: bool,
    pub renamed: bool,
    pub other: bool,
}

impl PendingEventFlags {
    fn include(&mut self, kind: &FsEventKind) {
        match kind {
            FsEventKind::Created => self.created = true,
            FsEventKind::Modified => self.modified = true,
            FsEventKind::Removed => self.removed = true,
            FsEventKind::Renamed => self.renamed = true,
            FsEventKind::Other => self.other = true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingEventRecord {
    pub path: PathBuf,
    pub first_observed_at: SystemTime,
    pub last_observed_at: SystemTime,
    pub last_event_kind: FsEventKind,
    pub flags: PendingEventFlags,
    pub burst_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingIntentKind {
    Upload,
    Delete,
    Rename,
    ReconcileSubtree,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingIntentRecord {
    pub path: PathBuf,
    pub kind: PendingIntentKind,
    pub observed_at: SystemTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackpressureReason {
    SubtreeCapExceeded,
    GlobalCapExceeded,
    DirectoryUniquePathsStorm,
    DirectoryEventCountStorm,
    GlobalPendingEventCountStorm,
}

impl BackpressureReason {
    fn storm_reason(self) -> Option<StormReason> {
        match self {
            Self::DirectoryUniquePathsStorm => Some(StormReason::DirectoryUniquePathsThreshold),
            Self::DirectoryEventCountStorm => Some(StormReason::DirectoryEventCountThreshold),
            Self::GlobalPendingEventCountStorm => {
                Some(StormReason::GlobalPendingEventCountThreshold)
            }
            Self::SubtreeCapExceeded | Self::GlobalCapExceeded => None,
        }
    }
}

impl From<StormReason> for BackpressureReason {
    fn from(value: StormReason) -> Self {
        match value {
            StormReason::DirectoryUniquePathsThreshold => Self::DirectoryUniquePathsStorm,
            StormReason::DirectoryEventCountThreshold => Self::DirectoryEventCountStorm,
            StormReason::GlobalPendingEventCountThreshold => Self::GlobalPendingEventCountStorm,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactedSubtreeRecord {
    pub root: PathBuf,
    pub reason: BackpressureReason,
    pub first_compacted_at: SystemTime,
    pub last_observed_at: SystemTime,
    pub suppressed_event_count: usize,
    pub suppressed_intent_count: usize,
    pub compaction_count: usize,
}

impl CompactedSubtreeRecord {
    fn note_event(&mut self, observed_at: SystemTime) {
        self.last_observed_at = observed_at;
        self.suppressed_event_count += 1;
    }

    fn note_intent(&mut self, observed_at: SystemTime) {
        self.last_observed_at = observed_at;
        self.suppressed_intent_count += 1;
    }
}

#[derive(Debug)]
pub struct BoundedEventIntentMaps {
    watch_root: PathBuf,
    limits: EventIntentLimits,
    event_map: BTreeMap<PathBuf, PendingEventRecord>,
    intent_map: BTreeMap<PathBuf, PendingIntentRecord>,
    compacted_subtrees: BTreeMap<PathBuf, CompactedSubtreeRecord>,
    deferred_reconciles: BTreeMap<PathBuf, DeferredReconcileRecord>,
    tracked_path_refcounts: BTreeMap<PathBuf, usize>,
    subtree_pending_counts: BTreeMap<PathBuf, usize>,
    storm_detector: StormDetector,
}

impl BoundedEventIntentMaps {
    pub fn new(watch_root: impl Into<PathBuf>) -> Self {
        Self::with_limits(watch_root, EventIntentLimits::default())
    }

    pub fn with_limits(watch_root: impl Into<PathBuf>, limits: EventIntentLimits) -> Self {
        Self::with_limits_and_storm_thresholds(watch_root, limits, StormThresholds::default())
    }

    pub fn with_limits_and_storm_thresholds(
        watch_root: impl Into<PathBuf>,
        limits: EventIntentLimits,
        storm_thresholds: StormThresholds,
    ) -> Self {
        let watch_root = watch_root.into();
        Self {
            watch_root: watch_root.clone(),
            limits,
            event_map: BTreeMap::new(),
            intent_map: BTreeMap::new(),
            compacted_subtrees: BTreeMap::new(),
            deferred_reconciles: BTreeMap::new(),
            tracked_path_refcounts: BTreeMap::new(),
            subtree_pending_counts: BTreeMap::new(),
            storm_detector: StormDetector::with_thresholds(watch_root, storm_thresholds),
        }
    }

    pub fn watch_root(&self) -> &Path {
        &self.watch_root
    }

    pub fn limits(&self) -> &EventIntentLimits {
        &self.limits
    }

    pub fn pending_event_count(&self) -> usize {
        self.event_map.len()
    }

    pub fn pending_intent_count(&self) -> usize {
        self.intent_map.len()
    }

    pub fn tracked_path_count(&self) -> usize {
        self.tracked_path_refcounts.len()
    }

    pub fn compacted_subtree_count(&self) -> usize {
        self.compacted_subtrees.len()
    }

    pub fn deferred_reconcile_count(&self) -> usize {
        self.deferred_reconciles.len()
    }

    pub fn pending_event(&self, path: &Path) -> Option<&PendingEventRecord> {
        self.event_map.get(path)
    }

    pub fn pending_intent(&self, path: &Path) -> Option<&PendingIntentRecord> {
        self.intent_map.get(path)
    }

    pub fn compacted_subtree(&self, root: &Path) -> Option<&CompactedSubtreeRecord> {
        self.compacted_subtrees.get(root)
    }

    pub fn deferred_reconcile(&self, root: &Path) -> Option<&DeferredReconcileRecord> {
        self.deferred_reconciles.get(root)
    }

    pub fn compacted_subtree_requires_follow_up_reconcile(&self, root: &Path) -> bool {
        self.deferred_reconciles.contains_key(root)
            || matches!(self.intent_map.get(root), Some(record) if record.kind == PendingIntentKind::ReconcileSubtree)
    }

    pub fn subtree_pending_count(&self, root: &Path) -> usize {
        self.subtree_pending_counts.get(root).copied().unwrap_or(0)
    }

    pub fn drain_ready_events(
        &mut self,
        mut is_ready: impl FnMut(&PendingEventRecord) -> bool,
    ) -> Vec<PendingEventRecord> {
        self.drain_ready_events_with(|record| is_ready(record).then_some(()))
            .into_iter()
            .map(|(record, ())| record)
            .collect()
    }

    pub fn drain_ready_events_with<T>(
        &mut self,
        mut select_ready_metadata: impl FnMut(&PendingEventRecord) -> Option<T>,
    ) -> Vec<(PendingEventRecord, T)> {
        let ready_paths_with_metadata: Vec<(PathBuf, T)> = self
            .event_map
            .iter()
            .filter_map(|(path, record)| {
                select_ready_metadata(record).map(|metadata| (path.clone(), metadata))
            })
            .collect();

        let mut ready_records = Vec::with_capacity(ready_paths_with_metadata.len());
        for (path, metadata) in ready_paths_with_metadata {
            if let Some(record) = self.event_map.remove(&path) {
                self.untrack_path(&path);
                ready_records.push((record, metadata));
            }
        }

        ready_records
    }

    pub fn drain_pending_intents(&mut self) -> Vec<PendingIntentRecord> {
        let pending_paths: Vec<PathBuf> = self.intent_map.keys().cloned().collect();
        let mut pending_intents = Vec::with_capacity(pending_paths.len());
        for path in pending_paths {
            if let Some(record) = self.intent_map.remove(&path) {
                self.untrack_path(&path);
                pending_intents.push(record);
            }
        }

        pending_intents
    }

    pub fn take_ready_deferred_reconcile_intents(
        &mut self,
        now: SystemTime,
    ) -> Vec<PendingIntentRecord> {
        let ready_roots: Vec<PathBuf> = self
            .deferred_reconciles
            .iter()
            .filter_map(|(root, record)| (record.available_at <= now).then_some(root.clone()))
            .collect();
        let mut ready_intents = Vec::with_capacity(ready_roots.len());
        for root in ready_roots {
            if let Some(record) = self.deferred_reconciles.remove(&root) {
                self.untrack_path(&root);
                ready_intents.push(PendingIntentRecord {
                    path: root,
                    kind: PendingIntentKind::ReconcileSubtree,
                    observed_at: record.available_at,
                });
            }
        }

        ready_intents
    }

    pub fn clear_compacted_subtree_boundary(&mut self, root: &Path) -> bool {
        if self.compacted_subtree_requires_follow_up_reconcile(root) {
            return false;
        }

        self.compacted_subtrees.remove(root).is_some()
    }

    pub fn record_event(&mut self, event: FsEventRecord) {
        if !self.path_is_in_scope(&event.path) {
            logging::warning(
                "Ignored filesystem event outside watch root",
                &[
                    ("watch_root", self.watch_root.display().to_string()),
                    ("path", event.path.display().to_string()),
                ],
            );
            return;
        }

        if let Some(compacted_root) = self.compacted_subtree_root_for(&event.path) {
            self.note_absorbed_event(compacted_root.as_path(), event.observed_at);
            return;
        }

        if let Some(existing) = self.event_map.get_mut(&event.path) {
            existing.last_observed_at = event.observed_at;
            existing.last_event_kind = event.kind.clone();
            existing.flags.include(&event.kind);
            existing.burst_count += 1;
        } else {
            if !self.path_is_tracked(&event.path)
                && let Some(compacted_root) =
                    self.prepare_capacity_for_new_path(&event.path, event.observed_at)
            {
                self.note_absorbed_event(compacted_root.as_path(), event.observed_at);
                return;
            }

            let mut flags = PendingEventFlags::default();
            flags.include(&event.kind);
            self.track_path(&event.path);
            self.event_map.insert(
                event.path.clone(),
                PendingEventRecord {
                    path: event.path.clone(),
                    first_observed_at: event.observed_at,
                    last_observed_at: event.observed_at,
                    last_event_kind: event.kind,
                    flags,
                    burst_count: 1,
                },
            );
        }

        if let Some(trigger) = self.storm_detector.observe_event(
            &event.path,
            event.observed_at,
            self.pending_event_count(),
        ) {
            self.compact_subtree(
                trigger.root.as_path(),
                trigger.reason.into(),
                event.observed_at,
                Some(trigger.available_at),
            );
        }
    }

    pub fn upsert_intent(
        &mut self,
        path: impl Into<PathBuf>,
        kind: PendingIntentKind,
        observed_at: SystemTime,
    ) {
        let path = path.into();
        if !self.path_is_in_scope(&path) {
            logging::warning(
                "Ignored pending intent outside watch root",
                &[
                    ("watch_root", self.watch_root.display().to_string()),
                    ("path", path.display().to_string()),
                ],
            );
            return;
        }

        if let Some(compacted_root) = self.compacted_subtree_root_for(&path) {
            self.note_absorbed_intent(compacted_root.as_path(), observed_at);
            return;
        }

        if let Some(existing) = self.intent_map.get_mut(&path) {
            existing.kind = kind;
            existing.observed_at = observed_at;
        } else {
            if !self.path_is_tracked(&path)
                && let Some(compacted_root) = self.prepare_capacity_for_new_path(&path, observed_at)
            {
                self.note_absorbed_intent(compacted_root.as_path(), observed_at);
                return;
            }

            self.track_path(&path);
            self.intent_map.insert(
                path.clone(),
                PendingIntentRecord {
                    path: path.clone(),
                    kind,
                    observed_at,
                },
            );
        }
    }

    fn prepare_capacity_for_new_path(
        &mut self,
        path: &Path,
        observed_at: SystemTime,
    ) -> Option<PathBuf> {
        if let Some(compacted_root) = self.compacted_subtree_root_for(path) {
            return Some(compacted_root);
        }

        if let Some(subtree_root) = self.deepest_full_subtree_for_path(path) {
            self.compact_subtree(
                subtree_root.as_path(),
                BackpressureReason::SubtreeCapExceeded,
                observed_at,
                None,
            );
        } else if self.tracked_path_count() >= self.limits.max_pending_paths {
            let watch_root = self.watch_root.clone();
            self.compact_subtree(
                watch_root.as_path(),
                BackpressureReason::GlobalCapExceeded,
                observed_at,
                None,
            );
        }

        self.compacted_subtree_root_for(path)
    }

    fn compact_subtree(
        &mut self,
        subtree_root: &Path,
        reason: BackpressureReason,
        observed_at: SystemTime,
        deferred_available_at: Option<SystemTime>,
    ) {
        let event_paths: Vec<PathBuf> = self
            .event_map
            .keys()
            .filter(|path| path_is_within_subtree(path.as_path(), subtree_root))
            .cloned()
            .collect();
        let intent_paths: Vec<PathBuf> = self
            .intent_map
            .keys()
            .filter(|path| path_is_within_subtree(path.as_path(), subtree_root))
            .cloned()
            .collect();
        let compacted_roots: Vec<PathBuf> = self
            .compacted_subtrees
            .keys()
            .filter(|path| path_is_within_subtree(path.as_path(), subtree_root))
            .cloned()
            .collect();
        let deferred_roots: Vec<PathBuf> = self
            .deferred_reconciles
            .keys()
            .filter(|path| path_is_within_subtree(path.as_path(), subtree_root))
            .cloned()
            .collect();

        let suppressed_event_count = event_paths.len();
        let suppressed_intent_count = intent_paths.len();

        for path in event_paths {
            self.event_map.remove(&path);
            self.untrack_path(&path);
        }

        for path in intent_paths {
            self.intent_map.remove(&path);
            self.untrack_path(&path);
        }

        for root in deferred_roots {
            self.deferred_reconciles.remove(&root);
            self.untrack_path(&root);
        }

        let mut total_suppressed_event_count = suppressed_event_count;
        let mut total_suppressed_intent_count = suppressed_intent_count;
        let mut compaction_count = 1usize;
        let mut first_compacted_at = observed_at;

        for root in compacted_roots {
            if let Some(existing) = self.compacted_subtrees.remove(&root) {
                total_suppressed_event_count += existing.suppressed_event_count;
                total_suppressed_intent_count += existing.suppressed_intent_count;
                compaction_count += existing.compaction_count;
                if root == subtree_root {
                    first_compacted_at = existing.first_compacted_at;
                }
            }
        }

        self.compacted_subtrees.insert(
            subtree_root.to_path_buf(),
            CompactedSubtreeRecord {
                root: subtree_root.to_path_buf(),
                reason,
                first_compacted_at,
                last_observed_at: observed_at,
                suppressed_event_count: total_suppressed_event_count,
                suppressed_intent_count: total_suppressed_intent_count,
                compaction_count,
            },
        );
        if let Some(deferred_available_at) = deferred_available_at {
            self.schedule_deferred_reconcile(
                subtree_root,
                reason
                    .storm_reason()
                    .expect("deferred reconcile requires a storm reason"),
                observed_at,
                deferred_available_at,
            );
        } else {
            self.upsert_reconcile_intent(subtree_root, observed_at);
        }

        logging::warning(
            "Compacted pending paths into a subtree reconcile intent",
            &[
                ("subtree", subtree_root.display().to_string()),
                ("reason", format!("{:?}", reason)),
                ("tracked_path_count", self.tracked_path_count().to_string()),
                (
                    "suppressed_event_count",
                    total_suppressed_event_count.to_string(),
                ),
                (
                    "suppressed_intent_count",
                    total_suppressed_intent_count.to_string(),
                ),
                (
                    "deferred_reconcile",
                    deferred_available_at.is_some().to_string(),
                ),
            ],
        );
    }

    fn deepest_full_subtree_for_path(&self, path: &Path) -> Option<PathBuf> {
        path.ancestors()
            .take_while(|ancestor| ancestor.starts_with(&self.watch_root))
            .filter(|ancestor| *ancestor != self.watch_root.as_path())
            .find(|ancestor| {
                self.subtree_pending_count(ancestor) >= self.limits.max_pending_paths_per_subtree
            })
            .map(Path::to_path_buf)
    }

    fn note_absorbed_event(&mut self, subtree_root: &Path, observed_at: SystemTime) {
        if let Some(record) = self.compacted_subtrees.get_mut(subtree_root) {
            record.note_event(observed_at);
        }
        self.refresh_compacted_subtree_reconcile(subtree_root, observed_at);
    }

    fn note_absorbed_intent(&mut self, subtree_root: &Path, observed_at: SystemTime) {
        if let Some(record) = self.compacted_subtrees.get_mut(subtree_root) {
            record.note_intent(observed_at);
        }
        self.refresh_compacted_subtree_reconcile(subtree_root, observed_at);
    }

    fn refresh_compacted_subtree_reconcile(
        &mut self,
        subtree_root: &Path,
        observed_at: SystemTime,
    ) {
        if let Some(storm_reason) = self.compacted_storm_reason(subtree_root) {
            self.schedule_deferred_reconcile(
                subtree_root,
                storm_reason,
                observed_at,
                self.storm_detector.deferred_available_at(observed_at),
            );
            return;
        }

        self.upsert_reconcile_intent(subtree_root, observed_at);
    }

    fn upsert_reconcile_intent(&mut self, subtree_root: &Path, observed_at: SystemTime) {
        if let Some(existing) = self.intent_map.get_mut(subtree_root) {
            existing.kind = PendingIntentKind::ReconcileSubtree;
            existing.observed_at = observed_at;
            return;
        }

        self.track_path(subtree_root);
        self.intent_map.insert(
            subtree_root.to_path_buf(),
            PendingIntentRecord {
                path: subtree_root.to_path_buf(),
                kind: PendingIntentKind::ReconcileSubtree,
                observed_at,
            },
        );
    }

    fn schedule_deferred_reconcile(
        &mut self,
        subtree_root: &Path,
        reason: StormReason,
        observed_at: SystemTime,
        available_at: SystemTime,
    ) {
        if let Some(existing) = self.deferred_reconciles.get_mut(subtree_root) {
            existing.reason = reason;
            existing.last_observed_at = observed_at;
            existing.available_at = existing.available_at.max(available_at);
            existing.reschedule_count += 1;
            return;
        }

        self.track_path(subtree_root);
        self.deferred_reconciles.insert(
            subtree_root.to_path_buf(),
            DeferredReconcileRecord {
                root: subtree_root.to_path_buf(),
                reason,
                first_detected_at: observed_at,
                last_observed_at: observed_at,
                available_at,
                reschedule_count: 0,
            },
        );
    }

    fn compacted_storm_reason(&self, subtree_root: &Path) -> Option<StormReason> {
        self.compacted_subtrees
            .get(subtree_root)
            .and_then(|record| record.reason.storm_reason())
    }

    fn compacted_subtree_root_for(&self, path: &Path) -> Option<PathBuf> {
        self.compacted_subtrees
            .keys()
            .filter(|root| path_is_within_subtree(path, root.as_path()))
            .max_by_key(|root| root.components().count())
            .cloned()
    }

    fn path_is_in_scope(&self, path: &Path) -> bool {
        path.starts_with(&self.watch_root)
    }

    fn path_is_tracked(&self, path: &Path) -> bool {
        self.tracked_path_refcounts.contains_key(path)
    }

    fn track_path(&mut self, path: &Path) {
        if let Some(refcount) = self.tracked_path_refcounts.get_mut(path) {
            *refcount += 1;
            return;
        }

        self.tracked_path_refcounts.insert(path.to_path_buf(), 1);
        for subtree_root in self.subtree_roots_for_path(path) {
            *self.subtree_pending_counts.entry(subtree_root).or_insert(0) += 1;
        }
    }

    fn untrack_path(&mut self, path: &Path) {
        let should_remove = match self.tracked_path_refcounts.get_mut(path) {
            Some(refcount) if *refcount > 1 => {
                *refcount -= 1;
                false
            }
            Some(_) => true,
            None => false,
        };

        if !should_remove {
            return;
        }

        self.tracked_path_refcounts.remove(path);
        for subtree_root in self.subtree_roots_for_path(path) {
            let Some(count) = self.subtree_pending_counts.get_mut(&subtree_root) else {
                continue;
            };
            *count -= 1;
            let remove_entry = *count == 0;
            if remove_entry {
                self.subtree_pending_counts.remove(&subtree_root);
            }
        }
    }

    fn subtree_roots_for_path(&self, path: &Path) -> Vec<PathBuf> {
        let mut subtree_roots = Vec::new();
        for ancestor in path.ancestors() {
            if ancestor.starts_with(&self.watch_root) {
                subtree_roots.push(ancestor.to_path_buf());
            }
            if ancestor == self.watch_root {
                break;
            }
        }
        subtree_roots
    }
}

#[derive(Debug)]
pub struct BoundedFsEventRecorder {
    maps: Mutex<BoundedEventIntentMaps>,
    error_count: Mutex<usize>,
}

impl BoundedFsEventRecorder {
    pub fn new(watch_root: impl Into<PathBuf>) -> Self {
        Self::with_limits(watch_root, EventIntentLimits::default())
    }

    pub fn with_limits(watch_root: impl Into<PathBuf>, limits: EventIntentLimits) -> Self {
        Self {
            maps: Mutex::new(BoundedEventIntentMaps::with_limits(watch_root, limits)),
            error_count: Mutex::new(0),
        }
    }

    pub fn error_count(&self) -> usize {
        *self.error_count.lock().expect("error count mutex poisoned")
    }

    pub fn with_state<R>(&self, reader: impl FnOnce(&BoundedEventIntentMaps) -> R) -> R {
        let guard = self.maps.lock().expect("event intent mutex poisoned");
        reader(&guard)
    }

    pub fn with_mut_state<R>(&self, writer: impl FnOnce(&mut BoundedEventIntentMaps) -> R) -> R {
        let mut guard = self.maps.lock().expect("event intent mutex poisoned");
        writer(&mut guard)
    }
}

impl FsEventRecording for BoundedFsEventRecorder {
    fn record_event(&self, event: FsEventRecord) {
        self.maps
            .lock()
            .expect("event intent mutex poisoned")
            .record_event(event);
    }

    fn record_error(&self, error: FsEventErrorRecord) {
        *self.error_count.lock().expect("error count mutex poisoned") += 1;
        logging::warning(
            "Recorded filesystem watcher error for bounded callback storage",
            &[("error", error.description)],
        );
    }
}

fn path_is_within_subtree(path: &Path, subtree_root: &Path) -> bool {
    path.starts_with(subtree_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storm::{StormReason, StormThresholds};
    use std::time::{Duration, Instant, UNIX_EPOCH};

    #[test]
    fn event_and_intent_for_same_path_share_one_tracked_path_count() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("project/file.txt");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root, EventIntentLimits::new(10, 10));

        maps.record_event(fs_event(path.clone(), FsEventKind::Modified, 1));
        maps.upsert_intent(path.clone(), PendingIntentKind::Upload, timestamp(2));

        assert_eq!(maps.pending_event_count(), 1);
        assert_eq!(maps.pending_intent_count(), 1);
        assert_eq!(maps.tracked_path_count(), 1);
    }

    #[test]
    fn event_record_updates_flags_and_burst_count_for_existing_path() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("project/file.txt");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root, EventIntentLimits::new(10, 10));

        maps.record_event(fs_event(path.clone(), FsEventKind::Created, 1));
        maps.record_event(fs_event(path.clone(), FsEventKind::Modified, 2));
        maps.record_event(fs_event(path.clone(), FsEventKind::Renamed, 3));

        let pending = maps.pending_event(&path).expect("missing pending event");
        assert_eq!(pending.first_observed_at, timestamp(1));
        assert_eq!(pending.last_observed_at, timestamp(3));
        assert_eq!(pending.last_event_kind, FsEventKind::Renamed);
        assert_eq!(pending.burst_count, 3);
        assert!(pending.flags.created);
        assert!(pending.flags.modified);
        assert!(pending.flags.renamed);
    }

    #[test]
    fn subtree_cap_compacts_noisy_subtree_into_reconcile_intent() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let project_root = watch_root.join("project");
        let outside_path = watch_root.join("other/file.txt");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root, EventIntentLimits::new(10, 2));

        maps.record_event(fs_event(
            project_root.join("a.txt"),
            FsEventKind::Modified,
            1,
        ));
        maps.record_event(fs_event(
            project_root.join("b.txt"),
            FsEventKind::Modified,
            2,
        ));
        maps.record_event(fs_event(outside_path.clone(), FsEventKind::Created, 3));
        maps.record_event(fs_event(
            project_root.join("c.txt"),
            FsEventKind::Modified,
            4,
        ));

        assert!(maps.pending_event(&project_root.join("a.txt")).is_none());
        assert!(maps.pending_event(&project_root.join("b.txt")).is_none());
        assert!(maps.pending_event(&project_root.join("c.txt")).is_none());
        assert!(maps.pending_event(&outside_path).is_some());
        assert_eq!(maps.pending_event_count(), 1);
        assert_eq!(maps.pending_intent_count(), 1);
        assert_eq!(maps.tracked_path_count(), 2);

        let reconcile = maps
            .pending_intent(&project_root)
            .expect("missing reconcile intent");
        assert_eq!(reconcile.kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(reconcile.observed_at, timestamp(4));

        let compacted = maps
            .compacted_subtree(&project_root)
            .expect("missing compacted subtree record");
        assert_eq!(compacted.reason, BackpressureReason::SubtreeCapExceeded);
        assert_eq!(compacted.suppressed_event_count, 3);
        assert_eq!(compacted.suppressed_intent_count, 0);
        assert_eq!(maps.subtree_pending_count(&project_root), 1);
    }

    #[test]
    fn global_cap_compacts_watch_root_when_unique_paths_exceed_limit() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root.clone(), EventIntentLimits::new(3, 10));

        maps.record_event(fs_event(watch_root.join("a.txt"), FsEventKind::Modified, 1));
        maps.record_event(fs_event(watch_root.join("b.txt"), FsEventKind::Modified, 2));
        maps.record_event(fs_event(watch_root.join("c.txt"), FsEventKind::Modified, 3));
        maps.record_event(fs_event(watch_root.join("d.txt"), FsEventKind::Modified, 4));

        assert_eq!(maps.pending_event_count(), 0);
        assert_eq!(maps.pending_intent_count(), 1);
        assert_eq!(maps.tracked_path_count(), 1);

        let reconcile = maps
            .pending_intent(&watch_root)
            .expect("missing global reconcile intent");
        assert_eq!(reconcile.kind, PendingIntentKind::ReconcileSubtree);

        let compacted = maps
            .compacted_subtree(&watch_root)
            .expect("missing compacted watch root record");
        assert_eq!(compacted.reason, BackpressureReason::GlobalCapExceeded);
        assert_eq!(compacted.suppressed_event_count, 4);
    }

    #[test]
    fn paths_inside_compacted_subtree_are_absorbed_without_direct_growth() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let project_root = watch_root.join("project");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root, EventIntentLimits::new(10, 1));

        maps.record_event(fs_event(
            project_root.join("a.txt"),
            FsEventKind::Modified,
            1,
        ));
        maps.record_event(fs_event(
            project_root.join("b.txt"),
            FsEventKind::Modified,
            2,
        ));

        maps.record_event(fs_event(
            project_root.join("c.txt"),
            FsEventKind::Removed,
            3,
        ));
        maps.upsert_intent(
            project_root.join("d.txt"),
            PendingIntentKind::Delete,
            timestamp(4),
        );

        assert_eq!(maps.pending_event_count(), 0);
        assert_eq!(maps.pending_intent_count(), 1);
        assert_eq!(maps.tracked_path_count(), 1);

        let compacted = maps
            .compacted_subtree(&project_root)
            .expect("missing compacted subtree record");
        assert_eq!(compacted.suppressed_event_count, 3);
        assert_eq!(compacted.suppressed_intent_count, 1);

        let reconcile = maps
            .pending_intent(&project_root)
            .expect("missing reconcile intent");
        assert_eq!(reconcile.observed_at, timestamp(4));
    }

    #[test]
    fn bounded_recorder_routes_events_and_errors_into_shared_state() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let recorder =
            BoundedFsEventRecorder::with_limits(watch_root.clone(), EventIntentLimits::new(10, 10));

        recorder.record_event(fs_event(
            watch_root.join("project/file.txt"),
            FsEventKind::Created,
            1,
        ));
        recorder.record_error(FsEventErrorRecord {
            description: "callback failed".to_string(),
        });

        assert_eq!(recorder.error_count(), 1);
        recorder.with_state(|maps| {
            assert_eq!(maps.pending_event_count(), 1);
            assert_eq!(maps.tracked_path_count(), 1);
        });
    }

    #[test]
    fn draining_pending_intents_clears_intent_entries_and_tracking() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("project/file.txt");
        let mut maps =
            BoundedEventIntentMaps::with_limits(watch_root, EventIntentLimits::new(10, 10));

        maps.upsert_intent(path.clone(), PendingIntentKind::Upload, timestamp(1));

        let intents = maps.drain_pending_intents();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].path, path);
        assert_eq!(intents[0].kind, PendingIntentKind::Upload);
        assert_eq!(maps.pending_intent_count(), 0);
        assert_eq!(maps.tracked_path_count(), 0);
    }

    #[test]
    fn unique_path_storm_compacts_subtree_into_deferred_reconcile() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let subtree_root = watch_root.join("project/sub");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root,
            EventIntentLimits::new(100, 100),
            StormThresholds {
                window: Duration::from_secs(2),
                directory_unique_paths_threshold: 2,
                directory_event_count_threshold: 99,
                global_pending_event_count_threshold: 99,
                deferred_reconcile_delay: Duration::from_secs(30),
            },
        );

        maps.record_event(fs_event(
            subtree_root.join("a.txt"),
            FsEventKind::Modified,
            1,
        ));
        maps.record_event(fs_event(
            subtree_root.join("b.txt"),
            FsEventKind::Modified,
            2,
        ));

        assert_eq!(maps.pending_event_count(), 0);
        assert_eq!(maps.pending_intent_count(), 0);
        assert_eq!(maps.deferred_reconcile_count(), 1);
        assert_eq!(maps.tracked_path_count(), 1);

        let deferred = maps
            .deferred_reconcile(&subtree_root)
            .expect("missing deferred reconcile");
        assert_eq!(deferred.reason, StormReason::DirectoryUniquePathsThreshold);
        assert_eq!(deferred.available_at, timestamp(32));

        let compacted = maps
            .compacted_subtree(&subtree_root)
            .expect("missing compacted subtree record");
        assert_eq!(
            compacted.reason,
            BackpressureReason::DirectoryUniquePathsStorm
        );
        assert_eq!(compacted.suppressed_event_count, 2);

        assert!(
            maps.take_ready_deferred_reconcile_intents(timestamp(31))
                .is_empty()
        );
        let ready = maps.take_ready_deferred_reconcile_intents(timestamp(32));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].path, subtree_root);
        assert_eq!(ready[0].kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(maps.deferred_reconcile_count(), 0);
        assert_eq!(maps.pending_intent_count(), 0);
    }

    #[test]
    fn absorbed_events_extend_deferred_reconcile_delay_inside_stormed_subtree() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let subtree_root = watch_root.join("project/sub");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root,
            EventIntentLimits::new(100, 100),
            StormThresholds {
                window: Duration::from_secs(2),
                directory_unique_paths_threshold: 2,
                directory_event_count_threshold: 99,
                global_pending_event_count_threshold: 99,
                deferred_reconcile_delay: Duration::from_secs(30),
            },
        );

        maps.record_event(fs_event(
            subtree_root.join("a.txt"),
            FsEventKind::Modified,
            1,
        ));
        maps.record_event(fs_event(
            subtree_root.join("b.txt"),
            FsEventKind::Modified,
            2,
        ));
        maps.record_event(fs_event(
            subtree_root.join("c.txt"),
            FsEventKind::Removed,
            3,
        ));

        let deferred = maps
            .deferred_reconcile(&subtree_root)
            .expect("missing deferred reconcile");
        assert_eq!(deferred.available_at, timestamp(33));
        assert_eq!(deferred.reschedule_count, 1);
        assert_eq!(maps.pending_event_count(), 0);

        let compacted = maps
            .compacted_subtree(&subtree_root)
            .expect("missing compacted subtree record");
        assert_eq!(compacted.suppressed_event_count, 3);
    }

    #[test]
    fn repeated_events_can_trigger_directory_event_count_storm() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let subtree_root = watch_root.join("project");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root,
            EventIntentLimits::new(100, 100),
            StormThresholds {
                window: Duration::from_secs(2),
                directory_unique_paths_threshold: 99,
                directory_event_count_threshold: 3,
                global_pending_event_count_threshold: 99,
                deferred_reconcile_delay: Duration::from_secs(10),
            },
        );

        let path = subtree_root.join("file.txt");
        maps.record_event(fs_event(path.clone(), FsEventKind::Modified, 1));
        maps.record_event(fs_event(path.clone(), FsEventKind::Modified, 2));
        maps.record_event(fs_event(path, FsEventKind::Modified, 3));

        let deferred = maps
            .deferred_reconcile(&subtree_root)
            .expect("missing deferred reconcile");
        assert_eq!(deferred.reason, StormReason::DirectoryEventCountThreshold);

        let compacted = maps
            .compacted_subtree(&subtree_root)
            .expect("missing compacted subtree record");
        assert_eq!(
            compacted.reason,
            BackpressureReason::DirectoryEventCountStorm
        );
    }

    #[test]
    fn global_pending_event_threshold_defers_watch_root_reconcile() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root.clone(),
            EventIntentLimits::new(100, 100),
            StormThresholds {
                window: Duration::from_secs(2),
                directory_unique_paths_threshold: 99,
                directory_event_count_threshold: 99,
                global_pending_event_count_threshold: 2,
                deferred_reconcile_delay: Duration::from_secs(15),
            },
        );

        maps.record_event(fs_event(watch_root.join("a.txt"), FsEventKind::Modified, 1));
        maps.record_event(fs_event(watch_root.join("b.txt"), FsEventKind::Modified, 2));

        assert_eq!(maps.pending_event_count(), 0);
        let deferred = maps
            .deferred_reconcile(&watch_root)
            .expect("missing global deferred reconcile");
        assert_eq!(
            deferred.reason,
            StormReason::GlobalPendingEventCountThreshold
        );

        let compacted = maps
            .compacted_subtree(&watch_root)
            .expect("missing compacted watch root record");
        assert_eq!(
            compacted.reason,
            BackpressureReason::GlobalPendingEventCountStorm
        );
        assert_eq!(compacted.suppressed_event_count, 2);
    }

    #[test]
    fn subtree_cap_stress_keeps_large_single_directory_bounded() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let subtree_root = watch_root.join("project/stress");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root,
            EventIntentLimits::new(20_000, 5_000),
            disabled_storm_thresholds(),
        );

        let start = Instant::now();
        for index in 0..6_000 {
            maps.record_event(fs_event(
                subtree_root.join(format!("file-{index}.txt")),
                FsEventKind::Modified,
                index as u64,
            ));
        }
        let elapsed = start.elapsed();

        assert_eq!(maps.pending_event_count(), 0);
        assert_eq!(maps.pending_intent_count(), 1);
        assert_eq!(maps.tracked_path_count(), 1);
        let compacted = maps
            .compacted_subtree(&subtree_root)
            .expect("missing compacted subtree record");
        assert_eq!(compacted.reason, BackpressureReason::SubtreeCapExceeded);
        assert_eq!(compacted.suppressed_event_count, 6_000);
        assert!(
            elapsed < Duration::from_secs(5),
            "subtree cap stress test took {:?}, expected < 5s",
            elapsed
        );
    }

    #[test]
    fn global_cap_stress_keeps_large_root_bounded() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root.clone(),
            EventIntentLimits::new(20_000, 50_000),
            disabled_storm_thresholds(),
        );

        let start = Instant::now();
        for index in 0..25_000 {
            maps.record_event(fs_event(
                watch_root.join(format!("file-{index}.txt")),
                FsEventKind::Modified,
                index as u64,
            ));
        }
        let elapsed = start.elapsed();

        assert_eq!(maps.pending_event_count(), 0);
        assert_eq!(maps.pending_intent_count(), 1);
        assert_eq!(maps.tracked_path_count(), 1);
        let compacted = maps
            .compacted_subtree(&watch_root)
            .expect("missing compacted watch root record");
        assert_eq!(compacted.reason, BackpressureReason::GlobalCapExceeded);
        assert_eq!(compacted.suppressed_event_count, 25_000);
        assert!(
            elapsed < Duration::from_secs(5),
            "global cap stress test took {:?}, expected < 5s",
            elapsed
        );
    }

    #[test]
    fn multi_subtree_storm_stress_keeps_only_deferred_markers_in_memory() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root.clone(),
            EventIntentLimits::new(20_000, 5_000),
            StormThresholds {
                global_pending_event_count_threshold: usize::MAX,
                ..StormThresholds::default()
            },
        );

        let start = Instant::now();
        for subtree_index in 0..40 {
            let subtree_root = watch_root.join(format!("storm-{subtree_index}"));
            for file_index in 0..250 {
                maps.record_event(fs_event(
                    subtree_root.join(format!("file-{file_index}.txt")),
                    FsEventKind::Modified,
                    (subtree_index * 10 + file_index / 200) as u64,
                ));
            }
        }
        let elapsed = start.elapsed();

        assert_eq!(maps.pending_event_count(), 0);
        assert_eq!(maps.pending_intent_count(), 0);
        assert_eq!(maps.deferred_reconcile_count(), 40);
        assert_eq!(maps.compacted_subtree_count(), 40);
        assert_eq!(maps.tracked_path_count(), 40);

        for subtree_index in 0..40 {
            let subtree_root = watch_root.join(format!("storm-{subtree_index}"));
            let deferred = maps
                .deferred_reconcile(&subtree_root)
                .expect("missing deferred reconcile marker");
            assert_eq!(deferred.reason, StormReason::DirectoryUniquePathsThreshold);

            let compacted = maps
                .compacted_subtree(&subtree_root)
                .expect("missing compacted subtree record");
            assert_eq!(compacted.suppressed_event_count, 250);
        }
        assert!(
            elapsed < Duration::from_secs(5),
            "multi-subtree storm stress test took {:?}, expected < 5s",
            elapsed
        );
    }

    fn disabled_storm_thresholds() -> StormThresholds {
        StormThresholds {
            window: Duration::from_secs(2),
            directory_unique_paths_threshold: usize::MAX,
            directory_event_count_threshold: usize::MAX,
            global_pending_event_count_threshold: usize::MAX,
            deferred_reconcile_delay: Duration::from_secs(30),
        }
    }

    fn fs_event(path: PathBuf, kind: FsEventKind, seconds: u64) -> FsEventRecord {
        FsEventRecord {
            path,
            kind,
            observed_at: timestamp(seconds),
        }
    }

    fn timestamp(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }
}
