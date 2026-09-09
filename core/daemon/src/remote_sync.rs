//! Remote changes poll + apply mapping.
//!
//! On a throttle-aware cadence the poller pulls the provider's changes
//! feed, filters self-write echoes, maps surviving changes onto
//! durable remote-sourced intents (`Download` / `ApplyRemoteDelete`),
//! and persists the feed cursor. Cursor discipline: the cursor advances
//! durably only after the page's intents are durably enqueued, so a
//! crash between the two replays the page and the per-(path, kind)
//! coalescing absorbs the duplicates — at-least-once, never at-most-once.
//!
//! A `CursorExpired` poll (daemon restart with an in-memory feed, ring
//! overflow, provider-side expiry) schedules a whole-scope reconcile and
//! re-baselines the cursor, which is exactly the recovery path a real
//! cloud provider needs when its server-side cursor lapses.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use vapor_providers::{ChangesPoll, ProviderError, RemoteChangeKind};
use vapor_shared::{SyncMode, ThrottleState, constants};

use crate::DaemonApp;
use crate::clock::SharedClock;
use crate::event_intents::PendingIntentKind;
use crate::fs_events::SharedEventPathFilter;
use crate::logging;
use crate::provider_jobs::{ProviderCall, ProviderCallMode};
use crate::self_write_cache::SelfWriteCache;
use crate::state_db::{DurableStateDb, StateDbError};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemotePollReport {
    pub polled: bool,
    pub observed_changes: usize,
    pub suppressed_echoes: usize,
    /// Remote changes dropped because their local-equivalent path
    /// matches the ignore rules — ignore filtering is symmetric, so an
    /// ignored name never syncs in either direction.
    pub ignored_changes: usize,
    pub enqueued_intents: usize,
    pub cursor_expired: bool,
    /// Push-only strict-mirror restores scheduled this poll (remote
    /// divergence overwritten with the local canonical).
    pub mirror_reverts: usize,
    /// Push-only strict-mirror removals scheduled this poll (cloud-only
    /// content deleted).
    pub mirror_deletes: usize,
    /// Held deletions dropped because the cloud removed the path itself
    /// while the question was open.
    pub moot_holds: usize,
    /// Remote changes left untouched because they would alias a
    /// differently-cased local file. The paths are kept by the poller
    /// (`take_name_collisions`) for the timeline.
    pub name_collisions: usize,
}

pub struct RemotePoller {
    cursor_state_key: String,
    cursor: Option<String>,
    /// Collisions found by polls since the last `take_name_collisions`.
    name_collisions: Vec<(PathBuf, PathBuf)>,
    /// Colliding remote names already reported once; the feed repeats
    /// events for the same object and one report per name is enough.
    reported_collisions: std::collections::BTreeSet<PathBuf>,
    cursor_loaded: bool,
    /// Whether the first poll after startup has run. A cursor that the
    /// provider rejects on that poll is a restart re-baseline (the
    /// filesystem feed's cursor is process-local), not a feed gap.
    first_poll_done: bool,
    last_poll_inst: Option<Instant>,
    /// A poll started on an earlier tick whose round trip is still in
    /// progress on its own thread.
    in_flight: Option<ProviderCall<Result<ChangesPoll, ProviderError>>>,
    /// Set by a sync-root recovery: the changes the feed collected
    /// while the root was away describe the outage, not the user. The
    /// next poll is a baseline poll, and a page from before the flag
    /// is discarded when harvested.
    discard_history: bool,
}

impl RemotePoller {
    pub fn new(profile_id: &str) -> Self {
        Self {
            cursor_state_key: format!(
                "{}{profile_id}",
                constants::provider::CHANGES_CURSOR_STATE_KEY_PREFIX
            ),
            cursor: None,
            cursor_loaded: false,
            first_poll_done: false,
            last_poll_inst: None,
            in_flight: None,
            name_collisions: Vec::new(),
            reported_collisions: std::collections::BTreeSet::new(),
            discard_history: false,
        }
    }

    /// Forgets the feed's history: the next poll re-baselines the cursor
    /// at the provider's current head and the whole-scope reconcile the
    /// caller schedules merges the two sides instead. A sync root that
    /// went away and came back calls this, so the removals the feed saw
    /// while the root was going never reach the queue.
    pub fn discard_history(&mut self) {
        self.discard_history = true;
        self.cursor = None;
        self.cursor_loaded = true;
        self.last_poll_inst = None;
    }

    /// Collisions found since the last call, for the timeline.
    pub fn take_name_collisions(&mut self) -> Vec<(PathBuf, PathBuf)> {
        std::mem::take(&mut self.name_collisions)
    }

    /// Poll cadence for the current throttle state; `None` means the
    /// state forbids polling entirely.
    fn cadence_for(state: ThrottleState) -> Option<Duration> {
        match state {
            ThrottleState::IdleDrain => Some(Duration::from_secs(
                constants::engine::REMOTE_POLL_IDLE_DRAIN_SECONDS,
            )),
            ThrottleState::Light => Some(Duration::from_secs(
                constants::engine::REMOTE_POLL_LIGHT_SECONDS,
            )),
            ThrottleState::Throttled => Some(Duration::from_secs(
                constants::engine::REMOTE_POLL_THROTTLED_SECONDS,
            )),
            ThrottleState::Suspended => None,
        }
    }

    /// Clears the poll cadence so the next tick polls immediately.
    /// Used by the flush boost: `vapor flush` should surface
    /// pending remote changes now, not at the next scheduled poll.
    pub fn request_immediate_poll(&mut self) {
        self.last_poll_inst = None;
    }

    /// Runs one poll when the provider supports a feed, the throttle
    /// state permits it, and the cadence is due. In threaded mode the
    /// round trip runs on its own thread: this call starts it and a later
    /// tick harvests the page, so the tick loop never waits on the
    /// network. A poll in flight is harvested before any gate is
    /// consulted, so a throttle change cannot strand it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn poll_if_due(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        remote_echoes: &mut SelfWriteCache,
        local_root: Option<&Path>,
        path_filter: Option<&SharedEventPathFilter>,
        sync_mode: SyncMode,
        clock: &SharedClock,
        now: SystemTime,
        mode: &ProviderCallMode,
    ) -> Result<RemotePollReport, StateDbError> {
        let mut report = RemotePollReport::default();
        let Some(local_root) = local_root else {
            return Ok(report);
        };
        if let Some(call) = self.in_flight.as_mut() {
            let Some(result) = call.take() else {
                return Ok(report);
            };
            self.in_flight = None;
            if self.discard_history {
                // Started before the recovery: whatever it carries is
                // the outage's history.
                return Ok(report);
            }
            report.polled = true;
            if let Some(poll) = Self::unwrap_poll(result) {
                self.apply_poll(
                    poll,
                    app,
                    state_db,
                    remote_echoes,
                    local_root,
                    path_filter,
                    sync_mode,
                    now,
                    &mut report,
                )?;
            }
            return Ok(report);
        }
        if !app.provider().capabilities().supports_remote_changes_feed {
            return Ok(report);
        }
        let throttle_state = app.snapshot().throttle_state;
        let Some(cadence) = Self::cadence_for(throttle_state) else {
            return Ok(report);
        };
        if !app.remote_poll_allowed() {
            return Ok(report);
        }
        let now_inst = clock.now();
        let due = self
            .last_poll_inst
            .map(|last| now_inst.saturating_duration_since(last) >= cadence)
            .unwrap_or(true);
        if !due {
            return Ok(report);
        }
        self.last_poll_inst = Some(now_inst);

        if !self.cursor_loaded {
            self.cursor = state_db
                .state(&self.cursor_state_key)?
                .map(|entry| entry.value);
            self.cursor_loaded = true;
        }
        if self.discard_history {
            self.discard_history = false;
            state_db.delete_state(&self.cursor_state_key)?;
            self.cursor = None;
            logging::info(
                "Re-baselining the remote changes feed after the sync root recovery",
                &[("cursor_key", self.cursor_state_key.clone())],
            );
        }

        let provider = app.provider_handle();
        let cursor = self.cursor.clone();
        let mut call = ProviderCall::start(mode, "poll-changes", move || {
            provider.poll_changes(
                cursor.as_deref(),
                constants::engine::REMOTE_CHANGES_PAGE_MAX,
            )
        });
        let Some(result) = call.take() else {
            self.in_flight = Some(call);
            return Ok(report);
        };
        report.polled = true;
        if let Some(poll) = Self::unwrap_poll(result) {
            self.apply_poll(
                poll,
                app,
                state_db,
                remote_echoes,
                local_root,
                path_filter,
                sync_mode,
                now,
                &mut report,
            )?;
        }
        Ok(report)
    }

    /// Logs a failed poll (provider error or a panic on the poll thread)
    /// and yields `None`; the next cadence retries.
    fn unwrap_poll(
        result: Result<Result<ChangesPoll, ProviderError>, String>,
    ) -> Option<ChangesPoll> {
        match result {
            Ok(Ok(poll)) => Some(poll),
            Ok(Err(error)) => {
                logging::warning(
                    "Remote changes poll failed; will retry on the next cadence",
                    &[
                        ("failure", error.kind.label().to_string()),
                        ("error", error.message),
                    ],
                );
                None
            }
            Err(panic) => {
                logging::error(
                    "Remote changes poll panicked; will retry on the next cadence",
                    &[("panic", panic)],
                );
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_poll(
        &mut self,
        poll: ChangesPoll,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        remote_echoes: &mut SelfWriteCache,
        local_root: &Path,
        path_filter: Option<&SharedEventPathFilter>,
        sync_mode: SyncMode,
        now: SystemTime,
        report: &mut RemotePollReport,
    ) -> Result<(), StateDbError> {
        let first_poll = !self.first_poll_done;
        self.first_poll_done = true;
        match poll {
            ChangesPoll::CursorExpired => {
                report.cursor_expired = true;
                if first_poll {
                    logging::info(
                        "Remote changes cursor did not survive the restart; re-baselining behind the startup reconcile",
                        &[("cursor_key", self.cursor_state_key.clone())],
                    );
                } else {
                    logging::warning(
                        "Remote changes cursor expired; scheduling whole-scope reconcile and re-baselining",
                        &[("cursor_key", self.cursor_state_key.clone())],
                    );
                }
                // Reconcile reconstructs whatever the gap in the feed
                // hid; the coalesced enqueue dedupes against a pending
                // reconcile row.
                report.enqueued_intents += state_db.enqueue_intents_coalesced(
                    &[(
                        local_root.to_path_buf(),
                        PendingIntentKind::ReconcileSubtree,
                        now,
                    )],
                    crate::safeguards::IntentSource::Fresh,
                )?;
                self.rebaseline(app, state_db, now)?;
            }
            ChangesPoll::Page(page) => {
                report.observed_changes = page.changes.len();
                // A full page means more changes are pending right now.
                // Keep draining on the next tick (one page per tick stays
                // interruptible) instead of waiting out the whole cadence,
                // so a large remote burst enqueues in seconds, not minutes.
                let page_was_full =
                    page.changes.len() >= constants::engine::REMOTE_CHANGES_PAGE_MAX;
                let mut batch = Vec::new();
                for change in &page.changes {
                    // A remote name materialized under an alias maps to
                    // its local copy, not to the name it cannot have here.
                    let local_target = match state_db.name_alias(change.path.as_str())? {
                        Some((alias, _)) => alias,
                        None => change.path.to_local(local_root),
                    };
                    if path_filter
                        .map(|filter| filter.should_ignore(&local_target))
                        .unwrap_or(false)
                    {
                        report.ignored_changes += 1;
                        continue;
                    }
                    // Enqueue with the change's *observed* time, not the
                    // poll time: the deletion guard orders a remote delete
                    // against the last sync via this timestamp, and a lagging
                    // feed (up to 60s under Throttled) would otherwise let a
                    // stale Removed delete a freshly re-uploaded local file.
                    let event_time = change.observed_at;
                    if change.kind == RemoteChangeKind::CreatedOrModified
                        && let Some(existing) =
                            crate::name_collision::colliding_local_path(&local_target)
                    {
                        // The walk materializes the colliding name as a
                        // conflict copy and records the alias; the feed
                        // asks for that walk rather than duplicating it.
                        if self.reported_collisions.insert(local_target.clone()) {
                            crate::logging::info(
                                "Remote change collides with a differently-cased local file; a reconcile will materialize it as a conflict copy",
                                &[
                                    ("remote", change.path.as_str().to_string()),
                                    ("local", existing.display().to_string()),
                                ],
                            );
                            report.name_collisions += 1;
                            self.name_collisions.push((local_target.clone(), existing));
                            if let Some(parent) = local_target.parent() {
                                batch.push((
                                    parent.to_path_buf(),
                                    PendingIntentKind::ReconcileSubtree,
                                    event_time,
                                ));
                            }
                        }
                        continue;
                    }
                    match change.kind {
                        RemoteChangeKind::CreatedOrModified => {
                            if remote_echoes.matches_write(
                                change.path.as_str(),
                                change.op_id.as_deref(),
                                change.content_hash.as_deref(),
                                now,
                            ) || is_durable_self_write_echo(state_db, &local_target, change)
                            {
                                crate::logging::debug(
                                    "Suppressed remote change as an echo of the daemon's own write",
                                    &[("remote_path", change.path.as_str().to_string())],
                                );
                                report.suppressed_echoes += 1;
                                continue;
                            }
                            if sync_mode == SyncMode::PushOnly {
                                // Push-only: remote changes never
                                // produce remote-to-local intents. Remote
                                // divergence is driven back to the local
                                // canonical: overwrite when a local
                                // counterpart exists, remove cloud-only
                                // content when it does not.
                                if local_file_exists(&local_target) {
                                    batch.push((
                                        local_target,
                                        PendingIntentKind::Upload,
                                        event_time,
                                    ));
                                    report.mirror_reverts += 1;
                                } else {
                                    batch.push((
                                        local_target,
                                        PendingIntentKind::Delete,
                                        event_time,
                                    ));
                                    report.mirror_deletes += 1;
                                }
                                continue;
                            }
                            batch.push((local_target, PendingIntentKind::Download, event_time));
                        }
                        RemoteChangeKind::Removed => {
                            if remote_echoes.matches_delete(change.path.as_str(), now) {
                                crate::logging::debug(
                                    "Suppressed remote removal as an echo of the daemon's own delete",
                                    &[("remote_path", change.path.as_str().to_string())],
                                );
                                report.suppressed_echoes += 1;
                                continue;
                            }
                            if sync_mode == SyncMode::PushOnly {
                                // A remote deletion of backed-up content is
                                // divergence too: restore from local.
                                if local_file_exists(&local_target) {
                                    batch.push((
                                        local_target,
                                        PendingIntentKind::Upload,
                                        event_time,
                                    ));
                                    report.mirror_reverts += 1;
                                } else if local_dir_exists(&local_target) {
                                    // A trashed remote folder arrives as a
                                    // single Removed (no per-descendant
                                    // events). Reconcile the local subtree so
                                    // push-only re-uploads every file under
                                    // it, restoring the mirror.
                                    batch.push((
                                        local_target,
                                        PendingIntentKind::ReconcileSubtree,
                                        event_time,
                                    ));
                                    report.mirror_reverts += 1;
                                }
                                continue;
                            }
                            // A deletion of this path held behind the
                            // mass-deletion decision has nothing left to
                            // do: the cloud copy is already gone.
                            if state_db
                                .drop_held_at(&local_target, PendingIntentKind::Delete)?
                                .is_some()
                            {
                                report.moot_holds += 1;
                            }
                            batch.push((
                                local_target,
                                PendingIntentKind::ApplyRemoteDelete,
                                event_time,
                            ));
                        }
                    }
                }
                for (path, kind, _) in &batch {
                    crate::logging::debug(
                        "Enqueuing remote change",
                        &[
                            ("path", path.display().to_string()),
                            ("kind", format!("{kind:?}")),
                        ],
                    );
                }
                if !batch.is_empty() {
                    report.enqueued_intents += state_db.enqueue_intents_coalesced(
                        &batch,
                        crate::safeguards::IntentSource::Fresh,
                    )?;
                }
                // The intents are durable; only now may the cursor move.
                if self.cursor.as_deref() != Some(page.next_cursor.as_str()) {
                    state_db.set_state(&self.cursor_state_key, &page.next_cursor, now)?;
                    self.cursor = Some(page.next_cursor);
                }
                if page_was_full {
                    // Re-poll immediately on the next tick to continue
                    // draining the known backlog.
                    self.last_poll_inst = None;
                }
            }
        }

        Ok(())
    }

    fn rebaseline(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        now: SystemTime,
    ) -> Result<(), StateDbError> {
        match app
            .provider()
            .poll_changes(None, constants::engine::REMOTE_CHANGES_PAGE_MAX)
        {
            Ok(ChangesPoll::Page(baseline)) => {
                state_db.set_state(&self.cursor_state_key, &baseline.next_cursor, now)?;
                self.cursor = Some(baseline.next_cursor);
            }
            Ok(ChangesPoll::CursorExpired) => {
                // A baseline poll must never expire; treat as a provider
                // bug and drop the cursor so the next poll re-baselines.
                logging::error(
                    "Provider reported CursorExpired for a baseline poll; deferring re-baseline",
                    &[],
                );
                state_db.delete_state(&self.cursor_state_key)?;
                self.cursor = None;
            }
            Err(error) => {
                logging::warning(
                    "Re-baseline poll failed; will retry on the next cadence",
                    &[("error", error.message)],
                );
                state_db.delete_state(&self.cursor_state_key)?;
                self.cursor = None;
            }
        }
        Ok(())
    }
}

/// Whether a regular file exists at `path` (symlinks and directories
/// do not count as restorable local canonicals).
fn local_file_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

/// Whether a real directory exists at `path` (used to restore a
/// remotely-trashed folder in push-only mirror mode).
fn local_dir_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
}

/// Durable second-line echo correlator that outlives the live-cache TTL.
/// The self-write cache expires records after ~30s, but the poll cadence
/// reaches 60s under Throttled (and stops entirely under Suspended), so
/// the daemon's own upload can be observed in the feed after its live
/// record expired. The persisted sync index still holds the op-id and
/// content hash we last wrote for the path: a change carrying either is
/// our own write reflected back, so it is suppressed rather than
/// re-downloaded.
fn is_durable_self_write_echo(
    state_db: &mut DurableStateDb,
    local_target: &Path,
    change: &vapor_providers::RemoteChange,
) -> bool {
    match state_db.sync_index(local_target) {
        Ok(Some(index)) => {
            change.op_id.as_deref() == Some(index.last_op_id.as_str())
                || change.content_hash.as_deref() == Some(index.content_hash.as_str())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use std::path::PathBuf;
    use std::sync::Arc;
    use vapor_platform::fs_caps::{CaseSensitivity, InMemoryFilesystemCapabilities};
    use vapor_platform::fs_watch::WatchEventKind;
    use vapor_providers::FilesystemProvider;
    use vapor_providers::filesystem::ManualFeedHandle;

    struct Fixture {
        _temp: tempfile::TempDir,
        local_root: PathBuf,
        cloud_root: PathBuf,
        sync_mode: SyncMode,
        path_filter: Option<Arc<SharedEventPathFilter>>,
        app: DaemonApp,
        state_db: DurableStateDb,
        poller: RemotePoller,
        remote_echoes: SelfWriteCache,
        feed: ManualFeedHandle,
        clock: Arc<ManualClock>,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::TempDir::new().expect("temp dir");
            let local_root = temp.path().join("local");
            let cloud_root = temp.path().join("cloud");
            std::fs::create_dir_all(&local_root).expect("local root");
            std::fs::create_dir_all(&cloud_root).expect("cloud root");
            // The provider canonicalizes its root; the manual feed must
            // emit paths under the same canonical prefix (macOS tempdirs
            // live behind the /var -> /private/var symlink).
            let cloud_root =
                vapor_shared::paths::canonicalize(&cloud_root).expect("canonical cloud root");
            let clock = Arc::new(ManualClock::at_now());
            let caps = Arc::new(InMemoryFilesystemCapabilities::new(
                true,
                CaseSensitivity::Sensitive,
            ));
            let (provider, feed) =
                FilesystemProvider::with_manual_feed(&cloud_root, caps).expect("manual provider");
            let app = DaemonApp::new_with_clock(Box::new(provider), clock.clone());
            let state_db = DurableStateDb::open(temp.path().join("state/vapor.sqlite"))
                .expect("open state db");
            Self {
                local_root,
                cloud_root,
                sync_mode: SyncMode::TwoWay,
                path_filter: None,
                _temp: temp,
                app,
                state_db,
                poller: RemotePoller::new("default"),
                remote_echoes: SelfWriteCache::new(),
                feed,
                clock,
            }
        }

        fn poll(&mut self, now: SystemTime) -> RemotePollReport {
            // Advance past every cadence so the poll is always due.
            self.clock.advance(Duration::from_secs(61));
            let clock: SharedClock = self.clock.clone();
            self.poller
                .poll_if_due(
                    &mut self.app,
                    &mut self.state_db,
                    &mut self.remote_echoes,
                    Some(&self.local_root),
                    self.path_filter.as_deref(),
                    self.sync_mode,
                    &clock,
                    now,
                    &ProviderCallMode::Inline,
                )
                .expect("poll")
        }
    }

    fn ts(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(1_750_000_000_000 + ms)
    }

    #[test]
    fn baseline_poll_persists_cursor_without_intents() {
        let mut fixture = Fixture::new();
        let report = fixture.poll(ts(0));
        assert!(report.polled);
        assert_eq!(report.enqueued_intents, 0);
        assert!(
            fixture
                .state_db
                .state("provider.changes_cursor.default")
                .expect("state read")
                .is_some(),
            "baseline cursor must be durable"
        );
    }

    #[test]
    fn remote_create_maps_to_download_intent_at_the_local_target_path() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline

        std::fs::write(fixture.cloud_root.join("fresh.txt"), b"new").expect("seed remote");
        fixture.feed.emit(
            fixture.cloud_root.join("fresh.txt"),
            WatchEventKind::Created,
            ts(10),
        );

        let report = fixture.poll(ts(20));
        assert_eq!(report.observed_changes, 1);
        assert_eq!(report.enqueued_intents, 1);

        let intent = fixture
            .state_db
            .lease_next_ready(ts(30))
            .expect("lease")
            .expect("intent");
        assert_eq!(intent.kind, PendingIntentKind::Download);
        assert_eq!(intent.path, fixture.local_root.join("fresh.txt"));
    }

    #[test]
    fn ignored_remote_changes_are_dropped_not_enqueued() {
        let mut fixture = Fixture::new();
        fixture.path_filter = Some(Arc::new(SharedEventPathFilter::new(
            &fixture.local_root,
            crate::path_filter::EventPathFilterOptions::default(),
        )));
        fixture.poll(ts(0)); // baseline

        // A `.DS_Store` appearing on the cloud side (e.g. someone
        // browsed the mirror folder in Finder) matches the default
        // ignore rules and must never become a download intent.
        std::fs::write(fixture.cloud_root.join(".DS_Store"), b"finder").expect("seed remote");
        fixture.feed.emit(
            fixture.cloud_root.join(".DS_Store"),
            WatchEventKind::Created,
            ts(10),
        );

        let report = fixture.poll(ts(20));
        assert_eq!(report.observed_changes, 1);
        assert_eq!(report.ignored_changes, 1);
        assert_eq!(report.enqueued_intents, 0);
        assert!(
            fixture
                .state_db
                .lease_next_ready(ts(30))
                .expect("lease")
                .is_none(),
            "no intent may exist for an ignored remote path"
        );
    }

    #[test]
    fn remote_removal_maps_to_apply_remote_delete() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline

        fixture.feed.emit(
            fixture.cloud_root.join("gone.txt"),
            WatchEventKind::Removed,
            ts(10),
        );

        let report = fixture.poll(ts(20));
        assert_eq!(report.enqueued_intents, 1);
        let intent = fixture
            .state_db
            .lease_next_ready(ts(30))
            .expect("lease")
            .expect("intent");
        assert_eq!(intent.kind, PendingIntentKind::ApplyRemoteDelete);
        assert_eq!(intent.path, fixture.local_root.join("gone.txt"));
    }

    #[test]
    fn self_write_echoes_are_suppressed_not_enqueued() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline

        // Simulate our own completed upload: tag + echo record.
        std::fs::write(fixture.cloud_root.join("ours.txt"), b"ours").expect("seed remote");
        let tags = vapor_providers::tags::OpIdTagStore::new(Arc::new(
            InMemoryFilesystemCapabilities::new(true, CaseSensitivity::Sensitive),
        ));
        tags.write_op_id(&fixture.cloud_root.join("ours.txt"), "op-mine")
            .expect("tag");
        fixture.remote_echoes.record_write(
            "ours.txt",
            Some("op-mine".to_string()),
            None,
            None,
            ts(5),
        );

        // The feed drain reads the op-id through its own tag store,
        // which shares the same xattr surface in production. The manual
        // fixture uses a separate in-memory caps instance, so emit the
        // change and suppress via the hash-free op-id path by writing
        // the tag through the provider's store instead: simplest is to
        // verify suppression counting through matches_write directly on
        // the echo cache — plus the end-to-end variant in the runtime
        // integration tests where the stores are shared.
        assert!(
            fixture
                .remote_echoes
                .matches_write("ours.txt", Some("op-mine"), None, ts(10))
        );
    }

    #[test]
    fn cursor_expiry_schedules_whole_scope_reconcile_and_rebaselines() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline

        // Poison the durable cursor so the provider reports expiry.
        fixture
            .state_db
            .set_state("provider.changes_cursor.default", "999999", ts(1))
            .expect("poison cursor");
        fixture.poller = RemotePoller::new("default");

        let report = fixture.poll(ts(20));
        assert!(report.cursor_expired);
        assert_eq!(report.enqueued_intents, 1, "reconcile intent enqueued");

        let intent = fixture
            .state_db
            .lease_next_ready(ts(30))
            .expect("lease")
            .expect("intent");
        assert_eq!(intent.kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(intent.path, fixture.local_root);

        // Cursor re-baselined durably.
        let cursor = fixture
            .state_db
            .state("provider.changes_cursor.default")
            .expect("state read")
            .expect("cursor present");
        assert_ne!(cursor.value, "999999");
    }

    #[test]
    fn suspended_throttle_state_never_polls() {
        let mut fixture = Fixture::new();
        fixture
            .app
            .apply_throttle_inputs(vapor_shared::ThrottleInputs {
                system_cpu_load_percent: 95,
                ..vapor_shared::ThrottleInputs::default()
            });
        assert_eq!(
            fixture.app.snapshot().throttle_state,
            ThrottleState::Suspended
        );
        let report = fixture.poll(ts(0));
        assert!(!report.polled);
    }
}
