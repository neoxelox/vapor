use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use vapor_shared::{ThrottleState, constants};

use crate::event_intents::BoundedEventIntentMaps;
use crate::event_intents::PendingIntentKind;
use crate::scheduler::{CompletionDisposition, KeyedSupersedingScheduler};
use crate::workgate::{ThrottleWorkgate, WorkClass, WorkPermit, WorkPermitDenied};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconcileController {
    slice_budget: Duration,
    running: Option<RunningReconcile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RunningReconcile {
    root: PathBuf,
    started_at: SystemTime,
    slice_started_at: SystemTime,
    last_checkpoint_at: SystemTime,
    checkpoint_count: usize,
    permit: WorkPermit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcilePauseReason {
    SliceBudgetExpired,
    ThrottleNoLongerIdle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconcilePause {
    pub root: PathBuf,
    pub reason: ReconcilePauseReason,
    pub disposition: CompletionDisposition,
    pub checkpoint_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconcileCompletion {
    pub root: PathBuf,
    pub disposition: CompletionDisposition,
    pub boundary_cleared: bool,
    pub checkpoint_count: usize,
}

impl Default for ReconcileController {
    fn default() -> Self {
        Self {
            slice_budget: Duration::from_millis(constants::engine::RECONCILE_SLICE_MILLIS),
            running: None,
        }
    }
}

impl ReconcileController {
    pub fn with_slice_budget(slice_budget: Duration) -> Self {
        Self {
            slice_budget,
            running: None,
        }
    }

    pub fn running_root(&self) -> Option<&PathBuf> {
        self.running.as_ref().map(|running| &running.root)
    }

    pub fn release_ready_deferred_reconciles(
        &mut self,
        maps: &mut BoundedEventIntentMaps,
        scheduler: &mut KeyedSupersedingScheduler,
        throttle_state: ThrottleState,
        now: SystemTime,
    ) -> usize {
        if throttle_state != ThrottleState::IdleDrain {
            return 0;
        }

        let ready = maps.take_ready_deferred_reconcile_intents(now);
        for intent in &ready {
            scheduler.upsert_pending_intent_record(intent.clone());
        }
        ready.len()
    }

    pub fn try_start_next(
        &mut self,
        scheduler: &mut KeyedSupersedingScheduler,
        workgate: &mut ThrottleWorkgate,
        throttle_state: ThrottleState,
        now: SystemTime,
    ) -> Result<Option<PathBuf>, WorkPermitDenied> {
        if self.running.is_some() || throttle_state != ThrottleState::IdleDrain {
            return Ok(None);
        }

        let Some(claimed) = scheduler.claim_next_reconcile() else {
            return Ok(None);
        };

        let permit = match workgate.try_acquire(WorkClass::Reconcile) {
            Ok(permit) => permit,
            Err(error) => {
                requeue_claimed_reconcile(scheduler, &claimed.path, now);
                return Err(error);
            }
        };

        self.running = Some(RunningReconcile {
            root: claimed.path.clone(),
            started_at: now,
            slice_started_at: now,
            last_checkpoint_at: now,
            checkpoint_count: 0,
            permit,
        });
        Ok(Some(claimed.path))
    }

    pub fn checkpoint(
        &mut self,
        scheduler: &mut KeyedSupersedingScheduler,
        workgate: &mut ThrottleWorkgate,
        throttle_state: ThrottleState,
        now: SystemTime,
    ) -> Option<ReconcilePause> {
        let running = self.running.as_mut()?;
        running.checkpoint_count += 1;
        running.last_checkpoint_at = now;

        let reason = if throttle_state != ThrottleState::IdleDrain {
            Some(ReconcilePauseReason::ThrottleNoLongerIdle)
        } else if now
            .duration_since(running.slice_started_at)
            .unwrap_or(self.slice_budget)
            >= self.slice_budget
        {
            Some(ReconcilePauseReason::SliceBudgetExpired)
        } else {
            return None;
        }?;

        let running = self.running.take().expect("running reconcile disappeared");
        release_permit_or_log(workgate, running.permit, "reconcile pause");
        let disposition = requeue_claimed_reconcile(scheduler, &running.root, now);
        Some(ReconcilePause {
            root: running.root,
            reason,
            disposition,
            checkpoint_count: running.checkpoint_count,
        })
    }

    pub fn complete_success(
        &mut self,
        maps: &mut BoundedEventIntentMaps,
        scheduler: &mut KeyedSupersedingScheduler,
        workgate: &mut ThrottleWorkgate,
    ) -> Option<ReconcileCompletion> {
        let running = self.running.take()?;
        release_permit_or_log(workgate, running.permit, "reconcile success");
        let disposition = scheduler.complete_running(&running.root)?;
        let boundary_cleared = matches!(disposition, CompletionDisposition::Removed)
            && maps.clear_compacted_subtree_boundary(&running.root);

        Some(ReconcileCompletion {
            root: running.root,
            disposition,
            boundary_cleared,
            checkpoint_count: running.checkpoint_count,
        })
    }
}

fn requeue_claimed_reconcile(
    scheduler: &mut KeyedSupersedingScheduler,
    root: &Path,
    now: SystemTime,
) -> CompletionDisposition {
    scheduler.upsert_intent(root.to_path_buf(), PendingIntentKind::ReconcileSubtree, now);
    scheduler
        .complete_running(root)
        .expect("running reconcile intent should exist while requeueing")
}

fn release_permit_or_log(workgate: &mut ThrottleWorkgate, permit: WorkPermit, context: &str) {
    if !workgate.release(permit) {
        crate::logging::warning(
            "Workgate permit release was rejected",
            &[
                ("context", context.to_string()),
                ("permit_class", format!("{:?}", permit.class)),
                ("permit_id", permit.id.to_string()),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_intents::{BoundedEventIntentMaps, EventIntentLimits};
    use crate::fs_events::{FsEventKind, FsEventRecord};
    use crate::scheduler::KeyedSupersedingScheduler;
    use crate::storm::StormThresholds;
    use crate::throttle::ThrottleController;
    use std::time::UNIX_EPOCH;

    #[test]
    fn deferred_reconciles_are_released_only_in_idle_drain() {
        let subtree_root = PathBuf::from("/tmp/vapor-root/project/sub");
        let mut maps = stormed_maps(&subtree_root);
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut controller = ReconcileController::default();

        assert_eq!(
            controller.release_ready_deferred_reconciles(
                &mut maps,
                &mut scheduler,
                ThrottleState::Light,
                timestamp(32),
            ),
            0
        );
        assert!(scheduler.claim_next_reconcile().is_none());

        assert_eq!(
            controller.release_ready_deferred_reconciles(
                &mut maps,
                &mut scheduler,
                ThrottleState::IdleDrain,
                timestamp(32),
            ),
            1
        );
        let claimed = scheduler.claim_next_reconcile().expect("reconcile intent");
        assert_eq!(claimed.path, subtree_root);
    }

    #[test]
    fn running_reconcile_yields_after_slice_budget_and_requeues_work() {
        let root = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();
        scheduler.upsert_intent(
            root.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(1),
        );
        let mut workgate = idle_reconcile_gate();
        let mut controller = ReconcileController::with_slice_budget(Duration::from_secs(1));

        assert_eq!(
            controller
                .try_start_next(
                    &mut scheduler,
                    &mut workgate,
                    ThrottleState::IdleDrain,
                    timestamp(1),
                )
                .expect("start reconcile"),
            Some(root.clone())
        );

        let pause = controller
            .checkpoint(
                &mut scheduler,
                &mut workgate,
                ThrottleState::IdleDrain,
                timestamp(2),
            )
            .expect("reconcile should yield");

        assert_eq!(pause.root, root);
        assert_eq!(pause.reason, ReconcilePauseReason::SliceBudgetExpired);
        assert_eq!(pause.disposition, CompletionDisposition::RequeuedDirty);
        assert!(controller.running_root().is_none());
        assert_eq!(workgate.snapshot().active_reconciles, 0);
        assert_eq!(scheduler.pending_count(), 1);
    }

    #[test]
    fn running_reconcile_interrupts_when_throttle_leaves_idle_drain() {
        let root = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();
        scheduler.upsert_intent(
            root.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(1),
        );
        let mut workgate = idle_reconcile_gate();
        let mut controller = ReconcileController::with_slice_budget(Duration::from_secs(10));

        controller
            .try_start_next(
                &mut scheduler,
                &mut workgate,
                ThrottleState::IdleDrain,
                timestamp(1),
            )
            .expect("start reconcile");

        let pause = controller
            .checkpoint(
                &mut scheduler,
                &mut workgate,
                ThrottleState::Light,
                timestamp(2),
            )
            .expect("reconcile should interrupt");

        assert_eq!(pause.reason, ReconcilePauseReason::ThrottleNoLongerIdle);
        assert_eq!(scheduler.pending_count(), 1);
    }

    #[test]
    fn checkpoint_pauses_on_system_clock_rewind_instead_of_running_without_bound() {
        let root = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();
        scheduler.upsert_intent(
            root.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(1_000),
        );
        let mut workgate = idle_reconcile_gate();
        let mut controller = ReconcileController::with_slice_budget(Duration::from_secs(1));

        controller
            .try_start_next(
                &mut scheduler,
                &mut workgate,
                ThrottleState::IdleDrain,
                timestamp(1_000),
            )
            .expect("start reconcile");

        let pause = controller
            .checkpoint(
                &mut scheduler,
                &mut workgate,
                ThrottleState::IdleDrain,
                timestamp(500),
            )
            .expect("reconcile should pause after clock rewind");

        assert_eq!(pause.reason, ReconcilePauseReason::SliceBudgetExpired);
    }

    #[test]
    fn successful_reconcile_clears_compaction_boundary_when_quiet() {
        let subtree_root = PathBuf::from("/tmp/vapor-root/project/sub");
        let mut maps = stormed_maps(&subtree_root);
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut controller = ReconcileController::default();
        let mut workgate = idle_reconcile_gate();

        assert_eq!(
            controller.release_ready_deferred_reconciles(
                &mut maps,
                &mut scheduler,
                ThrottleState::IdleDrain,
                timestamp(32),
            ),
            1
        );
        controller
            .try_start_next(
                &mut scheduler,
                &mut workgate,
                ThrottleState::IdleDrain,
                timestamp(32),
            )
            .expect("start reconcile");

        let completion = controller
            .complete_success(&mut maps, &mut scheduler, &mut workgate)
            .expect("complete reconcile");

        assert_eq!(completion.root, subtree_root);
        assert_eq!(completion.disposition, CompletionDisposition::Removed);
        assert!(completion.boundary_cleared);
        assert!(maps.compacted_subtree(&completion.root).is_none());

        maps.record_event(FsEventRecord {
            path: completion.root.join("after.txt"),
            kind: FsEventKind::Modified,
            observed_at: timestamp(40),
        });
        assert_eq!(maps.pending_event_count(), 1);
    }

    #[test]
    fn successful_reconcile_keeps_boundary_when_follow_up_work_exists() {
        let subtree_root = PathBuf::from("/tmp/vapor-root/project/sub");
        let mut maps = stormed_maps(&subtree_root);
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut controller = ReconcileController::default();
        let mut workgate = idle_reconcile_gate();

        controller.release_ready_deferred_reconciles(
            &mut maps,
            &mut scheduler,
            ThrottleState::IdleDrain,
            timestamp(32),
        );
        controller
            .try_start_next(
                &mut scheduler,
                &mut workgate,
                ThrottleState::IdleDrain,
                timestamp(32),
            )
            .expect("start reconcile");

        maps.record_event(FsEventRecord {
            path: subtree_root.join("after.txt"),
            kind: FsEventKind::Modified,
            observed_at: timestamp(33),
        });

        let completion = controller
            .complete_success(&mut maps, &mut scheduler, &mut workgate)
            .expect("complete reconcile");

        assert!(!completion.boundary_cleared);
        assert!(maps.compacted_subtree(&subtree_root).is_some());
        assert!(maps.deferred_reconcile(&subtree_root).is_some());
    }

    fn idle_reconcile_gate() -> ThrottleWorkgate {
        let controller = ThrottleController::default();
        ThrottleWorkgate::new(
            ThrottleState::IdleDrain,
            controller.caps_for(ThrottleState::IdleDrain),
        )
    }

    fn stormed_maps(subtree_root: &Path) -> BoundedEventIntentMaps {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root,
            EventIntentLimits::new(100, 100),
            StormThresholds {
                window: Duration::from_secs(2),
                directory_unique_paths_threshold: 2,
                directory_event_count_threshold: 99,
                global_pending_event_count_threshold: 99,
                deferred_reconcile_delay: Duration::from_secs(30),
            },
        );

        maps.record_event(FsEventRecord {
            path: subtree_root.join("a.txt"),
            kind: FsEventKind::Modified,
            observed_at: timestamp(1),
        });
        maps.record_event(FsEventRecord {
            path: subtree_root.join("b.txt"),
            kind: FsEventKind::Modified,
            observed_at: timestamp(2),
        });
        maps
    }

    fn timestamp(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }
}
