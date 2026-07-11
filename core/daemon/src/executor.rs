//! Provider-driven staged executor.
//!
//! Every stage performs real work against the local filesystem and
//! the injected [`Provider`].
//! Stage transitions happen on work completion, never on synthetic
//! timers, and long work (hashing, transfers) is chunked so one advance
//! call never exceeds its per-tick byte budget — the slice-budget
//! interruptibility discipline (`AGENTS.md §3`) applied to transfers.
//!
//! Pipeline routes by intent kind:
//! - `Upload` / `Rename` → Planner → Hash → Upload (provider session)
//! - `Delete` → Planner → Upload slot (provider delete call)
//! - `Download` → Planner → Download (provider session) → atomic local
//!   apply (temp file + rename + op-id tag + self-write record)
//! - `ApplyRemoteDelete` → Planner (local delete + self-write record)
//!
//! Loop prevention: every completed provider write records into the
//! remote echo cache; every completed local apply records into the
//! local echo cache.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use sha2::{Digest, Sha256};
use vapor_providers::tags::OpIdTagStore;
use vapor_providers::{
    DownloadRequest, ProviderError, RemotePath, RemotePrecondition, TransferSession, TransferStep,
    UploadRequest,
};
use vapor_shared::constants;

use crate::clock::{Clock, SystemClock};
use crate::event_intents::PendingIntentKind;
use crate::retry::RetryFailureKind;
use crate::self_write_cache::SelfWriteCache;
use crate::state_db::{DurableIntentRecord, DurableStateDb, StateDbError};
use crate::workgate::WorkgateSnapshot;
use crate::{DaemonApp, workgate::WorkClass};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionStage {
    Planner,
    WaitingForHash,
    Hash,
    WaitingForUpload,
    Upload,
    WaitingForDownload,
    Download,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StagedExecutorSnapshot {
    pub active_total: usize,
    pub planner_running: usize,
    pub waiting_for_hash: usize,
    pub hash_running: usize,
    pub waiting_for_upload: usize,
    pub upload_running: usize,
    pub waiting_for_download: usize,
    pub download_running: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StagedExecutorReport {
    pub started: usize,
    pub completed: usize,
    pub retried: usize,
    pub failed: usize,
    /// Strict-mirror local removals performed this advance (pull-only
    /// restore path found no remote counterpart).
    pub mirror_deletes: usize,
    /// Keep-both conflict copies created this advance.
    pub conflicts: usize,
}

/// Everything stage work needs beyond the app + durable queue. The
/// runtime assembles one per tick; tests assemble their own.
pub struct ExecutionEnv<'a> {
    /// Canonical local sync root; `None` means no local scope is
    /// configured and every local-touching intent fails permanent.
    pub local_root: Option<&'a Path>,
    /// Sync direction for the scope. Direction gates live in
    /// the planner so no intent kind can bypass them; intents enqueued
    /// before a mode change complete as logged no-ops, which is what
    /// makes a mid-run mode change converge deterministically.
    pub sync_mode: vapor_shared::SyncMode,
    /// Stable device identifier: the conflict-suffix component
    /// and the op-id prefix.
    pub device_id: &'a str,
    /// Auto-tuned per-tick transfer step budget.
    pub transfer_step_bytes: u64,
    /// Daemon-wide bandwidth shaper: every transfer step asks
    /// it for a byte grant; a zero grant holds the session at its
    /// checkpoint until tokens refill.
    pub bandwidth: &'a std::sync::Mutex<vapor_providers::BandwidthShaper>,
    /// Op-id tag store for the local side (downloads tag the applied
    /// file so watcher echoes correlate).
    pub tags: &'a OpIdTagStore,
    /// Echo cache keyed by local path (suppresses watcher echoes).
    pub local_echoes: &'a mut SelfWriteCache,
    /// Echo cache keyed by remote path (suppresses feed echoes).
    pub remote_echoes: &'a mut SelfWriteCache,
}

pub struct StagedExecutor {
    active: BTreeMap<i64, ActiveExecution>,
    active_paths: BTreeSet<PathBuf>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for StagedExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedExecutor")
            .field("active_count", &self.active.len())
            .finish()
    }
}

struct ActiveExecution {
    intent: DurableIntentRecord,
    stage: ActiveStage,
    stage_started_inst: Instant,
}

/// Post-planner route for work that still needs stages.
#[derive(Debug)]
struct TransferPlan {
    remote_path: RemotePath,
    op_id: String,
    local_path: PathBuf,
    /// Download staging file inside the local root (hidden from the
    /// ingest path filter by its `.vapor-tmp-` prefix).
    staging_path: Option<PathBuf>,
    /// Filled by the hash stage for uploads.
    content_hash: Option<String>,
    /// Write guard chosen by the planner (two-way conflict safety).
    precondition: RemotePrecondition,
    /// Two-way upload onto a remote object with no sync-index history:
    /// compare content hashes at the upload gate — identical content
    /// converges silently, divergent content resolves as a conflict.
    verify_remote_before_upload: bool,
}

enum ActiveStage {
    Planner {
        permit: crate::workgate::WorkPermit,
    },
    WaitingForHash {
        plan: TransferPlan,
    },
    Hash {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        hasher: StreamingFileHash,
    },
    WaitingForUpload {
        plan: TransferPlan,
    },
    Upload {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        work: UploadWork,
    },
    WaitingForDownload {
        plan: TransferPlan,
    },
    Download {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        session: Box<dyn TransferSession>,
    },
}

enum UploadWork {
    Session(Box<dyn TransferSession>),
    RemoteDelete,
}

/// Outcome of the planner stage.
enum PlanOutcome {
    /// Nothing to do (file vanished, directory event, already
    /// converged). Completes the intent successfully.
    Noop(&'static str),
    /// Upload route (Upload / Rename intents): hash first.
    Upload(TransferPlan),
    /// Remote delete route (Delete intents): straight to the upload
    /// slot for the provider call.
    RemoteDelete(TransferPlan),
    /// Download route (Download intents).
    Download(TransferPlan),
    /// ApplyRemoteDelete completed inline (local deletes are cheap).
    AppliedLocally,
    /// A keep-both conflict was detected and resolved during planning:
    /// the local loser moved to its conflict-copy path, the follow-up
    /// intents are durably enqueued, and the original intent completes.
    ConflictResolved,
    Fail {
        failure: RetryFailureKind,
        message: String,
    },
}

impl StagedExecutor {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            active: BTreeMap::new(),
            active_paths: BTreeSet::new(),
            clock,
        }
    }

    pub fn snapshot(&self) -> StagedExecutorSnapshot {
        let mut snapshot = StagedExecutorSnapshot::default();
        for execution in self.active.values() {
            snapshot.active_total += 1;
            match execution.stage_name() {
                ExecutionStage::Planner => snapshot.planner_running += 1,
                ExecutionStage::WaitingForHash => snapshot.waiting_for_hash += 1,
                ExecutionStage::Hash => snapshot.hash_running += 1,
                ExecutionStage::WaitingForUpload => snapshot.waiting_for_upload += 1,
                ExecutionStage::Upload => snapshot.upload_running += 1,
                ExecutionStage::WaitingForDownload => snapshot.waiting_for_download += 1,
                ExecutionStage::Download => snapshot.download_running += 1,
            }
        }
        snapshot
    }

    /// Diagnostic view of every active execution: (intent id, path,
    /// kind, stage, elapsed-in-stage). Consumed by the IPC diagnostics
    /// surface.
    pub fn active_stages(&self) -> Vec<(i64, PathBuf, PendingIntentKind, ExecutionStage, u64)> {
        let now_inst = self.clock.now();
        self.active
            .values()
            .map(|execution| {
                (
                    execution.intent.id,
                    execution.intent.path.clone(),
                    execution.intent.kind,
                    execution.stage_name(),
                    now_inst
                        .saturating_duration_since(execution.stage_started_inst)
                        .as_millis() as u64,
                )
            })
            .collect()
    }

    pub fn try_start(
        &mut self,
        app: &mut DaemonApp,
        intent: DurableIntentRecord,
        _now: SystemTime,
    ) -> bool {
        if self.active.len() >= max_in_flight_items(app.workgate_snapshot()) {
            return false;
        }
        // Per-path serialization: two intents on the same path must not
        // run concurrently (a local upload racing its own remote apply
        // would corrupt the loop-prevention bookkeeping). The blocked
        // intent requeues and runs after the active one finishes.
        if self.active_paths.contains(&intent.path) {
            return false;
        }

        let Ok(permit) = app.try_acquire_work(WorkClass::Planner) else {
            return false;
        };

        self.active_paths.insert(intent.path.clone());
        let stage_started_inst = self.clock.now();
        self.active.insert(
            intent.id,
            ActiveExecution {
                intent,
                stage: ActiveStage::Planner { permit },
                stage_started_inst,
            },
        );
        true
    }

    pub fn advance(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        now: SystemTime,
    ) -> Result<StagedExecutorReport, StateDbError> {
        let now_inst = self.clock.now();
        let mut report = StagedExecutorReport::default();
        let intent_ids: Vec<i64> = self.active.keys().copied().collect();

        for intent_id in intent_ids {
            let Some(execution) = self.active.remove(&intent_id) else {
                continue;
            };
            let path = execution.intent.path.clone();
            let next =
                self.advance_one(app, state_db, env, execution, now, now_inst, &mut report)?;
            match next {
                Some(execution) => {
                    self.active.insert(intent_id, execution);
                }
                None => {
                    self.active_paths.remove(&path);
                }
            }
        }

        Ok(report)
    }

    /// Advances one execution by at most one unit of real work.
    /// Returns `None` when the execution finished (completed, retried,
    /// or failed terminally — the durable queue owns it again).
    #[allow(clippy::too_many_arguments)]
    fn advance_one(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        mut execution: ActiveExecution,
        now: SystemTime,
        now_inst: Instant,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        match execution.stage {
            ActiveStage::Planner { permit } => {
                let outcome = plan_intent(app, env, state_db, &execution.intent, now);
                app.release_work(permit);
                match outcome {
                    PlanOutcome::Noop(reason) => {
                        crate::logging::debug(
                            "Intent completed as a no-op during planning",
                            &[
                                ("intent_id", execution.intent.id.to_string()),
                                ("reason", reason.to_string()),
                            ],
                        );
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    PlanOutcome::AppliedLocally => {
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    PlanOutcome::ConflictResolved => {
                        report.conflicts += 1;
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    PlanOutcome::Upload(plan) => {
                        execution.stage = ActiveStage::WaitingForHash { plan };
                        execution.stage_started_inst = now_inst;
                        Ok(Some(execution))
                    }
                    PlanOutcome::RemoteDelete(plan) => {
                        execution.stage = ActiveStage::WaitingForUpload { plan };
                        execution.stage_started_inst = now_inst;
                        Ok(Some(execution))
                    }
                    PlanOutcome::Download(plan) => {
                        execution.stage = ActiveStage::WaitingForDownload { plan };
                        execution.stage_started_inst = now_inst;
                        Ok(Some(execution))
                    }
                    PlanOutcome::Fail { failure, message } => {
                        self.resolve_failure(
                            app,
                            state_db,
                            &execution.intent,
                            failure,
                            &message,
                            now,
                            report,
                        )?;
                        Ok(None)
                    }
                }
            }

            ActiveStage::WaitingForHash { plan } => {
                let Ok(permit) = app.try_acquire_work(WorkClass::Hash) else {
                    execution.stage = ActiveStage::WaitingForHash { plan };
                    return Ok(Some(execution));
                };
                // The provider's algorithm decides how local content is
                // hashed so local/remote comparisons agree (SHA-256 for
                // the filesystem provider, MD5 for Google Drive).
                let algorithm = app.provider().content_hash_algorithm();
                match StreamingFileHash::open(&plan.local_path, algorithm) {
                    Ok(hasher) => {
                        execution.stage = ActiveStage::Hash {
                            permit,
                            plan,
                            hasher,
                        };
                        execution.stage_started_inst = now_inst;
                        Ok(Some(execution))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        // The file vanished after planning: the pending
                        // Delete intent (or reconcile) owns convergence.
                        app.release_work(permit);
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    Err(error) => {
                        app.release_work(permit);
                        self.resolve_failure(
                            app,
                            state_db,
                            &execution.intent,
                            RetryFailureKind::Transient,
                            &format!("cannot open local file for hashing: {error}"),
                            now,
                            report,
                        )?;
                        Ok(None)
                    }
                }
            }

            ActiveStage::Hash {
                permit,
                mut plan,
                mut hasher,
            } => {
                // Throttle discipline: under Suspended, hashing stops at
                // its slice checkpoint — the permit and progress are
                // held, no new bytes are read (AGENTS.md §3).
                if !app.workgate_snapshot().caps.allow_hashing {
                    execution.stage = ActiveStage::Hash {
                        permit,
                        plan,
                        hasher,
                    };
                    return Ok(Some(execution));
                }
                match hasher.step(constants::engine::HASH_STAGE_STEP_BYTES) {
                    Ok(Some(content_hash)) => {
                        app.release_work(permit);
                        plan.content_hash = Some(content_hash);
                        execution.stage = ActiveStage::WaitingForUpload { plan };
                        execution.stage_started_inst = now_inst;
                        Ok(Some(execution))
                    }
                    Ok(None) => {
                        execution.stage = ActiveStage::Hash {
                            permit,
                            plan,
                            hasher,
                        };
                        Ok(Some(execution))
                    }
                    Err(error) => {
                        app.release_work(permit);
                        if error.kind() == std::io::ErrorKind::NotFound {
                            self.complete(state_db, &execution.intent, report)?;
                        } else {
                            self.resolve_failure(
                                app,
                                state_db,
                                &execution.intent,
                                RetryFailureKind::Transient,
                                &format!("hash stage failed: {error}"),
                                now,
                                report,
                            )?;
                        }
                        Ok(None)
                    }
                }
            }

            ActiveStage::WaitingForUpload { plan } => {
                let Ok(permit) = app.try_acquire_work(WorkClass::Upload) else {
                    execution.stage = ActiveStage::WaitingForUpload { plan };
                    return Ok(Some(execution));
                };
                if execution.intent.kind == PendingIntentKind::Delete {
                    execution.stage = ActiveStage::Upload {
                        permit,
                        plan,
                        work: UploadWork::RemoteDelete,
                    };
                    execution.stage_started_inst = now_inst;
                    return Ok(Some(execution));
                }
                if plan.verify_remote_before_upload {
                    // Two-way upload onto an unindexed remote object:
                    // identical content is silent convergence; divergent
                    // content is a genuine conflict (deterministic resolution
                    // for first-sync overlaps and index loss).
                    match app.provider().content_hash(&plan.remote_path) {
                        Ok(remote_hash) if Some(&remote_hash) == plan.content_hash.as_ref() => {
                            app.release_work(permit);
                            record_upload_index(
                                state_db,
                                &plan,
                                &remote_hash,
                                local_size(&plan.local_path),
                                now,
                            );
                            self.complete(state_db, &execution.intent, report)?;
                            return Ok(None);
                        }
                        Ok(_) => {
                            app.release_work(permit);
                            self.finish_as_conflict(
                                app,
                                state_db,
                                env,
                                &execution.intent,
                                now,
                                report,
                            )?;
                            return Ok(None);
                        }
                        Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                            // Remote vanished since planning: proceed as a
                            // fresh create.
                        }
                        Err(error) => {
                            app.release_work(permit);
                            self.resolve_provider_failure(
                                app,
                                state_db,
                                &execution.intent,
                                error,
                                now,
                                report,
                            )?;
                            return Ok(None);
                        }
                    }
                }
                let request = UploadRequest {
                    local_source: plan.local_path.clone(),
                    remote_path: plan.remote_path.clone(),
                    op_id: plan.op_id.clone(),
                    precondition: plan.precondition.clone(),
                };
                match app.provider().begin_upload(request) {
                    Ok(session) => {
                        execution.stage = ActiveStage::Upload {
                            permit,
                            plan,
                            work: UploadWork::Session(session),
                        };
                        execution.stage_started_inst = now_inst;
                        Ok(Some(execution))
                    }
                    Err(error) => {
                        app.release_work(permit);
                        self.resolve_provider_failure(
                            app,
                            state_db,
                            &execution.intent,
                            error,
                            now,
                            report,
                        )?;
                        Ok(None)
                    }
                }
            }

            ActiveStage::Upload {
                permit,
                plan,
                work: UploadWork::RemoteDelete,
            } => {
                let result = app.provider().delete(&plan.remote_path, &plan.op_id);
                app.release_work(permit);
                match result {
                    Ok(()) => {
                        env.remote_echoes
                            .record_delete(plan.remote_path.as_str(), now);
                        record_delete_tombstone(
                            state_db,
                            &plan.local_path,
                            crate::state_db::TombstoneOrigin::Local,
                            now,
                        );
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                        // Deleting something already gone is convergence.
                        env.remote_echoes
                            .record_delete(plan.remote_path.as_str(), now);
                        record_delete_tombstone(
                            state_db,
                            &plan.local_path,
                            crate::state_db::TombstoneOrigin::Local,
                            now,
                        );
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    Err(error) => {
                        self.resolve_provider_failure(
                            app,
                            state_db,
                            &execution.intent,
                            error,
                            now,
                            report,
                        )?;
                        Ok(None)
                    }
                }
            }

            ActiveStage::Upload {
                permit,
                plan,
                work: UploadWork::Session(mut session),
            } => {
                if !app.workgate_snapshot().caps.allow_uploads {
                    execution.stage = ActiveStage::Upload {
                        permit,
                        plan,
                        work: UploadWork::Session(session),
                    };
                    return Ok(Some(execution));
                }
                let step_budget = grant_transfer_budget(env, &self.clock);
                if step_budget == 0 {
                    // Bandwidth ceiling exhausted: hold at the slice
                    // checkpoint until tokens refill.
                    execution.stage = ActiveStage::Upload {
                        permit,
                        plan,
                        work: UploadWork::Session(session),
                    };
                    return Ok(Some(execution));
                }
                match session.step(step_budget) {
                    Ok(TransferStep::Progressed { .. }) => {
                        execution.stage = ActiveStage::Upload {
                            permit,
                            plan,
                            work: UploadWork::Session(session),
                        };
                        Ok(Some(execution))
                    }
                    Ok(TransferStep::Completed(outcome)) => {
                        app.release_work(permit);
                        env.remote_echoes.record_write(
                            plan.remote_path.as_str(),
                            Some(plan.op_id.clone()),
                            Some(outcome.content_hash.clone()),
                            Some(outcome.bytes_total),
                            now,
                        );
                        record_upload_index(
                            state_db,
                            &plan,
                            &outcome.content_hash,
                            outcome.bytes_total,
                            now,
                        );
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    Err(error) => {
                        session.abort();
                        app.release_work(permit);
                        if error.kind == vapor_shared::ProviderErrorKind::PreconditionFailed
                            && env.sync_mode == vapor_shared::SyncMode::TwoWay
                        {
                            // The remote changed underneath the guarded
                            // upload: a race lost by this side. Keep both
                            //.
                            self.finish_as_conflict(
                                app,
                                state_db,
                                env,
                                &execution.intent,
                                now,
                                report,
                            )?;
                            return Ok(None);
                        }
                        self.resolve_provider_failure(
                            app,
                            state_db,
                            &execution.intent,
                            error,
                            now,
                            report,
                        )?;
                        Ok(None)
                    }
                }
            }

            ActiveStage::WaitingForDownload { plan } => {
                let Ok(permit) = app.try_acquire_work(WorkClass::Download) else {
                    execution.stage = ActiveStage::WaitingForDownload { plan };
                    return Ok(Some(execution));
                };
                let staging = plan
                    .staging_path
                    .clone()
                    .expect("download plans always carry a staging path");
                match app.provider().begin_download(DownloadRequest {
                    remote_path: plan.remote_path.clone(),
                    destination: staging,
                }) {
                    Ok(session) => {
                        execution.stage = ActiveStage::Download {
                            permit,
                            plan,
                            session,
                        };
                        execution.stage_started_inst = now_inst;
                        Ok(Some(execution))
                    }
                    Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                        app.release_work(permit);
                        if env.sync_mode == vapor_shared::SyncMode::PullOnly {
                            // Strict mirror: a pull-only restore that finds
                            // no remote counterpart means the local file is
                            // local-only content — remove it.
                            match apply_remote_delete_locally(env, &execution.intent.path, now) {
                                PlanOutcome::AppliedLocally => report.mirror_deletes += 1,
                                PlanOutcome::Noop(_) => {}
                                PlanOutcome::Fail { failure, message } => {
                                    self.resolve_failure(
                                        app,
                                        state_db,
                                        &execution.intent,
                                        failure,
                                        &message,
                                        now,
                                        report,
                                    )?;
                                    return Ok(None);
                                }
                                _ => unreachable!("local delete apply has no other outcomes"),
                            }
                        }
                        // Otherwise the remote object vanished between the
                        // feed event and now; the Removed change follows.
                        self.complete(state_db, &execution.intent, report)?;
                        Ok(None)
                    }
                    Err(error) => {
                        app.release_work(permit);
                        self.resolve_provider_failure(
                            app,
                            state_db,
                            &execution.intent,
                            error,
                            now,
                            report,
                        )?;
                        Ok(None)
                    }
                }
            }

            ActiveStage::Download {
                permit,
                plan,
                mut session,
            } => {
                if !app.workgate_snapshot().caps.allow_downloads {
                    execution.stage = ActiveStage::Download {
                        permit,
                        plan,
                        session,
                    };
                    return Ok(Some(execution));
                }
                let step_budget = grant_transfer_budget(env, &self.clock);
                if step_budget == 0 {
                    execution.stage = ActiveStage::Download {
                        permit,
                        plan,
                        session,
                    };
                    return Ok(Some(execution));
                }
                match session.step(step_budget) {
                    Ok(TransferStep::Progressed { .. }) => {
                        execution.stage = ActiveStage::Download {
                            permit,
                            plan,
                            session,
                        };
                        Ok(Some(execution))
                    }
                    Ok(TransferStep::Completed(outcome)) => {
                        app.release_work(permit);
                        // Two-way keep-both: applying a download
                        // over a locally-diverged file must not lose the
                        // local edit. The loser (local) moves to its
                        // conflict-copy path first, and the copy uploads
                        // through a durably enqueued intent.
                        if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                            match preserve_diverged_local_before_apply(
                                env,
                                state_db,
                                &execution.intent,
                                &outcome.content_hash,
                                now,
                            ) {
                                Ok(preserved) => {
                                    if preserved {
                                        report.conflicts += 1;
                                    }
                                }
                                Err(message) => {
                                    self.resolve_failure(
                                        app,
                                        state_db,
                                        &execution.intent,
                                        RetryFailureKind::Transient,
                                        &message,
                                        now,
                                        report,
                                    )?;
                                    return Ok(None);
                                }
                            }
                        }
                        match apply_downloaded_payload(env, &plan) {
                            Ok(()) => {
                                env.local_echoes.record_write(
                                    path_key(&plan.local_path),
                                    Some(plan.op_id.clone()),
                                    Some(outcome.content_hash.clone()),
                                    Some(outcome.bytes_total),
                                    now,
                                );
                                record_download_index(
                                    state_db,
                                    &plan,
                                    &outcome.content_hash,
                                    outcome.bytes_total,
                                    now,
                                );
                                self.complete(state_db, &execution.intent, report)?;
                                Ok(None)
                            }
                            Err(error) => {
                                self.resolve_failure(
                                    app,
                                    state_db,
                                    &execution.intent,
                                    RetryFailureKind::Transient,
                                    &format!("local apply of downloaded payload failed: {error}"),
                                    now,
                                    report,
                                )?;
                                Ok(None)
                            }
                        }
                    }
                    Err(error) => {
                        session.abort();
                        app.release_work(permit);
                        if error.kind == vapor_shared::ProviderErrorKind::NotFound {
                            self.complete(state_db, &execution.intent, report)?;
                        } else {
                            self.resolve_provider_failure(
                                app,
                                state_db,
                                &execution.intent,
                                error,
                                now,
                                report,
                            )?;
                        }
                        Ok(None)
                    }
                }
            }
        }
    }

    fn complete(
        &mut self,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        if !state_db.complete_leased(intent.id)? {
            // The row is gone or no longer leased — a recovered-elsewhere
            // or externally-mutated intent. Dropping this one execution is
            // safe (at-least-once semantics); killing the whole daemon
            // over one inconsistent row is not.
            crate::logging::warning(
                "Dropped staged execution whose durable intent was no longer leased",
                &[("intent_id", intent.id.to_string())],
            );
            return Ok(());
        }
        report.completed += 1;
        Ok(())
    }

    fn resolve_provider_failure(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        error: ProviderError,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        let failure = error.kind.retry_classification();
        self.resolve_failure(app, state_db, intent, failure, &error.message, now, report)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_failure(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        failure: RetryFailureKind,
        message: &str,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        match failure {
            RetryFailureKind::Transient | RetryFailureKind::RateLimited { .. } => {
                app.schedule_retry(state_db, intent.id, failure, message, now)?;
                report.retried += 1;
            }
            RetryFailureKind::Authentication | RetryFailureKind::Permanent => {
                app.finalize_failure(state_db, intent.id, failure, message, now)?;
                report.failed += 1;
                crate::logging::error(
                    "Intent failed terminally",
                    &[
                        ("intent_id", intent.id.to_string()),
                        ("path", intent.path.display().to_string()),
                        ("failure", failure.label().to_string()),
                        ("error", message.to_string()),
                    ],
                );
            }
        }
        Ok(())
    }

    pub fn admission_capacity(&self, workgate: WorkgateSnapshot) -> usize {
        max_in_flight_items(workgate).saturating_sub(self.active.len())
    }
}

impl Default for StagedExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ActiveExecution {
    fn stage_name(&self) -> ExecutionStage {
        match self.stage {
            ActiveStage::Planner { .. } => ExecutionStage::Planner,
            ActiveStage::WaitingForHash { .. } => ExecutionStage::WaitingForHash,
            ActiveStage::Hash { .. } => ExecutionStage::Hash,
            ActiveStage::WaitingForUpload { .. } => ExecutionStage::WaitingForUpload,
            ActiveStage::Upload { .. } => ExecutionStage::Upload,
            ActiveStage::WaitingForDownload { .. } => ExecutionStage::WaitingForDownload,
            ActiveStage::Download { .. } => ExecutionStage::Download,
        }
    }
}

/// Plans one leased intent into its execution route. Cheap by design:
/// a stat, a path derivation, and an op-id allocation — no hashing, no
/// I/O beyond metadata (the fs-watch callback discipline's cousin).
fn plan_intent(
    app: &DaemonApp,
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    now: SystemTime,
) -> PlanOutcome {
    let Some(local_root) = env.local_root else {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Permanent,
            message: "no local sync directory configured".to_string(),
        };
    };
    let Some(remote_path) = RemotePath::from_local(local_root, &intent.path) else {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Permanent,
            message: format!(
                "intent path {} is not inside the local sync root",
                intent.path.display()
            ),
        };
    };
    let op_id = allocate_op_id(env, intent, now);

    // Direction gates: a one-way mode drops intents of
    // the gated direction as logged no-ops. This also absorbs stale
    // intents that were durably enqueued before a mode change.
    if matches!(
        intent.kind,
        PendingIntentKind::Upload | PendingIntentKind::Rename | PendingIntentKind::Delete
    ) && !env.sync_mode.allows_local_to_remote()
    {
        return PlanOutcome::Noop("local-to-remote propagation is gated off in pull-only mode")
            .tap_provider(app);
    }
    if matches!(
        intent.kind,
        PendingIntentKind::Download | PendingIntentKind::ApplyRemoteDelete
    ) && !env.sync_mode.allows_remote_to_local()
    {
        return PlanOutcome::Noop("remote-to-local propagation is gated off in push-only mode")
            .tap_provider(app);
    }

    match intent.kind {
        PendingIntentKind::Upload | PendingIntentKind::Rename => {
            match fs::symlink_metadata(&intent.path) {
                Ok(metadata) if metadata.is_dir() => {
                    PlanOutcome::Noop("directories materialize through their children")
                }
                Ok(metadata) if !metadata.is_file() => {
                    // Symlinks, FIFOs, sockets, device nodes. Must be
                    // refused at planning: opening a FIFO for hashing
                    // blocks until a writer appears, and the intent
                    // otherwise sits in the queue forever (observed as
                    // a permanently-WaitingForHash row).
                    PlanOutcome::Noop("symlinks and special files are outside the sync contract")
                }
                Ok(_) => plan_upload(app, env, state_db, intent, remote_path, op_id, now),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    PlanOutcome::Noop("local file vanished before upload")
                }
                Err(error) => PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot stat local file: {error}"),
                },
            }
        }
        PendingIntentKind::Delete => PlanOutcome::RemoteDelete(TransferPlan {
            remote_path,
            op_id,
            local_path: intent.path.clone(),
            staging_path: None,
            content_hash: None,
            precondition: RemotePrecondition::None,
            verify_remote_before_upload: false,
        }),
        PendingIntentKind::Download => {
            let staging_name = format!(
                "{}dl-{}",
                constants::provider::TEMP_FILE_PREFIX,
                sanitize_for_file_name(&op_id)
            );
            let staging_path = intent
                .path
                .parent()
                .unwrap_or(local_root)
                .join(staging_name);
            PlanOutcome::Download(TransferPlan {
                remote_path,
                op_id,
                local_path: intent.path.clone(),
                staging_path: Some(staging_path),
                content_hash: None,
                precondition: RemotePrecondition::None,
                verify_remote_before_upload: false,
            })
        }
        PendingIntentKind::ApplyRemoteDelete => {
            // Two-way deletion guard: "data preservation wins
            // over deletion". A remote deletion only applies when the
            // local copy is exactly what was last synced AND the sync
            // happened before the deletion was observed. A modified (or
            // unknown-provenance) local file survives; the pending
            // upload restores it remotely. One-way pull mirrors delete
            // unconditionally — that is their contract.
            if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                match deletion_loses_to_local_state(state_db, intent) {
                    Ok(Some(reason)) => return PlanOutcome::Noop(reason).tap_provider(app),
                    Ok(None) => {}
                    Err(message) => {
                        return PlanOutcome::Fail {
                            failure: RetryFailureKind::Transient,
                            message,
                        }
                        .tap_provider(app);
                    }
                }
            }
            let outcome = apply_remote_delete_locally(env, &intent.path, now);
            if matches!(outcome, PlanOutcome::AppliedLocally) {
                record_delete_tombstone(
                    state_db,
                    &intent.path,
                    crate::state_db::TombstoneOrigin::Remote,
                    now,
                );
            }
            outcome
        }
        PendingIntentKind::ReconcileSubtree => PlanOutcome::Fail {
            failure: RetryFailureKind::Permanent,
            message: "reconcile intents are routed to the reconcile controller, not the executor"
                .to_string(),
        },
    }
    .tap_provider(app)
}

impl PlanOutcome {
    fn tap_provider(self, _app: &DaemonApp) -> Self {
        self
    }
}

/// Plans an upload with the two-way conflict guard. The
/// sync index distinguishes "remote unchanged since our last sync"
/// (safe overwrite, hash-guarded) from "remote changed by another
/// writer" (keep both). One-way modes skip the guard entirely: strict
/// mirror overwrites by design.
fn plan_upload(
    app: &DaemonApp,
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    remote_path: RemotePath,
    op_id: String,
    now: SystemTime,
) -> PlanOutcome {
    let mut plan = TransferPlan {
        remote_path,
        op_id,
        local_path: intent.path.clone(),
        staging_path: None,
        content_hash: None,
        precondition: RemotePrecondition::None,
        verify_remote_before_upload: false,
    };
    if env.sync_mode != vapor_shared::SyncMode::TwoWay {
        return PlanOutcome::Upload(plan);
    }

    let index = match state_db.sync_index(&intent.path) {
        Ok(index) => index,
        Err(error) => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot read sync index: {error}"),
            };
        }
    };
    match app.provider().stat(&plan.remote_path) {
        Ok(None) => {
            // Remote absent. With an index this is a delete/modify race:
            // the modification wins over the deletion (data
            // preservation); either way the upload is a guarded fresh create.
            plan.precondition = RemotePrecondition::Absent;
            PlanOutcome::Upload(plan)
        }
        Ok(Some(remote_entry)) => match index {
            Some(index) if remote_entry.op_id.as_deref() == Some(index.last_op_id.as_str()) => {
                // Remote unchanged since our last sync: overwrite,
                // guarded against the tiny window between this stat and
                // the upload landing.
                plan.precondition = RemotePrecondition::HashEquals(index.content_hash);
                PlanOutcome::Upload(plan)
            }
            Some(index) => {
                // An op-id mismatch is not yet divergence: the tag is
                // absent on any externally-written remote file (which we
                // may have already synced *from*), and provider-side
                // copies can strip tags. The content hash is the ground
                // truth — an equal hash means the remote is
                // byte-identical to our last sync, so the upload is a
                // safe guarded overwrite. Without this check, editing a
                // file whose last change arrived from an untagged
                // external write manufactured a keep-both conflict on
                // every upload (and could revert a just-resolved one).
                let remote_hash = remote_entry
                    .content_hash
                    .clone()
                    .or_else(|| app.provider().content_hash(&plan.remote_path).ok());
                if remote_hash.as_deref() == Some(index.content_hash.as_str()) {
                    plan.precondition = RemotePrecondition::HashEquals(index.content_hash);
                    PlanOutcome::Upload(plan)
                } else {
                    // Another writer genuinely changed the remote since
                    // our last sync: concurrent divergence — keep both.
                    resolve_upload_conflict(env, state_db, intent, now)
                }
            }
            None => {
                // Remote exists but this path has never synced (first
                // sync overlap or index loss). Defer the decision to the
                // upload gate, where the local hash is known: identical
                // content converges, divergent content conflicts.
                plan.verify_remote_before_upload = true;
                PlanOutcome::Upload(plan)
            }
        },
        Err(error) => PlanOutcome::Fail {
            failure: error.kind.retry_classification(),
            message: format!("cannot stat remote before upload: {}", error.message),
        },
    }
}

/// Keep-both resolution when the local side lost an upload race
///: move the local loser to its conflict-copy path (suppressing
/// the rename's delete echo), enqueue an upload for the copy and a
/// download for the remote canonical, and let the caller complete the
/// original intent. Deterministic: the conflict path derives from the
/// device id and the intent's durable event time.
fn resolve_upload_conflict(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    now: SystemTime,
) -> PlanOutcome {
    let timestamp_ms = intent
        .enqueued_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let conflict_local = crate::conflict::conflict_copy_path(
        &intent.path,
        env.device_id,
        timestamp_ms,
        |candidate: &Path| candidate.exists(),
    );
    if let Err(error) = fs::rename(&intent.path, &conflict_local) {
        if error.kind() == std::io::ErrorKind::NotFound {
            // The local loser vanished mid-conflict: nothing to keep.
            return PlanOutcome::Noop("local file vanished during conflict resolution");
        }
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot stage conflict copy: {error}"),
        };
    }
    // The rename emits Removed(original) — an echo of our own write.
    env.local_echoes.record_delete(path_key(&intent.path), now);
    let _ = env.tags.relocate_side_file(&intent.path, &conflict_local);
    // The index entry described the pre-conflict canonical; it no longer
    // holds for either path.
    if let Err(error) = state_db.remove_sync_index(&intent.path) {
        crate::logging::warning(
            "Conflict resolution could not clear the sync index entry",
            &[("error", error.to_string())],
        );
    }
    let follow_ups = [
        (conflict_local.clone(), PendingIntentKind::Upload, now),
        (intent.path.clone(), PendingIntentKind::Download, now),
    ];
    if let Err(error) = state_db.enqueue_intents_coalesced(&follow_ups) {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot enqueue conflict follow-up intents: {error}"),
        };
    }
    crate::logging::warning(
        "Resolved concurrent divergence by keeping both versions",
        &[
            ("canonical", intent.path.display().to_string()),
            ("conflict_copy", conflict_local.display().to_string()),
        ],
    );
    PlanOutcome::ConflictResolved
}

/// Download-side keep-both: before a downloaded payload
/// replaces a local file, a locally-diverged version moves to its
/// conflict-copy path (unless its content already equals the incoming
/// payload). Returns whether a conflict copy was created; errors are
/// human-readable retry messages.
fn preserve_diverged_local_before_apply(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    incoming_hash: &str,
    now: SystemTime,
) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(&intent.path) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return Ok(false),
    };
    let index = state_db
        .sync_index(&intent.path)
        .map_err(|error| format!("cannot read sync index: {error}"))?;

    // Quick check first (size, then mtime), full hash only when needed.
    let locally_diverged = match &index {
        Some(index) => {
            if metadata.len() != index.size_bytes {
                true
            } else if index.local_modified_at.is_some()
                && metadata.modified().ok() == index.local_modified_at
            {
                false
            } else {
                let local_hash = hash_hex_of_file_or_err(&intent.path)?;
                local_hash != index.content_hash
            }
        }
        // Unknown provenance: treat as diverged unless content matches
        // the incoming payload (checked below).
        None => true,
    };
    if !locally_diverged {
        return Ok(false);
    }
    let local_hash = hash_hex_of_file_or_err(&intent.path)?;
    if local_hash == incoming_hash {
        return Ok(false);
    }

    match resolve_upload_conflict(env, state_db, intent, now) {
        PlanOutcome::ConflictResolved => Ok(true),
        PlanOutcome::Noop(_) => Ok(false),
        PlanOutcome::Fail { message, .. } => Err(message),
        _ => unreachable!("conflict resolution has no other outcomes"),
    }
}

/// Deletion guard: returns the preservation reason when a remote
/// deletion must NOT apply to the local file, `None` when the deletion
/// may proceed.
fn deletion_loses_to_local_state(
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
) -> Result<Option<&'static str>, String> {
    let metadata = match fs::symlink_metadata(&intent.path) {
        Ok(metadata) if metadata.is_file() => metadata,
        // Directories and absent paths have no unsynced content to
        // preserve; the apply path handles them.
        _ => return Ok(None),
    };
    let Some(index) = state_db
        .sync_index(&intent.path)
        .map_err(|error| format!("cannot read sync index: {error}"))?
    else {
        return Ok(Some(
            "preserving local file of unknown provenance over a remote deletion",
        ));
    };
    if index.updated_at > intent.enqueued_at {
        return Ok(Some(
            "remote deletion is older than the last sync of this path",
        ));
    }
    let diverged = if metadata.len() != index.size_bytes {
        true
    } else if index.local_modified_at.is_some()
        && metadata.modified().ok() == index.local_modified_at
    {
        false
    } else {
        hash_hex_of_file_or_err(&intent.path)? != index.content_hash
    };
    if diverged {
        return Ok(Some(
            "local file was modified after the last sync; modification wins over deletion",
        ));
    }
    Ok(None)
}

fn hash_hex_of_file_or_err(path: &Path) -> Result<String, String> {
    vapor_providers::filesystem::hash_hex_of_file(path)
        .map_err(|error| format!("cannot hash local file for conflict check: {error}"))
}

/// Records the post-upload sync-index entry; failures are logged, not
/// fatal (the next transfer overwrites the entry, and a stale index
/// resolves through the conflict-verification path).
fn record_upload_index(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    content_hash: &str,
    size_bytes: u64,
    now: SystemTime,
) {
    let local_modified_at = fs::symlink_metadata(&plan.local_path)
        .and_then(|metadata| metadata.modified())
        .ok();
    if let Err(error) = state_db.set_sync_index(
        &plan.local_path,
        content_hash,
        size_bytes,
        local_modified_at,
        &plan.op_id,
        now,
    ) {
        crate::logging::warning(
            "Could not record post-upload sync index entry",
            &[("error", error.to_string())],
        );
    }
    if let Err(error) = state_db.clear_tombstone(&plan.local_path) {
        crate::logging::warning(
            "Could not clear tombstone after upload",
            &[("error", error.to_string())],
        );
    }
}

fn record_download_index(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    content_hash: &str,
    size_bytes: u64,
    now: SystemTime,
) {
    // Identical bookkeeping; separated for call-site readability.
    record_upload_index(state_db, plan, content_hash, size_bytes, now);
}

fn record_delete_tombstone(
    state_db: &mut DurableStateDb,
    local_path: &Path,
    origin: crate::state_db::TombstoneOrigin,
    now: SystemTime,
) {
    if let Err(error) = state_db.remove_sync_index(local_path) {
        crate::logging::warning(
            "Could not clear sync index entry after delete",
            &[("error", error.to_string())],
        );
    }
    if let Err(error) = state_db.record_tombstone(local_path, origin, now) {
        crate::logging::warning(
            "Could not record deletion tombstone",
            &[("error", error.to_string())],
        );
    }
}

fn local_size(path: &Path) -> u64 {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// Method-form conflict finisher shared by the upload gate and the
/// precondition-failure arm.
impl StagedExecutor {
    fn finish_as_conflict(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: &DurableIntentRecord,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        match resolve_upload_conflict(env, state_db, intent, now) {
            PlanOutcome::ConflictResolved => {
                report.conflicts += 1;
                self.complete(state_db, intent, report)
            }
            PlanOutcome::Noop(_) => self.complete(state_db, intent, report),
            PlanOutcome::Fail { failure, message } => {
                self.resolve_failure(app, state_db, intent, failure, &message, now, report)
            }
            _ => unreachable!("conflict resolution has no other outcomes"),
        }
    }
}

/// ApplyRemoteDelete happens inline in the planner: local deletion is a
/// metadata operation, and splitting it into more stages would only add
/// latency to the remote→local pipeline.
fn apply_remote_delete_locally(
    env: &mut ExecutionEnv<'_>,
    path: &Path,
    now: SystemTime,
) -> PlanOutcome {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            PlanOutcome::Noop("local file already gone")
        }
        Err(error) => PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot stat local file for remote-delete apply: {error}"),
        },
        Ok(metadata) => {
            let removal = if metadata.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            match removal {
                Ok(()) => {
                    let _ = env.tags.remove(path);
                    env.local_echoes.record_delete(path_key(path), now);
                    PlanOutcome::AppliedLocally
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    PlanOutcome::Noop("local file already gone")
                }
                Err(error) => PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot delete local file for remote-delete apply: {error}"),
                },
            }
        }
    }
}

/// Moves a fully-downloaded staging payload into place atomically and
/// tags it with the operation id so the watcher echo correlates.
fn apply_downloaded_payload(
    env: &mut ExecutionEnv<'_>,
    plan: &TransferPlan,
) -> std::io::Result<()> {
    let staging = plan
        .staging_path
        .as_ref()
        .expect("download plans always carry a staging path");
    if let Some(parent) = plan.local_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(staging, &plan.local_path)?;
    // Tag after the rename: the side-file fallback must name the final
    // path, and the echo cache carries hash + size for the window
    // between rename and tag landing.
    if let Err(error) = env.tags.write_op_id(&plan.local_path, &plan.op_id) {
        crate::logging::warning(
            "Applied downloaded payload but could not record its op-id tag",
            &[
                ("path", plan.local_path.display().to_string()),
                ("error", error.to_string()),
            ],
        );
    }
    Ok(())
}

fn allocate_op_id(env: &ExecutionEnv<'_>, intent: &DurableIntentRecord, now: SystemTime) -> String {
    let now_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!(
        "{}-op{}-a{}-t{now_ms}",
        env.device_id, intent.id, intent.attempt_count
    )
}

fn sanitize_for_file_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// One transfer step's byte grant: the auto-tuned step budget, capped
/// by the bandwidth shaper's available tokens.
fn grant_transfer_budget(env: &ExecutionEnv<'_>, clock: &Arc<dyn Clock>) -> u64 {
    env.bandwidth
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .budget(env.transfer_step_bytes, clock.now())
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn max_in_flight_items(workgate: WorkgateSnapshot) -> usize {
    workgate.caps.planner_workers
        + workgate.caps.hash_workers
        + workgate.caps.upload_concurrency
        + workgate.caps.download_concurrency
}

/// Chunked content hash of a local file in the provider's algorithm:
/// at most `max_bytes` read per step so one hashing execution never
/// exceeds its per-tick budget.
struct StreamingFileHash {
    file: fs::File,
    hasher: HashState,
}

enum HashState {
    Sha256(Sha256),
    Md5(md5::Md5),
}

impl HashState {
    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(bytes),
            Self::Md5(hasher) => hasher.update(bytes),
        }
    }

    fn finalize_hex(&mut self) -> String {
        let digest: Vec<u8> = match self {
            Self::Sha256(hasher) => std::mem::take(hasher).finalize().to_vec(),
            Self::Md5(hasher) => std::mem::take(hasher).finalize().to_vec(),
        };
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }
}

impl StreamingFileHash {
    fn open(path: &Path, algorithm: vapor_providers::HashAlgorithm) -> std::io::Result<Self> {
        Ok(Self {
            file: fs::File::open(path)?,
            hasher: match algorithm {
                vapor_providers::HashAlgorithm::Sha256 => HashState::Sha256(Sha256::new()),
                vapor_providers::HashAlgorithm::Md5 => HashState::Md5(md5::Md5::new()),
            },
        })
    }

    /// Returns `Some(hex)` once the file is fully hashed.
    fn step(&mut self, max_bytes: u64) -> std::io::Result<Option<String>> {
        let mut remaining = max_bytes;
        let mut buffer = vec![0_u8; 64 * 1024];
        while remaining > 0 {
            let chunk = buffer.len().min(remaining as usize);
            let read = self.file.read(&mut buffer[..chunk])?;
            if read == 0 {
                return Ok(Some(self.hasher.finalize_hex()));
            }
            self.hasher.update(&buffer[..read]);
            remaining -= read as u64;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::event_intents::PendingIntentKind;
    use crate::throttle::ThrottleInputs;
    use std::time::Duration;
    use vapor_platform::fs_caps::NativeFilesystemCapabilities;
    use vapor_providers::FilesystemProvider;
    use vapor_providers::filesystem::hash_hex_of_bytes;

    struct Fixture {
        _temp: tempfile::TempDir,
        local_root: PathBuf,
        cloud_root: PathBuf,
        sync_mode: vapor_shared::SyncMode,
        bandwidth: std::sync::Mutex<vapor_providers::BandwidthShaper>,
        app: DaemonApp,
        state_db: DurableStateDb,
        executor: StagedExecutor,
        tags: OpIdTagStore,
        local_echoes: SelfWriteCache,
        remote_echoes: SelfWriteCache,
        clock: Arc<ManualClock>,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::TempDir::new().expect("temp dir");
            let local_root = temp.path().join("local");
            let cloud_root = temp.path().join("cloud");
            std::fs::create_dir_all(&local_root).expect("local root");
            std::fs::create_dir_all(&cloud_root).expect("cloud root");
            let clock = Arc::new(ManualClock::at_now());
            let provider = FilesystemProvider::with_root(&cloud_root).expect("provider");
            let app = DaemonApp::new_with_clock(Box::new(provider), clock.clone());
            let state_db = DurableStateDb::open(temp.path().join("state/vapor.sqlite"))
                .expect("open state db");
            Self {
                local_root,
                cloud_root,
                sync_mode: vapor_shared::SyncMode::TwoWay,
                bandwidth: std::sync::Mutex::new(vapor_providers::BandwidthShaper::unlimited()),
                _temp: temp,
                app,
                state_db,
                executor: StagedExecutor::with_clock(clock.clone()),
                tags: OpIdTagStore::new(Arc::new(NativeFilesystemCapabilities::for_current_host())),
                local_echoes: SelfWriteCache::new(),
                remote_echoes: SelfWriteCache::new(),
                clock,
            }
        }

        fn enqueue_and_lease(
            &mut self,
            path: &Path,
            kind: PendingIntentKind,
        ) -> DurableIntentRecord {
            self.state_db
                .enqueue_intent(path, kind, timestamp_ms(0))
                .expect("enqueue intent");
            self.state_db
                .lease_next_ready(timestamp_ms(0))
                .expect("lease next ready")
                .expect("leased intent")
        }

        /// Drives the executor until the given intent completes or the
        /// tick budget runs out. Returns the accumulated report.
        fn run_to_quiescence(&mut self, max_ticks: usize) -> StagedExecutorReport {
            let mut total = StagedExecutorReport::default();
            for _ in 0..max_ticks {
                self.clock.advance(Duration::from_millis(250));
                let mut env = ExecutionEnv {
                    local_root: Some(&self.local_root),
                    sync_mode: self.sync_mode,
                    device_id: "testdev",
                    transfer_step_bytes: constants::engine::TRANSFER_STAGE_STEP_BYTES,
                    bandwidth: &self.bandwidth,
                    tags: &self.tags,
                    local_echoes: &mut self.local_echoes,
                    remote_echoes: &mut self.remote_echoes,
                };
                let report = self
                    .executor
                    .advance(&mut self.app, &mut self.state_db, &mut env, timestamp_ms(0))
                    .expect("advance");
                total.completed += report.completed;
                total.retried += report.retried;
                total.failed += report.failed;
                if self.executor.snapshot().active_total == 0 {
                    break;
                }
            }
            total
        }
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds)
    }

    #[test]
    fn upload_intent_copies_real_bytes_to_the_provider() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("docs/report.md");
        std::fs::create_dir_all(local_file.parent().unwrap()).expect("dirs");
        std::fs::write(&local_file, b"real payload").expect("seed local");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        let uploaded = fixture.cloud_root.join("docs/report.md");
        assert_eq!(std::fs::read(&uploaded).expect("uploaded"), b"real payload");
        // The remote echo cache carries the op-id + hash of our write.
        assert!(
            fixture
                .remote_echoes
                .has_write_record("docs/report.md", timestamp_ms(1))
        );
        assert_eq!(
            fixture.state_db.queue_depth().expect("queue depth"),
            0,
            "intent must complete durably"
        );
    }

    #[test]
    fn delete_intent_removes_the_remote_object() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("a.txt");
        std::fs::write(fixture.cloud_root.join("a.txt"), b"remote copy").expect("seed remote");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert!(!fixture.cloud_root.join("a.txt").exists());
    }

    #[test]
    fn delete_of_already_missing_remote_completes_as_convergence() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("never-uploaded.txt");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
    }

    #[test]
    fn download_intent_applies_remote_content_atomically_with_op_id_tag() {
        let mut fixture = Fixture::new();
        std::fs::create_dir_all(fixture.cloud_root.join("docs")).expect("dirs");
        std::fs::write(fixture.cloud_root.join("docs/new.txt"), b"from the cloud")
            .expect("seed remote");
        let local_target = fixture.local_root.join("docs/new.txt");

        let intent = fixture.enqueue_and_lease(&local_target, PendingIntentKind::Download);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(&local_target).expect("applied payload"),
            b"from the cloud"
        );
        // Op-id tagged for watcher-echo correlation.
        assert!(fixture.tags.read_op_id(&local_target).is_some());
        // Local echo cache carries the write (hash matches content).
        assert!(fixture.local_echoes.matches_write(
            &local_target.to_string_lossy(),
            fixture.tags.read_op_id(&local_target).as_deref(),
            None,
            timestamp_ms(1),
        ));
        assert!(fixture.local_echoes.matches_write(
            &local_target.to_string_lossy(),
            None,
            Some(&hash_hex_of_bytes(b"from the cloud")),
            timestamp_ms(1),
        ));
        // No staging temp file survives.
        let staging_leftovers: Vec<_> = std::fs::read_dir(fixture.local_root.join("docs"))
            .expect("read docs")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(constants::provider::TEMP_FILE_PREFIX)
            })
            .collect();
        assert!(staging_leftovers.is_empty());
    }

    #[test]
    fn apply_remote_delete_removes_local_file_and_records_echo() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("gone.txt");
        std::fs::write(&local_file, b"stale").expect("seed local");
        // The file was previously synced: the index matches its current
        // content, so the preservation guard lets the deletion
        // proceed.
        let mtime = std::fs::symlink_metadata(&local_file)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"stale"),
                5,
                mtime,
                "op-past",
                timestamp_ms(0),
            )
            .expect("seed sync index");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::ApplyRemoteDelete);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert!(!local_file.exists());
        assert!(
            fixture
                .local_echoes
                .matches_delete(&local_file.to_string_lossy(), timestamp_ms(1))
        );
    }

    #[test]
    fn upload_over_untagged_remote_matching_the_index_overwrites_without_conflict() {
        // The remote was last written externally (no op-id tag) and we
        // synced *from* it, so the index holds its exact hash. A local
        // edit must upload as a guarded overwrite — the op-id mismatch
        // alone is not divergence. The old behavior manufactured a
        // keep-both copy here on every such upload (and could revert a
        // freshly resolved conflict, the S15 e2e flake).
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("doc.txt");
        std::fs::write(&local_file, b"local edit v2").expect("seed local");
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"external content")
            .expect("seed remote externally");
        let mtime = std::fs::symlink_metadata(&local_file)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"external content"),
                16,
                mtime,
                "op-of-the-download",
                timestamp_ms(0),
            )
            .expect("seed sync index");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(32);

        assert_eq!(report.completed, 1);
        assert_eq!(report.conflicts, 0, "no keep-both copy may be created");
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("doc.txt")).expect("remote bytes"),
            b"local edit v2"
        );
        for root in [&fixture.local_root, &fixture.cloud_root] {
            let conflicts: Vec<_> = std::fs::read_dir(root)
                .expect("read root")
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains("~conflict-"))
                .collect();
            assert!(
                conflicts.is_empty(),
                "spurious conflict copies: {conflicts:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn upload_of_a_fifo_completes_as_noop_instead_of_wedging() {
        // Special files are outside the sync contract and must be
        // refused at planning: hashing a FIFO blocks until a writer
        // appears, and before this guard the intent sat in the queue
        // forever as a permanently-WaitingForHash row.
        let mut fixture = Fixture::new();
        let fifo = fixture.local_root.join("pipe.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(status.success(), "mkfifo must succeed");

        let intent = fixture.enqueue_and_lease(&fifo, PendingIntentKind::Upload);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1, "the intent must complete, not wedge");
        assert_eq!(report.failed, 0);
        assert!(
            !fixture.cloud_root.join("pipe.fifo").exists(),
            "no remote object may be created for a special file"
        );
    }

    #[test]
    fn upload_of_vanished_local_file_completes_as_noop() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("ephemeral.txt");
        // Never created on disk: mimics a file deleted between event and
        // lease.

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert!(!fixture.cloud_root.join("ephemeral.txt").exists());
    }

    #[test]
    fn download_failure_is_scheduled_for_retry_with_transient_classification() {
        let mut fixture = Fixture::new();
        // Remote object exists at plan time and disappears before the
        // session starts? NotFound completes as noop — so instead break
        // the transfer mid-flight by pointing at a directory the
        // provider will fail to open as a file.
        std::fs::create_dir_all(fixture.cloud_root.join("hole")).expect("dirs");
        let local_target = fixture.local_root.join("hole");

        let intent = fixture.enqueue_and_lease(&local_target, PendingIntentKind::Download);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        // Opening a directory as a file yields a transient error →
        // retry, not terminal failure.
        assert_eq!(report.failed, 0);
        assert_eq!(report.retried, 1);
        assert_eq!(fixture.state_db.queue_depth().expect("depth"), 1);
    }

    #[test]
    fn same_path_intents_are_serialized() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("contended.txt");
        std::fs::write(&local_file, b"payload").expect("seed");

        fixture.app.apply_throttle_inputs(ThrottleInputs::default());
        let first = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        fixture
            .state_db
            .enqueue_intent(&local_file, PendingIntentKind::Delete, timestamp_ms(0))
            .expect("enqueue second");
        let second = fixture
            .state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease")
            .expect("second intent");

        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, first, timestamp_ms(0))
        );
        assert!(
            !fixture
                .executor
                .try_start(&mut fixture.app, second, timestamp_ms(0)),
            "second intent on the same path must not start concurrently"
        );
    }

    #[test]
    fn staged_executor_respects_workgate_caps() {
        let mut fixture = Fixture::new();
        // User activity drives the throttle to `Throttled`: exactly one
        // planner worker is available.
        fixture.app.apply_throttle_inputs(ThrottleInputs {
            user_active: true,
            ..ThrottleInputs::default()
        });

        for index in 0..2 {
            let path = fixture.local_root.join(format!("file-{index}.txt"));
            std::fs::write(&path, b"x").expect("seed");
            fixture
                .state_db
                .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(0))
                .expect("enqueue intent");
        }

        let first = fixture
            .state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease")
            .expect("first");
        let second = fixture
            .state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease")
            .expect("second");

        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, first, timestamp_ms(0))
        );
        assert!(
            !fixture
                .executor
                .try_start(&mut fixture.app, second, timestamp_ms(0)),
            "throttled state must cap planner admission at one"
        );
        assert_eq!(fixture.executor.snapshot().planner_running, 1);
    }

    #[test]
    fn suspended_throttle_holds_in_flight_transfers_at_slice_checkpoints() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("held.bin");
        let payload = vec![7_u8; (constants::engine::TRANSFER_STAGE_STEP_BYTES * 3) as usize];
        std::fs::write(&local_file, &payload).expect("seed large");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );

        // Drive into the transfer, then suspend.
        let _ = fixture.run_to_quiescence(4);
        assert!(
            fixture.executor.snapshot().active_total == 1,
            "large upload must still be in flight"
        );
        fixture.app.apply_throttle_inputs(ThrottleInputs {
            system_cpu_load_percent: 95,
            ..ThrottleInputs::default()
        });
        assert_eq!(
            fixture.app.snapshot().throttle_state,
            vapor_shared::ThrottleState::Suspended
        );

        // Suspended: many ticks pass, nothing completes, nothing fails —
        // the transfer holds at its slice checkpoint with progress kept.
        let held = fixture.run_to_quiescence(6);
        assert_eq!(held.completed, 0);
        assert_eq!(held.failed, 0);
        assert_eq!(fixture.executor.snapshot().active_total, 1);

        // Relax the throttle: the held transfer resumes and completes.
        // (Suspended has a 1s min dwell; advance the clock past it.)
        fixture.clock.advance(Duration::from_secs(2));
        fixture.app.apply_throttle_inputs(ThrottleInputs::default());
        let resumed = fixture.run_to_quiescence(16);
        assert_eq!(resumed.completed, 1);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("held.bin"))
                .expect("uploaded")
                .len(),
            payload.len()
        );
    }

    #[test]
    fn large_upload_progresses_across_multiple_ticks() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("large.bin");
        // Larger than one hash step + one transfer step so the pipeline
        // must take multiple advances.
        let payload = vec![42_u8; (constants::engine::HASH_STAGE_STEP_BYTES + 1024) as usize];
        std::fs::write(&local_file, &payload).expect("seed large");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );

        // One advance cannot complete it (planner only).
        fixture.clock.advance(Duration::from_millis(250));
        let mut env = ExecutionEnv {
            local_root: Some(&fixture.local_root),
            sync_mode: fixture.sync_mode,
            device_id: "testdev",
            transfer_step_bytes: constants::engine::TRANSFER_STAGE_STEP_BYTES,
            bandwidth: &fixture.bandwidth,
            tags: &fixture.tags,
            local_echoes: &mut fixture.local_echoes,
            remote_echoes: &mut fixture.remote_echoes,
        };
        let first = fixture
            .executor
            .advance(
                &mut fixture.app,
                &mut fixture.state_db,
                &mut env,
                timestamp_ms(0),
            )
            .expect("advance");
        assert_eq!(first.completed, 0);
        assert!(fixture.executor.snapshot().active_total == 1);

        let report = fixture.run_to_quiescence(32);
        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("large.bin"))
                .expect("uploaded")
                .len(),
            payload.len()
        );
    }
}
