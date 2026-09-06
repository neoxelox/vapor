//! Multi-profile daemon composition.
//!
//! One daemon process serves every enabled profile:
//!
//! - **Per-profile isolation**: each profile owns its `DaemonRuntime`
//!   (debounce, scheduler, executor, echo caches) and its own durable
//!   state DB (`state/profiles/<id>/vapor.sqlite`; the implicit
//!   `default` profile keeps the legacy path).
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
//!   profile suspends only its own queue. (With the release profile's
//!   `panic = "abort"` a panic still ends the process — there the
//!   process-level crash-loop guard takes over; the catcher gives
//!   containment in dev/test builds and against `Err` returns
//!   everywhere.)

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
use crate::runtime_control::RuntimeControl;
use crate::state_db::DurableStateDb;
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllSuspended {
    /// Every profile was suspended while composing the daemon.
    AtComposition,
    /// At least one profile was suspended by a runtime failure.
    AtRuntime,
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
    auto_tuner: crate::auto_tune::AutoTuner,
    clock: SharedClock,
    tick_interval: Duration,
    idle_tick_interval: Duration,
    /// Per-slot state fingerprints at the last status publish; a
    /// mismatch marks the published snapshot stale.
    last_published_slot_states: Vec<SlotStateFingerprint>,
    last_status_publish_inst: Option<Instant>,
    /// Whether the previous tick reported pending work — publishes one
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
}

/// The cheap observable state of one profile: enough to detect that a
/// published status snapshot went stale without re-running its SQL.
type SlotStateFingerprint = (RunState, ThrottleState, bool, String);

fn slot_state_fingerprint(slot: &ProfileSlot) -> SlotStateFingerprint {
    let app_snapshot = slot.runtime.app().snapshot();
    (
        slot_effective_run_state(slot),
        app_snapshot.throttle_state,
        slot.failed.is_some(),
        app_snapshot.reason.clone(),
    )
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
        let waker = self.tick_waker.clone();
        for slot in &mut self.slots {
            slot.runtime.enable_transfer_workers(Some(waker.clone()));
        }
    }

    /// Replaces the idle source every profile runtime consults for idle
    /// boost. The daemon uses it to honour `VAPOR_THROTTLE_INPUTS=static`.
    pub fn set_idle_notifier(&mut self, notifier: Arc<dyn vapor_platform::IdleNotifier>) {
        for slot in &mut self.slots {
            slot.runtime.set_idle_notifier(notifier.clone());
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
        // one bandwidth shaper, one tuned step knob for every profile.
        let max_concurrent_transfers = budget_config.max_concurrent_transfers;
        let shared_budget = Arc::new(Mutex::new(crate::resource_budget::ResourceBudget::new(
            budget_config,
        )));
        let shared_shaper = Arc::new(Mutex::new(vapor_providers::BandwidthShaper::unlimited()));
        let shared_step = Arc::new(std::sync::atomic::AtomicU64::new(
            constants::engine::TRANSFER_STAGE_STEP_BYTES,
        ));
        let idle_notifier: Arc<dyn vapor_platform::IdleNotifier> =
            Arc::new(vapor_platform::NativeIdleNotifier::for_current_host());

        let mut slots = Vec::new();
        // One ignore-rule filter per canonical local root: profiles that
        // watch the same directory share the instance, so an
        // ignore-file reload observed by the deduplicated watcher
        // reaches every runtime that filters through it.
        let mut filters_by_root: BTreeMap<PathBuf, Arc<SharedEventPathFilter>> = BTreeMap::new();
        for profile in profiles.into_iter().filter(|profile| profile.enabled) {
            let database_path = match &state_root {
                Some(root) => root
                    .join("profiles")
                    .join(&profile.id)
                    .join(constants::runtime::SQLITE_DATABASE_FILE_NAME),
                None => vapor_shared::runtime_paths::profile_database_path(&profile.id),
            };
            // Per-profile startup failures degrade to skipping that
            // profile, never aborting the whole daemon (which would stop
            // every healthy profile and feed the crash-loop guard).
            let state_db = match DurableStateDb::open_with_corruption_recovery(&database_path, now)
            {
                Ok(state_db) => state_db,
                Err(error) => {
                    logging::error(
                        "Skipping profile whose durable state DB could not be opened",
                        &[
                            ("profile_id", profile.id.clone()),
                            ("error", error.to_string()),
                        ],
                    );
                    continue;
                }
            };
            // Overlapping roots on a filesystem-backed profile feed the
            // engine its own provider writes (and can strict-mirror over
            // content the user never chose). Checked before provider
            // construction so the suspended profile never even ensures
            // (creates) the misconfigured cloud root.
            let overlap_failure = if profile.provider_kind.trim()
                == vapor_shared::constants::provider::FILESYSTEM
                && let Some(local_root) = profile.scope.local_sync_directory.as_deref()
            {
                crate::sync_directories::filesystem_roots_overlap(
                    local_root,
                    profile.scope.cloud_sync_directory.as_str(),
                )
                .map(|reason| format!("invalid sync directories: {reason}"))
            } else {
                None
            };
            // An invalid provider must NOT fall back to a functioning stub:
            // the stub reports empty enumerations, so a startup reconcile
            // on a pull-only profile would classify the whole local root as
            // local-only and strict-mirror-delete it. Suspend the profile
            // instead — it surfaces in status but performs zero sync work.
            // Overlapping roots suspend the same way (the inert stub
            // performs no filesystem work).
            let (provider, provider_failure) = if let Some(reason) = overlap_failure {
                logging::error(
                    "Suspending profile whose local and cloud sync directories overlap",
                    &[
                        ("profile_id", profile.id.clone()),
                        ("reason", reason.clone()),
                    ],
                );
                (vapor_providers::inert_stub_provider(), Some(reason))
            } else {
                match vapor_providers::select_provider_for_profile(
                    &profile.provider_kind,
                    &profile.id,
                ) {
                    Ok(provider) => (provider, None),
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
                        (vapor_providers::inert_stub_provider(), Some(reason))
                    }
                }
            };
            let app = DaemonApp::new_with_shared_workgate(
                provider,
                clock.clone(),
                shared_workgate.clone(),
            );
            let mut runtime = match DaemonRuntime::build_with_app(
                profile.scope.clone(),
                filter_options.clone(),
                state_db,
                app,
                metrics_sampler.clone(),
                clock.clone(),
                false, // watchers are deduplicated at this level
            ) {
                Ok(runtime) => runtime,
                Err(error) => {
                    logging::error(
                        "Skipping profile that could not be composed at startup",
                        &[
                            ("profile_id", profile.id.clone()),
                            ("error", format!("{error:?}")),
                        ],
                    );
                    continue;
                }
            };
            if let Some(built_filter) = runtime.shared_path_filter() {
                let canonical_root = built_filter.watch_root().to_path_buf();
                match filters_by_root.entry(canonical_root) {
                    std::collections::btree_map::Entry::Occupied(shared) => {
                        runtime.adopt_shared_path_filter(shared.get().clone());
                    }
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(built_filter);
                    }
                }
            }
            runtime.set_device_id(device_id);
            runtime.set_profile_id(profile.id.clone());
            runtime.configure_mass_delete_guard(mass_delete_settings);
            runtime.set_max_concurrent_transfers(max_concurrent_transfers);
            runtime.attach_timeline(timeline.clone());
            runtime.attach_resource_management(
                shared_budget.clone(),
                shared_shaper.clone(),
                shared_step.clone(),
                idle_notifier.clone(),
            );
            // A suspended-at-composition profile never schedules a reconcile
            // or starts a watcher; it only surfaces its Error state.
            let mut failed = provider_failure;
            if failed.is_none()
                && let Err(error) = runtime.schedule_startup_reconcile(now)
            {
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
            } else if let Some(reason) = &failed {
                runtime.set_error_state(reason.clone());
            }
            let control = Arc::new(RuntimeControl::new());
            runtime.attach_control(control.clone());
            slots.push(ProfileSlot {
                profile,
                runtime,
                control,
                consecutive_tick_errors: 0,
                failed_at_composition: failed.is_some(),
                failed,
            });
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
            auto_tuner: crate::auto_tune::AutoTuner::new(shared_step),
            clock,
            tick_interval: Duration::from_millis(constants::engine::DEBOUNCE_TICK_MILLIS),
            idle_tick_interval: Duration::from_millis(constants::engine::IDLE_TICK_MILLIS),
            last_published_slot_states: Vec::new(),
            last_status_publish_inst: None,
            last_any_pending_work: false,
            shared_budget,
            config_reloader: None,
            restart_required: Vec::new(),
            config_generation: 0,
            last_published_config_generation: 0,
        })
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
            let ceiling = budget.max_concurrent_transfers;
            self.shared_budget
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .replace_config(budget);
            for slot in &mut self.slots {
                slot.runtime.set_max_concurrent_transfers(ceiling);
            }
        }
        if change.live.contains(&keys::KEY_SAFEGUARDS) {
            let settings = crate::safeguards::MassDeleteGuardSettings::resolve(&config);
            for slot in &mut self.slots {
                slot.runtime.configure_mass_delete_guard(settings);
            }
        }
        if change.live.iter().any(|key| {
            matches!(
                *key,
                keys::KEY_USE_GIT_IGNORE
                    | keys::KEY_USE_VAPOR_IGNORE
                    | keys::KEY_PRE_IGNORE_RULES
                    | keys::KEY_POST_IGNORE_RULES
            )
        }) {
            let options = EventPathFilterOptions::from_environment_and_config(&config);
            let mut seen: Vec<*const crate::fs_events::SharedEventPathFilter> = Vec::new();
            for slot in &self.slots {
                if let Some(filter) = slot.runtime.shared_path_filter() {
                    let pointer = Arc::as_ptr(&filter);
                    if seen.contains(&pointer) {
                        continue;
                    }
                    seen.push(pointer);
                    filter.replace_options(options.clone());
                }
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
                "config",
                "daemon",
                format!("applied live settings: {}", live_keys.join(", ")),
                self.clock.now_system(),
            );
        }
        for key in change.restart {
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
                    display_name: slot.profile.display_name.clone(),
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

    /// Ticks every live profile once. Panics and repeated errors
    /// suspend only the offending profile.
    pub fn tick_all(&mut self, now: SystemTime) -> MultiTickReport {
        let control_forwarded = self.forward_external_control();
        self.apply_config_changes(self.clock.now());

        let mut report = MultiTickReport::default();
        let mut completed_this_tick: u64 = 0;
        for slot in &mut self.slots {
            if slot.failed.is_some() {
                report.failed_profiles += 1;
                continue;
            }
            let outcome = catch_unwind(AssertUnwindSafe(|| slot.runtime.tick(now)));
            match outcome {
                Ok(Ok(tick_report)) => {
                    slot.consecutive_tick_errors = 0;
                    report.ticked_profiles += 1;
                    completed_this_tick += tick_report.completed_intents as u64;
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
                            "profile",
                            slot.profile.id.clone(),
                            "profile suspended after repeated tick failures",
                            now,
                        );
                        suspend_profile(slot, format!("repeated tick failures: {error:?}"), now);
                        report.failed_profiles += 1;
                    }
                }
                Err(panic) => {
                    let reason = panic_message(panic);
                    self.timeline.push(
                        "profile",
                        slot.profile.id.clone(),
                        format!("profile suspended after panic: {reason}"),
                        now,
                    );
                    suspend_profile(slot, format!("panicked: {reason}"), now);
                    report.failed_profiles += 1;
                }
            }
        }

        // Auto-tuning: one small change per cycle, driven by
        // aggregate rate-limit + queue-depth + drain-rate signals,
        // bounded by the ceilings via the bandwidth shaper.
        // Suspended slots are excluded from the tuning signal: their
        // never-draining queue and stale state would skew the tuner, and
        // reading a half-mutated post-panic runtime here (outside the
        // catch_unwind) is a containment hazard.
        // The SQL-backed depth signal is gathered only when a tuning
        // cycle is actually due (~every 90s), not on every idle tick.
        self.auto_tuner.record_completions(completed_this_tick);
        let now_inst = self.clock.now();
        if self.auto_tuner.cycle_due(now_inst) {
            let rate_limited = self
                .slots
                .iter()
                .filter(|slot| slot.failed.is_none())
                .any(|slot| slot.runtime.app().retry_slowdown_until().is_some());
            let total_queue_depth: u64 = self
                .slots
                .iter()
                .filter(|slot| slot.failed.is_none())
                .map(|slot| slot.runtime.state_db().queue_depth().unwrap_or(0) as u64)
                .sum();
            self.auto_tuner
                .evaluate(rate_limited, total_queue_depth, now_inst);
        }

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
            let stale = report.any_pending_work
                || self.last_any_pending_work
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

    /// Runs the multi-profile tick loop until shutdown. Exits with an
    /// error only when EVERY profile has failed — a single broken
    /// profile never takes the daemon down.
    pub fn run_forever(&mut self) -> Result<(), DaemonRuntimeError> {
        // Let a shutdown signal wake the loop out of its idle sleep so
        // `vapor stop` / SIGTERM exits promptly instead of waiting out a
        // full idle interval.
        crate::runtime::register_shutdown_waker(self.tick_waker.clone());
        let mut reported_all_suspended = false;
        while !is_shutdown_requested() {
            let report = self.tick_all(self.clock.now_system());
            match self.all_suspended() {
                // Every profile died at runtime: exit and let the service
                // manager and the crash-loop guard decide about restarts.
                Some(AllSuspended::AtRuntime) => {
                    logging::error("Every profile has failed; exiting the daemon", &[]);
                    return Err(DaemonRuntimeError::StateDb(
                        crate::state_db::StateDbError::InvalidStateValue(
                            "all profiles suspended".to_string(),
                        ),
                    ));
                }
                // Every profile is misconfigured: exiting would only feed
                // the crash-loop guard and hide the reason. Stay up on the
                // idle cadence so `vapor status` can name what to fix.
                Some(AllSuspended::AtComposition) if !reported_all_suspended => {
                    reported_all_suspended = true;
                    logging::error(
                        "Every profile is suspended by its configuration; serving status \
                         only until the configuration is fixed and the daemon restarted",
                        &[],
                    );
                }
                Some(AllSuspended::AtComposition) | None => {}
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
        if self.slots.iter().all(|slot| slot.failed_at_composition) {
            Some(AllSuspended::AtComposition)
        } else {
            Some(AllSuspended::AtRuntime)
        }
    }

    /// Broadcast pending external control requests to every profile
    /// (pause/resume/flush/reconcile are daemon-wide actions). Returns
    /// whether any request was forwarded (a status-publish trigger).
    fn forward_external_control(&mut self) -> bool {
        let Some(external) = self.external_control.as_ref() else {
            return false;
        };
        let pause = external.take_pause_request();
        let flush = external.take_flush_request();
        let reconcile = external.take_reconcile_request();
        for slot in &self.slots {
            match pause {
                Some(true) => slot.control.request_pause(),
                Some(false) => slot.control.request_resume(),
                None => {}
            }
            if flush {
                slot.control.request_flush();
            }
            if reconcile {
                slot.control.request_reconcile();
            }
        }
        pause.is_some() || flush || reconcile
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
            return DaemonStatusSnapshot {
                run_state: format!("{:?}", RunState::Error),
                throttle_state: ThrottleState::Suspended,
                provider_name: "none".to_string(),
                throttle_reason: "no enabled profiles".to_string(),
                ..DaemonStatusSnapshot::default()
            };
        };

        let mut snapshot = DaemonStatusSnapshot::from_app(worst.runtime.app());
        snapshot.config_restart_required = (!self.restart_required.is_empty()).then(|| {
            format!(
                "{} changed; restart the daemon to apply (vapor service restart)",
                self.restart_required.join(", ")
            )
        });
        snapshot.run_state = format!("{:?}", slot_effective_run_state(worst));
        snapshot.resource_budget = self
            .slots
            .iter()
            .find_map(|slot| slot.runtime.resource_budget_status());
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
        for slot in &self.slots {
            let queue_depth = slot.runtime.state_db().queue_depth().unwrap_or(0) as u64;
            let failed_intents = slot.runtime.state_db().failed_depth().unwrap_or(0) as u64;
            let (mirror_reverts, mirror_deletes) = slot.runtime.mirror_counters();
            let conflicts = slot.runtime.conflict_count();
            let app_snapshot = slot.runtime.app().snapshot();
            snapshot.profiles.push(vapor_ipc::ProfileStatus {
                id: slot.profile.id.clone(),
                display_name: slot.profile.display_name.clone(),
                provider_name: slot.profile.provider_kind.clone(),
                sync_mode: slot.profile.scope.sync_mode.as_config_value().to_string(),
                run_state: format!("{:?}", slot_effective_run_state(slot)),
                reason: app_snapshot.reason.clone(),
                queue_depth,
                failed_intents,
                conflicts,
                mirror_reverts,
                mirror_deletes,
                suspended_reason: slot.failed.clone(),
            });
            snapshot.queue_depth += queue_depth;
            snapshot.failed_intents += failed_intents;
            snapshot.conflicts += conflicts;
            snapshot.mirror_reverts += mirror_reverts;
            snapshot.mirror_deletes += mirror_deletes;
            snapshot.loop_prevention_suppressions += slot.runtime.loop_suppression_count();
            snapshot.dropped_incoming_events += slot.runtime.dropped_incoming_event_count();

            if snapshot.intent_diagnostics.len() < DIAGNOSTICS_ROW_CAP {
                let remaining = DIAGNOSTICS_ROW_CAP - snapshot.intent_diagnostics.len();
                let rows = slot.runtime.intent_diagnostics(
                    &slot.profile.id,
                    per_profile_cap.min(remaining),
                    now,
                );
                if rows.len() == per_profile_cap.min(remaining) && queue_depth > rows.len() as u64 {
                    snapshot.diagnostics_truncated = true;
                }
                snapshot.intent_diagnostics.extend(rows);
            } else {
                snapshot.diagnostics_truncated = true;
            }
        }
        snapshot
    }
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
    // (profile_id, reason) for profiles whose watcher could not start — a
    // watcher failure suspends only the affected profiles, never the whole
    // daemon.
    let mut suspend: Vec<(String, String)> = Vec::new();
    for slot in slots.iter() {
        if slot.failed.is_some() {
            continue;
        }
        let Some(root) = slot.profile.scope.local_sync_directory.clone() else {
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
    use crate::clock::ManualClock;
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
            let temp = TempDir::new().expect("temp dir");
            let clock = Arc::new(ManualClock::at_now());

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
                    display_name: id.to_string(),
                    provider_kind: "filesystem".to_string(),
                    scope: SyncScope {
                        local_sync_directory: Some(local_root),
                        cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
                        sync_mode: *mode,
                    },
                    enabled: true,
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
            for (id, _, _) in specs {
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
                display_name: "overlap".to_string(),
                provider_kind: "filesystem".to_string(),
                scope: SyncScope {
                    local_sync_directory: Some(local.clone()),
                    cloud_sync_directory: nested_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::TwoWay,
                },
                enabled: true,
            },
            ResolvedProfile {
                id: "good".to_string(),
                display_name: "good".to_string(),
                provider_kind: "filesystem".to_string(),
                scope: SyncScope {
                    local_sync_directory: Some(good_local.clone()),
                    cloud_sync_directory: good_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::TwoWay,
                },
                enabled: true,
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
                display_name: "bad".to_string(),
                provider_kind: "gdrvie".to_string(),
                scope: SyncScope {
                    local_sync_directory: Some(bad_local.clone()),
                    cloud_sync_directory: bad_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::PullOnly,
                },
                enabled: true,
            },
            ResolvedProfile {
                id: "good".to_string(),
                display_name: "good".to_string(),
                provider_kind: "filesystem".to_string(),
                scope: SyncScope {
                    local_sync_directory: Some(good_local.clone()),
                    cloud_sync_directory: good_cloud.to_string_lossy().into_owned(),
                    sync_mode: SyncMode::TwoWay,
                },
                enabled: true,
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
        // The suspended profile performed zero sync work — its local file
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
        fn ensure_cloud_sync_directory(
            &self,
            _cloud_sync_directory: &str,
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

    #[test]
    fn a_live_config_change_applies_mid_work_and_a_restart_key_is_reported() {
        let temp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::at_now());
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        std::fs::create_dir_all(&local).expect("local");
        std::fs::create_dir_all(&cloud).expect("cloud");
        let config_path = temp.path().join("vapor.json");
        std::fs::write(&config_path, "{}\n").expect("seed config");
        let local = vapor_shared::paths::canonicalize(&local).expect("canonical");
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
            display_name: "default".to_string(),
            provider_kind: "filesystem".to_string(),
            scope: SyncScope {
                local_sync_directory: Some(local.clone()),
                cloud_sync_directory: cloud.to_string_lossy().into_owned(),
                sync_mode: SyncMode::TwoWay,
            },
            enabled: true,
        }];
        let applied = vapor_shared::config::VaporConfig::default();
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
        std::fs::write(
            &config_path,
            "{\"resourceLimits\": {\"cpuPercent\": 7}, \"provider\": \"gdrive\"}\n",
        )
        .expect("edit config");
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
        assert_eq!(multi.restart_required(), &["provider"]);
        let status = multi.aggregate_status(clock.now_system());
        assert!(
            status
                .config_restart_required
                .as_deref()
                .is_some_and(|notice| notice.contains("provider") && notice.contains("restart")),
            "{:?}",
            status.config_restart_required
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
            display_name: "only".to_string(),
            provider_kind: "gdrvie".to_string(),
            scope: SyncScope {
                local_sync_directory: Some(local),
                cloud_sync_directory: "/Vapor".to_string(),
                sync_mode: SyncMode::TwoWay,
            },
            enabled: true,
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
        assert!(
            only.suspended_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("gdrvie")),
            "reason must name the bad provider: {:?}",
            only.suspended_reason
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
    fn shared_workgate_caps_bound_total_in_flight_work_across_profiles() {
        // Budget isolation: two busy profiles share ONE daemon-
        // level workgate — combined admissions never exceed the caps a
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
    pub display_name: String,
    pub provider_kind: String,
    pub sync_mode: vapor_shared::SyncMode,
    pub run_state: RunState,
    pub failed_reason: Option<String>,
    pub mirror_reverts: u64,
    pub mirror_deletes: u64,
    pub conflicts: u64,
}
