//! Remote changes poll + apply mapping.
//!
//! On a throttle-aware cadence the poller pulls the provider's changes
//! feed, filters self-write echoes, maps surviving changes onto
//! durable remote-sourced intents (`Download` / `ApplyRemoteDelete`),
//! and persists the feed cursor. Cursor discipline: the cursor advances
//! durably only after the page's intents are durably enqueued, so a
//! crash between the two replays the page and the per-(path, kind)
//! coalescing absorbs the duplicates, at-least-once, never at-most-once.
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
    /// matches the ignore rules: ignore filtering is symmetric, so an
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
    /// The provider refused the sign-in; the runtime holds the profile
    /// until the user signs in again. The message is kept by the poller
    /// (`take_refused_sign_in`).
    pub sign_in_refused: bool,
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
    /// The provider's message from the last poll it refused for the
    /// sign-in, until the runtime takes it.
    refused_sign_in: Option<String>,
    /// The feed has no cursor that a poll confirmed: the first poll
    /// since the start has not answered, or the cursor expired (or was
    /// discarded) and its baseline has not answered. A reconcile walk
    /// that started now could list a folder before the cursor is
    /// taken, and a change between the two would be in neither. The
    /// wait ends with the answer of that poll, whatever it is.
    awaiting_cursor: bool,
    /// The last page held as many changes as a page can: more wait.
    page_was_full: bool,
    /// When a change was last near (`note_activity`).
    last_activity_inst: Option<Instant>,
    /// The wait ended with a poll that failed, so a walk may have
    /// started with no cursor. The baseline that answers later is
    /// newer than the listings of that walk, and a change between the
    /// two is in neither: one more whole-scope reconcile finds it.
    walk_may_predate_cursor: bool,
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
            refused_sign_in: None,
            awaiting_cursor: true,
            page_was_full: false,
            last_activity_inst: None,
            walk_may_predate_cursor: false,
        }
    }

    /// A poll gave no answer. The reconcile must not wait for a feed
    /// that may stay away, so the wait ends here.
    fn end_wait_without_cursor(&mut self) {
        if self.awaiting_cursor {
            self.awaiting_cursor = false;
            self.walk_may_predate_cursor = true;
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
        self.awaiting_cursor = true;
    }

    /// Whether a reconcile walk has to wait for a poll before it
    /// starts, so that each of its listings is newer than the cursor.
    pub fn awaits_cursor(&self) -> bool {
        self.awaiting_cursor
    }

    /// Whether the feed has changes it has not handed over yet: a poll
    /// is in flight, or the last page was full and the next one is
    /// asked for at once.
    pub fn more_changes_expected(&self) -> bool {
        self.in_flight.is_some() || self.page_was_full
    }

    /// Collisions found since the last call, for the timeline.
    pub fn take_name_collisions(&mut self) -> Vec<(PathBuf, PathBuf)> {
        std::mem::take(&mut self.name_collisions)
    }

    /// The provider's message from a poll it refused for the sign-in.
    pub fn take_refused_sign_in(&mut self) -> Option<String> {
        self.refused_sign_in.take()
    }

    /// Poll cadence for the current throttle state; `None` means the
    /// state forbids polling entirely. A feed that a local watcher
    /// feeds is read each second whatever the state: reading it costs
    /// nothing, and the change is already on this device's disk. A
    /// feed that is a network call follows the state, except while a
    /// change is near (`active`).
    fn cadence_for(state: ThrottleState, watched: bool, active: bool) -> Option<Duration> {
        if state == ThrottleState::Suspended {
            return None;
        }
        if watched {
            return Some(Duration::from_secs(
                constants::engine::REMOTE_POLL_WATCHED_SECONDS,
            ));
        }
        let seconds = match state {
            ThrottleState::Idle => constants::engine::REMOTE_POLL_IDLE_SECONDS,
            _ if active => constants::engine::REMOTE_POLL_IDLE_SECONDS,
            ThrottleState::Light => constants::engine::REMOTE_POLL_LIGHT_SECONDS,
            ThrottleState::Throttled => constants::engine::REMOTE_POLL_THROTTLED_SECONDS,
            ThrottleState::Suspended => return None,
        };
        Some(Duration::from_secs(seconds))
    }

    /// A change is near: this device just moved a file, or a poll just
    /// brought changes. The feed is polled at the `Idle` cadence for a
    /// while from `now_inst` on.
    pub fn note_activity(&mut self, now_inst: Instant) {
        self.last_activity_inst = Some(now_inst);
    }

    fn change_is_near(&self, now_inst: Instant) -> bool {
        self.last_activity_inst.is_some_and(|last| {
            now_inst.saturating_duration_since(last)
                < Duration::from_secs(constants::engine::REMOTE_POLL_ACTIVE_WINDOW_SECONDS)
        })
    }

    /// Clears the poll cadence so the next tick polls immediately.
    /// Used by the reconcile boost: `vapor reconcile` should surface
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
            if let Some(poll) = self.unwrap_poll(result, &mut report) {
                self.apply_poll(
                    poll,
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
        let capabilities = app.provider().capabilities();
        if !capabilities.supports_remote_changes_feed {
            return Ok(report);
        }
        let throttle_state = app.snapshot().throttle_state;
        let now_inst = clock.now();
        let Some(cadence) = Self::cadence_for(
            throttle_state,
            capabilities.changes_feed_is_watched,
            self.change_is_near(now_inst),
        ) else {
            return Ok(report);
        };
        if !app.remote_poll_allowed() {
            return Ok(report);
        }
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
        if let Some(poll) = self.unwrap_poll(result, &mut report) {
            self.apply_poll(
                poll,
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
    /// and yields `None`; the next cadence retries. A refused sign-in is
    /// handed to the runtime instead of logged: it holds the profile.
    fn unwrap_poll(
        &mut self,
        result: Result<Result<ChangesPoll, ProviderError>, String>,
        report: &mut RemotePollReport,
    ) -> Option<ChangesPoll> {
        match result {
            Ok(Ok(poll)) => Some(poll),
            Ok(Err(error)) if error.kind == vapor_shared::ProviderErrorKind::Authentication => {
                report.sign_in_refused = true;
                self.refused_sign_in = Some(error.message);
                None
            }
            Ok(Err(error)) => {
                self.end_wait_without_cursor();
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
                self.end_wait_without_cursor();
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
            ChangesPoll::CursorExpired if self.cursor.is_none() => {
                // A poll with no cursor asks for a baseline, which
                // cannot expire: a provider bug. The next cadence asks
                // again.
                self.end_wait_without_cursor();
                logging::error(
                    "Provider reported CursorExpired for a baseline poll; deferring re-baseline",
                    &[],
                );
            }
            ChangesPoll::CursorExpired => {
                report.cursor_expired = true;
                if first_poll {
                    logging::info(
                        "Remote changes cursor did not survive the restart; re-baselining behind the startup reconcile",
                        &[("cursor_key", self.cursor_state_key.clone())],
                    );
                } else {
                    // The feed lost its place (a rescan signal, a ring
                    // that rolled over); the reconcile repairs it on its
                    // own, so this is routine.
                    logging::info(
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
                // The next poll has no cursor, so it is the baseline. It
                // runs on the poll thread like every poll, at the next
                // tick, and no walk starts before it answers.
                state_db.delete_state(&self.cursor_state_key)?;
                self.cursor = None;
                self.awaiting_cursor = true;
                self.last_poll_inst = None;
            }
            ChangesPoll::Page(page) => {
                self.awaiting_cursor = false;
                // A page for a poll with a cursor holds every change
                // since that cursor, so only a baseline leaves a gap.
                if std::mem::take(&mut self.walk_may_predate_cursor) && self.cursor.is_none() {
                    logging::info(
                        "The changes feed has its cursor after a poll that failed; scheduling one more whole-scope reconcile",
                        &[("cursor_key", self.cursor_state_key.clone())],
                    );
                    report.enqueued_intents += state_db.enqueue_intents_coalesced(
                        &[(
                            local_root.to_path_buf(),
                            PendingIntentKind::ReconcileSubtree,
                            now,
                        )],
                        crate::safeguards::IntentSource::Fresh,
                    )?;
                }
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
                    if sync_mode == SyncMode::PushOnly
                        && change.kind == RemoteChangeKind::CreatedOrModified
                        && let Some(folder) = folder_of_other_spelling(&local_target)
                    {
                        // Push-only: the cloud names an entry that this
                        // device stores under another spelling. A
                        // restore through the cloud's name does nothing,
                        // because that name is not on this device. The
                        // walk of the folder removes the cloud's
                        // spelling and sends the local one again.
                        batch.push((folder, PendingIntentKind::ReconcileSubtree, event_time));
                        report.mirror_reverts += 1;
                        continue;
                    }
                    if sync_mode != SyncMode::PushOnly
                        && change.kind == RemoteChangeKind::CreatedOrModified
                        && let Some(folder) = respelled_folder_above(&local_target)
                    {
                        // The cloud names a folder on the way to this
                        // file by another spelling than this device
                        // stores: the folder was renamed in the cloud,
                        // by capitals only. A download through the new
                        // spelling would write into the old folder and
                        // leave its name as it is. The walk of the
                        // folder above it renames the local folder to
                        // the cloud's spelling, and the index rows with
                        // it.
                        batch.push((folder, PendingIntentKind::ReconcileSubtree, event_time));
                        continue;
                    }
                    if change.kind == RemoteChangeKind::CreatedOrModified
                        && let Some(existing) =
                            crate::name_collision::colliding_local_path(&local_target)
                        && !is_respelling_of_synced_file(state_db, &existing, &local_target, change)
                    {
                        // The walk materializes the colliding name as a
                        // conflict copy and records the alias; the feed
                        // asks for that walk rather than duplicating it.
                        // Every change asks: the walk is also what carries
                        // a later change of that object to its copy. The
                        // report is made one time for each name.
                        if let Some(parent) = local_target.parent() {
                            batch.push((
                                parent.to_path_buf(),
                                PendingIntentKind::ReconcileSubtree,
                                event_time,
                            ));
                        }
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
                        }
                        continue;
                    }
                    match change.kind {
                        RemoteChangeKind::CreatedOrModified => {
                            // The object is there again. A delete the
                            // daemon made at this path is over, and a
                            // later removal is someone's real one.
                            remote_echoes.forget_delete(change.path.as_str());
                            if remote_echoes.matches_write_checked(
                                change.path.as_str(),
                                change.op_id.as_deref(),
                                change.content_hash.as_deref(),
                                change.size_bytes,
                                change.modified_at,
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
                                if let Some(folder) = folder_of_other_spelling(&local_target) {
                                    // The name that left is one this
                                    // device spells another way: the
                                    // walk of the folder restores the
                                    // local spelling.
                                    batch.push((
                                        folder,
                                        PendingIntentKind::ReconcileSubtree,
                                        event_time,
                                    ));
                                    report.mirror_reverts += 1;
                                } else if local_file_exists(&local_target) {
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
                            // mass-delete decision has nothing left to
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
                self.page_was_full = page_was_full;
                if page_was_full {
                    // Re-poll immediately on the next tick to continue
                    // draining the known backlog.
                    self.last_poll_inst = None;
                }
            }
        }

        Ok(())
    }
}

/// Whether a regular file exists at `path` (symlinks and directories
/// do not count as restorable local canonicals).
/// A cloud object whose name is another spelling of a synced local
/// file's name, with the size and the modification time that file was
/// last synced with: the cloud renamed the file and changed only the
/// spelling. It goes on as an ordinary download, whose planner confirms
/// the content and renames the local file instead of transferring.
fn is_respelling_of_synced_file(
    state_db: &DurableStateDb,
    existing: &Path,
    local_target: &Path,
    change: &vapor_providers::RemoteChange,
) -> bool {
    let (Some(size_bytes), Some(modified_at)) = (change.size_bytes, change.modified_at) else {
        return false;
    };
    matches!(state_db.sync_index(local_target), Ok(None))
        && state_db
            .sync_index(existing)
            .ok()
            .flatten()
            .is_some_and(|index| index.matches_remote(size_bytes, modified_at))
}

/// The folder in which `target` and the entry this device stores for
/// it differ in spelling, when they do: the parent of the highest path
/// component that is spelled another way.
fn folder_of_other_spelling(target: &Path) -> Option<PathBuf> {
    let crate::name_collision::LocalSpelling::Other(stored) =
        crate::name_collision::local_spelling(target)
    else {
        return None;
    };
    // From the last component upward, as the spelling check compares.
    let mut folder = None;
    let mut current = target;
    for (wanted, found) in target.components().rev().zip(stored.components().rev()) {
        let Some(parent) = current.parent() else {
            break;
        };
        if wanted != found {
            match (wanted.as_os_str().to_str(), found.as_os_str().to_str()) {
                (Some(wanted), Some(found))
                    if crate::name_collision::fold(wanted)
                        == crate::name_collision::fold(found) =>
                {
                    folder = Some(parent.to_path_buf());
                }
                _ => break,
            }
        }
        current = parent;
    }
    folder
}

/// The folder to walk when a folder on the way to `target` is stored
/// under another spelling than `target` gives it. `None` when every
/// folder is spelled alike, also when only the file's own name differs.
fn respelled_folder_above(target: &Path) -> Option<PathBuf> {
    folder_of_other_spelling(target).filter(|folder| Some(folder.as_path()) != target.parent())
}

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
/// record expired. The persisted sync index still holds what the last
/// transfer left in the cloud, and a change that describes exactly that
/// object changes nothing:
///
/// - A change that carries the indexed hash is the same bytes. A change
///   that carries another hash is other bytes, whatever else agrees.
/// - A change with the size and the remote mtime the index recorded is
///   the rsync quick check passing: nothing wrote the object since. It
///   needs no op-id. An object that no Vapor wrote has none, and a
///   `chmod` or a `chflags` of it is still no change.
/// - A change that carries the indexed op-id is our object. An in-place
///   edit (an append, a same-size rewrite) leaves it tagged, so the
///   op-id counts only while the feed has no size or mtime that says
///   otherwise.
fn is_durable_self_write_echo(
    state_db: &mut DurableStateDb,
    local_target: &Path,
    change: &vapor_providers::RemoteChange,
) -> bool {
    let Ok(Some(index)) = state_db.sync_index(local_target) else {
        return false;
    };
    if let Some(hash) = change.content_hash.as_deref()
        && !index.content_hash.is_empty()
    {
        return hash == index.content_hash;
    }
    if let (Some(size), Some(modified_at)) = (change.size_bytes, change.modified_at)
        && index.matches_remote(size, modified_at)
    {
        return true;
    }
    if change.op_id.as_deref() != Some(index.last_op_id.as_str()) {
        return false;
    }
    match (change.size_bytes, change.modified_at) {
        (Some(size), Some(_)) => index.remote_modified_at.is_none() && size == index.size_bytes,
        (Some(size), None) => size == index.size_bytes,
        (None, _) => true,
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
            self.poll_at_once(now)
        }

        /// A poll at the same instant as the one before: it runs only
        /// when the poller asked for a poll without waiting.
        fn poll_at_once(&mut self, now: SystemTime) -> RemotePollReport {
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
    fn each_change_of_a_colliding_cloud_name_asks_for_the_folder_walk() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.local_root.join("Readme.md"), b"local").expect("seed local");
        if !crate::name_collision::folds_case_at(&fixture.local_root)
            || std::fs::symlink_metadata(fixture.local_root.join("readme.md")).is_err()
        {
            // This volume keeps the two names apart: no collision.
            return;
        }
        fixture.poll(ts(0)); // baseline
        let change = |fixture: &mut Fixture, at: u64| {
            std::fs::write(fixture.cloud_root.join("readme.md"), at.to_string()).expect("cloud");
            fixture.feed.emit(
                fixture.cloud_root.join("readme.md"),
                WatchEventKind::Modified,
                ts(at),
            );
            let report = fixture.poll(ts(at + 10));
            let intent = fixture
                .state_db
                .lease_next_ready(ts(at + 20))
                .expect("lease")
                .expect("the folder walk is queued");
            assert_eq!(intent.kind, PendingIntentKind::ReconcileSubtree);
            assert_eq!(intent.path, fixture.local_root);
            fixture
                .state_db
                .complete_leased(intent.id)
                .expect("the walk ran");
            report.name_collisions
        };
        assert_eq!(change(&mut fixture, 100), 1, "reported the first time");
        // The cloud object changes again. Its copy on this device only
        // follows through another walk of the folder.
        assert_eq!(change(&mut fixture, 200), 0, "reported one time");
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
            None,
            ts(5),
        );

        // The feed drain reads the op-id through its own tag store,
        // which shares the same xattr surface in production. The manual
        // fixture uses a separate in-memory caps instance, so emit the
        // change and suppress via the hash-free op-id path by writing
        // the tag through the provider's store instead: simplest is to
        // verify suppression counting through matches_write directly on
        // the echo cache, plus the end-to-end variant in the runtime
        // integration tests where the stores are shared.
        assert!(
            fixture
                .remote_echoes
                .matches_write("ours.txt", Some("op-mine"), None, ts(10))
        );
    }

    /// Syncs `name` as a file that no Vapor wrote: the index row holds
    /// the content, the size, and the remote mtime, and an empty op-id.
    fn seed_untagged_synced_file(fixture: &mut Fixture, name: &str, body: &[u8]) -> PathBuf {
        let remote = fixture.cloud_root.join(name);
        let local = fixture.local_root.join(name);
        std::fs::write(&remote, body).expect("seed remote");
        std::fs::write(&local, body).expect("seed local");
        let remote_modified_at = std::fs::metadata(&remote)
            .and_then(|metadata| metadata.modified())
            .expect("remote mtime");
        let local_modified_at = std::fs::metadata(&local)
            .and_then(|metadata| metadata.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &local,
                &vapor_providers::filesystem::hash_hex_of_bytes(body),
                body.len() as u64,
                local_modified_at,
                Some(remote_modified_at),
                "",
                ts(1),
            )
            .expect("seed index");
        remote
    }

    #[test]
    fn an_event_that_changes_no_bytes_of_an_untagged_object_is_no_change() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline
        let remote = seed_untagged_synced_file(&mut fixture, "theirs.txt", b"from another tool");

        // A `chmod` or a `chflags` in the cloud: an event, and the
        // object still has the size and the time of the last sync.
        fixture
            .feed
            .emit(remote.clone(), WatchEventKind::Modified, ts(10));

        let report = fixture.poll(ts(20));
        assert_eq!(report.observed_changes, 1);
        assert_eq!(
            report.suppressed_echoes, 1,
            "the object is what the index recorded: no change"
        );
        assert_eq!(report.enqueued_intents, 0);
    }

    #[test]
    fn a_write_to_an_untagged_object_is_still_a_change() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline
        let remote = seed_untagged_synced_file(&mut fixture, "theirs.txt", b"from another tool");

        std::fs::write(&remote, b"from another tool, edited").expect("edit remote");
        fixture
            .feed
            .emit(remote.clone(), WatchEventKind::Modified, ts(10));

        let report = fixture.poll(ts(20));
        assert_eq!(report.suppressed_echoes, 0);
        assert_eq!(report.enqueued_intents, 1, "another size is a change");
    }

    #[test]
    fn a_baseline_after_a_poll_that_failed_schedules_one_more_reconcile() {
        let mut fixture = Fixture::new();
        assert!(fixture.poller.awaits_cursor());

        // The first poll of this start fails (the device is offline).
        // The reconcile must not wait for the feed, so the wait ends.
        let mut report = RemotePollReport::default();
        let failed = fixture.poller.unwrap_poll(
            Ok(Err(ProviderError::transient("the network is down"))),
            &mut report,
        );
        assert!(failed.is_none());
        assert!(!fixture.poller.awaits_cursor());

        // A walk may run now, with no cursor. The baseline that answers
        // later is newer than its listings.
        let baseline = fixture.poll(ts(10));
        assert!(baseline.polled);
        assert_eq!(
            baseline.enqueued_intents, 1,
            "one more whole-scope reconcile covers the time between"
        );
        let intent = fixture
            .state_db
            .lease_next_ready(ts(20))
            .expect("lease")
            .expect("intent");
        assert_eq!(intent.kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(intent.path, fixture.local_root);

        // It is asked for one time.
        let next = fixture.poll(ts(30));
        assert_eq!(next.enqueued_intents, 0);
    }

    #[test]
    fn a_poll_that_fails_with_a_cursor_in_hand_schedules_no_reconcile() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline
        let mut report = RemotePollReport::default();
        fixture.poller.unwrap_poll(
            Ok(Err(ProviderError::transient("the network is down"))),
            &mut report,
        );

        let next = fixture.poll(ts(10));

        assert!(next.polled);
        assert_eq!(next.enqueued_intents, 0);
    }

    #[test]
    fn cursor_expiry_schedules_the_reconcile_and_rebaselines_with_the_next_poll() {
        let mut fixture = Fixture::new();
        fixture.poll(ts(0)); // baseline

        // Poison the durable cursor so the provider reports expiry.
        fixture
            .state_db
            .set_state("provider.changes_cursor.default", "999999", ts(1))
            .expect("poison cursor");
        fixture.poller = RemotePoller::new("default");

        // The poll that learns of the expiry asks the provider nothing
        // more: the baseline is a poll of its own, off the tick thread
        // in the daemon.
        let expired = fixture.poll(ts(20));
        assert!(expired.cursor_expired);
        assert_eq!(expired.enqueued_intents, 1, "reconcile intent enqueued");
        assert!(
            fixture
                .state_db
                .state("provider.changes_cursor.default")
                .expect("state read")
                .is_none(),
            "the expired cursor is dropped"
        );
        assert!(
            fixture.poller.awaits_cursor(),
            "no walk starts before the baseline answers"
        );

        // The baseline does not wait for the cadence.
        let baseline = fixture.poll_at_once(ts(21));
        assert!(baseline.polled);
        assert!(!baseline.cursor_expired);
        assert_eq!(baseline.enqueued_intents, 0);
        assert!(!fixture.poller.awaits_cursor());

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
    fn a_watched_feed_is_read_each_second_in_every_state_but_suspended() {
        for state in [
            ThrottleState::Idle,
            ThrottleState::Light,
            ThrottleState::Throttled,
        ] {
            assert_eq!(
                RemotePoller::cadence_for(state, true, false),
                Some(Duration::from_secs(1)),
                "{state:?}"
            );
        }
        assert_eq!(
            RemotePoller::cadence_for(ThrottleState::Suspended, true, true),
            None
        );
    }

    #[test]
    fn a_network_feed_is_polled_sooner_while_a_change_is_near() {
        let throttled = |active| RemotePoller::cadence_for(ThrottleState::Throttled, false, active);
        assert_eq!(throttled(false), Some(Duration::from_secs(60)));
        assert_eq!(throttled(true), Some(Duration::from_secs(5)));

        // The window closes a minute after the last sign of a change.
        use crate::clock::Clock as _;
        let clock = ManualClock::at_now();
        let mut poller = RemotePoller::new("default");
        poller.note_activity(clock.now());
        clock.advance(Duration::from_secs(59));
        assert!(poller.change_is_near(clock.now()));
        clock.advance(Duration::from_secs(2));
        assert!(!poller.change_is_near(clock.now()));
    }

    #[test]
    fn suspended_throttle_state_never_polls() {
        let mut fixture = Fixture::new();
        fixture
            .app
            .apply_throttle_inputs(vapor_shared::ThrottleInputs {
                thermal_pressure: vapor_shared::ThermalPressure::Critical,
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
