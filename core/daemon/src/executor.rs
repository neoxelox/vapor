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
    /// The provider's content-hash algorithm. Every local hash compared
    /// against an index/outcome/echo hash must use this algorithm — the
    /// index and transfer outcomes carry provider-algorithm hashes (MD5
    /// for Google Drive), so hashing local files with a hard-coded
    /// SHA-256 would never match.
    pub hash_algorithm: vapor_providers::HashAlgorithm,
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
    /// (size, mtime) captured when the hash stage opened the file. Lets
    /// the post-upload index write detect a mid-transfer edit and decline
    /// to record a stale mtime.
    hashed_local_state: Option<(u64, Option<SystemTime>)>,
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

    /// Intent ids of every execution currently held in-process. The
    /// runtime renews their leases before the stale-lease sweep so a long
    /// transfer (or a Suspended stall past the lease timeout) is not
    /// reclaimed out from under a live execution.
    pub fn active_intent_ids(&self) -> Vec<i64> {
        self.active.keys().copied().collect()
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

            ActiveStage::WaitingForHash { mut plan } => {
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
                        // Capture (size, mtime) as the hash begins so a
                        // mid-transfer edit can be detected at index-write
                        // time (a same-length edit would otherwise pair a
                        // post-edit mtime with the pre-edit content hash and
                        // let the mtime fast-path skip a needed re-hash).
                        plan.hashed_local_state = fs::symlink_metadata(&plan.local_path)
                            .map(|metadata| (metadata.len(), metadata.modified().ok()))
                            .ok();
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
                            match apply_remote_delete_locally(
                                env,
                                state_db,
                                &execution.intent.path,
                                now,
                            ) {
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
                        // Two-way keep-both must not lose a local edit that
                        // lands while the download applies. The apply
                        // captures the current local bytes (rename aside)
                        // before renaming the payload in, then decides
                        // divergence on the exact displaced bytes — closing
                        // the check-then-act window a pre-apply hash left
                        // open.
                        let apply_result = if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                            apply_downloaded_payload_keep_both(
                                env,
                                state_db,
                                &execution.intent,
                                &plan,
                                &outcome.content_hash,
                                now,
                            )
                        } else {
                            apply_downloaded_payload(env, &plan)
                                .map(|()| false)
                                .map_err(|error| {
                                    format!("local apply of downloaded payload failed: {error}")
                                })
                        };
                        match apply_result {
                            Ok(conflict_created) => {
                                if conflict_created {
                                    report.conflicts += 1;
                                }
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
            // Retry budget exhausted: finalize as a permanent failure
            // rather than calling schedule_retry (which refuses past the
            // cap) and letting the stale-lease sweep re-pend it forever.
            RetryFailureKind::Transient | RetryFailureKind::RateLimited { .. }
                if intent.attempt_count >= constants::state::MAX_ATTEMPT_COUNT =>
            {
                let message = format!(
                    "{message} (retry budget exhausted after {} attempts)",
                    intent.attempt_count
                );
                app.finalize_failure(
                    state_db,
                    intent.id,
                    RetryFailureKind::Permanent,
                    &message,
                    now,
                )?;
                report.failed += 1;
                crate::logging::error(
                    "Intent failed terminally after exhausting its retry budget",
                    &[
                        ("intent_id", intent.id.to_string()),
                        ("path", intent.path.display().to_string()),
                        ("attempts", intent.attempt_count.to_string()),
                    ],
                );
            }
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

    /// Drops every in-flight execution, releasing its shared-workgate
    /// permit and aborting any transfer session. The leased durable rows
    /// stay leased and are recovered by the stale-lease sweep; this only
    /// reclaims the in-memory permits so a suspended/aborted profile does
    /// not leak daemon-wide concurrency slots to healthy profiles.
    pub fn abort_all(&mut self, app: &mut DaemonApp) {
        let active = std::mem::take(&mut self.active);
        self.active_paths.clear();
        for (_id, execution) in active {
            match execution.stage {
                ActiveStage::Planner { permit }
                | ActiveStage::Hash { permit, .. }
                | ActiveStage::Upload {
                    permit,
                    work: UploadWork::RemoteDelete,
                    ..
                } => {
                    app.release_work(permit);
                }
                ActiveStage::Upload {
                    permit,
                    work: UploadWork::Session(mut session),
                    ..
                } => {
                    session.abort();
                    app.release_work(permit);
                }
                ActiveStage::Download {
                    permit,
                    mut session,
                    ..
                } => {
                    session.abort();
                    app.release_work(permit);
                }
                // Waiting stages hold a plan but no permit.
                ActiveStage::WaitingForHash { .. }
                | ActiveStage::WaitingForUpload { .. }
                | ActiveStage::WaitingForDownload { .. } => {}
            }
        }
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
                Ok(_) => plan_upload(app, env, state_db, intent, remote_path, op_id),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    PlanOutcome::Noop("local file vanished before upload")
                }
                Err(error) => PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot stat local file: {error}"),
                },
            }
        }
        PendingIntentKind::Delete => {
            plan_delete(app, env, state_db, intent, remote_path, op_id, now)
        }
        PendingIntentKind::Download => {
            if let Err(reason) = verify_within_local_root(local_root, &intent.path) {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Permanent,
                    message: reason,
                }
                .tap_provider(app);
            }
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
                hashed_local_state: None,
            })
        }
        PendingIntentKind::ApplyRemoteDelete => {
            if let Err(reason) = verify_within_local_root(local_root, &intent.path) {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Permanent,
                    message: reason,
                }
                .tap_provider(app);
            }
            // Two-way deletion guard: "data preservation wins
            // over deletion". A remote deletion only applies when the
            // local copy is exactly what was last synced AND the sync
            // happened before the deletion was observed. A modified (or
            // unknown-provenance) local file survives; the pending
            // upload restores it remotely. One-way pull mirrors delete
            // unconditionally — that is their contract.
            if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                // The Removed event may be stale: another device (or our own
                // re-upload) could have recreated the remote object after
                // the deletion was observed. If the remote exists again,
                // this delete is obsolete — completing it would remove a
                // file that is present remotely. Complete as a no-op.
                if let Ok(Some(_)) = app.provider().stat(&remote_path) {
                    return PlanOutcome::Noop("remote object exists again; deletion is stale")
                        .tap_provider(app);
                }
                match deletion_loses_to_local_state(state_db, intent, env.hash_algorithm) {
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
            let outcome = apply_remote_delete_locally(env, state_db, &intent.path, now);
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

/// Plans a remote delete with the two-way "modification wins over
/// deletion" guard. A local delete only propagates when the remote is
/// still exactly what we last synced (op-id or content hash matches the
/// sync index). If another writer changed the remote since our last
/// sync, the delete is refused and a Download is enqueued to bring the
/// newer remote content back locally — mirroring the upload guard so the
/// newest version is never silently destroyed. One-way push mirrors
/// delete unconditionally; that is its contract.
fn plan_delete(
    app: &DaemonApp,
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    remote_path: RemotePath,
    op_id: String,
    now: SystemTime,
) -> PlanOutcome {
    let plan = TransferPlan {
        remote_path,
        op_id,
        local_path: intent.path.clone(),
        staging_path: None,
        content_hash: None,
        precondition: RemotePrecondition::None,
        verify_remote_before_upload: false,
        hashed_local_state: None,
    };
    if env.sync_mode != vapor_shared::SyncMode::TwoWay {
        return PlanOutcome::RemoteDelete(plan);
    }

    let index = match state_db.sync_index(&intent.path) {
        Ok(index) => index,
        Err(error) => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot read sync index before delete: {error}"),
            };
        }
    };
    match app.provider().stat(&plan.remote_path) {
        Ok(None) => {
            // Remote already gone: the deletion converged. Record the
            // tombstone and complete as a no-op.
            record_delete_tombstone(
                state_db,
                &intent.path,
                crate::state_db::TombstoneOrigin::Local,
                now,
            );
            PlanOutcome::Noop("remote already absent; deletion converged")
        }
        Ok(Some(remote_entry)) => {
            let unchanged = match &index {
                Some(index) => {
                    if remote_entry.op_id.as_deref() == Some(index.last_op_id.as_str()) {
                        true
                    } else {
                        let remote_hash = remote_entry
                            .content_hash
                            .clone()
                            .or_else(|| app.provider().content_hash(&plan.remote_path).ok());
                        remote_hash.as_deref() == Some(index.content_hash.as_str())
                    }
                }
                // No index: divergence is only defined relative to a last
                // sync. Without one there is no baseline to detect a
                // concurrent remote modification, so fall back to the
                // unconditional delete (matching pre-guard behavior); a
                // remote-absent stat already short-circuited above.
                None => true,
            };
            if unchanged {
                return PlanOutcome::RemoteDelete(plan);
            }
            // Another writer changed the remote since our last sync:
            // modification wins over deletion. Refuse the delete and pull
            // the newer remote content back locally.
            if let Err(error) = state_db.enqueue_intents_coalesced(&[(
                intent.path.clone(),
                PendingIntentKind::Download,
                now,
            )]) {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot enqueue delete-preservation download: {error}"),
                };
            }
            if let Err(error) = state_db.remove_sync_index(&intent.path) {
                crate::logging::warning(
                    "Could not clear stale sync index after refusing a remote delete",
                    &[("error", error.to_string())],
                );
            }
            crate::logging::warning(
                "Remote changed since last sync; preserving it over a local deletion",
                &[("path", intent.path.display().to_string())],
            );
            PlanOutcome::Noop("remote changed since last sync; modification wins over deletion")
        }
        Err(error) => PlanOutcome::Fail {
            failure: error.kind.retry_classification(),
            message: format!("cannot stat remote before delete: {}", error.message),
        },
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
) -> PlanOutcome {
    let mut plan = TransferPlan {
        remote_path,
        op_id,
        local_path: intent.path.clone(),
        staging_path: None,
        content_hash: None,
        precondition: RemotePrecondition::None,
        verify_remote_before_upload: false,
        hashed_local_state: None,
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
                    // The remote no longer matches our last sync. This is
                    // usually a concurrent writer (keep both), but it is
                    // also the crash-replay case: an upload that committed
                    // at the provider but crashed before the durable index
                    // write leaves the remote holding *our own* new content
                    // under a fresh op-id, with the index still on the old
                    // hash. Defer to the upload gate, which compares the
                    // remote hash against the *local* content hash:
                    // byte-identical content (crash replay) converges
                    // silently, genuine divergence still resolves keep-both.
                    plan.verify_remote_before_upload = true;
                    PlanOutcome::Upload(plan)
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

/// Keep-both resolution when the local side lost an upload race: move
/// the local loser to its conflict-copy path (suppressing the rename's
/// delete echo), enqueue an upload for the copy, and — when the caller
/// does not itself hold the canonical remote payload — a download for the
/// remote canonical, letting the caller complete the original intent.
/// Deterministic: the conflict path derives from the device id and the
/// intent's durable event time.
///
/// Ordering is crash-safe: the Download(original) restore is enqueued
/// *before* the rename, so a crash between rename and the copy's
/// Upload-enqueue cannot strand the canonical path with nothing pending
/// to restore it.
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
    if let Err(error) = state_db.enqueue_intents_coalesced(&[(
        intent.path.clone(),
        PendingIntentKind::Download,
        now,
    )]) {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot enqueue conflict canonical-restore download: {error}"),
        };
    }
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
    if let Err(error) = state_db.enqueue_intents_coalesced(&[(
        conflict_local.clone(),
        PendingIntentKind::Upload,
        now,
    )]) {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot enqueue conflict-copy upload: {error}"),
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

/// Deletion guard: returns the preservation reason when a remote
/// deletion must NOT apply to the local file, `None` when the deletion
/// may proceed.
fn deletion_loses_to_local_state(
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    algorithm: vapor_providers::HashAlgorithm,
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
        hash_hex_of_file_or_err(&intent.path, algorithm)? != index.content_hash
    };
    if diverged {
        return Ok(Some(
            "local file was modified after the last sync; modification wins over deletion",
        ));
    }
    Ok(None)
}

fn hash_hex_of_file_or_err(
    path: &Path,
    algorithm: vapor_providers::HashAlgorithm,
) -> Result<String, String> {
    hash_hex_of_file_with(path, algorithm)
        .map_err(|error| format!("cannot hash local file for conflict check: {error}"))
}

/// Hashes a whole file in the provider's algorithm.
fn hash_hex_of_file_with(
    path: &Path,
    algorithm: vapor_providers::HashAlgorithm,
) -> std::io::Result<String> {
    vapor_providers::filesystem::hash_hex_of_file_with(path, algorithm)
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
    // The recorded content_hash is the bytes we hashed. If the local file
    // changed since (a mid-transfer edit), recording the current mtime
    // would pair a post-edit mtime with the pre-edit hash, letting the
    // mtime fast-path in the divergence checks skip a needed re-hash and
    // silently overwrite the edit. Detect that and record no mtime, which
    // forces the hash path next time.
    let current = fs::symlink_metadata(&plan.local_path).ok();
    let current_mtime = current
        .as_ref()
        .and_then(|metadata| metadata.modified().ok());
    let current_size = current.as_ref().map(fs::Metadata::len);
    let local_modified_at = match plan.hashed_local_state {
        Some((hashed_size, hashed_mtime))
            if current_size == Some(hashed_size) && current_mtime == hashed_mtime =>
        {
            current_mtime
        }
        Some(_) => None,
        None => current_mtime,
    };
    write_sync_index_entry(
        state_db,
        plan,
        content_hash,
        size_bytes,
        local_modified_at,
        now,
    );
}

fn record_download_index(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    content_hash: &str,
    size_bytes: u64,
    now: SystemTime,
) {
    // We just wrote this file; its current mtime describes exactly the
    // content we applied, so the mtime fast-path is safe to record.
    let local_modified_at = fs::symlink_metadata(&plan.local_path)
        .and_then(|metadata| metadata.modified())
        .ok();
    write_sync_index_entry(
        state_db,
        plan,
        content_hash,
        size_bytes,
        local_modified_at,
        now,
    );
}

fn write_sync_index_entry(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    content_hash: &str,
    size_bytes: u64,
    local_modified_at: Option<SystemTime>,
    now: SystemTime,
) {
    if let Err(error) = state_db.set_sync_index(
        &plan.local_path,
        content_hash,
        size_bytes,
        local_modified_at,
        &plan.op_id,
        now,
    ) {
        crate::logging::warning(
            "Could not record post-transfer sync index entry",
            &[("error", error.to_string())],
        );
    }
    if let Err(error) = state_db.clear_tombstone(&plan.local_path) {
        crate::logging::warning(
            "Could not clear tombstone after transfer",
            &[("error", error.to_string())],
        );
    }
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

/// Verifies that applying a remote-sourced change at `target` cannot
/// escape the canonical local sync root through a symlinked parent
/// component. `RemotePath` validation is purely lexical, so a symlink in
/// an intermediate directory (`root/link -> /elsewhere`) would otherwise
/// let a remote `link/x` write into — or delete under — `/elsewhere`.
/// Canonicalizes the deepest existing ancestor of the target's parent and
/// confirms it is still inside the root.
fn verify_within_local_root(local_root: &Path, target: &Path) -> Result<(), String> {
    // Canonicalize the root itself so the containment comparison holds even
    // when the caller passed a not-yet-canonical root (tests, or a root
    // reached through a symlinked parent of its own).
    let root = fs::canonicalize(local_root).unwrap_or_else(|_| local_root.to_path_buf());
    let mut ancestor = target.parent().unwrap_or(local_root).to_path_buf();
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(real) => {
                if real == root || real.starts_with(&root) {
                    return Ok(());
                }
                return Err(format!(
                    "refusing remote apply of {}: parent resolves outside the sync root ({})",
                    target.display(),
                    real.display()
                ));
            }
            // The parent does not exist yet; create_dir_all will make
            // fresh real directories under the deepest existing ancestor.
            // Keep walking up to find it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match ancestor.parent() {
                    Some(parent) if parent.starts_with(local_root) || parent == local_root => {
                        ancestor = parent.to_path_buf();
                    }
                    // Walked above the root without finding an existing
                    // ancestor inside it: lexically the target claimed to be
                    // under the root, so this is a malformed path.
                    _ => {
                        return Err(format!(
                            "refusing remote apply of {}: no existing ancestor inside the sync root",
                            target.display()
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(format!(
                    "cannot verify sync-root containment of {}: {error}",
                    ancestor.display()
                ));
            }
        }
    }
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
///
/// Directory deletions are the delicate case. In two-way mode a remote
/// folder deletion arrives as one `Removed(dir)` change, but the local
/// subtree may hold unsynced or locally-modified files (data preservation
/// wins over deletion). Rather than `remove_dir_all` the whole tree, the
/// two-way path walks it and removes only synced-and-unchanged files,
/// preserving (and re-uploading) the rest. One-way pull is a strict
/// mirror and removes the tree unconditionally.
fn apply_remote_delete_locally(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
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
            if metadata.is_dir() {
                if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                    return guarded_remove_dir(env, state_db, path, now);
                }
                return match fs::remove_dir_all(path) {
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
                        message: format!(
                            "cannot delete local directory for remote-delete apply: {error}"
                        ),
                    },
                };
            }
            match fs::remove_file(path) {
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

/// Two-way guarded recursive delete of a remotely-removed directory.
/// Returns `AppliedLocally` when the whole subtree (including the root
/// directory) was removed, or a `Noop` describing the preservation when
/// at least one entry was kept.
fn guarded_remove_dir(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    dir: &Path,
    now: SystemTime,
) -> PlanOutcome {
    match remove_dir_preserving_unsynced(env, state_db, dir, now) {
        Ok(true) => match fs::remove_dir(dir) {
            Ok(()) => {
                let _ = env.tags.remove(dir);
                env.local_echoes.record_delete(path_key(dir), now);
                PlanOutcome::AppliedLocally
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PlanOutcome::Noop("local directory already gone")
            }
            // Something reappeared under the directory between the walk and
            // this rmdir: leave it, the next reconcile converges.
            Err(_) => PlanOutcome::Noop(
                "preserved local content that reappeared under a deleted directory",
            ),
        },
        Ok(false) => {
            PlanOutcome::Noop("preserved unsynced local content under a remotely-deleted directory")
        }
        Err(message) => PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message,
        },
    }
}

/// Depth-first guarded delete. Returns `true` when everything inside the
/// directory was removed, `false` when at least one entry was preserved.
/// A synced-and-unchanged file is removed; an unsynced, locally-modified,
/// or unknown-provenance file (or any symlink/special node) is preserved
/// and its Upload re-enqueued so the remote is restored.
fn remove_dir_preserving_unsynced(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    dir: &Path,
    now: SystemTime,
) -> Result<bool, String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("cannot read directory for remote-delete apply: {error}"))?;
    let mut all_removed = true;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let child = entry.path();
        let metadata = match fs::symlink_metadata(&child) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot stat {}: {error}", child.display())),
        };
        if metadata.is_dir() {
            if remove_dir_preserving_unsynced(env, state_db, &child, now)? {
                match fs::remove_dir(&child) {
                    Ok(()) => {
                        let _ = env.tags.remove(&child);
                        env.local_echoes.record_delete(path_key(&child), now);
                        record_delete_tombstone(
                            state_db,
                            &child,
                            crate::state_db::TombstoneOrigin::Remote,
                            now,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => all_removed = false,
                }
            } else {
                all_removed = false;
            }
        } else if metadata.is_file() {
            if child_delete_is_safe(state_db, &child, &metadata, env.hash_algorithm)? {
                match fs::remove_file(&child) {
                    Ok(()) => {
                        let _ = env.tags.remove(&child);
                        env.local_echoes.record_delete(path_key(&child), now);
                        record_delete_tombstone(
                            state_db,
                            &child,
                            crate::state_db::TombstoneOrigin::Remote,
                            now,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!("cannot delete {}: {error}", child.display()));
                    }
                }
            } else {
                all_removed = false;
                if let Err(error) = state_db.enqueue_intents_coalesced(&[(
                    child.clone(),
                    PendingIntentKind::Upload,
                    now,
                )]) {
                    crate::logging::warning(
                        "Could not enqueue re-upload for preserved file under deleted directory",
                        &[
                            ("path", child.display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                }
            }
        } else {
            // Symlinks and special files are outside the sync contract;
            // never follow or delete them.
            all_removed = false;
        }
    }
    Ok(all_removed)
}

/// Whether a file under a remotely-deleted directory is safe to remove:
/// only when the sync index proves it is exactly what was last synced.
/// Unknown provenance or any divergence (including a hash failure) fails
/// safe to `false` (preserve).
fn child_delete_is_safe(
    state_db: &mut DurableStateDb,
    path: &Path,
    metadata: &fs::Metadata,
    algorithm: vapor_providers::HashAlgorithm,
) -> Result<bool, String> {
    let Some(index) = state_db
        .sync_index(path)
        .map_err(|error| format!("cannot read sync index for {}: {error}", path.display()))?
    else {
        return Ok(false);
    };
    let diverged = if metadata.len() != index.size_bytes {
        true
    } else if index.local_modified_at.is_some()
        && metadata.modified().ok() == index.local_modified_at
    {
        false
    } else {
        match hash_hex_of_file_with(path, algorithm) {
            Ok(hash) => hash != index.content_hash,
            Err(_) => true,
        }
    };
    Ok(!diverged)
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

/// Two-way download apply that closes the check-then-act window. The
/// existing local file (if any) is renamed aside first — atomically
/// capturing whatever bytes are present — then the canonical payload is
/// renamed in, then the displaced bytes are examined and either dropped
/// (they matched the sync index or the incoming payload) or promoted to a
/// keep-both conflict copy (they diverged). The divergence decision runs
/// on the exact displaced bytes, so a local edit present at apply time is
/// never silently overwritten. Returns whether a conflict copy was made.
fn apply_downloaded_payload_keep_both(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    plan: &TransferPlan,
    incoming_hash: &str,
    now: SystemTime,
) -> Result<bool, String> {
    let parent = plan
        .local_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let aside = parent.join(format!(
        "{}aside-{}",
        constants::provider::TEMP_FILE_PREFIX,
        sanitize_for_file_name(&plan.op_id)
    ));

    // Read the pre-apply index before renaming; it still keys off the
    // canonical path and describes the bytes we are about to displace.
    let index = state_db
        .sync_index(&plan.local_path)
        .map_err(|error| format!("cannot read sync index: {error}"))?;

    let displaced = match fs::symlink_metadata(&plan.local_path) {
        Ok(metadata) if metadata.is_file() => match fs::rename(&plan.local_path, &aside) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(format!("cannot set aside local file before apply: {error}"));
            }
        },
        // Directory or special node at the path: apply_downloaded_payload's
        // rename will surface an appropriate error. Nothing to preserve.
        _ => false,
    };

    if let Err(error) = apply_downloaded_payload(env, plan) {
        // Restore the displaced file so a failed apply loses nothing.
        if displaced {
            let _ = fs::rename(&aside, &plan.local_path);
        }
        return Err(format!("local apply of downloaded payload failed: {error}"));
    }

    if !displaced {
        return Ok(false);
    }

    let aside_hash = hash_hex_of_file_with(&aside, env.hash_algorithm)
        .map_err(|error| format!("cannot hash displaced local file: {error}"))?;
    let unchanged = aside_hash == incoming_hash
        || index
            .as_ref()
            .map(|index| aside_hash == index.content_hash)
            .unwrap_or(false);
    if unchanged {
        let _ = fs::remove_file(&aside);
        let _ = env.tags.remove(&aside);
        return Ok(false);
    }

    // The displaced bytes diverged from both the last sync and the
    // incoming payload: keep both.
    let timestamp_ms = intent
        .enqueued_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let conflict_local = crate::conflict::conflict_copy_path(
        &plan.local_path,
        env.device_id,
        timestamp_ms,
        |candidate: &Path| candidate.exists(),
    );
    if let Err(error) = fs::rename(&aside, &conflict_local) {
        return Err(format!(
            "cannot stage conflict copy from displaced file: {error}"
        ));
    }
    let _ = env.tags.relocate_side_file(&aside, &conflict_local);
    if let Err(error) = state_db.enqueue_intents_coalesced(&[(
        conflict_local.clone(),
        PendingIntentKind::Upload,
        now,
    )]) {
        return Err(format!("cannot enqueue conflict-copy upload: {error}"));
    }
    crate::logging::warning(
        "Kept both versions on download-apply (local edit diverged from the incoming payload)",
        &[
            ("canonical", plan.local_path.display().to_string()),
            ("conflict_copy", conflict_local.display().to_string()),
        ],
    );
    Ok(true)
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
                let hash_algorithm = self.app.provider().content_hash_algorithm();
                let mut env = ExecutionEnv {
                    local_root: Some(&self.local_root),
                    sync_mode: self.sync_mode,
                    device_id: "testdev",
                    hash_algorithm,
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
    fn local_delete_refused_when_remote_diverged_and_enqueues_download() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("shared.txt");
        // We last synced this path (op-id "op-mine", content "v1").
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"v1"),
                2,
                None,
                "op-mine",
                timestamp_ms(0),
            )
            .expect("seed index");
        // Another device modified the remote since our last sync.
        std::fs::write(
            fixture.cloud_root.join("shared.txt"),
            b"v2-from-other-device",
        )
        .expect("seed remote");
        // The user deleted the file locally (hence the Delete intent).
        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        // The newer remote content is preserved, not destroyed.
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("shared.txt")).expect("remote intact"),
            b"v2-from-other-device"
        );
        // A Download was enqueued to restore the newer remote locally.
        let restored = fixture
            .state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease")
            .expect("download intent enqueued");
        assert_eq!(restored.kind, PendingIntentKind::Download);
        assert_eq!(restored.path, local_file);
    }

    #[test]
    fn two_way_remote_dir_delete_preserves_unsynced_child() {
        let mut fixture = Fixture::new();
        let dir = fixture.local_root.join("project");
        std::fs::create_dir_all(&dir).expect("dir");
        let synced = dir.join("synced.txt");
        let unsynced = dir.join("unsynced.txt");
        std::fs::write(&synced, b"synced body").expect("synced");
        std::fs::write(&unsynced, b"brand new local file").expect("unsynced");
        // Only the synced child has an index entry matching its content.
        let mtime = std::fs::symlink_metadata(&synced)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &synced,
                &hash_hex_of_bytes(b"synced body"),
                11,
                mtime,
                "op-synced",
                timestamp_ms(0),
            )
            .expect("seed index");

        let intent = fixture.enqueue_and_lease(&dir, PendingIntentKind::ApplyRemoteDelete);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        // The synced-and-unchanged child was removed; the unsynced child
        // (and thus the directory) survived.
        assert!(!synced.exists());
        assert!(unsynced.exists());
        assert!(dir.exists());
        // The preserved child has an Upload re-enqueued to restore it.
        let restore = fixture
            .state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease")
            .expect("upload intent enqueued");
        assert_eq!(restore.kind, PendingIntentKind::Upload);
        assert_eq!(restore.path, unsynced);
    }

    #[test]
    fn download_apply_keeps_both_when_local_diverged() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"remote version").expect("remote");
        let local = fixture.local_root.join("doc.txt");
        // A locally-edited file that never matched the incoming payload and
        // has no index (unknown provenance -> keep both).
        std::fs::write(&local, b"local edit").expect("local");

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        // Canonical payload applied.
        assert_eq!(std::fs::read(&local).expect("applied"), b"remote version");
        // The diverged local edit was kept as a conflict copy, not lost.
        let conflict = std::fs::read_dir(&fixture.local_root)
            .expect("read local root")
            .filter_map(|e| e.ok())
            .find(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("doc") && name != "doc.txt"
            });
        let conflict = conflict.expect("a conflict copy was created");
        assert_eq!(
            std::fs::read(conflict.path()).expect("conflict body"),
            b"local edit"
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_download_through_symlinked_parent_is_refused() {
        use std::os::unix::fs::symlink;
        let mut fixture = Fixture::new();
        // A directory outside the sync root, reachable via a symlink inside
        // it: an attacker-controlled cloud path `link/x` must not write
        // there.
        let outside = fixture._temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        symlink(&outside, fixture.local_root.join("link")).expect("symlink");

        let target = fixture.local_root.join("link/payload.txt");
        std::fs::write(fixture.cloud_root.join("link/payload.txt"), b"x").ok();
        let intent = fixture.enqueue_and_lease(&target, PendingIntentKind::Download);
        assert!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0))
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.failed, 1);
        // Nothing was written outside the sync root.
        assert!(!outside.join("payload.txt").exists());
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
        let hash_algorithm = fixture.app.provider().content_hash_algorithm();
        let mut env = ExecutionEnv {
            local_root: Some(&fixture.local_root),
            sync_mode: fixture.sync_mode,
            device_id: "testdev",
            hash_algorithm,
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
