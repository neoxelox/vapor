use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use vapor_shared::constants;

use crate::fs_events::{FsEventErrorRecord, FsEventKind, FsEventRecord, FsEventRecording};
use crate::logging;

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
    tracked_path_refcounts: BTreeMap<PathBuf, usize>,
    subtree_pending_counts: BTreeMap<PathBuf, usize>,
}

impl BoundedEventIntentMaps {
    pub fn new(watch_root: impl Into<PathBuf>) -> Self {
        Self::with_limits(watch_root, EventIntentLimits::default())
    }

    pub fn with_limits(watch_root: impl Into<PathBuf>, limits: EventIntentLimits) -> Self {
        Self {
            watch_root: watch_root.into(),
            limits,
            event_map: BTreeMap::new(),
            intent_map: BTreeMap::new(),
            compacted_subtrees: BTreeMap::new(),
            tracked_path_refcounts: BTreeMap::new(),
            subtree_pending_counts: BTreeMap::new(),
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

    pub fn pending_event(&self, path: &Path) -> Option<&PendingEventRecord> {
        self.event_map.get(path)
    }

    pub fn pending_intent(&self, path: &Path) -> Option<&PendingIntentRecord> {
        self.intent_map.get(path)
    }

    pub fn compacted_subtree(&self, root: &Path) -> Option<&CompactedSubtreeRecord> {
        self.compacted_subtrees.get(root)
    }

    pub fn subtree_pending_count(&self, root: &Path) -> usize {
        self.subtree_pending_counts.get(root).copied().unwrap_or(0)
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
                    flags,
                    burst_count: 1,
                },
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
            );
        } else if self.tracked_path_count() >= self.limits.max_pending_paths {
            let watch_root = self.watch_root.clone();
            self.compact_subtree(
                watch_root.as_path(),
                BackpressureReason::GlobalCapExceeded,
                observed_at,
            );
        }

        self.compacted_subtree_root_for(path)
    }

    fn compact_subtree(
        &mut self,
        subtree_root: &Path,
        reason: BackpressureReason,
        observed_at: SystemTime,
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
        self.upsert_reconcile_intent(subtree_root, observed_at);

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
        self.upsert_reconcile_intent(subtree_root, observed_at);
    }

    fn note_absorbed_intent(&mut self, subtree_root: &Path, observed_at: SystemTime) {
        if let Some(record) = self.compacted_subtrees.get_mut(subtree_root) {
            record.note_intent(observed_at);
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
    use std::time::{Duration, UNIX_EPOCH};

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
