use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::clock::{Clock, SystemClock};
use crate::event_intents::PendingIntentKind;
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
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StagedExecutorSnapshot {
    pub active_total: usize,
    pub planner_running: usize,
    pub waiting_for_hash: usize,
    pub hash_running: usize,
    pub waiting_for_upload: usize,
    pub upload_running: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StagedExecutorReport {
    pub started: usize,
    pub completed: usize,
}

pub struct StagedExecutor {
    stage_duration: Duration,
    active: BTreeMap<i64, ActiveExecution>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for StagedExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedExecutor")
            .field("stage_duration", &self.stage_duration)
            .field("active", &self.active)
            .finish()
    }
}

#[derive(Debug)]
struct ActiveExecution {
    intent_id: i64,
    kind: PendingIntentKind,
    stage: ActiveStage,
}

#[derive(Debug)]
enum ActiveStage {
    Planner {
        permit: crate::workgate::WorkPermit,
        started_inst: Instant,
    },
    WaitingForHash,
    Hash {
        permit: crate::workgate::WorkPermit,
        started_inst: Instant,
    },
    WaitingForUpload,
    Upload {
        permit: crate::workgate::WorkPermit,
        started_inst: Instant,
    },
}

impl StagedExecutor {
    pub fn new(stage_duration: Duration) -> Self {
        Self::with_clock(stage_duration, Arc::new(SystemClock))
    }

    pub fn with_clock(stage_duration: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            stage_duration,
            active: BTreeMap::new(),
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
            }
        }
        snapshot
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

        let Ok(permit) = app.try_acquire_work(WorkClass::Planner) else {
            return false;
        };

        let started_inst = self.clock.now();
        self.active.insert(
            intent.id,
            ActiveExecution {
                intent_id: intent.id,
                kind: intent.kind,
                stage: ActiveStage::Planner {
                    permit,
                    started_inst,
                },
            },
        );
        true
    }

    pub fn advance(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        _now: SystemTime,
    ) -> Result<StagedExecutorReport, StateDbError> {
        // Stage timing uses monotonic Instant pairs so wall-clock rewinds
        // never make a stage falsely "complete" or "expire". The
        // `_now: SystemTime` parameter stays in the signature for the
        // public contract — durable fields downstream still need it. C2-3.
        let now_inst = self.clock.now();
        let mut report = StagedExecutorReport::default();
        let intent_ids: Vec<i64> = self.active.keys().copied().collect();

        for intent_id in intent_ids {
            let Some(mut execution) = self.active.remove(&intent_id) else {
                continue;
            };

            match &mut execution.stage {
                ActiveStage::Planner {
                    permit,
                    started_inst,
                } => {
                    if !stage_elapsed_inst(*started_inst, now_inst, self.stage_duration) {
                        self.active.insert(intent_id, execution);
                        continue;
                    }

                    app.release_work(*permit);
                    execution.stage = if requires_hash(execution.kind) {
                        ActiveStage::WaitingForHash
                    } else {
                        ActiveStage::WaitingForUpload
                    };
                }
                ActiveStage::Hash {
                    permit,
                    started_inst,
                } => {
                    if !stage_elapsed_inst(*started_inst, now_inst, self.stage_duration) {
                        self.active.insert(intent_id, execution);
                        continue;
                    }

                    app.release_work(*permit);
                    execution.stage = ActiveStage::WaitingForUpload;
                }
                ActiveStage::Upload {
                    permit,
                    started_inst,
                } => {
                    if !stage_elapsed_inst(*started_inst, now_inst, self.stage_duration) {
                        self.active.insert(intent_id, execution);
                        continue;
                    }

                    app.release_work(*permit);
                    if !state_db.complete_leased(execution.intent_id)? {
                        return Err(StateDbError::InvalidIntentState(format!(
                            "intent {} was not leased during staged completion",
                            execution.intent_id
                        )));
                    }
                    report.completed += 1;
                    continue;
                }
                ActiveStage::WaitingForHash | ActiveStage::WaitingForUpload => {}
            }

            execution.stage = match execution.stage {
                ActiveStage::WaitingForHash => match app.try_acquire_work(WorkClass::Hash) {
                    Ok(permit) => ActiveStage::Hash {
                        permit,
                        started_inst: now_inst,
                    },
                    Err(_) => ActiveStage::WaitingForHash,
                },
                ActiveStage::WaitingForUpload => match app.try_acquire_work(WorkClass::Upload) {
                    Ok(permit) => ActiveStage::Upload {
                        permit,
                        started_inst: now_inst,
                    },
                    Err(_) => ActiveStage::WaitingForUpload,
                },
                stage @ ActiveStage::Planner { .. }
                | stage @ ActiveStage::Hash { .. }
                | stage @ ActiveStage::Upload { .. } => stage,
            };

            self.active.insert(intent_id, execution);
        }

        Ok(report)
    }

    pub fn admission_capacity(&self, workgate: WorkgateSnapshot) -> usize {
        max_in_flight_items(workgate).saturating_sub(self.active.len())
    }
}

impl ActiveExecution {
    fn stage_name(&self) -> ExecutionStage {
        match self.stage {
            ActiveStage::Planner { .. } => ExecutionStage::Planner,
            ActiveStage::WaitingForHash => ExecutionStage::WaitingForHash,
            ActiveStage::Hash { .. } => ExecutionStage::Hash,
            ActiveStage::WaitingForUpload => ExecutionStage::WaitingForUpload,
            ActiveStage::Upload { .. } => ExecutionStage::Upload,
        }
    }
}

fn requires_hash(kind: PendingIntentKind) -> bool {
    matches!(kind, PendingIntentKind::Upload | PendingIntentKind::Rename)
}

fn stage_elapsed_inst(started_inst: Instant, now_inst: Instant, stage_duration: Duration) -> bool {
    now_inst.saturating_duration_since(started_inst) >= stage_duration
}

fn max_in_flight_items(workgate: WorkgateSnapshot) -> usize {
    workgate.caps.planner_workers + workgate.caps.hash_workers + workgate.caps.upload_concurrency
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::event_intents::PendingIntentKind;
    use crate::throttle::ThrottleInputs;
    use std::path::PathBuf;

    #[test]
    fn upload_intent_moves_through_planner_hash_and_upload_stages() {
        let clock = Arc::new(ManualClock::at_now());
        let mut app = DaemonApp::new_with_clock(default_provider_for_test(), clock.clone());
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open state db");
        state_db
            .enqueue_intent(
                &PathBuf::from("/tmp/vapor-root/file.txt"),
                PendingIntentKind::Upload,
                timestamp_ms(0),
            )
            .expect("enqueue intent");
        let intent = state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease next ready")
            .expect("leased intent");
        let mut executor = StagedExecutor::with_clock(Duration::from_millis(100), clock.clone());
        let intent_id = intent.id;

        assert!(executor.try_start(&mut app, intent, timestamp_ms(0)));
        assert_eq!(executor.snapshot().planner_running, 1);

        clock.advance(Duration::from_millis(100));
        executor
            .advance(&mut app, &mut state_db, timestamp_ms(100))
            .expect("advance to hash");
        assert_eq!(executor.snapshot().hash_running, 1);

        clock.advance(Duration::from_millis(100));
        executor
            .advance(&mut app, &mut state_db, timestamp_ms(200))
            .expect("advance to upload");
        assert_eq!(executor.snapshot().upload_running, 1);

        clock.advance(Duration::from_millis(100));
        let report = executor
            .advance(&mut app, &mut state_db, timestamp_ms(300))
            .expect("complete upload");
        assert_eq!(report.completed, 1);
        assert_eq!(state_db.queue_depth().expect("queue depth"), 0);
        assert_eq!(executor.snapshot().active_total, 0);
        assert_eq!(intent_id, 1);
    }

    #[test]
    fn delete_intent_skips_hash_stage() {
        let clock = Arc::new(ManualClock::at_now());
        let mut app = DaemonApp::new_with_clock(default_provider_for_test(), clock.clone());
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open state db");
        state_db
            .enqueue_intent(
                &PathBuf::from("/tmp/vapor-root/file.txt"),
                PendingIntentKind::Delete,
                timestamp_ms(0),
            )
            .expect("enqueue intent");
        let leased = state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease next ready")
            .expect("leased intent");
        let mut executor = StagedExecutor::with_clock(Duration::from_millis(100), clock.clone());

        assert!(executor.try_start(&mut app, leased, timestamp_ms(0)));
        clock.advance(Duration::from_millis(100));
        executor
            .advance(&mut app, &mut state_db, timestamp_ms(100))
            .expect("advance to upload");
        assert_eq!(executor.snapshot().upload_running, 1);
    }

    fn default_provider_for_test() -> Box<dyn vapor_providers::Provider> {
        vapor_providers::default_provider()
    }

    #[test]
    fn staged_executor_respects_workgate_caps() {
        let mut app = DaemonApp::default();
        app.apply_throttle_inputs(ThrottleInputs {
            user_active: true,
            ..ThrottleInputs::default()
        });

        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open state db");
        let mut executor = StagedExecutor::new(Duration::from_millis(100));

        for index in 0..3 {
            state_db
                .enqueue_intent(
                    &PathBuf::from(format!("/tmp/vapor-root/file-{index}.txt")),
                    PendingIntentKind::Upload,
                    timestamp_ms(0),
                )
                .expect("enqueue intent");
        }

        let first = state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease next ready")
            .expect("first leased intent");
        let second = state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease next ready")
            .expect("second leased intent");

        assert!(executor.try_start(&mut app, first, timestamp_ms(0)));
        assert!(!executor.try_start(&mut app, second, timestamp_ms(0)));
        assert_eq!(executor.snapshot().planner_running, 1);
    }

    #[test]
    fn staged_executor_admission_is_bounded_by_total_pipeline_capacity() {
        let mut app = DaemonApp::default();
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open state db");
        let mut executor = StagedExecutor::new(Duration::from_millis(100));

        for index in 0..12 {
            state_db
                .enqueue_intent(
                    &PathBuf::from(format!("/tmp/vapor-root/file-{index}.txt")),
                    PendingIntentKind::Upload,
                    timestamp_ms(0),
                )
                .expect("enqueue intent");
        }

        let initial_workgate = app.workgate_snapshot();
        let expected_capacity = initial_workgate.caps.planner_workers
            + initial_workgate.caps.hash_workers
            + initial_workgate.caps.upload_concurrency;
        let planner_capacity = initial_workgate.available_planner_workers();
        let capacity = executor.admission_capacity(initial_workgate);
        assert_eq!(capacity, expected_capacity);

        let mut started = 0;
        while let Some(intent) = state_db
            .lease_next_ready(timestamp_ms(0))
            .expect("lease intent")
        {
            if executor.try_start(&mut app, intent, timestamp_ms(0)) {
                started += 1;
            } else {
                break;
            }
        }

        assert_eq!(started, planner_capacity);
        assert_eq!(executor.snapshot().active_total, started);
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
