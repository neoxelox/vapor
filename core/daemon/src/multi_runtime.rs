//! Multi-profile daemon composition.
//!
//! One daemon process serves every enabled profile:
//!
//! - **Per-profile isolation**: each profile owns its `DaemonRuntime`
//!   (debounce, scheduler, executor, echo caches) and its own durable
//!   state DB (`state/profiles/<id>/vapor.sqlite`; the `default`
//!   profile's sits in `state/` itself).
//! - **Deduplicated watches**: one fs watcher per distinct
//!   canonical local root; the raw callback fans events out into each
//!   matching profile's bounded ingest recorder, tagged implicitly by
//!   the recorder it lands in.
//! - **Shared workgate** (`data-flow.md §Multi-profile watch
//!   coordination`): every profile's `DaemonApp` gates work against one
//!   daemon-level `ThrottleWorkgate`, so per-profile queues cannot
//!   multiply the daemon's device impact.
//! - **Blast-radius containment**: each profile ticks under a
//!   panic catcher and its own consecutive-error budget; a failed
//!   profile suspends only its own queue and is composed again with
//!   a backoff. Release builds unwind on panic, so the catcher holds
//!   there too.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use vapor_shared::{RunState, ThrottleState, constants};

use crate::DaemonApp;
use crate::clock::SharedClock;
use crate::fs_events::{
    FsEventErrorRecord, FsEventRecord, FsEventRecording, FsEventsWatcher, SharedEventPathFilter,
    normalize_watch_root,
};
use crate::ipc_service::{DaemonStatusSnapshot, StatusPublisher};
use crate::logging;
use crate::metrics::MetricsSampler;
use crate::path_filter::EventPathFilterOptions;
use crate::profiles::ResolvedProfile;
use crate::runtime::{DaemonRuntime, DaemonRuntimeError, TickWaker, is_shutdown_requested};
use crate::runtime_control::{ControlRequest, RuntimeControl};
use crate::state_db::DurableStateDb;
use crate::sync_directories::SyncScope;
use crate::throttle::ThrottleCaps;
use crate::workgate::ThrottleWorkgate;

/// Consecutive tick failures tolerated per profile before that profile
/// (and only that profile) is suspended.
const MAX_CONSECUTIVE_PROFILE_TICK_ERRORS: u32 = 5;

/// Fan-out recorder handed to each deduplicated watcher: every callback
/// is forwarded to the profiles whose canonical root contains the event
/// path, then the shared tick waker fires.
struct FanOutRecorder {
    targets: Vec<(PathBuf, Arc<crate::event_intents::BoundedFsEventRecorder>)>,
    waker: Arc<TickWaker>,
}

impl FsEventRecording for FanOutRecorder {
    fn record_event(&self, event: FsEventRecord) {
        for (root, recorder) in &self.targets {
            if event.path.starts_with(root) {
                recorder.record_event(event.clone());
            }
        }
        self.waker.notify();
    }

    fn record_error(&self, error: FsEventErrorRecord) {
        for (_, recorder) in &self.targets {
            recorder.record_error(error.clone());
        }
        self.waker.notify();
    }
}

struct ProfileSlot {
    profile: ResolvedProfile,
    runtime: DaemonRuntime,
    control: Arc<RuntimeControl>,
    consecutive_tick_errors: u32,
    /// `Some(reason)` once the profile is suspended (panic or repeated
    /// tick errors). A suspended profile stops ticking; the others and
    /// the watcher keep running.
    failed: Option<String>,
    /// The suspension happened while composing the daemon (invalid
    /// provider, overlapping roots, unopenable DB, watcher that could not
    /// start): a configuration problem the user has to fix, not a
    /// runtime failure worth restarting for.
    failed_at_composition: bool,
    /// The local root this profile waits for. The slot is a placeholder
    /// composed again when the folder returns or is re-created.
    missing_root: Option<PathBuf>,
    /// Set when the runtime suspended the profile: when to compose it
    /// again, and how many times that was tried.
    runtime_retry: Option<RuntimeRetry>,
    /// Set on a profile that was composed again after a suspension,
    /// until it ran without a failure for the wait it had reached. A
    /// profile that composes and then fails the same way would
    /// otherwise start from the shortest wait every time, and each
    /// attempt opens a database and builds a filter on the tick thread.
    relapse: Option<Relapse>,
}

#[derive(Clone, Copy, Debug)]
struct RuntimeRetry {
    attempts: usize,
    at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct Relapse {
    /// The attempts the next suspension starts from.
    attempts: usize,
    /// From here on the profile counts as healthy again.
    forgiven_at: Instant,
}

/// What a change of the profile set came to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reshape {
    /// The profiles that run are the ones the configuration names.
    Applied,
    /// A profile that runs has other folders, another provider, or
    /// another mode in the file than the ones it was composed with.
    NeedsRestart,
}

/// Whether `wanted` is the profile that `running` was composed from. A
/// name decides nothing of a runtime.
fn same_definition(running: &ResolvedProfile, wanted: &ResolvedProfile) -> bool {
    running.provider_kind == wanted.provider_kind
        && running.scope == wanted.scope
        && running.invalid == wanted.invalid
}

/// The wait before the next attempt to compose a suspended profile.
fn suspended_profile_retry_delay(attempts: usize) -> Duration {
    let waits = constants::engine::SUSPENDED_PROFILE_RETRY_SECONDS;
    Duration::from_secs(waits[attempts.min(waits.len() - 1)])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllSuspended {
    /// Every profile was suspended while composing the daemon.
    AtComposition,
    /// At least one profile was suspended by a runtime failure.
    AtRuntime,
    /// Every profile is parked on a local root that is not
    /// there; each is composed again when its folder returns.
    WaitingForRoots,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MultiTickReport {
    pub ticked_profiles: usize,
    pub failed_profiles: usize,
    pub any_pending_work: bool,
}

pub struct MultiProfileRuntime {
    slots: Vec<ProfileSlot>,
    _watchers: Vec<FsEventsWatcher>,
    tick_waker: Arc<TickWaker>,
    external_control: Option<Arc<RuntimeControl>>,
    status_publisher: Option<Arc<dyn StatusPublisher>>,
    timeline: Arc<crate::timeline::TimelineBuffer>,
    clock: SharedClock,
    tick_interval: Duration,
    idle_tick_interval: Duration,
    /// Per-slot state fingerprints at the last status publish; a
    /// mismatch marks the published snapshot stale.
    last_published_slot_states: Vec<SlotStateFingerprint>,
    last_status_publish_inst: Option<Instant>,
    /// When the trash of the profiles that do not run was last purged,
    /// and the purge in flight.
    last_other_trash_purge_inst: Option<Instant>,
    other_trash_purge: Option<crate::provider_jobs::ProviderCall<usize>>,
    /// Whether the previous tick reported pending work: publishes one
    /// trailing snapshot after work quiets so final counts land.
    last_any_pending_work: bool,
    /// Daemon-wide resource budget shared by every profile runtime; kept
    /// here so a live configuration change can replace it.
    shared_budget: Arc<Mutex<crate::resource_budget::ResourceBudget>>,
    /// Watches `vapor.json` once the daemon asks for live reload.
    config_reloader: Option<crate::config_reload::ConfigReloader>,
    /// Restart-required keys that changed since start, for status.
    restart_required: Vec<&'static str>,
    /// Bumped on every applied configuration change so the next status
    /// publish is not skipped as "nothing observable changed".
    config_generation: u64,
    last_published_config_generation: u64,
    /// What profiles are composed from, for composing a parked profile
    /// again once its local root is back.
    composer: SlotComposer,
    watchers_enabled: bool,
    last_missing_root_check_inst: Option<Instant>,
    /// Provider I/O runs on worker threads; a profile composed later
    /// gets the same.
    transfer_workers: bool,
    /// The user paused the daemon and did not resume it yet.
    paused_by_user: bool,
    /// The profiles the user paused one by one. A profile that is
    /// composed again while its name is here starts paused.
    paused_profiles: std::collections::BTreeSet<String>,
    /// What stops `vapor.json` from giving the profiles it means to,
    /// for the status of a daemon with no profile.
    config_problems: Vec<String>,
    /// The runtimes of profiles that left the configuration while the
    /// daemon ran, each ending on a thread of its own: the end of a
    /// runtime waits for its provider calls in flight, and the tick
    /// thread must not wait with it.
    retiring: Vec<(String, std::thread::JoinHandle<()>)>,
    /// Profiles that the configuration names while a runtime with
    /// their id still ends. Two runtimes on one state database would
    /// both lease its queue, so each is composed when that end is over.
    awaiting_retirement: Vec<ResolvedProfile>,
    /// The ids of the profiles in the order of the configuration, which
    /// is the order status lists them in.
    profile_order: Vec<String>,
}

/// The cheap observable state of one profile: enough to detect that a
/// published status snapshot went stale without re-running its SQL.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SlotStateFingerprint {
    run_state: RunState,
    throttle_state: ThrottleState,
    suspended: bool,
    reason: String,
    decision_changes: u64,
    sign_in_required: bool,
    reconcile: crate::runtime::ReconcileStatus,
}

fn slot_state_fingerprint(slot: &ProfileSlot) -> SlotStateFingerprint {
    let app_snapshot = slot.runtime.app().snapshot();
    SlotStateFingerprint {
        run_state: slot_effective_run_state(slot),
        throttle_state: app_snapshot.throttle_state,
        suspended: slot.failed.is_some(),
        reason: app_snapshot.reason.clone(),
        decision_changes: slot.runtime.decision_changes(),
        sign_in_required: slot.runtime.sign_in_required(),
        reconcile: slot.runtime.reconcile_status(),
    }
}

impl MultiProfileRuntime {
    /// Composes the full multi-profile daemon: shared workgate, one
    /// runtime per enabled profile (each on its own durable DB), and
    /// deduplicated watchers with per-profile fan-out.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        profiles: Vec<ResolvedProfile>,
        filter_options: EventPathFilterOptions,
        metrics_sampler: Arc<dyn MetricsSampler>,
        clock: SharedClock,
        device_id: &str,
        start_watchers: bool,
        budget_config: crate::resource_budget::EffectiveBudgetConfig,
        mass_delete_settings: crate::safeguards::MassDeleteGuardSettings,
    ) -> Result<Self, DaemonRuntimeError> {
        Self::start_with_state_root(
            profiles,
            filter_options,
            metrics_sampler,
            clock,
            device_id,
            start_watchers,
            None,
            budget_config,
            mass_delete_settings,
        )
    }

    /// The shared daemon activity timeline. Every profile
    /// runtime appends to it; the IPC service reads it.
    pub fn timeline(&self) -> Arc<crate::timeline::TimelineBuffer> {
        self.timeline.clone()
    }

    /// Moves every profile's provider I/O onto worker threads (see
    /// [`DaemonRuntime::enable_transfer_workers`]). The daemon
    /// bootstrap calls this once after composition; tests keep the
    /// deterministic inline mode by not calling it.
    pub fn enable_transfer_workers(&mut self) {
        self.transfer_workers = true;
        let waker = self.tick_waker.clone();
        for slot in &mut self.slots {
            slot.runtime.enable_transfer_workers(Some(waker.clone()));
        }
    }

    /// What stops the configuration from giving its profiles; shown in
    /// status when the daemon has none.
    pub fn set_config_problems(&mut self, problems: Vec<String>) {
        self.config_problems = problems;
    }

    /// Gives a profile composed while the daemon runs what the ones
    /// composed at start got from the bootstrap.
    fn finish_late_slot(&self, slot: &mut ProfileSlot) {
        if self.transfer_workers && slot.failed.is_none() {
            slot.runtime
                .enable_transfer_workers(Some(self.tick_waker.clone()));
        }
        // `vapor pause` with no profile is for the whole daemon. A
        // profile that joins while it holds starts paused, like the
        // ones that were there, and so does one the user paused by
        // name and that is composed again.
        if self.paused_by_user || self.paused_profiles.contains(&slot.profile.id) {
            slot.control.request_pause();
        }
    }

    /// Fixes the link capacity for every profile instead of measuring
    /// it. The daemon uses it to honour `VAPOR_THROTTLE_INPUTS=static`.
    pub fn pin_link_capacity_kbps(&mut self, kbps: u32) {
        self.composer.shared_bandwidth.pin_capacity_kbps(kbps);
    }

    /// Replaces the idle source every profile runtime consults for idle
    /// boost. The daemon uses it to honour `VAPOR_THROTTLE_INPUTS=static`.
    pub fn set_idle_notifier(&mut self, notifier: Arc<dyn vapor_platform::IdleNotifier>) {
        self.composer.idle_notifier = notifier.clone();
        for slot in &mut self.slots {
            slot.runtime.set_idle_notifier(notifier.clone());
        }
    }

    /// Applies the `trash` config group to every profile's trash.
    pub fn configure_trash(&mut self, settings: crate::trash::TrashSettings) {
        self.composer.trash_settings = settings;
        for slot in &mut self.slots {
            slot.runtime.configure_trash(settings);
        }
    }

    /// Applies the configured `timelineLimit`.
    pub fn set_timeline_limit(&self, limit: i64) {
        if let Ok(limit) = usize::try_from(limit)
            && limit > 0
        {
            self.timeline.set_max_entries(limit);
        }
    }

    /// `state_root` overrides where per-profile durable DBs live
    /// (tests use a fixture directory; production resolves under
    /// `vapor_dir/state`).
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_state_root(
        profiles: Vec<ResolvedProfile>,
        filter_options: EventPathFilterOptions,
        metrics_sampler: Arc<dyn MetricsSampler>,
        clock: SharedClock,
        device_id: &str,
        start_watchers: bool,
        state_root: Option<PathBuf>,
        budget_config: crate::resource_budget::EffectiveBudgetConfig,
        mass_delete_settings: crate::safeguards::MassDeleteGuardSettings,
    ) -> Result<Self, DaemonRuntimeError> {
        let now = clock.now_system();
        let tick_waker = Arc::new(TickWaker::default());
        let timeline = crate::timeline::TimelineBuffer::new(
            constants::config::DEFAULT_TIMELINE_LIMIT as usize,
        );
        let initial_state = ThrottleState::Light;
        let shared_workgate = Arc::new(Mutex::new(ThrottleWorkgate::new(
            initial_state,
            initial_caps(initial_state),
        )));
        // Daemon-wide resource management: one budget,
        // one bandwidth shaper for every profile.
        let concurrent_transfers = budget_config.concurrent_transfers;
        let shared_budget = Arc::new(Mutex::new(crate::resource_budget::ResourceBudget::new(
            budget_config,
        )));
        let shared_bandwidth = Arc::new(crate::bandwidth::TransferBandwidth::default());
        let idle_notifier: Arc<dyn vapor_platform::IdleNotifier> =
            Arc::new(vapor_platform::NativeIdleNotifier::for_current_host());

        let mut composer = SlotComposer {
            state_root,
            filter_options,
            metrics_sampler,
            clock: clock.clone(),
            device_id: device_id.to_string(),
            shared_workgate,
            timeline: timeline.clone(),
            shared_budget: shared_budget.clone(),
            shared_bandwidth,
            idle_notifier,
            concurrent_transfers,
            mass_delete_settings,
            trash_settings: crate::trash::TrashSettings::default(),
            filters_by_root: BTreeMap::new(),
        };
        let mut slots = Vec::new();
        let mut profile_order = Vec::new();
        for profile in profiles.into_iter().filter(|profile| profile.enabled) {
            profile_order.push(profile.id.clone());
            slots.push(compose_slot(&mut composer, profile, now));
        }

        let watchers = if start_watchers {
            start_deduplicated_watchers(&mut slots, &tick_waker)
        } else {
            Vec::new()
        };

        Ok(Self {
            slots,
            _watchers: watchers,
            tick_waker,
            external_control: None,
            status_publisher: None,
            timeline,
            clock,
            tick_interval: Duration::from_millis(constants::engine::DEBOUNCE_TICK_MILLIS),
            idle_tick_interval: Duration::from_millis(constants::engine::IDLE_TICK_MILLIS),
            last_published_slot_states: Vec::new(),
            last_status_publish_inst: None,
            last_other_trash_purge_inst: None,
            other_trash_purge: None,
            last_any_pending_work: false,
            shared_budget,
            config_reloader: None,
            restart_required: Vec::new(),
            config_generation: 0,
            last_published_config_generation: 0,
            composer,
            watchers_enabled: start_watchers,
            last_missing_root_check_inst: None,
            transfer_workers: false,
            paused_by_user: false,
            paused_profiles: std::collections::BTreeSet::new(),
            config_problems: Vec::new(),
            retiring: Vec::new(),
            awaiting_retirement: Vec::new(),
            profile_order,
        })
    }

    /// Composes again every profile parked on a missing local root
    /// whose folder is back, or whose `root-missing` decision was
    /// answered `recreate`. On the root-check cadence.
    fn retry_missing_roots_if_due(&mut self, now: SystemTime) {
        if !self.slots.iter().any(|slot| slot.missing_root.is_some()) {
            return;
        }
        let now_inst = self.clock.now();
        let interval = Duration::from_secs(constants::engine::ROOT_CHECK_INTERVAL_SECONDS);
        let due = self
            .last_missing_root_check_inst
            .map(|last| now_inst.saturating_duration_since(last) >= interval)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_missing_root_check_inst = Some(now_inst);
        for index in 0..self.slots.len() {
            let Some(root) = self.slots[index].missing_root.clone() else {
                continue;
            };
            let profile = self.slots[index].profile.clone();
            // `recreate` answered: the re-created folder becomes the
            // root, with a fresh marker and a merge that deletes
            // nothing. The merge flag goes first and the decision is
            // marked applied last, and the first failure stops the
            // attempt: a folder re-created without the flag would be
            // read as "everything was deleted" by the next walk.
            let recreate = match self.recreate_local_root(&profile.id, &root, now) {
                Ok(recreated) => recreated,
                Err(error) => {
                    logging::warning(
                        "Could not re-create the local root; trying again later",
                        &[
                            ("profile_id", profile.id.clone()),
                            ("path", root.display().to_string()),
                            ("error", error),
                        ],
                    );
                    continue;
                }
            };
            if !recreate && !root.is_dir() {
                continue;
            }
            logging::info(
                "Local root is back; composing the profile again",
                &[
                    ("profile_id", profile.id.clone()),
                    ("path", root.display().to_string()),
                    ("recreated", recreate.to_string()),
                ],
            );
            let mut slot = compose_slot(&mut self.composer, profile.clone(), now);
            self.finish_late_slot(&mut slot);
            if slot.missing_root.is_none() {
                if let Ok(mut state_db) =
                    DurableStateDb::open(self.composer.database_path(&profile.id))
                    && let Ok(Some(decision)) = state_db
                        .open_decision(crate::root_identity::MISSING_DECISION_KIND, Some(&root))
                {
                    let _ = state_db.withdraw_decision(decision.id, now);
                    self.timeline.push(
                        constants::timeline::KIND_DECISION,
                        profile.id.clone(),
                        format!(
                            "Decision {} (root-missing) withdrawn: the local root is back",
                            decision.id
                        ),
                        now,
                    );
                }
                if self.watchers_enabled {
                    let single = std::slice::from_mut(&mut slot);
                    let watchers = start_deduplicated_watchers(single, &self.tick_waker);
                    self._watchers.extend(watchers);
                }
            }
            self.slots[index] = slot;
            self.last_published_slot_states.clear();
        }
    }

    /// Applies a `recreate` answer to the profile's `root-missing`
    /// decision for `root`, when there is one that was not applied yet.
    /// `Ok(true)` when the folder was re-created.
    fn recreate_local_root(
        &self,
        profile_id: &str,
        root: &Path,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut state_db = DurableStateDb::open(self.composer.database_path(profile_id))
            .map_err(|error| format!("cannot open the state database: {error}"))?;
        let answered = state_db
            .resolved_unapplied_decisions()
            .map_err(|error| format!("cannot read the decisions: {error}"))?
            .into_iter()
            .find(|decision| {
                decision.kind == crate::root_identity::MISSING_DECISION_KIND
                    && decision.path.as_deref() == Some(root)
                    && decision.choice.as_deref() == Some(crate::root_identity::OPTION_RECREATE)
            });
        let Some(decision) = answered else {
            return Ok(false);
        };
        state_db
            .set_state(constants::state::MERGE_WITHOUT_DELETIONS_KEY, "1", now)
            .map_err(|error| format!("cannot set the merge flag: {error}"))?;
        std::fs::create_dir_all(root)
            .map_err(|error| format!("cannot create the folder: {error}"))?;
        crate::root_identity::reattach_local(&mut state_db, root, &self.composer.device_id, now)
            .map_err(|error| format!("cannot record the new root: {error}"))?;
        state_db
            .mark_decision_applied(decision.id, now)
            .map_err(|error| format!("cannot mark the decision applied: {error}"))?;
        self.timeline.push(
            constants::timeline::KIND_DECISION,
            profile_id.to_string(),
            format!(
                "Decision {} (root-missing) recreate: re-created {} and merging without deletions",
                decision.id,
                root.display()
            ),
            now,
        );
        Ok(true)
    }

    /// Composes again each profile that the runtime suspended, when its
    /// wait is over. A profile that fails again waits longer.
    fn retry_suspended_profiles(&mut self, now: SystemTime) {
        let now_inst = self.clock.now();
        let mut recomposed = false;
        for index in 0..self.slots.len() {
            let Some(retry) = self.slots[index].runtime_retry else {
                continue;
            };
            if now_inst < retry.at {
                continue;
            }
            let profile = self.slots[index].profile.clone();
            let mut slot = compose_slot(&mut self.composer, profile.clone(), now);
            self.finish_late_slot(&mut slot);
            match slot {
                // Running again, or parked on a missing root, which has
                // its own way back.
                slot if slot.failed.is_none() || slot.missing_root.is_some() => {
                    logging::info(
                        "Composed a suspended profile again",
                        &[
                            ("profile_id", profile.id.clone()),
                            ("attempt", (retry.attempts + 1).to_string()),
                        ],
                    );
                    self.timeline.push(
                        constants::timeline::KIND_PROFILE,
                        profile.id.clone(),
                        "profile composed again after a runtime failure",
                        now,
                    );
                    let attempts = retry.attempts + 1;
                    self.slots[index] = slot;
                    self.slots[index].relapse = Some(Relapse {
                        attempts,
                        forgiven_at: now_inst + suspended_profile_retry_delay(attempts),
                    });
                    recomposed = true;
                }
                still_failing => {
                    let attempts = retry.attempts + 1;
                    let wait = suspended_profile_retry_delay(attempts);
                    if let Some(reason) = still_failing.failed {
                        self.slots[index].failed = Some(reason);
                    }
                    self.slots[index].runtime_retry = Some(RuntimeRetry {
                        attempts,
                        at: now_inst + wait,
                    });
                    logging::warning(
                        "A suspended profile could not be composed again",
                        &[
                            ("profile_id", profile.id.clone()),
                            ("attempt", attempts.to_string()),
                            ("next_attempt_in_seconds", wait.as_secs().to_string()),
                        ],
                    );
                }
            }
        }
        if recomposed {
            self.last_published_slot_states.clear();
            if self.watchers_enabled {
                // New watchers for every live profile before the old
                // ones go: the old ones feed the recorder of the
                // runtime that was replaced. The overlap gives a few
                // duplicate events, which coalesce; a gap would lose
                // some.
                let watchers = start_deduplicated_watchers(&mut self.slots, &self.tick_waker);
                self._watchers = watchers;
            }
        }
    }

    /// Brings the profiles that run in step with the set the
    /// configuration names now. A profile with the definition it was
    /// composed from stays as it is, and takes a new name. One the set
    /// no longer names, or names disabled, ends. One that is new is
    /// composed. A profile that runs and whose definition changed keeps
    /// running as it was composed: that change waits for a restart, and
    /// the answer says so.
    fn reshape_profiles(&mut self, set: crate::profiles::ProfileSet, now: SystemTime) -> Reshape {
        self.config_problems = set.problems;
        let wanted: Vec<ResolvedProfile> = set
            .profiles
            .into_iter()
            .filter(|profile| profile.enabled)
            .collect();
        self.profile_order = wanted.iter().map(|profile| profile.id.clone()).collect();
        // A profile that waited for the end of its old runtime is
        // decided again from the set of now.
        self.awaiting_retirement.clear();

        let mut outcome = Reshape::Applied;
        let mut changed = false;
        let mut kept: Vec<ProfileSlot> = Vec::new();
        for mut slot in std::mem::take(&mut self.slots) {
            let target = wanted.iter().find(|profile| profile.id == slot.profile.id);
            match target {
                Some(profile) if same_definition(&slot.profile, profile) => {
                    if slot.profile.name != profile.name {
                        logging::info(
                            "A profile has a new name",
                            &[
                                ("profile_id", profile.id.clone()),
                                ("name", profile.name.clone()),
                            ],
                        );
                        slot.profile.name = profile.name.clone();
                        changed = true;
                    }
                    kept.push(slot);
                }
                // A stand-in for a profile that was never composed
                // holds no state and no work: the profile is composed
                // from what the file says now.
                Some(_) if slot.failed_at_composition => {
                    drop(slot);
                    changed = true;
                }
                Some(_) => {
                    outcome = Reshape::NeedsRestart;
                    kept.push(slot);
                }
                None => {
                    self.retire_slot(slot, now);
                    changed = true;
                }
            }
        }
        self.slots = kept;
        let mut composed = false;
        for profile in wanted {
            if self.slots.iter().any(|slot| slot.profile.id == profile.id) {
                continue;
            }
            if self.retiring.iter().any(|(id, _)| *id == profile.id) {
                self.awaiting_retirement.push(profile);
                continue;
            }
            self.compose_late(profile, now);
            composed = true;
        }
        if changed || composed {
            self.after_reshape();
        }
        outcome
    }

    /// Composes a profile that joins while the daemon runs.
    fn compose_late(&mut self, profile: ResolvedProfile, now: SystemTime) {
        logging::info(
            "Composing a profile from the new configuration",
            &[("profile_id", profile.id.clone())],
        );
        self.timeline.push(
            constants::timeline::KIND_PROFILE,
            profile.id.clone(),
            "profile composed from the new configuration",
            now,
        );
        let mut slot = compose_slot(&mut self.composer, profile, now);
        self.finish_late_slot(&mut slot);
        self.slots.push(slot);
    }

    /// What every change of the set of slots needs: the order of the
    /// configuration, watchers for the profiles of now, and a status
    /// that is published again.
    fn after_reshape(&mut self) {
        let order = &self.profile_order;
        self.slots.sort_by_key(|slot| {
            order
                .iter()
                .position(|id| *id == slot.profile.id)
                .unwrap_or(usize::MAX)
        });
        // The filter of a folder that no profile watches any more goes:
        // a profile that comes back builds one from the rules of then.
        let in_use: Vec<Arc<SharedEventPathFilter>> = self
            .slots
            .iter()
            .filter_map(|slot| slot.runtime.shared_path_filter())
            .collect();
        self.composer
            .filters_by_root
            .retain(|_, filter| in_use.iter().any(|used| Arc::ptr_eq(used, filter)));
        if self.watchers_enabled {
            // New watchers before the old ones go, so no event falls in
            // between; the overlap gives duplicates, which coalesce.
            let watchers = start_deduplicated_watchers(&mut self.slots, &self.tick_waker);
            self._watchers = watchers;
        }
        self.last_published_slot_states.clear();
    }

    /// Ends the runtime of a profile that left the configuration. What
    /// its watcher reported goes to the durable queue first, as at a
    /// shutdown, so a profile that is enabled again finds it there.
    fn retire_slot(&mut self, mut slot: ProfileSlot, now: SystemTime) {
        let profile_id = slot.profile.id.clone();
        logging::info(
            "A profile left the configuration or was disabled; ending its runtime",
            &[("profile_id", profile_id.clone())],
        );
        self.timeline.push(
            constants::timeline::KIND_PROFILE,
            profile_id.clone(),
            "profile stopped: it is disabled or no longer in the configuration",
            now,
        );
        if slot.failed.is_none() {
            match catch_unwind(AssertUnwindSafe(|| slot.runtime.flush_for_shutdown(now))) {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => logging::warning(
                    "The last flush of a profile that ends failed; its pending changes wait for its next startup reconcile",
                    &[
                        ("profile_id", profile_id.clone()),
                        ("error", format!("{error:?}")),
                    ],
                ),
                Err(_) => logging::warning(
                    "The last flush of a profile that ends panicked; its pending changes wait for its next startup reconcile",
                    &[("profile_id", profile_id.clone())],
                ),
            }
            let _ = catch_unwind(AssertUnwindSafe(|| slot.runtime.abort_and_release(now)));
        }
        if !self.transfer_workers {
            // No worker thread holds a call of this runtime.
            drop(slot);
            return;
        }
        let waker = self.tick_waker.clone();
        let ended = std::thread::Builder::new()
            .name("vapor-profile-retire".to_string())
            .spawn(move || {
                drop(slot);
                waker.notify();
            });
        match ended {
            Ok(handle) => self.retiring.push((profile_id, handle)),
            // The runtime ended on this thread, with the closure.
            Err(error) => logging::warning(
                "Could not start a thread to end a profile; it ended on the tick thread",
                &[("profile_id", profile_id), ("error", error.to_string())],
            ),
        }
    }

    /// Composes each profile that waited for the end of the runtime
    /// that held its id.
    fn compose_after_retirements(&mut self, now: SystemTime) {
        if self.retiring.is_empty() {
            return;
        }
        self.retiring.retain(|(_, handle)| !handle.is_finished());
        let retiring = &self.retiring;
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.awaiting_retirement)
            .into_iter()
            .partition(|profile| !retiring.iter().any(|(id, _)| *id == profile.id));
        self.awaiting_retirement = waiting;
        if ready.is_empty() {
            return;
        }
        for profile in ready {
            self.compose_late(profile, now);
        }
        self.after_reshape();
    }

    /// Starts watching `path` for changes to the configuration the daemon
    /// was composed from. Live keys apply on the tick that notices the
    /// change; the rest are reported as restart-required in status.
    pub fn watch_config(&mut self, path: &Path, applied: vapor_shared::config::VaporConfig) {
        self.config_reloader = Some(crate::config_reload::ConfigReloader::new(path, applied));
    }

    /// Restart-required keys changed since the daemon started.
    pub fn restart_required(&self) -> &[&'static str] {
        &self.restart_required
    }

    fn apply_config_changes(&mut self, now_inst: Instant) {
        let Some(reloader) = self.config_reloader.as_mut() else {
            return;
        };
        let Some((change, config)) = reloader.poll(now_inst) else {
            return;
        };
        use vapor_shared::constants::config as keys;
        self.config_generation += 1;
        let live_keys: Vec<String> = change.live.iter().map(|key| key.to_string()).collect();
        if change.live.iter().any(|key| {
            matches!(
                *key,
                keys::KEY_RESOURCE_LIMITS | keys::KEY_IDLE_BOOST | keys::KEY_PROFILES
            )
        }) || change.restart.contains(&keys::KEY_PROFILES)
        {
            let budget = crate::resource_budget::EffectiveBudgetConfig::resolve(&config);
            let ceiling = budget.concurrent_transfers;
            // A profile composed later starts from the settings that
            // hold now, not from the ones the daemon started with.
            self.composer.concurrent_transfers = ceiling;
            self.shared_budget
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .replace_config(budget);
            for slot in &mut self.slots {
                slot.runtime.set_concurrent_transfers(ceiling);
            }
        }
        if change.live.contains(&keys::KEY_SAFEGUARDS) {
            let settings = crate::safeguards::MassDeleteGuardSettings::resolve(&config);
            self.composer.mass_delete_settings = settings;
            for slot in &mut self.slots {
                slot.runtime.configure_mass_delete_guard(settings);
            }
        }
        if change.live.contains(&keys::KEY_TRASH) {
            let settings = crate::trash::TrashSettings::resolve(&config);
            self.composer.trash_settings = settings;
            for slot in &mut self.slots {
                slot.runtime.configure_trash(settings);
            }
        }
        if change.live.iter().any(|key| {
            matches!(
                *key,
                keys::KEY_USE_GITIGNORE
                    | keys::KEY_USE_VAPORIGNORE
                    | keys::KEY_PRE_IGNORE_RULES
                    | keys::KEY_POST_IGNORE_RULES
            )
        }) {
            let options = EventPathFilterOptions::from_config(&config);
            self.composer.filter_options = options.clone();
            // Every filter a profile holds is in this map, and so is
            // the filter of a profile that waits for its local root: it
            // is composed again with the filter of its folder, which
            // must hold the rules of now.
            for filter in self.composer.filters_by_root.values() {
                filter.replace_options(options.clone());
            }
        }
        if change.live.contains(&keys::KEY_TIMELINE_LIMIT) {
            self.set_timeline_limit(config.timeline_limit);
        }
        if !live_keys.is_empty() {
            logging::info(
                "Applied a live configuration change",
                &[("keys", live_keys.join(","))],
            );
            self.timeline.push(
                constants::timeline::KIND_CONFIG,
                "daemon",
                format!("applied live settings: {}", live_keys.join(", ")),
                self.clock.now_system(),
            );
        }
        // A change of the profile set applies now: a profile that was
        // added or enabled is composed, one that was removed or
        // disabled ends, and a new name is taken. A profile that runs
        // keeps the folders, the provider, and the mode it was composed
        // with until a restart.
        let scope_keys = [
            keys::KEY_PROFILES,
            keys::KEY_LOCAL_ROOT,
            keys::KEY_CLOUD_ROOT,
            keys::KEY_PROVIDER,
            keys::KEY_SYNC_MODE,
        ];
        let mut restart = change.restart;
        if restart.iter().any(|key| scope_keys.contains(key)) {
            let set = crate::profiles::resolve_profile_set_from(&config);
            let now = self.clock.now_system();
            if self.reshape_profiles(set, now) == Reshape::Applied {
                restart.retain(|key| !scope_keys.contains(key));
                // An earlier change that waited for a restart is
                // applied now or was taken back: the profiles that run
                // are the ones the file names.
                self.restart_required
                    .retain(|key| !scope_keys.contains(key));
            }
        }
        for key in restart {
            if !self.restart_required.contains(&key) {
                self.restart_required.push(key);
            }
        }
        if !self.restart_required.is_empty() {
            logging::warning(
                "Configuration keys changed that take effect on the next daemon start",
                &[("keys", self.restart_required.join(","))],
            );
        }
    }

    /// Daemon-wide control channel (IPC): requests are broadcast to
    /// every profile on the next cycle.
    pub fn attach_control(&mut self, control: Arc<RuntimeControl>) {
        control.set_waker(self.tick_waker.clone());
        self.external_control = Some(control);
    }

    pub fn attach_status_publisher(&mut self, publisher: Arc<dyn StatusPublisher>) {
        publisher.publish(self.aggregate_status(self.clock.now_system()));
        self.status_publisher = Some(publisher);
    }

    pub fn profile_count(&self) -> usize {
        self.slots.len()
    }

    pub fn failed_profile_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.failed.is_some())
            .count()
    }

    /// Per-profile view for diagnostics: id, display
    /// name, sync mode, run state, mirror counters, conflicts.
    pub fn profile_summaries(&self) -> Vec<ProfileSummary> {
        self.slots
            .iter()
            .map(|slot| {
                let (mirror_reverts, mirror_deletes) = slot.runtime.mirror_counters();
                ProfileSummary {
                    id: slot.profile.id.clone(),
                    name: slot.profile.name.clone(),
                    provider_kind: slot.profile.provider_kind.clone(),
                    sync_mode: slot.profile.scope.sync_mode,
                    run_state: slot.runtime.app().snapshot().run_state,
                    failed_reason: slot.failed.clone(),
                    mirror_reverts,
                    mirror_deletes,
                    conflicts: slot.runtime.conflict_count(),
                }
            })
            .collect()
    }

    /// Direct access for composition tests.
    pub fn runtime_for(&mut self, profile_id: &str) -> Option<&mut DaemonRuntime> {
        self.slots
            .iter_mut()
            .find(|slot| slot.profile.id == profile_id)
            .map(|slot| &mut slot.runtime)
    }

    /// Purges the trash of every profile that has no runtime to do it:
    /// one that is disabled, one that is suspended, one that left
    /// `vapor.json`. Same cadence and same retention as a running
    /// profile's own purge, on a worker.
    fn purge_other_trash_if_due(&mut self, now: SystemTime) {
        if let Some(purged) = self.other_trash_purge.as_mut().and_then(|call| call.take()) {
            self.other_trash_purge = None;
            match purged {
                Ok(0) => {}
                Ok(purged) => logging::info(
                    "Purged expired trash entries of profiles that do not run",
                    &[("purged", purged.to_string())],
                ),
                Err(panic) => logging::warning(
                    "The trash purge failed; it runs again at the next interval",
                    &[("error", panic)],
                ),
            }
        }
        let settings = self.composer.trash_settings;
        if !settings.enabled || self.other_trash_purge.is_some() {
            return;
        }
        let now_inst = self.clock.now();
        let interval = Duration::from_secs(constants::trash::PURGE_INTERVAL_SECONDS);
        let due = self
            .last_other_trash_purge_inst
            .map(|last| now_inst.saturating_duration_since(last) >= interval)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_other_trash_purge_inst = Some(now_inst);
        let running: Vec<String> = self
            .slots
            .iter()
            .filter(|slot| slot.failed.is_none())
            .map(|slot| slot.profile.id.clone())
            .collect();
        let mode = if self.transfer_workers {
            crate::provider_jobs::ProviderCallMode::Threaded {
                waker: Some(self.tick_waker.clone()),
            }
        } else {
            crate::provider_jobs::ProviderCallMode::Inline
        };
        self.other_trash_purge = Some(crate::provider_jobs::ProviderCall::start(
            &mode,
            "trash-purge",
            crate::trash::purge_other_profiles_job(
                self.composer.trash_directory(),
                running,
                settings.retention,
                now,
            ),
        ));
    }

    /// Ticks every live profile once. Panics and repeated errors
    /// suspend only the offending profile.
    pub fn tick_all(&mut self, now: SystemTime) -> MultiTickReport {
        let control_forwarded = self.forward_external_control();
        self.apply_config_changes(self.clock.now());
        self.compose_after_retirements(now);
        self.retry_missing_roots_if_due(now);
        self.retry_suspended_profiles(now);
        self.purge_other_trash_if_due(now);

        let mut report = MultiTickReport::default();
        let now_inst = self.clock.now();
        for slot in &mut self.slots {
            if slot.failed.is_some() {
                report.failed_profiles += 1;
                continue;
            }
            // The wait a profile starts from when it is suspended now.
            let attempts = slot.relapse.map_or(0, |relapse| relapse.attempts);
            let outcome = catch_unwind(AssertUnwindSafe(|| slot.runtime.tick(now)));
            match outcome {
                Ok(Ok(tick_report)) => {
                    slot.consecutive_tick_errors = 0;
                    if slot
                        .relapse
                        .is_some_and(|relapse| now_inst >= relapse.forgiven_at)
                    {
                        slot.relapse = None;
                    }
                    report.ticked_profiles += 1;
                    report.any_pending_work |= slot.runtime.profile_has_pending_work(&tick_report);
                }
                Ok(Err(error)) => {
                    slot.consecutive_tick_errors += 1;
                    logging::error(
                        "Profile tick failed",
                        &[
                            ("profile_id", slot.profile.id.clone()),
                            ("error", format!("{error:?}")),
                            (
                                "consecutive_failures",
                                slot.consecutive_tick_errors.to_string(),
                            ),
                        ],
                    );
                    if slot.consecutive_tick_errors >= MAX_CONSECUTIVE_PROFILE_TICK_ERRORS {
                        self.timeline.push(
                            constants::timeline::KIND_PROFILE,
                            slot.profile.id.clone(),
                            "profile suspended after repeated tick failures",
                            now,
                        );
                        suspend_profile(slot, format!("repeated tick failures: {error:?}"), now);
                        slot.runtime_retry = Some(RuntimeRetry {
                            attempts,
                            at: now_inst + suspended_profile_retry_delay(attempts),
                        });
                        report.failed_profiles += 1;
                    }
                }
                Err(panic) => {
                    let reason = panic_message(panic);
                    self.timeline.push(
                        constants::timeline::KIND_PROFILE,
                        slot.profile.id.clone(),
                        format!("profile suspended after panic: {reason}"),
                        now,
                    );
                    suspend_profile(slot, format!("panicked: {reason}"), now);
                    slot.runtime_retry = Some(RuntimeRetry {
                        attempts,
                        at: now_inst + suspended_profile_retry_delay(attempts),
                    });
                    report.failed_profiles += 1;
                }
            }
        }

        // A local root that came back, or that the user reattached, is
        // a folder the running watch may not follow: a watch on the
        // folder itself (inotify) stays with the old one. New watchers
        // start before the old ones go, so no event falls in between.
        let mut local_root_changed = false;
        for slot in &mut self.slots {
            local_root_changed |= slot.runtime.take_local_root_changed();
        }
        if local_root_changed && self.watchers_enabled {
            logging::info(
                "A local root is back; starting the filesystem watchers again",
                &[],
            );
            let watchers = start_deduplicated_watchers(&mut self.slots, &self.tick_waker);
            self._watchers = watchers;
        }

        let now_inst = self.clock.now();
        // Publish a fresh status snapshot only when something observable
        // could have changed: building one runs per-profile SQL (depths,
        // failed counts, diagnostics rows), which a fully idle daemon
        // must not pay on every 1 Hz wakeup. Busy ticks (plus one
        // trailing tick so final counts land), control requests,
        // per-slot state flips, and a slow heartbeat for anything the
        // fingerprint misses all mark it stale.
        if let Some(publisher) = self.status_publisher.as_ref() {
            let slot_states: Vec<SlotStateFingerprint> =
                self.slots.iter().map(slot_state_fingerprint).collect();
            let heartbeat_due = self
                .last_status_publish_inst
                .map(|last| {
                    now_inst.saturating_duration_since(last)
                        >= Duration::from_secs(
                            constants::engine::STATUS_REPUBLISH_HEARTBEAT_SECONDS,
                        )
                })
                .unwrap_or(true);
            // Work in flight changes the counts at every tick. Those
            // are published a few times a second; everything else that
            // can change is published at once, and so is the tick
            // after the work ended, where the final counts land.
            let busy_due = self
                .last_status_publish_inst
                .map(|last| {
                    now_inst.saturating_duration_since(last)
                        >= Duration::from_millis(constants::engine::STATUS_BUSY_REPUBLISH_MILLIS)
                })
                .unwrap_or(true);
            let work_ended = self.last_any_pending_work && !report.any_pending_work;
            let stale = (report.any_pending_work && busy_due)
                || work_ended
                || control_forwarded
                || heartbeat_due
                || slot_states != self.last_published_slot_states
                || self.config_generation != self.last_published_config_generation;
            if stale {
                publisher.publish(self.aggregate_status(now));
                self.last_status_publish_inst = Some(now_inst);
                self.last_published_slot_states = slot_states;
                self.last_published_config_generation = self.config_generation;
            }
        }
        self.last_any_pending_work = report.any_pending_work;
        report
    }

    /// Runs the multi-profile tick loop until shutdown. A broken profile
    /// never takes the daemon down, and neither do all of them.
    pub fn run_forever(&mut self) -> Result<(), DaemonRuntimeError> {
        // Let a shutdown signal wake the loop out of its idle sleep so
        // `vapor stop` / SIGTERM exits promptly instead of waiting out a
        // full idle interval.
        crate::runtime::register_shutdown_waker(self.tick_waker.clone());
        let mut reported_all_suspended = false;
        while !is_shutdown_requested() {
            let report = self.tick_all(self.clock.now_system());
            match self.all_suspended() {
                // Every profile is suspended by a runtime failure. Each
                // is composed again with a backoff, so the daemon stays
                // up: an exit would hand a failure that clears on its
                // own (a full disk, a locked database) to the crash-loop
                // guard, which gives up after a few restarts.
                Some(AllSuspended::AtRuntime) if !reported_all_suspended => {
                    reported_all_suspended = true;
                    logging::error(
                        "Every profile is suspended after a runtime failure; each is tried again with a backoff",
                        &[],
                    );
                }
                // Every profile is misconfigured: exiting would only feed
                // the crash-loop guard and hide the reason. Stay up on the
                // idle cadence so `vapor status` can name what to fix.
                Some(AllSuspended::AtComposition) if !reported_all_suspended => {
                    reported_all_suspended = true;
                    logging::warning(
                        "Every profile is suspended by its configuration; serving status \
                         only until the configuration is fixed",
                        &[],
                    );
                }
                // Every profile waits for a sync root that is not there:
                // nothing to fix, nothing to restart. The profiles are
                // composed again when their roots return.
                Some(AllSuspended::WaitingForRoots) if !reported_all_suspended => {
                    reported_all_suspended = true;
                    logging::info(
                        "Every profile is waiting for its local root; serving status until one returns",
                        &[],
                    );
                }
                Some(_) | None => {}
            }
            if reported_all_suspended && self.all_suspended().is_none() {
                reported_all_suspended = false;
            }
            let wait = if report.any_pending_work {
                self.tick_interval
            } else {
                self.idle_tick_interval
            };
            self.tick_waker.wait_timeout(wait);
        }
        let mut flushed = 0usize;
        let now = self.clock.now_system();
        for slot in &mut self.slots {
            if slot.failed.is_some() {
                continue;
            }
            match catch_unwind(AssertUnwindSafe(|| slot.runtime.flush_for_shutdown(now))) {
                Ok(Ok(count)) => flushed += count,
                Ok(Err(error)) => logging::warning(
                    "Shutdown flush failed for a profile; its pending changes wait for the startup reconcile",
                    &[
                        ("profile_id", slot.profile.id.clone()),
                        ("error", format!("{error:?}")),
                    ],
                ),
                Err(_) => logging::warning(
                    "Shutdown flush panicked for a profile; its pending changes wait for the startup reconcile",
                    &[("profile_id", slot.profile.id.clone())],
                ),
            }
        }
        logging::info(
            "Received shutdown signal; exiting multi-profile runtime loop cleanly",
            &[("intents_flushed", flushed.to_string())],
        );
        Ok(())
    }

    /// `Some` when no profile can do any work. Which kind decides whether
    /// the daemon exits (runtime failures) or keeps serving status
    /// (configuration failures); a mix counts as runtime.
    pub fn all_suspended(&self) -> Option<AllSuspended> {
        if self.slots.is_empty() || self.slots.iter().any(|slot| slot.failed.is_none()) {
            return None;
        }
        if self.slots.iter().all(|slot| slot.missing_root.is_some()) {
            Some(AllSuspended::WaitingForRoots)
        } else if self.slots.iter().all(|slot| slot.failed_at_composition) {
            Some(AllSuspended::AtComposition)
        } else {
            Some(AllSuspended::AtRuntime)
        }
    }

    /// Hands pending external control requests on: one with no profile
    /// to every profile, one that names a profile to that profile.
    /// Returns whether any request was forwarded (a status-publish
    /// trigger).
    fn forward_external_control(&mut self) -> bool {
        let Some(external) = self.external_control.as_ref() else {
            return false;
        };
        let pause = external.take_pause_request();
        let reconcile = external.take_reconcile_request();
        let for_profile = external.take_profile_requests();
        if let Some(pause) = pause {
            self.paused_by_user = pause;
            // The whole daemon was just paused or resumed: what was
            // said about single profiles before no longer holds.
            self.paused_profiles.clear();
        }
        for slot in &self.slots {
            match pause {
                Some(true) => slot.control.request_pause(),
                Some(false) => slot.control.request_resume(),
                None => {}
            }
            if reconcile {
                slot.control.request_reconcile();
            }
        }
        for (profile, request) in &for_profile {
            match request {
                ControlRequest::Pause => {
                    self.paused_profiles.insert(profile.clone());
                }
                ControlRequest::Resume => {
                    if self.paused_by_user {
                        // One profile leaves a pause of the whole
                        // daemon: the others stay paused, by name.
                        self.paused_by_user = false;
                        self.paused_profiles = self
                            .slots
                            .iter()
                            .map(|slot| slot.profile.id.clone())
                            .collect();
                    }
                    self.paused_profiles.remove(profile);
                }
                ControlRequest::Reconcile => {}
            }
            if let Some(slot) = self.slots.iter().find(|slot| &slot.profile.id == profile) {
                match request {
                    ControlRequest::Pause => slot.control.request_pause(),
                    ControlRequest::Resume => slot.control.request_resume(),
                    ControlRequest::Reconcile => slot.control.request_reconcile(),
                }
            }
        }
        pause.is_some() || reconcile || !for_profile.is_empty()
    }

    /// One status snapshot for the whole daemon: the most
    /// conservative run state wins, totals aggregate across profiles,
    /// and each profile contributes a status row plus a bounded slice
    /// of per-intent diagnostics.
    fn aggregate_status(&self, now: SystemTime) -> DaemonStatusSnapshot {
        /// Per-response bound on diagnostics rows: recency beats
        /// completeness in a status payload.
        const DIAGNOSTICS_ROW_CAP: usize = 100;

        let mut worst: Option<&ProfileSlot> = None;
        for slot in &self.slots {
            let candidate_rank = run_state_rank(slot_effective_run_state(slot));
            let current_rank = worst
                .map(|w| run_state_rank(slot_effective_run_state(w)))
                .unwrap_or(-1);
            if candidate_rank > current_rank {
                worst = Some(slot);
            }
        }
        let Some(worst) = worst else {
            // No profile: the daemon is up and syncs nothing. Either
            // nothing is set up yet, or the file has something to fix.
            let (run_state, run_reason) = if self.config_problems.is_empty() {
                (
                    RunState::Unconfigured,
                    "no sync profile yet; set Vapor up with `vapor init`".to_string(),
                )
            } else {
                (RunState::Error, self.config_problems.join("; "))
            };
            return DaemonStatusSnapshot {
                run_state: run_state.wire_name().to_string(),
                run_reason,
                throttle_state: ThrottleState::Idle,
                provider: "none".to_string(),
                throttle_reason: "no sync profile to throttle".to_string(),
                ..DaemonStatusSnapshot::default()
            };
        };

        let mut snapshot = DaemonStatusSnapshot::from_app(worst.runtime.app());
        // A suspended or parked profile runs on an inert stand-in that
        // never ticks: name its configured provider, and take the
        // throttle from a profile that is actually running.
        snapshot.provider = worst.profile.provider_kind.clone();
        if let Some(live) = self.slots.iter().find(|slot| slot.failed.is_none()) {
            let live_snapshot = DaemonStatusSnapshot::from_app(live.runtime.app());
            snapshot.throttle_state = live_snapshot.throttle_state;
            snapshot.throttle_reason = live_snapshot.throttle_reason;
        }
        snapshot.restart_required = (!self.restart_required.is_empty()).then(|| {
            format!(
                "{} changed; restart the daemon to apply (vapor service restart)",
                self.restart_required.join(", ")
            )
        });
        snapshot.run_state = slot_effective_run_state(worst).wire_name().to_string();
        snapshot.resources = self
            .slots
            .iter()
            .find_map(|slot| slot.runtime.resources_status());
        if self.slots.len() > 1 {
            let live = self.slots.iter().filter(|s| s.failed.is_none()).count();
            snapshot.throttle_reason = format!(
                "{} of {} profiles active; {}",
                live,
                self.slots.len(),
                snapshot.throttle_reason
            );
        }

        let per_profile_cap = (DIAGNOSTICS_ROW_CAP / self.slots.len().max(1)).max(10);
        let mut daemon_reconcile = crate::runtime::ReconcileStatus::Idle;
        for slot in &self.slots {
            // A file that keeps failing is not work in progress: it is
            // counted apart, and named with its reason.
            let blocked = slot.runtime.state_db().blocked_depth().unwrap_or(0) as u64;
            let queued =
                (slot.runtime.state_db().queue_depth().unwrap_or(0) as u64).saturating_sub(blocked);
            let failed = slot.runtime.state_db().failed_depth().unwrap_or(0) as u64;
            let room = constants::state::STATUS_BLOCKED_FILES_MAX
                .saturating_sub(snapshot.blocked_files.len());
            if blocked > 0 && room > 0 {
                let rows = slot
                    .runtime
                    .state_db()
                    .blocked_intents(room)
                    .unwrap_or_default();
                snapshot
                    .blocked_files
                    .extend(rows.into_iter().map(|intent| {
                        vapor_ipc::BlockedFile {
                            profile_id: slot.profile.id.clone(),
                            path: intent.path.to_string_lossy().into_owned(),
                            action: intent.kind.wire_action().to_string(),
                            reason: intent.last_error.unwrap_or_default(),
                            attempts: intent.attempt_count,
                            next_attempt_ms: intent
                                .available_at
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .map(|since| since.as_millis() as u64)
                                .unwrap_or(0),
                        }
                    }));
            }
            let decisions = slot.runtime.state_db().open_decision_count().unwrap_or(0) as u64;
            let (reverts, deletes) = slot.runtime.mirror_counters();
            let reconcile = slot.runtime.reconcile_status();
            let reason = match &slot.failed {
                Some(suspension) => suspension.clone(),
                None => slot.runtime.app().snapshot().reason.clone(),
            };
            snapshot.profiles.push(vapor_ipc::ProfileStatus {
                id: slot.profile.id.clone(),
                name: slot.profile.name.clone(),
                provider: slot.profile.provider_kind.clone(),
                sync_mode: slot.profile.scope.sync_mode.as_config_value().to_string(),
                run: vapor_ipc::StateReason {
                    state: slot_effective_run_state(slot).wire_name().to_string(),
                    reason,
                },
                reconcile: vapor_ipc::ReconcileStatus {
                    state: reconcile.label().to_string(),
                    detail: reconcile.detail(),
                },
                queued,
                failed,
                blocked,
                decisions,
                mirror: vapor_ipc::MirrorCounts { reverts, deletes },
                suspended: slot.failed.is_some(),
                sign_in_required: slot.runtime.sign_in_required(),
                ignored_paths: slot.runtime.ignored_paths(),
            });
            daemon_reconcile = daemon_reconcile.more_active(reconcile);
            snapshot.queued += queued;
            snapshot.failed += failed;
            snapshot.blocked += blocked;
            snapshot.decisions += decisions;
            snapshot.mirror.reverts += reverts;
            snapshot.mirror.deletes += deletes;
            snapshot.suppressed_echoes += slot.runtime.loop_suppression_count();
            snapshot.dropped_events += slot.runtime.dropped_incoming_event_count();

            if snapshot.work_items.len() < DIAGNOSTICS_ROW_CAP {
                let remaining = DIAGNOSTICS_ROW_CAP - snapshot.work_items.len();
                let rows =
                    slot.runtime
                        .work_items(&slot.profile.id, per_profile_cap.min(remaining), now);
                if rows.len() == per_profile_cap.min(remaining) && queued > rows.len() as u64 {
                    snapshot.diagnostics_truncated = true;
                }
                snapshot.work_items.extend(rows);
            } else {
                snapshot.diagnostics_truncated = true;
            }
        }
        snapshot.reconcile = vapor_ipc::ReconcileStatus {
            state: daemon_reconcile.label().to_string(),
            detail: daemon_reconcile.detail(),
        };
        snapshot.run_reason = daemon_run_reason(&snapshot.profiles, &snapshot.run_state)
            .unwrap_or(snapshot.run_reason);
        snapshot
    }
}

/// The reason that explains the daemon-wide run state: a suspended
/// profile names itself first, otherwise the first profile in the
/// daemon-wide state that gives a reason.
fn daemon_run_reason(profiles: &[vapor_ipc::ProfileStatus], state: &str) -> Option<String> {
    profiles
        .iter()
        .find(|profile| profile.suspended && !profile.run.reason.is_empty())
        .or_else(|| {
            profiles
                .iter()
                .find(|profile| profile.run.state == state && !profile.run.reason.is_empty())
        })
        .map(|profile| profile.run.reason.clone())
}

fn slot_effective_run_state(slot: &ProfileSlot) -> RunState {
    if slot.failed.is_some() {
        RunState::Error
    } else {
        slot.runtime.app().snapshot().run_state
    }
}

fn run_state_rank(state: RunState) -> i32 {
    match state {
        RunState::Running => 0,
        RunState::Starting => 1,
        RunState::Paused => 2,
        RunState::Error => 3,
        // A profile is never unconfigured; the daemon with no profile is.
        RunState::Unconfigured => 0,
    }
}

fn suspend_profile(slot: &mut ProfileSlot, reason: String, now: SystemTime) {
    logging::error(
        "Suspending failed profile; other profiles keep running",
        &[
            ("profile_id", slot.profile.id.clone()),
            ("reason", reason.clone()),
        ],
    );
    // Reclaim the shared-workgate permits the suspended runtime holds so
    // healthy profiles are not starved of concurrency for the process
    // lifetime. Best-effort: guarded against a panic in the post-panic
    // suspension path (the runtime may be half-mutated).
    let _ = catch_unwind(AssertUnwindSafe(|| slot.runtime.abort_and_release(now)));
    slot.failed = Some(reason);
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "opaque panic payload".to_string()
    }
}

fn initial_caps(state: ThrottleState) -> ThrottleCaps {
    crate::throttle::ThrottleController::default().caps_for(state)
}

/// One watcher per distinct canonical local root, fanning out to every
/// profile that watches that root. Each watcher adopts the
/// per-root shared filter its profile runtimes already hold, so the
/// callback path and the runtimes' reconcile/remote filtering stay one
/// instance (and reload together).
/// Everything one profile's runtime is composed from, kept so a profile
/// that could not be composed (its local root missing) can be composed
/// again when the root returns, without a daemon restart.
struct SlotComposer {
    state_root: Option<PathBuf>,
    filter_options: EventPathFilterOptions,
    metrics_sampler: Arc<dyn MetricsSampler>,
    clock: SharedClock,
    device_id: String,
    shared_workgate: Arc<Mutex<ThrottleWorkgate>>,
    timeline: Arc<crate::timeline::TimelineBuffer>,
    shared_budget: Arc<Mutex<crate::resource_budget::ResourceBudget>>,
    shared_bandwidth: Arc<crate::bandwidth::TransferBandwidth>,
    idle_notifier: Arc<dyn vapor_platform::IdleNotifier>,
    concurrent_transfers: Option<usize>,
    mass_delete_settings: crate::safeguards::MassDeleteGuardSettings,
    trash_settings: crate::trash::TrashSettings,
    /// One ignore-rule filter per canonical local root: profiles that
    /// watch the same directory share the instance, so an ignore-file
    /// reload observed by the deduplicated watcher reaches every runtime
    /// that filters through it.
    filters_by_root: BTreeMap<PathBuf, Arc<SharedEventPathFilter>>,
}

impl SlotComposer {
    /// The directory that holds one trash per profile.
    fn trash_directory(&self) -> PathBuf {
        match &self.state_root {
            Some(root) => root.join(constants::runtime::TRASH_DIRECTORY_NAME),
            None => vapor_shared::runtime_paths::trash_directory(),
        }
    }

    fn trash_root(&self, profile_id: &str) -> PathBuf {
        self.trash_directory().join(profile_id)
    }

    fn database_path(&self, profile_id: &str) -> PathBuf {
        match &self.state_root {
            Some(root) => root
                .join(constants::runtime::PROFILES_DIRECTORY_NAME)
                .join(profile_id)
                .join(constants::runtime::SQLITE_DATABASE_FILE_NAME),
            None => vapor_shared::runtime_paths::profile_database_path(profile_id),
        }
    }
}

/// Composes one profile's slot. Every profile gets one: a profile that
/// cannot run (its state DB does not open, its scope is not valid)
/// comes back suspended with the reason, and a profile whose local
/// root is missing comes back as a placeholder that waits for the root.
fn compose_slot(
    composer: &mut SlotComposer,
    profile: ResolvedProfile,
    now: SystemTime,
) -> ProfileSlot {
    // A profile that cannot run as it is written is still a profile:
    // it stands in status with its reason, and syncs nothing.
    if let Some(reason) = profile.invalid.clone() {
        logging::warning(
            "Suspending a profile that cannot run as configured",
            &[
                ("profile_id", profile.id.clone()),
                ("reason", reason.clone()),
            ],
        );
        return suspended_placeholder(composer, profile, reason);
    }
    // The roots and the provider are checked before anything of the
    // profile is touched: the first contact with a local root creates
    // and adopts it, and a profile that cannot run must leave no folder
    // behind. Overlapping roots on a filesystem-backed profile feed the
    // engine its own provider writes (and can strict-mirror over
    // content the user never chose).
    if profile.provider_kind.trim() == vapor_shared::constants::provider::FILESYSTEM
        && let Some(local_root) = profile.scope.local_root.as_deref()
        && let Some(reason) = crate::sync_directories::filesystem_roots_overlap(
            local_root,
            profile.scope.cloud_root.as_str(),
        )
    {
        let reason = format!("invalid sync directories: {reason}");
        logging::error(
            "Suspending profile whose local and cloud sync directories overlap",
            &[
                ("profile_id", profile.id.clone()),
                ("reason", reason.clone()),
            ],
        );
        return suspended_placeholder(composer, profile, reason);
    }
    // An invalid provider must NOT fall back to a functioning stub:
    // the stub reports empty enumerations, so a startup reconcile
    // on a pull-only profile would classify the whole local root as
    // local-only and strict-mirror-delete it. The profile is suspended
    // instead: it surfaces in status and performs zero sync work.
    let provider =
        match vapor_providers::select_provider_for_profile(&profile.provider_kind, &profile.id) {
            Ok(provider) => provider,
            Err(error) => {
                let reason = format!(
                    "invalid provider '{}': {}",
                    profile.provider_kind, error.message
                );
                logging::error(
                    "Profile has an invalid provider; suspending it until the config is fixed",
                    &[
                        ("profile_id", profile.id.clone()),
                        ("reason", reason.clone()),
                    ],
                );
                return suspended_placeholder(composer, profile, reason);
            }
        };
    let database_path = composer.database_path(&profile.id);
    // Per-profile startup failures suspend that profile, never the
    // whole daemon (which would stop every healthy profile and feed
    // the crash-loop guard).
    let state_db = match DurableStateDb::open_with_corruption_recovery(&database_path, now) {
        Ok(state_db) => state_db,
        Err(error) => {
            logging::error(
                "Suspending profile whose durable state DB could not be opened",
                &[
                    ("profile_id", profile.id.clone()),
                    ("error", error.to_string()),
                ],
            );
            let reason = format!("the state database cannot be opened: {error}");
            return retried_placeholder(composer, profile, reason);
        }
    };
    let app = DaemonApp::new_with_shared_workgate(
        provider,
        composer.clock.clone(),
        composer.shared_workgate.clone(),
    );
    let mut runtime = match DaemonRuntime::build_with_app(
        profile.scope.clone(),
        composer.filter_options.clone(),
        state_db,
        app,
        composer.metrics_sampler.clone(),
        composer.clock.clone(),
        crate::runtime::Startup::DaemonProfile,
    ) {
        Ok(runtime) => runtime,
        Err(DaemonRuntimeError::LocalRootMissing(root)) => {
            // Never re-created once adopted: the profile waits for
            // the folder (a volume being plugged back in) and is
            // composed again when it returns or when the user asks
            // for it to be re-created.
            logging::warning(
                "Local root is missing; holding the profile until it returns",
                &[
                    ("profile_id", profile.id.clone()),
                    ("path", root.display().to_string()),
                ],
            );
            return missing_root_placeholder(composer, profile, root, now);
        }
        Err(error) => {
            logging::error(
                "Suspending profile that could not be composed",
                &[
                    ("profile_id", profile.id.clone()),
                    ("error", format!("{error:?}")),
                ],
            );
            let reason = format!("the profile could not be composed: {error:?}");
            return retried_placeholder(composer, profile, reason);
        }
    };
    if let Some(built_filter) = runtime.shared_path_filter() {
        let canonical_root = built_filter.watch_root().to_path_buf();
        match composer.filters_by_root.entry(canonical_root) {
            std::collections::btree_map::Entry::Occupied(shared) => {
                runtime.adopt_shared_path_filter(shared.get().clone());
            }
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(built_filter);
            }
        }
    }
    runtime.set_device_id(composer.device_id.as_str());
    runtime.set_profile_id(profile.id.clone());
    runtime.configure_mass_delete_guard(composer.mass_delete_settings);
    let trash_root = composer.trash_root(&profile.id);
    runtime.attach_trash(
        crate::trash::LocalTrash::new(
            &profile.id,
            trash_root,
            composer.trash_settings,
            Arc::new(vapor_platform::NativeTrashBin::for_current_host()),
        )
        .with_sync_root(profile.scope.local_root.clone()),
    );
    runtime.set_concurrent_transfers(composer.concurrent_transfers);
    runtime.attach_timeline(composer.timeline.clone());
    runtime.attach_resource_management(
        composer.shared_budget.clone(),
        composer.shared_bandwidth.clone(),
        composer.idle_notifier.clone(),
    );
    // A profile whose startup reconcile cannot be scheduled starts no
    // watcher; it only surfaces its Error state.
    let mut failed = None;
    if let Err(error) = runtime.schedule_startup_reconcile(now) {
        let reason = format!("could not schedule startup reconcile: {error:?}");
        logging::error(
            "Suspending profile whose startup reconcile could not be scheduled",
            &[
                ("profile_id", profile.id.clone()),
                ("reason", reason.clone()),
            ],
        );
        runtime.set_error_state(reason.clone());
        failed = Some(reason);
    }
    let control = Arc::new(RuntimeControl::new());
    runtime.attach_control(control.clone());
    // The schedule of the startup reconcile is a write to the state
    // database: a failure of it can pass, like one of the open.
    let runtime_retry = failed.as_ref().map(|_| RuntimeRetry {
        attempts: 0,
        at: composer.clock.now() + suspended_profile_retry_delay(0),
    });
    ProfileSlot {
        profile,
        runtime,
        control,
        consecutive_tick_errors: 0,
        failed_at_composition: false,
        failed,
        missing_root: None,
        runtime_retry,
        relapse: None,
    }
}

/// A slot for a profile whose composition failed for a reason that can
/// pass on its own: a state database that cannot be opened (a full
/// disk, a lock held by another process), a runtime that cannot be
/// built. Nothing in the configuration is wrong, so nothing the user
/// changes there would bring it back; the profile is composed again
/// after the waits of a suspended profile, as one that ran and then
/// failed is.
fn retried_placeholder(
    composer: &mut SlotComposer,
    profile: ResolvedProfile,
    reason: String,
) -> ProfileSlot {
    let first_try = composer.clock.now() + suspended_profile_retry_delay(0);
    let mut slot = suspended_placeholder(composer, profile, reason);
    slot.failed_at_composition = false;
    slot.runtime_retry = Some(RuntimeRetry {
        attempts: 0,
        at: first_try,
    });
    slot
}

/// A slot for a profile that cannot be composed: an inert runtime on
/// the stub provider with a state DB in memory. It never ticks; it
/// carries the reason into status.
fn suspended_placeholder(
    composer: &mut SlotComposer,
    profile: ResolvedProfile,
    reason: String,
) -> ProfileSlot {
    let mut runtime = inert_runtime(composer, &profile);
    runtime.set_device_id(composer.device_id.as_str());
    runtime.set_profile_id(profile.id.clone());
    runtime.attach_timeline(composer.timeline.clone());
    runtime.set_error_state(reason.clone());
    let control = Arc::new(RuntimeControl::new());
    runtime.attach_control(control.clone());
    ProfileSlot {
        profile,
        runtime,
        control,
        consecutive_tick_errors: 0,
        failed_at_composition: true,
        failed: Some(reason),
        missing_root: None,
        runtime_retry: None,
        relapse: None,
    }
}

/// A runtime with no roots, the stub provider, and a state DB in
/// memory: it holds a slot's place and does no work.
fn inert_runtime(composer: &SlotComposer, profile: &ResolvedProfile) -> DaemonRuntime {
    let app = DaemonApp::new_with_shared_workgate(
        vapor_providers::suspended_provider(),
        composer.clock.clone(),
        composer.shared_workgate.clone(),
    );
    DaemonRuntime::build_with_app(
        SyncScope {
            local_root: None,
            ..profile.scope.clone()
        },
        composer.filter_options.clone(),
        DurableStateDb::open_in_memory().expect("in-memory state DB"),
        app,
        composer.metrics_sampler.clone(),
        composer.clock.clone(),
        crate::runtime::Startup::DaemonProfile,
    )
    .expect("an inert runtime with no roots always builds")
}

/// A slot for a profile whose adopted local root is not there: an
/// inert runtime on the stub provider that never ticks, a `root-missing`
/// decision in the profile's state DB so the user can ask for the
/// folder to be re-created, and the path the multi runtime watches for.
fn missing_root_placeholder(
    composer: &mut SlotComposer,
    profile: ResolvedProfile,
    root: PathBuf,
    now: SystemTime,
) -> ProfileSlot {
    let reason = format!(
        "local root {} is missing; Vapor waits for it and never re-creates a folder it synced before (answer the root-missing decision to re-create it)",
        root.display()
    );
    let database_path = composer.database_path(&profile.id);
    if let Ok(mut state_db) = DurableStateDb::open(&database_path) {
        match state_db.open_decision(crate::root_identity::MISSING_DECISION_KIND, Some(&root)) {
            Ok(Some(_)) => {}
            Ok(None) => {
                let question = crate::root_identity::missing_question(
                    crate::root_identity::RootSide::Local,
                    &root.display().to_string(),
                );
                let options = [crate::state_db::DecisionOption {
                    key: crate::root_identity::OPTION_RECREATE.to_string(),
                    label: "Re-create the folder empty and sync the cloud into it".to_string(),
                }];
                let evidence = serde_json::json!({
                    "side": "local",
                    "root": root.display().to_string(),
                });
                match state_db.create_decision(
                    crate::root_identity::MISSING_DECISION_KIND,
                    crate::state_db::DecisionScope::Profile,
                    Some(&root),
                    &question,
                    &options,
                    &evidence,
                    now,
                ) {
                    Ok(id) => composer.timeline.push(
                        constants::timeline::KIND_DECISION,
                        profile.id.clone(),
                        format!("Decision {id} (root-missing) opened: {question}"),
                        now,
                    ),
                    Err(error) => logging::warning(
                        "Could not open the root-missing decision",
                        &[("error", error.to_string())],
                    ),
                }
            }
            Err(error) => logging::warning(
                "Could not look up the root-missing decision",
                &[("error", error.to_string())],
            ),
        }
    }
    let state_db = DurableStateDb::open(&database_path).ok();
    let scope = SyncScope {
        local_root: None,
        ..profile.scope.clone()
    };
    let app = DaemonApp::new_with_shared_workgate(
        vapor_providers::suspended_provider(),
        composer.clock.clone(),
        composer.shared_workgate.clone(),
    );
    let mut runtime = match state_db.and_then(|state_db| {
        DaemonRuntime::build_with_app(
            scope,
            composer.filter_options.clone(),
            state_db,
            app,
            composer.metrics_sampler.clone(),
            composer.clock.clone(),
            crate::runtime::Startup::DaemonProfile,
        )
        .ok()
    }) {
        Some(runtime) => runtime,
        // The state DB is unreadable: the slot still parks with its
        // reason.
        None => inert_runtime(composer, &profile),
    };
    runtime.set_device_id(composer.device_id.as_str());
    runtime.set_profile_id(profile.id.clone());
    runtime.attach_timeline(composer.timeline.clone());
    runtime.set_error_state(reason.clone());
    let control = Arc::new(RuntimeControl::new());
    runtime.attach_control(control.clone());
    ProfileSlot {
        profile,
        runtime,
        control,
        consecutive_tick_errors: 0,
        failed_at_composition: true,
        failed: Some(reason),
        missing_root: Some(root),
        runtime_retry: None,
        relapse: None,
    }
}

fn start_deduplicated_watchers(
    slots: &mut [ProfileSlot],
    waker: &Arc<TickWaker>,
) -> Vec<FsEventsWatcher> {
    struct RootGroup {
        recorder: FanOutRecorder,
        filter: Arc<SharedEventPathFilter>,
        profile_ids: Vec<String>,
    }
    let mut by_root: BTreeMap<PathBuf, RootGroup> = BTreeMap::new();
    // (profile_id, reason) for profiles whose watcher could not start: a
    // watcher failure suspends only the affected profiles, never the whole
    // daemon.
    let mut suspend: Vec<(String, String)> = Vec::new();
    for slot in slots.iter() {
        if slot.failed.is_some() {
            continue;
        }
        let Some(root) = slot.profile.scope.local_root.clone() else {
            continue;
        };
        let Some(recorder) = slot.runtime.event_recorder() else {
            continue;
        };
        let Some(path_filter) = slot.runtime.shared_path_filter() else {
            continue;
        };
        let canonical = match normalize_watch_root(root) {
            Ok(canonical) => canonical,
            Err(error) => {
                suspend.push((
                    slot.profile.id.clone(),
                    format!("watch root unavailable: {error:?}"),
                ));
                continue;
            }
        };
        let group = by_root
            .entry(canonical.clone())
            .or_insert_with(|| RootGroup {
                recorder: FanOutRecorder {
                    targets: Vec::new(),
                    waker: waker.clone(),
                },
                filter: path_filter,
                profile_ids: Vec::new(),
            });
        group.recorder.targets.push((canonical, recorder));
        group.profile_ids.push(slot.profile.id.clone());
    }

    let mut watchers = Vec::new();
    for (root, group) in by_root {
        let shared_profiles = group.recorder.targets.len();
        match FsEventsWatcher::start_with_shared_filter(
            root.clone(),
            Arc::new(group.recorder),
            group.filter,
        ) {
            Ok(watcher) => {
                watchers.push(watcher);
                logging::info(
                    "Started deduplicated fs watcher",
                    &[
                        ("watch_root", root.display().to_string()),
                        ("fan_out_profiles", shared_profiles.to_string()),
                    ],
                );
            }
            Err(error) => {
                for profile_id in group.profile_ids {
                    suspend.push((profile_id, format!("fs watcher failed to start: {error:?}")));
                }
            }
        }
    }

    for (profile_id, reason) in suspend {
        if let Some(slot) = slots.iter_mut().find(|slot| slot.profile.id == profile_id) {
            logging::error(
                "Suspending profile whose fs watcher could not start; others keep running",
                &[
                    ("profile_id", profile_id.clone()),
                    ("reason", reason.clone()),
                ],
            );
            slot.runtime.set_error_state(reason.clone());
            slot.failed = Some(reason);
            slot.failed_at_composition = true;
        }
    }
    watchers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Clock as _, ManualClock};
    use crate::metrics::StaticMetricsSampler;
    use crate::profiles::ResolvedProfile;
    use crate::sync_directories::SyncScope;
    use std::path::Path;
    use tempfile::TempDir;
    use vapor_platform::fs_watch::WatchEventKind;
    use vapor_shared::SyncMode;

    struct MultiFixture {
        _temp: TempDir,
        clock: Arc<ManualClock>,
        multi: MultiProfileRuntime,
        cloud_roots: BTreeMap<String, PathBuf>,
        local_roots: BTreeMap<String, PathBuf>,
        feeds: BTreeMap<String, vapor_providers::filesystem::ManualFeedHandle>,
        now_ms: u64,
    }

    fn timestamp_ms(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }

    impl MultiFixture {
        /// Builds a multi-profile runtime over `specs`:
        /// (profile id, sync mode, shares_local_root_with_first).
        fn new(specs: &[(&str, SyncMode, bool)]) -> Self {
            Self::new_after(specs, |_| {})
        }

        /// [`Self::new`], with `prepare` run on the fixture's directory
        /// before the daemon composes its profiles.
        fn new_after(specs: &[(&str, SyncMode, bool)], prepare: impl FnOnce(&Path)) -> Self {
            let temp = TempDir::new().expect("temp dir");
            let clock = Arc::new(ManualClock::at_now());
            prepare(temp.path());

            let shared_local = temp.path().join("local-shared");
            std::fs::create_dir_all(&shared_local).expect("shared local root");

            let mut profiles = Vec::new();
            let mut cloud_roots = BTreeMap::new();
            let mut local_roots = BTreeMap::new();
            let mut feeds = BTreeMap::new();
            for (id, mode, shares_root) in specs {
                let local_root = if *shares_root {
                    shared_local.clone()
                } else {
                    let dedicated = temp.path().join(format!("local-{id}"));
                    std::fs::create_dir_all(&dedicated).expect("local root");
                    dedicated
                };
                let cloud_root = temp.path().join(format!("cloud-{id}"));
                std::fs::create_dir_all(&cloud_root).expect("cloud root");
                let cloud_root =
                    vapor_shared::paths::canonicalize(&cloud_root).expect("canonical cloud");
                cloud_roots.insert(id.to_string(), cloud_root.clone());
                local_roots.insert(
                    id.to_string(),
                    vapor_shared::paths::canonicalize(&local_root).expect("canonical local"),
                );
                profiles.push(ResolvedProfile {
                    id: id.to_string(),
                    name: id.to_string(),
                    provider_kind: "filesystem".to_string(),
                    scope: SyncScope {
                        local_root: Some(local_root),
                        cloud_root: cloud_root.to_string_lossy().into_owned(),
                        sync_mode: *mode,
                    },
                    enabled: true,
                    invalid: None,
                });
            }

            let mut multi = MultiProfileRuntime::start_with_state_root(
                profiles,
                EventPathFilterOptions::default(),
                Arc::new(StaticMetricsSampler::default()),
                clock.clone(),
                "testdev",
                false,
                Some(temp.path().join("state")),
                crate::resource_budget::EffectiveBudgetConfig::resolve(
                    &vapor_shared::config::VaporConfig::default(),
                ),
                crate::safeguards::MassDeleteGuardSettings::default(),
            )
            .expect("multi runtime");

            // Swap each profile's provider feed for a manual handle so
            // remote events are test-driven: rebuild is heavyweight, so
            // instead the fixture drives remote changes through real
            // files + the per-profile provider's native feed being off;
            // remote propagation tests use the feed handles below.
            for (id, mode, _) in specs {
                // A one-way profile is confirmed the way the user
                // confirms it, so it runs.
                if *mode != SyncMode::TwoWay {
                    multi
                        .runtime_for(id)
                        .expect("runtime")
                        .acknowledge_one_way_mode(timestamp_ms(0));
                }
                let cloud = cloud_roots.get(*id).expect("cloud root").clone();
                let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> = Arc::new(
                    vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host(),
                );
                let (provider, feed) =
                    vapor_providers::FilesystemProvider::with_manual_feed(&cloud, caps)
                        .expect("manual provider");
                let runtime = multi.runtime_for(id).expect("runtime");
                runtime.replace_provider_for_testing(Box::new(provider));
                feeds.insert(id.to_string(), feed);
            }

            Self {
                _temp: temp,
                clock,
                multi,
                cloud_roots,
                local_roots,
                feeds,
                now_ms: 0,
            }
        }

        fn tick(&mut self, advance_ms: u64) -> MultiTickReport {
            self.clock.advance(Duration::from_millis(advance_ms));
            self.now_ms += advance_ms;
            self.multi.tick_all(timestamp_ms(self.now_ms))
        }

        fn converge(&mut self, max_ticks: usize) {
            for _ in 0..max_ticks {
                let report = self.tick(6_000);
                if !report.any_pending_work {
                    break;
                }
            }
        }

        fn record_local_event(&mut self, profile_id: &str, path: &Path, kind: WatchEventKind) {
            let observed_at = timestamp_ms(self.now_ms);
            let runtime = self.multi.runtime_for(profile_id).expect("runtime");
            let recorder = runtime.event_recorder().expect("recorder");
            crate::fs_events::FsEventRecording::record_event(
                recorder.as_ref(),
                FsEventRecord {
                    path: path.to_path_buf(),
                    kind: match kind {
                        WatchEventKind::Created => crate::fs_events::FsEventKind::Created,
                        WatchEventKind::Modified => crate::fs_events::FsEventKind::Modified,
                        WatchEventKind::Removed => crate::fs_events::FsEventKind::Removed,
                        _ => crate::fs_events::FsEventKind::Other,
                    },
                    observed_at,
                },
            );
        }
    }

    /// A profile whose adopted local root is gone at start.
    fn parked_profile() -> (TempDir, Arc<ManualClock>, MultiProfileRuntime, PathBuf) {
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let local_root = temp.path().join("local");
        std::fs::create_dir_all(&local_root).expect("local root");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let profile = || ResolvedProfile {
            id: "default".to_string(),
            name: "default".to_string(),
            provider_kind: "filesystem".to_string(),
            scope: SyncScope {
                local_root: Some(local_root.clone()),
                cloud_root: cloud_root.to_string_lossy().into_owned(),
                sync_mode: SyncMode::TwoWay,
            },
            enabled: true,
            invalid: None,
        };
        let start = |clock: Arc<ManualClock>| {
            MultiProfileRuntime::start_with_state_root(
                vec![profile()],
                EventPathFilterOptions::default(),
                Arc::new(StaticMetricsSampler::default()),
                clock,
                "testdev",
                false,
                Some(temp.path().join("state")),
                crate::resource_budget::EffectiveBudgetConfig::resolve(
                    &vapor_shared::config::VaporConfig::default(),
                ),
                crate::safeguards::MassDeleteGuardSettings::default(),
            )
            .expect("multi runtime")
        };
        // First start adopts the root and syncs one file.
        {
            let mut multi = start(clock.clone());
            std::fs::write(local_root.join("kept.txt"), b"kept").expect("seed");
            for _ in 0..30 {
                clock.advance(Duration::from_secs(6));
                clock.advance_system(Duration::from_secs(6));
                let report = multi.tick_all(clock.now_system());
                if !report.any_pending_work && cloud_root.join("kept.txt").exists() {
                    break;
                }
            }
            assert!(cloud_root.join("kept.txt").exists(), "baseline sync");
        }
        // The volume is unplugged before the next start.
        std::fs::rename(&local_root, temp.path().join("parked")).expect("unplug");
        let multi = start(clock.clone());
        assert_eq!(multi.failed_profile_count(), 1);
        let summary = &multi.profile_summaries()[0];
        assert!(
            summary
                .failed_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("is missing")),
            "{summary:?}"
        );
        assert!(
            !local_root.exists(),
            "a once-adopted root is never re-created silently"
        );
        (temp, clock, multi, local_root)
    }

    #[test]
    fn a_parked_profile_is_composed_again_when_its_local_root_returns() {
        let (temp, clock, mut multi, local_root) = parked_profile();
        let db = DurableStateDb::open(temp.path().join("state/profiles/default/vapor.sqlite"))
            .expect("open");
        let decision = db
            .open_decision(
                crate::root_identity::MISSING_DECISION_KIND,
                Some(&local_root),
            )
            .expect("query")
            .expect("a root-missing decision is open while parked");
        assert_eq!(decision.evidence["side"], "local");
        drop(db);

        // The volume comes back, marker and all.
        std::fs::rename(temp.path().join("parked"), &local_root).expect("plug in");
        for _ in 0..4 {
            clock.advance(Duration::from_secs(16));
            clock.advance_system(Duration::from_secs(16));
            multi.tick_all(clock.now_system());
        }
        assert_eq!(
            multi.failed_profile_count(),
            0,
            "the profile is composed again"
        );
        assert_eq!(multi.profile_summaries()[0].run_state, RunState::Running);
        let db = DurableStateDb::open(temp.path().join("state/profiles/default/vapor.sqlite"))
            .expect("open");
        assert!(
            db.open_decision(
                crate::root_identity::MISSING_DECISION_KIND,
                Some(&local_root)
            )
            .expect("query")
            .is_none(),
            "the question is withdrawn"
        );
        // And the profile syncs: a new file reaches the cloud.
        std::fs::write(local_root.join("after.txt"), b"after").expect("write");
        let runtime = multi.runtime_for("default").expect("runtime");
        let recorder = runtime.event_recorder().expect("recorder");
        crate::fs_events::FsEventRecording::record_event(
            recorder.as_ref(),
            FsEventRecord {
                path: vapor_shared::paths::canonicalize(&local_root)
                    .expect("canonical")
                    .join("after.txt"),
                kind: crate::fs_events::FsEventKind::Created,
                observed_at: clock.now_system(),
            },
        );
        for _ in 0..30 {
            clock.advance(Duration::from_secs(6));
            clock.advance_system(Duration::from_secs(6));
            let report = multi.tick_all(clock.now_system());
            if !report.any_pending_work && temp.path().join("cloud/after.txt").exists() {
                break;
            }
        }
        assert!(temp.path().join("cloud/after.txt").exists());
    }

    #[test]
    fn a_parked_profile_recreates_its_local_root_on_request() {
        let (temp, clock, mut multi, local_root) = parked_profile();
        let mut db = DurableStateDb::open(temp.path().join("state/profiles/default/vapor.sqlite"))
            .expect("open");
        let decision = db
            .open_decision(
                crate::root_identity::MISSING_DECISION_KIND,
                Some(&local_root),
            )
            .expect("query")
            .expect("decision");
        db.resolve_decision(
            decision.id,
            crate::root_identity::OPTION_RECREATE,
            clock.now_system(),
        )
        .expect("resolve");
        drop(db);
        for _ in 0..4 {
            clock.advance(Duration::from_secs(16));
            clock.advance_system(Duration::from_secs(16));
            multi.tick_all(clock.now_system());
        }
        assert!(local_root.is_dir(), "re-created on request");
        assert!(
            local_root
                .join(constants::provider::ROOT_MARKER_FILE_NAME)
                .is_file(),
            "and adopted afresh"
        );
        assert_eq!(multi.failed_profile_count(), 0);
        // The cloud fills the empty folder: nothing is deleted anywhere.
        for _ in 0..30 {
            clock.advance(Duration::from_secs(6));
            let report = multi.tick_all(clock.now_system());
            if !report.any_pending_work && local_root.join("kept.txt").exists() {
                break;
            }
        }
        assert_eq!(
            std::fs::read(local_root.join("kept.txt")).expect("downloaded"),
            b"kept"
        );
        assert!(temp.path().join("cloud/kept.txt").exists());
    }

    #[test]
    fn profiles_sync_concurrently_with_isolated_state() {
        // Two profiles with different roots sync in parallel and
        // never cross-pollinate.
        let mut fixture = MultiFixture::new(&[
            ("alpha", SyncMode::TwoWay, false),
            ("beta", SyncMode::TwoWay, false),
        ]);
        fixture.tick(6_000); // baseline

        let alpha_local = fixture.local_roots["alpha"].clone();
        let beta_local = fixture.local_roots["beta"].clone();
        std::fs::write(alpha_local.join("a.txt"), b"alpha payload").expect("seed alpha");
        std::fs::write(beta_local.join("b.txt"), b"beta payload").expect("seed beta");
        fixture.record_local_event("alpha", &alpha_local.join("a.txt"), WatchEventKind::Created);
        fixture.record_local_event("beta", &beta_local.join("b.txt"), WatchEventKind::Created);

        fixture.converge(16);

        assert_eq!(
            std::fs::read(fixture.cloud_roots["alpha"].join("a.txt")).expect("alpha upload"),
            b"alpha payload"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_roots["beta"].join("b.txt")).expect("beta upload"),
            b"beta payload"
        );
        assert!(
            !fixture.cloud_roots["alpha"].join("b.txt").exists(),
            "profiles must not cross-pollinate"
        );
        assert!(!fixture.cloud_roots["beta"].join("a.txt").exists());
    }

    #[test]
    fn shared_local_root_fans_out_to_every_matching_profile() {
        // Two profiles over the same local root, different
        // clouds: one local change reaches both clouds.
        let mut fixture = MultiFixture::new(&[
            ("primary", SyncMode::TwoWay, true),
            ("backup", SyncMode::TwoWay, true),
        ]);
        fixture.tick(6_000); // baseline

        let local = fixture.local_roots["primary"].clone();
        std::fs::write(local.join("shared.txt"), b"fan out").expect("seed");
        // In production the deduplicated watcher fans out; watcher-less
        // tests record into each profile's recorder the way the fan-out
        // recorder would.
        fixture.record_local_event(
            "primary",
            &local.join("shared.txt"),
            WatchEventKind::Created,
        );
        fixture.record_local_event("backup", &local.join("shared.txt"), WatchEventKind::Created);

        fixture.converge(16);

        assert_eq!(
            std::fs::read(fixture.cloud_roots["primary"].join("shared.txt")).expect("primary"),
            b"fan out"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_roots["backup"].join("shared.txt")).expect("backup"),
            b"fan out"
        );
    }

    #[test]
    fn mixed_sync_modes_stay_isolated_per_profile() {
        // A pull-only mirror beside a two-way profile on one
        // daemon; each applies its own mode.
        let mut fixture = MultiFixture::new(&[
            ("normal", SyncMode::TwoWay, false),
            ("mirror", SyncMode::PullOnly, false),
        ]);
        fixture.tick(6_000); // baseline

        // Two-way profile: local edit uploads.
        let normal_local = fixture.local_roots["normal"].clone();
        std::fs::write(normal_local.join("up.txt"), b"goes up").expect("seed");
        fixture.record_local_event(
            "normal",
            &normal_local.join("up.txt"),
            WatchEventKind::Created,
        );

        // Pull-only profile: a local-only file must be removed, never
        // uploaded.
        let mirror_local = fixture.local_roots["mirror"].clone();
        std::fs::write(mirror_local.join("local-only.txt"), b"doomed").expect("seed");
        fixture.record_local_event(
            "mirror",
            &mirror_local.join("local-only.txt"),
            WatchEventKind::Created,
        );

        fixture.converge(20);

        assert!(
            fixture.cloud_roots["normal"].join("up.txt").exists(),
            "two-way profile must upload"
        );
        assert!(
            !fixture.cloud_roots["mirror"]
                .join("local-only.txt")
                .exists(),
            "pull-only profile must never upload"
        );
        assert!(
            !mirror_local.join("local-only.txt").exists(),
            "pull-only mirror removes local-only content"
        );

        let summaries = fixture.multi.profile_summaries();
        let mirror = summaries.iter().find(|s| s.id == "mirror").expect("mirror");
        assert_eq!(mirror.sync_mode, SyncMode::PullOnly);
        assert!(
            mirror.mirror_deletes >= 1,
            "mirror actions must be observable"
        );
        let normal = summaries.iter().find(|s| s.id == "normal").expect("normal");
        assert_eq!(normal.mirror_deletes, 0);
        assert_eq!(normal.mirror_reverts, 0);
    }

    #[test]
    fn overlapping_filesystem_roots_suspend_the_profile_instead_of_syncing_into_itself() {
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());

        // The "cloud" directory nests inside the watched local root: the
        // provider would write into the watched tree and the engine would
        // re-ingest its own output forever.
        let local = temp.path().join("local");
        std::fs::create_dir_all(&local).expect("local root");
        let nested_cloud = local.join("cloud-copy");

        // A healthy disjoint profile alongside, to prove isolation.
        let good_local = temp.path().join("good-local");
        std::fs::create_dir_all(&good_local).expect("good local");
        let good_cloud = temp.path().join("good-cloud");
        std::fs::create_dir_all(&good_cloud).expect("good cloud");

        let profiles = vec![
            ResolvedProfile {
                id: "overlap".to_string(),
                name: "overlap".to_string(),
                provider_kind: "filesystem".to_string(),
                scope: SyncScope {
                    local_root: Some(local.clone()),
                    cloud_root: nested_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::TwoWay,
                },
                enabled: true,
                invalid: None,
            },
            ResolvedProfile {
                id: "good".to_string(),
                name: "good".to_string(),
                provider_kind: "filesystem".to_string(),
                scope: SyncScope {
                    local_root: Some(good_local.clone()),
                    cloud_root: good_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::TwoWay,
                },
                enabled: true,
                invalid: None,
            },
        ];

        let mut multi = MultiProfileRuntime::start_with_state_root(
            profiles,
            EventPathFilterOptions::default(),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            "testdev",
            false,
            Some(temp.path().join("state")),
            crate::resource_budget::EffectiveBudgetConfig::resolve(
                &vapor_shared::config::VaporConfig::default(),
            ),
            crate::safeguards::MassDeleteGuardSettings::default(),
        )
        .expect("multi runtime");

        let statuses = multi.profile_summaries();
        let overlap = statuses
            .iter()
            .find(|status| status.id == "overlap")
            .expect("overlap profile present");
        assert!(
            overlap
                .failed_reason
                .as_deref()
                .is_some_and(|reason: &str| reason.contains("inside")),
            "overlapping roots must suspend with an actionable reason, got {:?}",
            overlap.failed_reason
        );
        let good = statuses
            .iter()
            .find(|status| status.id == "good")
            .expect("good profile present");
        assert!(good.failed_reason.is_none(), "disjoint profile must run");

        // The suspended profile performs zero sync work.
        use crate::clock::Clock as _;
        let report = multi.tick_all(clock.now_system());
        assert_eq!(report.failed_profiles, 1);
        assert_eq!(report.ticked_profiles, 1);
        assert!(
            !nested_cloud.exists(),
            "a suspended overlap profile must not create or write its cloud root"
        );
    }

    #[test]
    fn a_profile_that_cannot_run_creates_no_local_folder() {
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        // Neither local folder exists yet. One profile names a provider
        // that does not exist; the other puts its cloud folder inside
        // its local folder.
        let unknown_local = temp.path().join("unknown-local");
        let nested_local = temp.path().join("nested-local");
        let profile = |id: &str, provider: &str, local: &Path, cloud: PathBuf| ResolvedProfile {
            id: id.to_string(),
            name: id.to_string(),
            provider_kind: provider.to_string(),
            scope: SyncScope {
                local_root: Some(local.to_path_buf()),
                cloud_root: cloud.to_string_lossy().into_owned(),
                sync_mode: SyncMode::TwoWay,
            },
            enabled: true,
            invalid: None,
        };
        let profiles = vec![
            profile(
                "unknown",
                "gdrvie",
                &unknown_local,
                temp.path().join("unknown-cloud"),
            ),
            profile(
                "nested",
                "filesystem",
                &nested_local,
                nested_local.join("cloud"),
            ),
        ];

        let mut multi = MultiProfileRuntime::start_with_state_root(
            profiles,
            EventPathFilterOptions::default(),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            "testdev",
            false,
            Some(temp.path().join("state")),
            crate::resource_budget::EffectiveBudgetConfig::resolve(
                &vapor_shared::config::VaporConfig::default(),
            ),
            crate::safeguards::MassDeleteGuardSettings::default(),
        )
        .expect("multi runtime");

        assert_eq!(multi.failed_profile_count(), 2);
        let mut now_ms = 0;
        for _ in 0..4 {
            now_ms += 6_000;
            clock.advance(Duration::from_millis(6_000));
            multi.tick_all(timestamp_ms(now_ms));
        }
        assert!(
            !unknown_local.exists(),
            "a profile with no provider made its local folder"
        );
        assert!(
            !nested_local.exists(),
            "a profile with overlapping folders made its local folder"
        );
    }

    #[test]
    fn invalid_provider_suspends_the_profile_instead_of_mirror_deleting_its_local_root() {
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());

        // A pull-only profile with a misspelled provider and a local file
        // present. The old stub fallback would enumerate an empty remote
        // and strict-mirror-delete the whole local root; suspension must
        // prevent any sync work.
        let bad_local = temp.path().join("bad-local");
        std::fs::create_dir_all(&bad_local).expect("bad local");
        std::fs::write(bad_local.join("keep.txt"), b"precious").expect("seed");
        let bad_cloud = temp.path().join("bad-cloud");

        // A healthy filesystem profile alongside it, to prove isolation.
        let good_local = temp.path().join("good-local");
        std::fs::create_dir_all(&good_local).expect("good local");
        let good_cloud = temp.path().join("good-cloud");
        std::fs::create_dir_all(&good_cloud).expect("good cloud");

        let profiles = vec![
            ResolvedProfile {
                id: "bad".to_string(),
                name: "bad".to_string(),
                provider_kind: "gdrvie".to_string(),
                scope: SyncScope {
                    local_root: Some(bad_local.clone()),
                    cloud_root: bad_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::PullOnly,
                },
                enabled: true,
                invalid: None,
            },
            ResolvedProfile {
                id: "good".to_string(),
                name: "good".to_string(),
                provider_kind: "filesystem".to_string(),
                scope: SyncScope {
                    local_root: Some(good_local.clone()),
                    cloud_root: good_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::TwoWay,
                },
                enabled: true,
                invalid: None,
            },
        ];

        let mut multi = MultiProfileRuntime::start_with_state_root(
            profiles,
            EventPathFilterOptions::default(),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            "testdev",
            false,
            Some(temp.path().join("state")),
            crate::resource_budget::EffectiveBudgetConfig::resolve(
                &vapor_shared::config::VaporConfig::default(),
            ),
            crate::safeguards::MassDeleteGuardSettings::default(),
        )
        .expect("multi runtime");

        assert_eq!(multi.failed_profile_count(), 1);
        let mut now_ms = 0;
        for _ in 0..8 {
            now_ms += 6_000;
            clock.advance(Duration::from_millis(6_000));
            multi.tick_all(timestamp_ms(now_ms));
        }
        // The suspended profile performed zero sync work: its local file
        // is intact.
        assert!(
            bad_local.join("keep.txt").exists(),
            "suspended profile must not touch its local root"
        );
    }

    /// Provider whose changes-feed poll panics: the inducement
    /// (a bug inside one profile's provider execution).
    struct PanickingProvider;

    impl vapor_providers::Provider for PanickingProvider {
        fn name(&self) -> &'static str {
            "panicking_test_provider"
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            vapor_providers::ProviderCapabilities::FILESYSTEM
        }
        fn ensure_cloud_root(
            &self,
            _cloud_root: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            Ok(())
        }
        fn enumerate(
            &self,
            _directory: &vapor_providers::RemotePath,
        ) -> Result<Vec<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            panic!("provider bug: enumerate exploded")
        }
        fn stat(
            &self,
            _path: &vapor_providers::RemotePath,
        ) -> Result<Option<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            panic!("provider bug: stat exploded")
        }
        fn content_hash(
            &self,
            _path: &vapor_providers::RemotePath,
        ) -> Result<String, vapor_providers::ProviderError> {
            panic!("provider bug: content_hash exploded")
        }
        fn begin_upload(
            &self,
            _request: vapor_providers::UploadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            panic!("provider bug: begin_upload exploded")
        }
        fn begin_download(
            &self,
            _request: vapor_providers::DownloadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            panic!("provider bug: begin_download exploded")
        }
        fn delete(
            &self,
            _path: &vapor_providers::RemotePath,
            _op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            panic!("provider bug: delete exploded")
        }
        fn poll_changes(
            &self,
            _cursor: Option<&str>,
            _max_changes: usize,
        ) -> Result<vapor_providers::ChangesPoll, vapor_providers::ProviderError> {
            panic!("provider bug: poll_changes exploded")
        }
    }

    /// A daemon composed from the configuration at `config_path`, the
    /// way the bootstrap composes it.
    fn daemon_from_config(
        temp: &TempDir,
        config_path: &Path,
        clock: Arc<ManualClock>,
    ) -> MultiProfileRuntime {
        let load = vapor_shared::config::load_from(config_path);
        let set = crate::profiles::resolve_profile_set(&load);
        let mut multi = MultiProfileRuntime::start_with_state_root(
            set.profiles,
            EventPathFilterOptions::default(),
            Arc::new(StaticMetricsSampler::default()),
            clock,
            "testdev",
            false,
            Some(temp.path().join("state")),
            crate::resource_budget::EffectiveBudgetConfig::resolve(&load.config),
            crate::safeguards::MassDeleteGuardSettings::default(),
        )
        .expect("multi runtime");
        multi.set_config_problems(set.problems);
        multi.watch_config(config_path, load.config);
        multi
    }

    /// Rewrites the configuration with a modification time the
    /// reloader sees as new.
    fn rewrite_config(config_path: &Path, contents: &str, seconds_later: u64) {
        std::fs::write(config_path, contents).expect("edit config");
        let later = SystemTime::now() + Duration::from_secs(seconds_later);
        std::fs::File::options()
            .write(true)
            .open(config_path)
            .and_then(|file| file.set_modified(later))
            .expect("advance mtime");
    }

    #[test]
    fn an_install_with_no_sync_scope_syncs_nothing_creates_nothing_and_says_so() {
        use crate::clock::Clock as _;
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let config_path = temp.path().join("vapor.json");
        // No file at all, then a file that sets everything but a scope.
        for contents in [
            None,
            Some("{\"provider\": \"filesystem\", \"syncMode\": \"two-way\"}\n"),
        ] {
            if let Some(contents) = contents {
                std::fs::write(&config_path, contents).expect("seed config");
            }
            let mut multi = daemon_from_config(&temp, &config_path, clock.clone());
            multi.tick_all(clock.now_system());
            assert_eq!(multi.failed_profile_count(), 0);
            assert_eq!(
                multi.all_suspended(),
                None,
                "nothing failed; nothing is set up"
            );
            let status = multi.aggregate_status(clock.now_system());
            assert_eq!(status.run_state, constants::run_state::UNCONFIGURED);
            assert!(
                status.run_reason.contains("vapor init"),
                "status names the command that sets a profile up: {:?}",
                status.run_reason
            );
            assert!(status.profiles.is_empty());
        }
        let created: Vec<_> = std::fs::read_dir(temp.path())
            .expect("list")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "vapor.json")
            .collect();
        assert!(
            created.is_empty(),
            "no folder, no state, no marker for a scope nobody chose: {created:?}"
        );
    }

    #[test]
    fn a_configuration_that_cannot_be_read_runs_no_profile_and_names_the_problem() {
        use crate::clock::Clock as _;
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let config_path = temp.path().join("vapor.json");
        std::fs::write(&config_path, "{ \"localRoot\": ").expect("seed broken config");

        let mut multi = daemon_from_config(&temp, &config_path, clock.clone());
        multi.tick_all(clock.now_system());
        let status = multi.aggregate_status(clock.now_system());
        assert_eq!(status.run_state, constants::run_state::ERROR);
        assert!(
            status.run_reason.starts_with("vapor.json cannot be read"),
            "{:?}",
            status.run_reason
        );
        assert!(
            status.profiles.is_empty(),
            "the default profile must not run in its place"
        );
    }

    #[test]
    fn a_profile_that_cannot_run_as_written_is_listed_suspended_with_its_reason() {
        use crate::clock::Clock as _;
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        let config_path = temp.path().join("vapor.json");
        std::fs::write(
            &config_path,
            serde_json::json!({
                "profiles": [
                    { "id": "typo", "localRoot": local, "cloudRoot": cloud,
                      "syncMode": "pull_only" },
                    { "id": "half", "localRoot": temp.path().join("other") },
                ]
            })
            .to_string(),
        )
        .expect("seed config");

        let mut multi = daemon_from_config(&temp, &config_path, clock.clone());
        multi.tick_all(clock.now_system());
        assert_eq!(multi.failed_profile_count(), 2);
        let status = multi.aggregate_status(clock.now_system());
        let reason_of = |id: &str| {
            let row = status
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .unwrap_or_else(|| panic!("profile {id} is listed"));
            assert!(row.suspended, "{id}");
            row.run.reason.clone()
        };
        assert!(
            reason_of("typo").contains("\"pull_only\""),
            "{}",
            reason_of("typo")
        );
        assert_eq!(reason_of("half"), "`cloudRoot` is not set");
        assert!(
            !local.exists() && !cloud.exists(),
            "a profile that cannot run creates no folder"
        );
    }

    #[test]
    fn the_first_sync_scope_composes_without_a_restart() {
        use crate::clock::Clock as _;
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let local = temp.path().join("local");
        let config_path = temp.path().join("vapor.json");
        std::fs::write(&config_path, "{}\n").expect("seed config");
        let mut multi = daemon_from_config(&temp, &config_path, clock.clone());
        multi.tick_all(clock.now_system());
        assert_eq!(
            multi.aggregate_status(clock.now_system()).run_state,
            constants::run_state::UNCONFIGURED
        );

        // One root: a profile that names what is missing, still no folder.
        rewrite_config(
            &config_path,
            &serde_json::json!({ "localRoot": local }).to_string(),
            5,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        let status = multi.aggregate_status(clock.now_system());
        assert_eq!(status.profiles.len(), 1);
        assert_eq!(status.profiles[0].run.reason, "`cloudRoot` is not set");
        assert!(!local.exists());
        assert!(multi.restart_required().is_empty());

        // The second root, one `config set` later: the profile runs,
        // still with no restart.
        let cloud = temp.path().join("cloud");
        rewrite_config(
            &config_path,
            &serde_json::json!({ "localRoot": local, "cloudRoot": cloud }).to_string(),
            10,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(multi.failed_profile_count(), 0);
        assert!(multi.restart_required().is_empty());
        assert!(local.is_dir(), "the profile adopted its local root");

        // A profile that runs is reshaped only by a restart.
        rewrite_config(
            &config_path,
            &serde_json::json!({ "localRoot": local, "cloudRoot": temp.path().join("other") })
                .to_string(),
            15,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(multi.restart_required(), &["cloudRoot"]);
    }

    #[test]
    fn both_roots_set_on_an_unconfigured_daemon_start_the_profile() {
        use crate::clock::Clock as _;
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        let config_path = temp.path().join("vapor.json");
        let mut multi = daemon_from_config(&temp, &config_path, clock.clone());
        multi.tick_all(clock.now_system());

        rewrite_config(
            &config_path,
            &serde_json::json!({ "localRoot": local, "cloudRoot": cloud }).to_string(),
            5,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(multi.failed_profile_count(), 0);
        assert!(
            multi.restart_required().is_empty(),
            "the first scope needs no restart"
        );
        assert!(local.is_dir(), "the profile adopted its local root");
        std::fs::write(local.join("first.txt"), b"hello").expect("seed");
        for _ in 0..40 {
            clock.advance(Duration::from_secs(6));
            clock.advance_system(Duration::from_secs(6));
            multi.tick_all(clock.now_system());
            if cloud.join("first.txt").exists() {
                break;
            }
        }
        assert!(
            cloud.join("first.txt").exists(),
            "the startup scan of the new profile uploads what is there"
        );
    }

    /// Ticks until `done` holds, with the clock moved between ticks.
    fn tick_until(
        multi: &mut MultiProfileRuntime,
        clock: &Arc<ManualClock>,
        mut done: impl FnMut() -> bool,
    ) -> bool {
        use crate::clock::Clock as _;
        for _ in 0..40 {
            if done() {
                return true;
            }
            clock.advance(Duration::from_secs(6));
            clock.advance_system(Duration::from_secs(6));
            multi.tick_all(clock.now_system());
        }
        done()
    }

    #[test]
    fn a_change_of_the_profile_set_applies_to_a_daemon_that_runs() {
        use crate::clock::Clock as _;
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let config_path = temp.path().join("vapor.json");
        let root = |name: &str| temp.path().join(name);
        let profile = |id: &str, enabled: bool, name: Option<&str>| {
            let mut entry = serde_json::json!({
                "id": id,
                "localRoot": root(&format!("{id}-local")),
                "cloudRoot": root(&format!("{id}-cloud")),
                "enabled": enabled,
            });
            if let Some(name) = name {
                entry["name"] = serde_json::json!(name);
            }
            entry
        };
        let write = |profiles: Vec<serde_json::Value>, seconds_later: u64| {
            rewrite_config(
                &config_path,
                &serde_json::json!({ "profiles": profiles }).to_string(),
                seconds_later,
            );
        };
        std::fs::write(
            &config_path,
            serde_json::json!({ "profiles": [profile("home", true, None)] }).to_string(),
        )
        .expect("seed config");
        let mut multi = daemon_from_config(&temp, &config_path, clock.clone());
        // The test starts no watcher: a scan on request finds a change.
        let control = Arc::new(RuntimeControl::new());
        multi.attach_control(control.clone());
        std::fs::create_dir_all(root("home-local")).expect("local root");
        std::fs::write(root("home-local").join("a.txt"), b"a").expect("seed");
        assert!(
            tick_until(&mut multi, &clock, || root("home-cloud")
                .join("a.txt")
                .exists()),
            "the first profile syncs"
        );
        let ids = |multi: &MultiProfileRuntime| -> Vec<String> {
            multi
                .profile_summaries()
                .into_iter()
                .map(|profile| profile.id)
                .collect()
        };

        // A second profile, written before the first one: it starts
        // with no restart, and status lists it where the file has it.
        write(
            vec![profile("work", true, None), profile("home", true, None)],
            5,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(ids(&multi), ["work", "home"]);
        assert!(multi.restart_required().is_empty());
        std::fs::write(root("work-local").join("w.txt"), b"w").expect("seed");
        assert!(
            tick_until(&mut multi, &clock, || root("work-cloud")
                .join("w.txt")
                .exists()),
            "the profile that joined syncs"
        );

        // Disabled: its runtime ends, and nothing of it syncs. The
        // other profile goes on.
        write(
            vec![profile("work", false, None), profile("home", true, None)],
            10,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(ids(&multi), ["home"]);
        assert!(multi.restart_required().is_empty());
        std::fs::write(root("work-local").join("while-off.txt"), b"x").expect("seed");
        std::fs::write(root("home-local").join("b.txt"), b"b").expect("seed");
        control.request_reconcile();
        assert!(
            tick_until(&mut multi, &clock, || root("home-cloud")
                .join("b.txt")
                .exists()),
            "the profile that stays keeps syncing"
        );
        assert!(
            !root("work-cloud").join("while-off.txt").exists(),
            "a disabled profile syncs nothing"
        );

        // Enabled again, with a new name: it is composed on the state
        // it left, and its scan finds what changed meanwhile.
        write(
            vec![
                profile("work", true, Some("Work files")),
                profile("home", true, None),
            ],
            15,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(ids(&multi), ["work", "home"]);
        assert!(multi.restart_required().is_empty());
        assert!(
            tick_until(&mut multi, &clock, || root("work-cloud")
                .join("while-off.txt")
                .exists()),
            "the profile that came back syncs what it missed"
        );

        // A new name alone: taken at once, and the runtime is the same
        // one (a profile composed again would scan its folders again).
        let scans_before = multi
            .runtime_for("home")
            .expect("home runs")
            .reconcile_status();
        write(
            vec![
                profile("work", true, Some("Work files")),
                profile("home", true, Some("Home")),
            ],
            20,
        );
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        let names: Vec<String> = multi
            .profile_summaries()
            .into_iter()
            .map(|profile| profile.name)
            .collect();
        assert_eq!(names, ["Work files", "Home"]);
        assert!(multi.restart_required().is_empty());
        assert_eq!(
            multi
                .runtime_for("home")
                .expect("home runs")
                .reconcile_status(),
            scans_before,
            "a new name starts no scan"
        );

        // Taken out of the file: the same as disabled.
        write(vec![profile("home", true, Some("Home"))], 25);
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(ids(&multi), ["home"]);
        assert!(multi.restart_required().is_empty());
    }

    #[test]
    fn a_profile_that_runs_keeps_its_definition_until_a_restart_and_a_change_taken_back_clears_it()
    {
        use crate::clock::Clock as _;
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let config_path = temp.path().join("vapor.json");
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        let with_cloud = |cloud: &Path| {
            serde_json::json!({ "profiles": [
                { "id": "home", "localRoot": local, "cloudRoot": cloud },
            ] })
            .to_string()
        };
        std::fs::write(&config_path, with_cloud(&cloud)).expect("seed config");
        let mut multi = daemon_from_config(&temp, &config_path, clock.clone());
        multi.tick_all(clock.now_system());
        assert_eq!(multi.failed_profile_count(), 0);

        let other = temp.path().join("other");
        rewrite_config(&config_path, &with_cloud(&other), 5);
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert_eq!(multi.restart_required(), &["profiles"]);
        assert!(
            !other.exists(),
            "the profile still runs on the folder it was composed with"
        );

        rewrite_config(&config_path, &with_cloud(&cloud), 10);
        clock.advance(Duration::from_secs(2));
        multi.tick_all(clock.now_system());
        assert!(
            multi.restart_required().is_empty(),
            "the file names what runs again: nothing waits for a restart"
        );
    }

    #[test]
    fn a_live_config_change_applies_mid_work_and_a_restart_key_is_reported() {
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        std::fs::create_dir_all(&local).expect("local");
        std::fs::create_dir_all(&cloud).expect("cloud");
        let config_path = temp.path().join("vapor.json");
        let local = vapor_shared::paths::canonicalize(&local).expect("canonical");
        let roots = serde_json::json!({ "localRoot": local, "cloudRoot": cloud });
        std::fs::write(&config_path, roots.to_string()).expect("seed config");
        // Work in flight across the reload: a durable upload queued
        // before the daemon starts.
        let keep = local.join("keep.txt");
        std::fs::write(&keep, b"survives a reload").expect("seed file");
        {
            let state_root = temp.path().join("state");
            let mut db = DurableStateDb::open(
                state_root
                    .join("profiles")
                    .join("default")
                    .join(constants::runtime::SQLITE_DATABASE_FILE_NAME),
            )
            .expect("open profile db");
            db.enqueue_intent(
                &keep,
                crate::event_intents::PendingIntentKind::Upload,
                SystemTime::now(),
            )
            .expect("enqueue");
        }

        let profiles = vec![ResolvedProfile {
            id: "default".to_string(),
            name: "default".to_string(),
            provider_kind: "filesystem".to_string(),
            scope: SyncScope {
                local_root: Some(local.clone()),
                cloud_root: cloud.to_string_lossy().into_owned(),
                sync_mode: SyncMode::TwoWay,
            },
            enabled: true,
            invalid: None,
        }];
        let applied = vapor_shared::config::load_from(&config_path).config;
        let mut multi = MultiProfileRuntime::start_with_state_root(
            profiles,
            EventPathFilterOptions::default(),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            "testdev",
            false,
            Some(temp.path().join("state")),
            crate::resource_budget::EffectiveBudgetConfig::resolve(&applied),
            crate::safeguards::MassDeleteGuardSettings::default(),
        )
        .expect("multi runtime");
        multi.watch_config(&config_path, applied);
        use crate::clock::Clock as _;
        multi.tick_all(clock.now_system());

        // A live key: the ceiling drops without touching the queue.
        let mut edited = roots.clone();
        edited["resourceLimits"] = serde_json::json!({ "cpuPercent": 7, "concurrentTransfers": 3 });
        edited["postIgnoreRules"] = serde_json::json!(["*.late"]);
        edited["provider"] = serde_json::json!("gdrive");
        std::fs::write(&config_path, edited.to_string()).expect("edit config");
        let later = std::fs::metadata(&config_path)
            .expect("metadata")
            .modified()
            .expect("mtime")
            + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&config_path)
            .expect("open")
            .set_modified(later)
            .expect("set mtime");
        clock.advance(Duration::from_secs(2));
        clock.advance_system(Duration::from_secs(2));
        multi.tick_all(clock.now_system());

        assert_eq!(
            multi
                .shared_budget
                .lock()
                .expect("budget")
                .config()
                .cpu_percent,
            7,
            "resourceLimits applies live"
        );
        // A profile composed after this change starts from the settings
        // that hold now.
        assert_eq!(multi.composer.concurrent_transfers, Some(3));
        assert_eq!(
            multi.composer.filter_options.post_user_rules,
            vec!["*.late".to_string()]
        );
        assert_eq!(multi.restart_required(), &["provider"]);
        let status = multi.aggregate_status(clock.now_system());
        assert!(
            status
                .restart_required
                .as_deref()
                .is_some_and(|notice| notice.contains("provider") && notice.contains("restart")),
            "{:?}",
            status.restart_required
        );
        // The queued upload either finished or is still queued; a reload
        // never fails or drops it.
        for _ in 0..40 {
            clock.advance(Duration::from_millis(250));
            clock.advance_system(Duration::from_millis(250));
            multi.tick_all(clock.now_system());
            if cloud.join("keep.txt").exists() {
                break;
            }
        }
        let db = multi.runtime_for("default").expect("runtime").state_db();
        assert_eq!(db.failed_depth().expect("failed"), 0);
        assert!(
            cloud.join("keep.txt").exists() || db.queue_depth().expect("depth") >= 1,
            "a reload never drops queued intents"
        );
    }

    #[test]
    fn a_daemon_whose_only_profile_is_misconfigured_keeps_serving_status() {
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let local = temp.path().join("local");
        std::fs::create_dir_all(&local).expect("local");
        let profiles = vec![ResolvedProfile {
            id: "only".to_string(),
            name: "only".to_string(),
            provider_kind: "gdrvie".to_string(),
            scope: SyncScope {
                local_root: Some(local),
                cloud_root: "/Vapor".to_string(),
                sync_mode: SyncMode::TwoWay,
            },
            enabled: true,
            invalid: None,
        }];
        let mut multi = MultiProfileRuntime::start_with_state_root(
            profiles,
            EventPathFilterOptions::default(),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            "testdev",
            false,
            Some(temp.path().join("state")),
            crate::resource_budget::EffectiveBudgetConfig::resolve(
                &vapor_shared::config::VaporConfig::default(),
            ),
            crate::safeguards::MassDeleteGuardSettings::default(),
        )
        .expect("multi runtime");
        assert_eq!(multi.failed_profile_count(), 1);
        // Ticking is harmless and the loop must not exit: the user needs
        // `vapor status` to name the reason.
        use crate::clock::Clock as _;
        multi.tick_all(clock.now_system());
        assert_eq!(multi.all_suspended(), Some(AllSuspended::AtComposition));
        let snapshot = multi.aggregate_status(clock.now_system());
        let only = snapshot
            .profiles
            .iter()
            .find(|profile| profile.id == "only")
            .expect("profile row");
        assert!(only.suspended);
        assert!(
            only.run.reason.contains("gdrvie"),
            "reason must name the bad provider: {:?}",
            only.run.reason
        );
        assert!(
            snapshot.run_reason.contains("gdrvie"),
            "the daemon-wide reason names the suspension: {:?}",
            snapshot.run_reason
        );
    }

    #[test]
    fn a_failing_profile_suspends_alone_and_the_rest_keep_running() {
        // Blast radius: one profile's provider panicking must not
        // stop the healthy profile (or poison the shared workgate).
        let mut fixture = MultiFixture::new(&[
            ("healthy", SyncMode::TwoWay, false),
            ("doomed", SyncMode::TwoWay, false),
        ]);
        fixture.tick(6_000);

        {
            let doomed = fixture.multi.runtime_for("doomed").expect("runtime");
            doomed.replace_provider_for_testing(Box::new(PanickingProvider));
        }

        // The next remote poll panics inside the doomed profile's tick;
        // the catcher suspends exactly that profile.
        fixture.tick(6_000);
        assert_eq!(fixture.multi.failed_profile_count(), 1);
        assert_eq!(
            fixture.multi.all_suspended(),
            None,
            "a healthy profile keeps the daemon alive"
        );

        // The healthy profile still syncs.
        let healthy_local = fixture.local_roots["healthy"].clone();
        std::fs::write(healthy_local.join("still-works.txt"), b"alive").expect("seed");
        fixture.record_local_event(
            "healthy",
            &healthy_local.join("still-works.txt"),
            WatchEventKind::Created,
        );
        fixture.converge(16);
        assert!(
            fixture.cloud_roots["healthy"]
                .join("still-works.txt")
                .exists(),
            "the healthy profile must keep syncing after a sibling fails"
        );

        let summaries = fixture.multi.profile_summaries();
        let doomed = summaries.iter().find(|s| s.id == "doomed").expect("doomed");
        assert!(doomed.failed_reason.is_some());
    }

    #[test]
    fn a_profile_that_the_runtime_suspended_is_composed_again_after_its_wait() {
        let mut fixture = MultiFixture::new(&[("only", SyncMode::TwoWay, false)]);
        fixture.tick(6_000);
        fixture
            .multi
            .runtime_for("only")
            .expect("runtime")
            .replace_provider_for_testing(Box::new(PanickingProvider));
        fixture.tick(6_000);
        assert_eq!(fixture.multi.failed_profile_count(), 1);
        assert_eq!(
            fixture.multi.all_suspended(),
            Some(AllSuspended::AtRuntime),
            "the daemon stays up with its one profile suspended"
        );

        // Inside the first wait the profile stays suspended.
        fixture.tick(30_000);
        assert_eq!(fixture.multi.failed_profile_count(), 1);

        // The wait is over: the profile is composed again from its
        // configuration and syncs.
        fixture.tick(31_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0);
        assert_eq!(fixture.multi.all_suspended(), None);
        let local = fixture.local_roots["only"].clone();
        std::fs::write(local.join("after-the-failure.txt"), b"back").expect("seed");
        fixture.record_local_event(
            "only",
            &local.join("after-the-failure.txt"),
            WatchEventKind::Created,
        );
        fixture.converge(16);
        assert!(
            fixture.cloud_roots["only"]
                .join("after-the-failure.txt")
                .exists(),
            "the profile syncs again"
        );
    }

    #[test]
    fn a_profile_whose_state_database_cannot_be_opened_is_tried_again() {
        // A file stands where the folder of the profile's database
        // goes, as a full disk or a lock would stop the open.
        let in_the_way = |root: &Path| root.join("state/profiles/only");
        let mut fixture = MultiFixture::new_after(&[("only", SyncMode::TwoWay, false)], |root| {
            std::fs::create_dir_all(root.join("state/profiles")).expect("state");
            std::fs::write(in_the_way(root), b"in the way").expect("obstacle");
        });
        assert_eq!(fixture.multi.failed_profile_count(), 1);
        assert_eq!(
            fixture.multi.all_suspended(),
            Some(AllSuspended::AtRuntime),
            "nothing in the configuration is wrong: the daemon tries again"
        );

        // The cause is still there at the first try: the profile waits
        // for the next one.
        fixture.tick(61_000);
        assert_eq!(fixture.multi.failed_profile_count(), 1);

        // The cause goes, and the next try composes the profile. No
        // restart, and no change of the configuration.
        std::fs::remove_file(in_the_way(fixture._temp.path())).expect("the cause goes");
        fixture.tick(301_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0);
        assert_eq!(fixture.multi.all_suspended(), None);
    }

    #[test]
    fn a_profile_composed_while_the_daemon_is_paused_starts_paused() {
        let mut fixture = MultiFixture::new(&[("only", SyncMode::TwoWay, false)]);
        let control = Arc::new(RuntimeControl::new());
        fixture.multi.attach_control(control.clone());
        fixture.tick(6_000);
        fixture
            .multi
            .runtime_for("only")
            .expect("runtime")
            .replace_provider_for_testing(Box::new(PanickingProvider));
        fixture.tick(6_000);
        assert_eq!(fixture.multi.failed_profile_count(), 1);

        // The user pauses the daemon while the profile is suspended.
        control.request_pause();
        fixture.tick(6_000);

        // The profile is composed again: it joins a paused daemon.
        let local = fixture.local_roots["only"].clone();
        std::fs::write(local.join("waits.txt"), b"paused").expect("seed");
        fixture.tick(61_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0, "composed again");
        fixture.converge(8);
        assert_eq!(
            fixture
                .multi
                .runtime_for("only")
                .expect("runtime")
                .app()
                .snapshot()
                .run_state,
            RunState::Paused
        );
        assert!(
            !fixture.cloud_roots["only"].join("waits.txt").exists(),
            "a paused daemon admits no work, in a profile that joined late too"
        );

        control.request_resume();
        fixture.converge(16);
        assert!(fixture.cloud_roots["only"].join("waits.txt").exists());
    }

    #[test]
    fn a_pause_for_one_profile_leaves_the_others_syncing() {
        let mut fixture = MultiFixture::new(&[
            ("work", SyncMode::TwoWay, false),
            ("home", SyncMode::TwoWay, false),
        ]);
        let control = Arc::new(RuntimeControl::new());
        fixture.multi.attach_control(control.clone());
        fixture.tick(6_000);
        let run_state = |fixture: &mut MultiFixture, id: &str| {
            fixture
                .multi
                .runtime_for(id)
                .expect("runtime")
                .app()
                .snapshot()
                .run_state
        };

        control.request_for_profile("work", ControlRequest::Pause);
        fixture.tick(6_000);
        assert_eq!(run_state(&mut fixture, "work"), RunState::Paused);
        assert_eq!(run_state(&mut fixture, "home"), RunState::Running);

        for id in ["work", "home"] {
            let file = fixture.local_roots[id].join("note.txt");
            std::fs::write(&file, b"note").expect("seed");
            fixture.record_local_event(id, &file, WatchEventKind::Created);
        }
        fixture.converge(16);
        assert!(fixture.cloud_roots["home"].join("note.txt").exists());
        assert!(
            !fixture.cloud_roots["work"].join("note.txt").exists(),
            "the paused profile admits no work"
        );

        control.request_for_profile("work", ControlRequest::Resume);
        fixture.converge(16);
        assert!(fixture.cloud_roots["work"].join("note.txt").exists());
    }

    #[test]
    fn one_profile_can_leave_a_pause_of_the_whole_daemon() {
        let mut fixture = MultiFixture::new(&[
            ("work", SyncMode::TwoWay, false),
            ("home", SyncMode::TwoWay, false),
        ]);
        let control = Arc::new(RuntimeControl::new());
        fixture.multi.attach_control(control.clone());
        fixture.tick(6_000);

        control.request_pause();
        fixture.tick(6_000);
        control.request_for_profile("home", ControlRequest::Resume);
        fixture.tick(6_000);

        for id in ["work", "home"] {
            let file = fixture.local_roots[id].join("note.txt");
            std::fs::write(&file, b"note").expect("seed");
            fixture.record_local_event(id, &file, WatchEventKind::Created);
        }
        fixture.converge(16);
        assert!(fixture.cloud_roots["home"].join("note.txt").exists());
        assert!(
            !fixture.cloud_roots["work"].join("note.txt").exists(),
            "the other profile stays paused"
        );

        // A resume of the whole daemon ends every pause.
        control.request_resume();
        fixture.converge(16);
        assert!(fixture.cloud_roots["work"].join("note.txt").exists());
    }

    /// Makes the provider of `id` panic, so that the profile is
    /// suspended. A paused profile calls no provider, so this is for a
    /// profile that runs.
    fn suspend(fixture: &mut MultiFixture, id: &str) {
        fixture
            .multi
            .runtime_for(id)
            .expect("runtime")
            .replace_provider_for_testing(Box::new(PanickingProvider));
        fixture.tick(6_000);
        fixture.tick(6_000);
        assert_eq!(fixture.multi.failed_profile_count(), 1);
    }

    fn run_state_of(fixture: &mut MultiFixture, id: &str) -> RunState {
        fixture
            .multi
            .runtime_for(id)
            .expect("runtime")
            .app()
            .snapshot()
            .run_state
    }

    #[test]
    fn a_pause_by_name_of_a_suspended_profile_holds_when_it_is_composed_again() {
        let mut fixture = MultiFixture::new(&[
            ("work", SyncMode::TwoWay, false),
            ("home", SyncMode::TwoWay, false),
        ]);
        let control = Arc::new(RuntimeControl::new());
        fixture.multi.attach_control(control.clone());
        fixture.tick(6_000);
        suspend(&mut fixture, "work");

        // The request names a profile that does not run now.
        control.request_for_profile("work", ControlRequest::Pause);
        fixture.tick(6_000);
        fixture.tick(61_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0, "composed again");
        fixture.tick(6_000);

        assert_eq!(run_state_of(&mut fixture, "work"), RunState::Paused);
        assert_eq!(run_state_of(&mut fixture, "home"), RunState::Running);
    }

    #[test]
    fn a_resume_of_everything_reaches_a_profile_that_is_suspended() {
        let mut fixture = MultiFixture::new(&[
            ("work", SyncMode::TwoWay, false),
            ("home", SyncMode::TwoWay, false),
        ]);
        let control = Arc::new(RuntimeControl::new());
        fixture.multi.attach_control(control.clone());
        fixture.tick(6_000);
        suspend(&mut fixture, "work");
        control.request_for_profile("work", ControlRequest::Pause);
        fixture.tick(6_000);

        // The resume comes while the profile does not run. The profile
        // that is composed later must not remember the old pause.
        control.request_resume();
        fixture.tick(61_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0, "composed again");
        fixture.tick(6_000);

        assert_eq!(run_state_of(&mut fixture, "work"), RunState::Running);
    }

    #[test]
    fn a_reconcile_for_one_profile_is_for_that_profile_alone() {
        let mut fixture = MultiFixture::new(&[
            ("work", SyncMode::TwoWay, false),
            ("home", SyncMode::TwoWay, false),
        ]);
        let control = Arc::new(RuntimeControl::new());
        fixture.multi.attach_control(control.clone());
        fixture.tick(6_000);
        fixture.converge(16);
        let requests_of = |fixture: &MultiFixture, id: &str| {
            fixture
                .multi
                .timeline()
                .snapshot(None)
                .into_iter()
                .filter(|event| {
                    event.kind == constants::timeline::KIND_RECONCILE
                        && event.profile_id == id
                        && event.message.contains("requested by the user")
                })
                .count()
        };
        assert_eq!(requests_of(&fixture, "work"), 0);

        control.request_for_profile("work", ControlRequest::Reconcile);
        fixture.tick(6_000);

        assert_eq!(requests_of(&fixture, "work"), 1);
        assert_eq!(
            requests_of(&fixture, "home"),
            0,
            "the request named one profile"
        );

        // A request with no profile is for every profile.
        control.request_reconcile();
        fixture.tick(6_000);
        assert_eq!(requests_of(&fixture, "work"), 2);
        assert_eq!(requests_of(&fixture, "home"), 1);
    }

    #[test]
    fn a_profile_that_composes_and_fails_again_waits_longer_each_time() {
        let mut fixture = MultiFixture::new(&[("only", SyncMode::TwoWay, false)]);
        fixture.tick(6_000);
        let fail = |fixture: &mut MultiFixture| {
            fixture
                .multi
                .runtime_for("only")
                .expect("runtime")
                .replace_provider_for_testing(Box::new(PanickingProvider));
            fixture.tick(6_000);
            assert_eq!(fixture.multi.failed_profile_count(), 1);
        };

        // The first suspension waits the shortest time.
        fail(&mut fixture);
        fixture.tick(61_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0, "composed again");

        // It fails again at once. The wait is the next one, not the
        // shortest one again.
        fail(&mut fixture);
        fixture.tick(61_000);
        assert_eq!(
            fixture.multi.failed_profile_count(),
            1,
            "one minute is not enough the second time"
        );
        fixture.tick(240_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0);

        // A profile that then runs for as long as the wait it reached
        // starts from the shortest wait when it fails much later.
        fixture.tick(1_800_000);
        fixture.tick(6_000);
        fail(&mut fixture);
        fixture.tick(61_000);
        assert_eq!(fixture.multi.failed_profile_count(), 0);
    }

    #[test]
    fn the_wait_before_a_suspended_profile_is_tried_again_grows_to_thirty_minutes() {
        let waits: Vec<u64> = (0..5)
            .map(|attempts| suspended_profile_retry_delay(attempts).as_secs())
            .collect();
        assert_eq!(waits, vec![60, 300, 1_800, 1_800, 1_800]);
    }

    #[test]
    fn shared_workgate_caps_bound_total_in_flight_work_across_profiles() {
        // Budget isolation: two busy profiles share ONE daemon-
        // level workgate: combined admissions never exceed the caps a
        // single profile would get.
        let mut fixture = MultiFixture::new(&[
            ("busy-a", SyncMode::TwoWay, false),
            ("busy-b", SyncMode::TwoWay, false),
        ]);
        fixture.tick(6_000); // baseline + startup reconciles

        for id in ["busy-a", "busy-b"] {
            let local = fixture.local_roots[id].clone();
            for index in 0..6 {
                let path = local.join(format!("f-{index}.txt"));
                std::fs::write(&path, b"x").expect("seed");
                fixture.record_local_event(id, &path, WatchEventKind::Created);
            }
        }

        // One ingest tick: both profiles stabilize + enqueue + admit.
        fixture.tick(6_000);
        let snapshot = {
            let runtime = fixture.multi.runtime_for("busy-a").expect("runtime");
            runtime.app().workgate_snapshot()
        };
        let total_active = snapshot.active_planner_workers
            + snapshot.active_hash_workers
            + snapshot.active_uploads
            + snapshot.active_downloads;
        let cap_budget = snapshot.caps.planner_workers
            + snapshot.caps.hash_workers
            + snapshot.caps.upload_concurrency
            + snapshot.caps.download_concurrency;
        assert!(
            total_active <= cap_budget,
            "shared workgate must bound combined in-flight work: {total_active} > {cap_budget}"
        );

        // And everything still converges.
        fixture.converge(24);
        for id in ["busy-a", "busy-b"] {
            for index in 0..6 {
                assert!(
                    fixture.cloud_roots[id]
                        .join(format!("f-{index}.txt"))
                        .exists(),
                    "{id} f-{index} must upload"
                );
            }
        }
    }

    #[test]
    fn external_control_broadcasts_to_every_profile() {
        let mut fixture = MultiFixture::new(&[
            ("one", SyncMode::TwoWay, false),
            ("two", SyncMode::TwoWay, false),
        ]);
        let control = Arc::new(RuntimeControl::new());
        fixture.multi.attach_control(control.clone());
        fixture.tick(6_000);

        control.request_pause();
        fixture.tick(250);
        for id in ["one", "two"] {
            let runtime = fixture.multi.runtime_for(id).expect("runtime");
            assert_eq!(
                runtime.app().snapshot().run_state,
                RunState::Paused,
                "profile {id} must observe the daemon-wide pause"
            );
        }

        control.request_resume();
        fixture.tick(250);
        for id in ["one", "two"] {
            let runtime = fixture.multi.runtime_for(id).expect("runtime");
            assert_eq!(runtime.app().snapshot().run_state, RunState::Running);
        }
    }

    #[test]
    fn remote_changes_flow_per_profile_through_their_own_feeds() {
        // Multi-provider fan-out: remote changes in one profile's
        // cloud only land in that profile's local root.
        let mut fixture = MultiFixture::new(&[
            ("left", SyncMode::TwoWay, false),
            ("right", SyncMode::TwoWay, false),
        ]);
        fixture.tick(6_000); // baseline both feeds

        std::fs::write(
            fixture.cloud_roots["left"].join("inbound.txt"),
            b"left only",
        )
        .expect("seed left cloud");
        let left_feed = fixture.feeds["left"].clone();
        left_feed.emit_created(
            fixture.cloud_roots["left"].join("inbound.txt"),
            timestamp_ms(fixture.now_ms),
        );

        fixture.converge(16);

        assert_eq!(
            std::fs::read(fixture.local_roots["left"].join("inbound.txt")).expect("left local"),
            b"left only"
        );
        assert!(
            !fixture.local_roots["right"].join("inbound.txt").exists(),
            "remote changes must stay inside their profile"
        );
    }
}

/// Per-profile diagnostics row (consumed by the IPC surface).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub provider_kind: String,
    pub sync_mode: vapor_shared::SyncMode,
    pub run_state: RunState,
    pub failed_reason: Option<String>,
    pub mirror_reverts: u64,
    pub mirror_deletes: u64,
    pub conflicts: u64,
}
