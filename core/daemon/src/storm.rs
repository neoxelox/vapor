use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use vapor_shared::constants;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StormReason {
    DirectoryUniquePathsThreshold,
    DirectoryEventCountThreshold,
    GlobalPendingEventCountThreshold,
    /// The bounded fs-event ingest buffer dropped events (callbacks
    /// outpaced the runtime drain). The dropped changes are unknown, so
    /// a deferred whole-scope reconcile reconstructs them once the
    /// engine is idle.
    IngestOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeferredReconcileRecord {
    pub root: PathBuf,
    pub reason: StormReason,
    pub first_detected_at: SystemTime,
    pub last_observed_at: SystemTime,
    pub available_at: SystemTime,
    pub reschedule_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StormThresholds {
    pub window: Duration,
    pub directory_unique_paths_threshold: usize,
    pub directory_event_count_threshold: usize,
    pub global_pending_event_count_threshold: usize,
    pub deferred_reconcile_delay: Duration,
}

impl Default for StormThresholds {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(constants::engine::STORM_WINDOW_MILLIS),
            directory_unique_paths_threshold:
                constants::engine::STORM_DIRECTORY_UNIQUE_PATHS_THRESHOLD,
            directory_event_count_threshold:
                constants::engine::STORM_DIRECTORY_EVENT_COUNT_THRESHOLD,
            global_pending_event_count_threshold:
                constants::engine::STORM_GLOBAL_PENDING_EVENT_COUNT_THRESHOLD,
            deferred_reconcile_delay: Duration::from_millis(
                constants::engine::DEFERRED_RECONCILE_DELAY_MILLIS,
            ),
        }
    }
}

#[derive(Debug)]
pub struct StormDetector {
    watch_root: PathBuf,
    thresholds: StormThresholds,
    directory_windows: BTreeMap<PathBuf, DirectoryStormWindow>,
    last_global_prune: Option<SystemTime>,
}

impl StormDetector {
    pub fn new(watch_root: impl Into<PathBuf>) -> Self {
        Self::with_thresholds(watch_root, StormThresholds::default())
    }

    pub fn with_thresholds(watch_root: impl Into<PathBuf>, thresholds: StormThresholds) -> Self {
        Self {
            watch_root: watch_root.into(),
            thresholds,
            directory_windows: BTreeMap::new(),
            last_global_prune: None,
        }
    }

    pub fn thresholds(&self) -> &StormThresholds {
        &self.thresholds
    }

    pub fn directory_window_count(&self) -> usize {
        self.directory_windows.len()
    }

    pub fn deferred_available_at(&self, observed_at: SystemTime) -> SystemTime {
        observed_at + self.thresholds.deferred_reconcile_delay
    }

    pub fn observe_event(
        &mut self,
        path: &Path,
        observed_at: SystemTime,
        pending_event_count: usize,
    ) -> Option<DeferredReconcileRecord> {
        let mut candidate: Option<(PathBuf, StormReason)> = None;
        for directory_root in self.directory_roots_for_path(path) {
            let window = self
                .directory_windows
                .entry(directory_root.clone())
                .or_default();
            window.observe(path, observed_at, self.thresholds.window);

            let reason =
                if window.unique_path_count() >= self.thresholds.directory_unique_paths_threshold {
                    Some(StormReason::DirectoryUniquePathsThreshold)
                } else if window.event_count() >= self.thresholds.directory_event_count_threshold {
                    Some(StormReason::DirectoryEventCountThreshold)
                } else {
                    None
                };

            if let Some(reason) = reason {
                match &candidate {
                    Some((existing_root, _))
                        if existing_root.components().count()
                            >= directory_root.components().count() => {}
                    _ => candidate = Some((directory_root, reason)),
                }
            }
        }

        self.maybe_prune_inactive_windows(observed_at);

        if let Some((root, reason)) = candidate {
            return Some(self.deferred_record(root, reason, observed_at));
        }

        if pending_event_count >= self.thresholds.global_pending_event_count_threshold {
            return Some(self.deferred_record(
                self.watch_root.clone(),
                StormReason::GlobalPendingEventCountThreshold,
                observed_at,
            ));
        }

        None
    }

    fn deferred_record(
        &self,
        root: PathBuf,
        reason: StormReason,
        observed_at: SystemTime,
    ) -> DeferredReconcileRecord {
        DeferredReconcileRecord {
            root,
            reason,
            first_detected_at: observed_at,
            last_observed_at: observed_at,
            available_at: self.deferred_available_at(observed_at),
            reschedule_count: 0,
        }
    }

    /// Global window pruning runs at most once per storm window instead
    /// of on every event: per-event pruning made a burst quadratic in the
    /// number of active directories (every event iterated every window).
    /// Individual windows still self-prune on each `observe`, so the
    /// thresholds themselves never see stale entries — this pass only
    /// reclaims memory for directories that went quiet.
    fn maybe_prune_inactive_windows(&mut self, observed_at: SystemTime) {
        let due = match self.last_global_prune {
            None => true,
            Some(last) => observed_at
                .duration_since(last)
                .map(|age| age >= self.thresholds.window)
                .unwrap_or(true),
        };
        if !due {
            return;
        }

        self.last_global_prune = Some(observed_at);
        let window = self.thresholds.window;
        self.directory_windows.retain(|_, state| {
            state.prune(observed_at, window);
            !state.is_empty()
        });
    }

    /// Ancestor directories whose per-directory windows observe this
    /// event. The watch root itself is deliberately excluded: rolled-up
    /// per-directory counts at the root would compact the *entire* sync
    /// scope for any moderately parallel workload (600 events anywhere in
    /// the tree within 2 s), while whole-root compaction is exactly what
    /// the separate — and much higher — global pending threshold governs.
    fn directory_roots_for_path(&self, path: &Path) -> Vec<PathBuf> {
        let Some(start) = path.parent() else {
            return Vec::new();
        };
        let mut roots = Vec::new();
        for ancestor in start.ancestors() {
            if ancestor == self.watch_root {
                break;
            }
            if ancestor.starts_with(&self.watch_root) {
                roots.push(ancestor.to_path_buf());
            }
        }
        roots
    }
}

#[derive(Debug, Default)]
struct DirectoryStormWindow {
    event_times: VecDeque<SystemTime>,
    path_last_seen: BTreeMap<PathBuf, SystemTime>,
}

impl DirectoryStormWindow {
    fn observe(&mut self, path: &Path, observed_at: SystemTime, window: Duration) {
        self.event_times.push_back(observed_at);
        self.path_last_seen.insert(path.to_path_buf(), observed_at);
        self.prune(observed_at, window);
    }

    fn event_count(&self) -> usize {
        self.event_times.len()
    }

    fn unique_path_count(&self) -> usize {
        self.path_last_seen.len()
    }

    fn is_empty(&self) -> bool {
        self.event_times.is_empty() && self.path_last_seen.is_empty()
    }

    fn prune(&mut self, observed_at: SystemTime, window: Duration) {
        while let Some(front) = self.event_times.front() {
            let age = observed_at.duration_since(*front);
            if matches!(age, Ok(age) if age > window) {
                self.event_times.pop_front();
            } else {
                break;
            }
        }

        self.path_last_seen.retain(|_, last_seen| {
            observed_at
                .duration_since(*last_seen)
                .map(|age| age <= window)
                .unwrap_or(true)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn deepest_directory_unique_path_storm_is_selected() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut detector = StormDetector::with_thresholds(
            watch_root.clone(),
            StormThresholds {
                directory_unique_paths_threshold: 2,
                directory_event_count_threshold: 99,
                global_pending_event_count_threshold: 99,
                deferred_reconcile_delay: Duration::from_secs(30),
                ..StormThresholds::default()
            },
        );

        assert!(
            detector
                .observe_event(&watch_root.join("project/sub/a.txt"), timestamp(1), 1)
                .is_none()
        );
        let storm = detector
            .observe_event(&watch_root.join("project/sub/b.txt"), timestamp(2), 2)
            .expect("expected storm trigger");

        assert_eq!(storm.root, watch_root.join("project/sub"));
        assert_eq!(storm.reason, StormReason::DirectoryUniquePathsThreshold);
        assert_eq!(storm.available_at, timestamp(2) + Duration::from_secs(30));
    }

    #[test]
    fn repeated_events_can_trigger_event_count_threshold_without_unique_paths() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut detector = StormDetector::with_thresholds(
            watch_root.clone(),
            StormThresholds {
                directory_unique_paths_threshold: 99,
                directory_event_count_threshold: 3,
                global_pending_event_count_threshold: 99,
                deferred_reconcile_delay: Duration::from_secs(30),
                ..StormThresholds::default()
            },
        );

        let path = watch_root.join("project/file.txt");
        assert!(detector.observe_event(&path, timestamp(1), 1).is_none());
        assert!(detector.observe_event(&path, timestamp(2), 2).is_none());
        let storm = detector
            .observe_event(&path, timestamp(3), 3)
            .expect("expected event-count storm");

        assert_eq!(storm.root, watch_root.join("project"));
        assert_eq!(storm.reason, StormReason::DirectoryEventCountThreshold);
    }

    #[test]
    fn global_pending_event_count_can_trigger_watch_root_storm() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut detector = StormDetector::with_thresholds(
            watch_root.clone(),
            StormThresholds {
                directory_unique_paths_threshold: 99,
                directory_event_count_threshold: 99,
                global_pending_event_count_threshold: 2,
                deferred_reconcile_delay: Duration::from_secs(15),
                ..StormThresholds::default()
            },
        );

        let storm = detector
            .observe_event(&watch_root.join("project/a.txt"), timestamp(1), 2)
            .expect("expected global pending storm");

        assert_eq!(storm.root, watch_root);
        assert_eq!(storm.reason, StormReason::GlobalPendingEventCountThreshold);
        assert_eq!(storm.available_at, timestamp(1) + Duration::from_secs(15));
    }

    #[test]
    fn inactive_directory_windows_are_pruned_after_the_storm_window_passes() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut detector = StormDetector::with_thresholds(
            watch_root.clone(),
            StormThresholds {
                window: Duration::from_secs(2),
                ..StormThresholds::default()
            },
        );

        assert!(
            detector
                .observe_event(&watch_root.join("alpha/file.txt"), timestamp(1), 1)
                .is_none()
        );
        assert_eq!(detector.directory_window_count(), 1);

        // 9 seconds later alpha's window is stale; the periodic prune
        // reclaims it while beta's window is created.
        assert!(
            detector
                .observe_event(&watch_root.join("beta/file.txt"), timestamp(10), 1)
                .is_none()
        );
        assert_eq!(detector.directory_window_count(), 1);
    }

    #[test]
    fn events_directly_under_the_watch_root_never_trip_per_directory_thresholds() {
        // The watch root is exempt from the per-directory thresholds:
        // otherwise rolled-up counts would compact the entire sync scope
        // for any busy-but-healthy workload. Only the (higher) global
        // pending threshold may compact the root.
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut detector = StormDetector::with_thresholds(
            watch_root.clone(),
            StormThresholds {
                directory_unique_paths_threshold: 2,
                directory_event_count_threshold: 2,
                global_pending_event_count_threshold: 99,
                ..StormThresholds::default()
            },
        );

        for index in 0..10 {
            assert!(
                detector
                    .observe_event(
                        &watch_root.join(format!("file-{index}.txt")),
                        timestamp(1),
                        index + 1,
                    )
                    .is_none(),
                "root-level events must not trigger a per-directory storm"
            );
        }
    }

    #[test]
    fn spread_out_events_across_subdirectories_do_not_compact_the_watch_root() {
        // A parallel build touching a few files in many unrelated
        // directories used to roll up into the watch-root window and
        // compact the whole scope. The per-directory thresholds now apply
        // strictly below the root.
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut detector = StormDetector::with_thresholds(
            watch_root.clone(),
            StormThresholds {
                directory_unique_paths_threshold: 5,
                directory_event_count_threshold: 5,
                global_pending_event_count_threshold: 999,
                ..StormThresholds::default()
            },
        );

        for directory in 0..20 {
            for file in 0..2 {
                let path = watch_root.join(format!("dir-{directory}/file-{file}.txt"));
                assert!(
                    detector.observe_event(&path, timestamp(1), 1).is_none(),
                    "two files per directory is below every per-directory threshold"
                );
            }
        }
    }

    fn timestamp(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }
}
