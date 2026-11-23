use std::collections::{BTreeMap, VecDeque};
use std::hash::{Hash, Hasher};
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
    /// The last moment the folder was in a storm: when it was detected,
    /// and after that each time the events it takes in reach storm
    /// level again. A few events a second do not move it.
    pub last_observed_at: SystemTime,
    /// When the reconcile starts in a throttle state other than `Idle`.
    pub available_at: SystemTime,
    pub reschedule_count: usize,
    /// The window in which the events the folder takes in are counted,
    /// and the count so far.
    pub busy_window_started_at: SystemTime,
    pub busy_window_events: usize,
}

/// The start of a deferred reconcile on an idle device: `quiet` after
/// the end of the storm, and never later than `max_delay` after the
/// storm was first seen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdleRelease {
    pub quiet: Duration,
    pub max_delay: Duration,
}

impl Default for IdleRelease {
    fn default() -> Self {
        Self {
            quiet: Duration::from_millis(
                constants::engine::DEFERRED_RECONCILE_IDLE_QUIET_RELEASE_MILLIS,
            ),
            max_delay: Duration::from_millis(
                constants::engine::DEFERRED_RECONCILE_IDLE_MAX_DELAY_MILLIS,
            ),
        }
    }
}

impl DeferredReconcileRecord {
    /// A storm first seen at `observed_at`; `delay` is the wait after
    /// its end in a throttle state other than `Idle`.
    pub fn new(
        root: PathBuf,
        reason: StormReason,
        observed_at: SystemTime,
        delay: Duration,
    ) -> Self {
        Self {
            root,
            reason,
            first_detected_at: observed_at,
            last_observed_at: observed_at,
            available_at: observed_at + delay,
            reschedule_count: 0,
            busy_window_started_at: observed_at,
            busy_window_events: 0,
        }
    }

    /// The storm was seen again at `observed_at` (a new trigger).
    pub fn note_storm(&mut self, reason: StormReason, observed_at: SystemTime, delay: Duration) {
        self.reason = reason;
        self.extend_to(observed_at, delay);
    }

    /// The compacted folder took in one more event. It moves the end of
    /// the storm only when the events of one window reach `busy_events`.
    pub fn note_absorbed(
        &mut self,
        observed_at: SystemTime,
        delay: Duration,
        window: Duration,
        busy_events: usize,
    ) {
        let in_window = observed_at
            .duration_since(self.busy_window_started_at)
            .is_ok_and(|age| age <= window);
        if !in_window {
            self.busy_window_started_at = observed_at;
            self.busy_window_events = 0;
        }
        self.busy_window_events += 1;
        if self.busy_window_events >= busy_events.max(1) {
            self.extend_to(observed_at, delay);
        }
    }

    fn extend_to(&mut self, observed_at: SystemTime, delay: Duration) {
        if observed_at > self.last_observed_at {
            self.last_observed_at = observed_at;
        }
        // "Defer until quiet", but bounded: a folder that never goes
        // quiet must not push its own reconcile out forever.
        let latest = self.first_detected_at
            + Duration::from_millis(constants::engine::DEFERRED_RECONCILE_MAX_DELAY_MILLIS);
        self.available_at = self
            .available_at
            .max(self.last_observed_at + delay)
            .min(latest);
        self.reschedule_count += 1;
    }

    /// Whether the reconcile starts at `now`. On an idle device it
    /// starts a moment after the storm ends; elsewhere it waits out the
    /// longer delay.
    pub fn is_ready(&self, now: SystemTime, idle: Option<IdleRelease>) -> bool {
        self.available_at <= now
            || idle.is_some_and(|idle| {
                (self.last_observed_at + idle.quiet).min(self.first_detected_at + idle.max_delay)
                    <= now
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StormThresholds {
    pub window: Duration,
    pub directory_unique_paths_threshold: usize,
    pub directory_event_count_threshold: usize,
    pub global_pending_event_count_threshold: usize,
    pub deferred_reconcile_delay: Duration,
    /// Events a compacted folder takes in inside one window while it
    /// still counts as in a storm.
    pub absorbed_busy_event_threshold: usize,
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
            absorbed_busy_event_threshold: constants::engine::STORM_ABSORBED_BUSY_EVENTS,
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

    /// The window and the event count that say a compacted folder is
    /// still in a storm.
    pub fn busy_measure(&self) -> (Duration, usize) {
        (
            self.thresholds.window,
            self.thresholds.absorbed_busy_event_threshold,
        )
    }

    pub fn observe_event(
        &mut self,
        path: &Path,
        observed_at: SystemTime,
        pending_event_count: usize,
    ) -> Option<DeferredReconcileRecord> {
        // This runs once per drained fs event, so it allocates only when
        // it must: the path is interned as one hash (not cloned into
        // every ancestor's window), ancestors are iterated in place (no
        // collected Vec), and a window's key is cloned only the first
        // time its directory appears.
        let mut candidate: Option<(PathBuf, StormReason)> = None;
        let path_hash = hash_path(path);
        let window_duration = self.thresholds.window;
        let unique_paths_threshold = self.thresholds.directory_unique_paths_threshold;
        let event_count_threshold = self.thresholds.directory_event_count_threshold;

        // The watch root itself is deliberately excluded: rolled-up
        // per-directory counts at the root would compact the *entire*
        // sync scope for any moderately parallel workload (600 events
        // anywhere in the tree within 2 s), while whole-root compaction
        // is exactly what the separate (and much higher) global
        // pending threshold governs.
        if let Some(start) = path.parent() {
            for ancestor in start.ancestors() {
                if ancestor == self.watch_root {
                    break;
                }
                if !ancestor.starts_with(&self.watch_root) {
                    continue;
                }
                if !self.directory_windows.contains_key(ancestor) {
                    self.directory_windows
                        .insert(ancestor.to_path_buf(), DirectoryStormWindow::default());
                }
                let window = self
                    .directory_windows
                    .get_mut(ancestor)
                    .expect("window inserted above");
                window.observe(
                    path_hash,
                    observed_at,
                    window_duration,
                    unique_paths_threshold,
                );

                let reason = if window.unique_path_count() >= unique_paths_threshold {
                    Some(StormReason::DirectoryUniquePathsThreshold)
                } else if window.event_count() >= event_count_threshold {
                    Some(StormReason::DirectoryEventCountThreshold)
                } else {
                    None
                };

                // Ancestors iterate deepest-first, so the first
                // triggering window is the deepest storm root.
                if candidate.is_none()
                    && let Some(reason) = reason
                {
                    candidate = Some((ancestor.to_path_buf(), reason));
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
        DeferredReconcileRecord::new(
            root,
            reason,
            observed_at,
            self.thresholds.deferred_reconcile_delay,
        )
    }

    /// Forgets what the windows hold of the storm under `root`. The
    /// folder is compacted now and its events no longer come here, but
    /// the windows of the folders above it still count the burst for a
    /// whole window: one event next to the compacted folder would then
    /// trip the parent, and the parent would be compacted for a burst
    /// that was already taken care of.
    pub fn forget_storm_under(&mut self, root: &Path) {
        self.directory_windows
            .retain(|directory, _| !directory.starts_with(root) && !root.starts_with(directory));
    }

    /// Global window pruning runs at most once per storm window instead
    /// of on every event: per-event pruning made a burst quadratic in the
    /// number of active directories (every event iterated every window).
    /// Individual windows still self-prune on each `observe`, so the
    /// thresholds themselves never see stale entries: this pass only
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
}

/// One stable 64-bit fingerprint per event path: the unique-path
/// windows count fingerprints instead of storing full `PathBuf` clones
/// per ancestor level. A hash collision merely undercounts unique paths
/// by one: harmless for a storm heuristic.
fn hash_path(path: &Path) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Default)]
struct DirectoryStormWindow {
    event_times: VecDeque<SystemTime>,
    path_last_seen: BTreeMap<u64, SystemTime>,
}

impl DirectoryStormWindow {
    fn observe(
        &mut self,
        path_hash: u64,
        observed_at: SystemTime,
        window: Duration,
        unique_paths_threshold: usize,
    ) {
        self.event_times.push_back(observed_at);
        self.path_last_seen.insert(path_hash, observed_at);
        self.prune_events(observed_at, window);
        // The unique-path map scan is deferred until its count could
        // trip the threshold: a burst spread below it never pays a
        // per-event map walk, and pruning exactly at the boundary keeps
        // the trigger accurate (a stale over-count is cleaned before the
        // comparison the caller runs). Memory stays bounded by the
        // threshold plus the periodic global prune.
        if self.path_last_seen.len() >= unique_paths_threshold {
            self.prune_paths(observed_at, window);
        }
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
        self.prune_events(observed_at, window);
        self.prune_paths(observed_at, window);
    }

    fn prune_events(&mut self, observed_at: SystemTime, window: Duration) {
        while let Some(front) = self.event_times.front() {
            let age = observed_at.duration_since(*front);
            if matches!(age, Ok(age) if age > window) {
                self.event_times.pop_front();
            } else {
                break;
            }
        }
    }

    fn prune_paths(&mut self, observed_at: SystemTime, window: Duration) {
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
                absorbed_busy_event_threshold: 1,
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
                absorbed_busy_event_threshold: 1,
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
                absorbed_busy_event_threshold: 1,
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
        // A parallel build touches a few files in many unrelated
        // directories. The per-directory thresholds apply strictly
        // below the root, so that spread never rolls up into the
        // watch-root window and compacts the whole scope.
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
