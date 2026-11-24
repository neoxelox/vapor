use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use vapor_shared::{ThrottleState, constants};

use crate::clock::{Clock, SystemClock};
use crate::event_intents::BoundedEventIntentMaps;
use crate::event_intents::PendingIntentKind;
use crate::scheduler::{CompletionDisposition, KeyedSupersedingScheduler};
use crate::workgate::{ThrottleWorkgate, WorkClass, WorkPermit, WorkPermitDenied};

#[derive(Clone, Debug)]
pub struct ReconcileController {
    slice_budget: Duration,
    running: Option<RunningReconcile>,
    clock: Arc<dyn Clock>,
}

#[derive(Clone, Debug)]
struct RunningReconcile {
    root: PathBuf,
    slice_started_inst: Instant,
    checkpoint_count: usize,
    permit: WorkPermit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcilePauseReason {
    SliceBudgetExpired,
    ThrottleSuspended,
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
        Self::with_clock(Arc::new(SystemClock))
    }
}

impl ReconcileController {
    pub fn with_slice_budget(slice_budget: Duration) -> Self {
        Self::with_slice_budget_and_clock(slice_budget, Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self::with_slice_budget_and_clock(
            Duration::from_millis(constants::engine::RECONCILE_SLICE_MILLIS),
            clock,
        )
    }

    pub fn with_slice_budget_and_clock(slice_budget: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            slice_budget,
            running: None,
            clock,
        }
    }

    pub fn running_root(&self) -> Option<&PathBuf> {
        self.running.as_ref().map(|running| &running.root)
    }

    /// Whether a reconcile may start or keep running under `state`.
    /// Every state but `Suspended` admits one, at that state's caps: a
    /// condition that can last for hours (battery power, a user at the
    /// keyboard, Low Power Mode) slows a scan down and never holds it.
    pub fn admits(state: ThrottleState) -> bool {
        state != ThrottleState::Suspended
    }

    pub fn release_ready_deferred_reconciles(
        &mut self,
        maps: &mut BoundedEventIntentMaps,
        scheduler: &mut KeyedSupersedingScheduler,
        throttle_state: ThrottleState,
        now: SystemTime,
    ) -> usize {
        if !Self::admits(throttle_state) {
            return 0;
        }

        // Idle: the scan starts a moment after the storm ends. The
        // deferral exists to avoid competing with the burst and the
        // user, and both are gone. In the other states the deferral
        // runs its length.
        let quiet_release =
            (throttle_state == ThrottleState::Idle).then(crate::storm::IdleRelease::default);
        let ready = maps.take_deferred_reconcile_intents_ready_by(now, quiet_release);
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
        if self.running.is_some() || !Self::admits(throttle_state) {
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

        let now_inst = self.clock.now();
        self.running = Some(RunningReconcile {
            root: claimed.path.clone(),
            slice_started_inst: now_inst,
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
        let now_inst = self.clock.now();
        let running = self.running.as_mut()?;
        running.checkpoint_count += 1;

        // Slice elapsed uses the monotonic Instant pair so wall-clock
        // rewinds cannot trick the controller into pausing for budget
        // expiry that didn't actually happen on the monotonic axis.
        let slice_elapsed = now_inst.saturating_duration_since(running.slice_started_inst);
        let admits = Self::admits(throttle_state);
        let reason = if !admits {
            Some(ReconcilePauseReason::ThrottleSuspended)
        } else if slice_elapsed >= self.slice_budget {
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

    /// Aborts the running reconcile (comparison-walk failure): the
    /// permit is released and the intent requeues so a later attempt
    /// retries from scratch. Distinct from `checkpoint` pauses, which
    /// only fire on slice/throttle conditions.
    pub fn abort_running(
        &mut self,
        scheduler: &mut KeyedSupersedingScheduler,
        workgate: &mut ThrottleWorkgate,
        now: SystemTime,
    ) -> Option<PathBuf> {
        let running = self.running.take()?;
        release_permit_or_log(workgate, running.permit, "reconcile abort");
        requeue_claimed_reconcile(scheduler, &running.root, now);
        Some(running.root)
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
    use crate::clock::ManualClock;
    use crate::event_intents::{BoundedEventIntentMaps, EventIntentLimits};
    use crate::fs_events::{FsEventKind, FsEventRecord};
    use crate::scheduler::KeyedSupersedingScheduler;
    use crate::storm::StormThresholds;
    use crate::throttle::ThrottleController;
    use std::time::UNIX_EPOCH;

    #[test]
    fn deferred_reconciles_are_released_in_every_state_but_suspended() {
        let subtree_root = PathBuf::from("/tmp/vapor-root/project/sub");
        let mut maps = stormed_maps(&subtree_root);
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut controller = ReconcileController::default();

        assert_eq!(
            controller.release_ready_deferred_reconciles(
                &mut maps,
                &mut scheduler,
                ThrottleState::Suspended,
                timestamp(32),
            ),
            0
        );
        assert!(scheduler.claim_next_reconcile().is_none());

        // A device on battery releases the folder when its deferral
        // ends; it does not wait for an idle moment.
        assert_eq!(
            controller.release_ready_deferred_reconciles(
                &mut maps,
                &mut scheduler,
                ThrottleState::Light,
                timestamp(32),
            ),
            1
        );
        let claimed = scheduler.claim_next_reconcile().expect("reconcile intent");
        assert_eq!(claimed.path, subtree_root);
    }

    #[test]
    fn a_scan_starts_in_every_state_but_suspended_and_only_suspended_pauses_it() {
        let root = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();
        scheduler.upsert_intent(
            root.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(1),
        );
        let mut workgate = idle_reconcile_gate();
        let clock = Arc::new(ManualClock::at_now());
        let mut controller = ReconcileController::with_slice_budget_and_clock(
            Duration::from_secs(60),
            clock.clone(),
        );

        assert_eq!(
            controller
                .try_start_next(
                    &mut scheduler,
                    &mut workgate,
                    ThrottleState::Suspended,
                    timestamp(1)
                )
                .expect("gate"),
            None,
            "Suspended stops everything"
        );
        assert_eq!(
            controller
                .try_start_next(
                    &mut scheduler,
                    &mut workgate,
                    ThrottleState::Throttled,
                    timestamp(1)
                )
                .expect("gate"),
            Some(root.clone()),
            "a user at the keyboard slows a scan down and does not hold it"
        );

        // A state that is merely not idle does not pause the walk;
        // Suspended does, and the scan resumes in the next state that
        // admits it.
        clock.advance(Duration::from_secs(1));
        for state in [ThrottleState::Throttled, ThrottleState::Light] {
            assert!(
                controller
                    .checkpoint(&mut scheduler, &mut workgate, state, timestamp(2))
                    .is_none(),
                "{state:?} keeps the scan running"
            );
        }
        let pause = controller
            .checkpoint(
                &mut scheduler,
                &mut workgate,
                ThrottleState::Suspended,
                timestamp(3),
            )
            .expect("suspended pauses");
        assert_eq!(pause.reason, ReconcilePauseReason::ThrottleSuspended);
        assert_eq!(
            controller
                .try_start_next(
                    &mut scheduler,
                    &mut workgate,
                    ThrottleState::Light,
                    timestamp(4)
                )
                .expect("gate"),
            Some(root.clone())
        );
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
        let clock = Arc::new(ManualClock::at_now());
        let mut controller =
            ReconcileController::with_slice_budget_and_clock(Duration::from_secs(1), clock.clone());

        assert_eq!(
            controller
                .try_start_next(
                    &mut scheduler,
                    &mut workgate,
                    ThrottleState::Idle,
                    timestamp(1),
                )
                .expect("start reconcile"),
            Some(root.clone())
        );

        // Advance the monotonic clock past the slice budget: this is
        // what should make `checkpoint` decide the slice expired.
        clock.advance(Duration::from_secs(2));
        let pause = controller
            .checkpoint(
                &mut scheduler,
                &mut workgate,
                ThrottleState::Idle,
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
    fn running_reconcile_interrupts_when_sync_is_suspended() {
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
                ThrottleState::Idle,
                timestamp(1),
            )
            .expect("start reconcile");

        assert!(
            controller
                .checkpoint(
                    &mut scheduler,
                    &mut workgate,
                    ThrottleState::Light,
                    timestamp(2),
                )
                .is_none(),
            "a state that is only slower keeps the scan running"
        );
        let pause = controller
            .checkpoint(
                &mut scheduler,
                &mut workgate,
                ThrottleState::Suspended,
                timestamp(2),
            )
            .expect("reconcile should interrupt");

        assert_eq!(pause.reason, ReconcilePauseReason::ThrottleSuspended);
        assert_eq!(scheduler.pending_count(), 1);
    }

    #[test]
    fn checkpoint_slice_budget_uses_monotonic_clock_under_wall_clock_rewind() {
        // Invariant: slice budget elapses on the monotonic axis. A
        // wall-clock rewind alone must not make `checkpoint` pause,
        // that would have been a false positive under the old
        // SystemTime path. Advancing the monotonic clock past the slice
        // budget is the only thing that should trigger pause.
        let root = PathBuf::from("/tmp/vapor-root/project");
        let mut scheduler = KeyedSupersedingScheduler::default();
        scheduler.upsert_intent(
            root.clone(),
            PendingIntentKind::ReconcileSubtree,
            timestamp(1_000),
        );
        let mut workgate = idle_reconcile_gate();
        let clock = Arc::new(ManualClock::at_now());
        let mut controller =
            ReconcileController::with_slice_budget_and_clock(Duration::from_secs(1), clock.clone());

        controller
            .try_start_next(
                &mut scheduler,
                &mut workgate,
                ThrottleState::Idle,
                timestamp(1_000),
            )
            .expect("start reconcile");

        // Rewind the wall clock far backwards while the monotonic axis
        // stays still: under the new contract, no slice expiration.
        clock.set_system(timestamp(500));
        assert!(
            controller
                .checkpoint(
                    &mut scheduler,
                    &mut workgate,
                    ThrottleState::Idle,
                    timestamp(500),
                )
                .is_none(),
            "wall-clock rewind alone must not expire the reconcile slice"
        );

        // Now advance the monotonic clock past the slice budget: pause
        // is finally legitimate.
        clock.advance(Duration::from_secs(2));
        let pause = controller
            .checkpoint(
                &mut scheduler,
                &mut workgate,
                ThrottleState::Idle,
                timestamp(500),
            )
            .expect("monotonic-axis slice budget exceeded");
        assert_eq!(pause.reason, ReconcilePauseReason::SliceBudgetExpired);
    }

    #[test]
    fn the_release_of_a_deferred_reconcile_ends_the_compaction_of_its_folder() {
        let subtree_root = PathBuf::from("/tmp/vapor-root/project/sub");
        let mut maps = stormed_maps(&subtree_root);
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut controller = ReconcileController::default();
        let mut workgate = idle_reconcile_gate();

        assert_eq!(
            controller.release_ready_deferred_reconciles(
                &mut maps,
                &mut scheduler,
                ThrottleState::Idle,
                timestamp(32),
            ),
            1
        );
        // The walk has not started yet, and the folder is open again: a
        // save from now on is an event of its own. The walk lists the
        // folder after this moment, so it sees every change that the
        // compaction took in.
        assert!(maps.compacted_subtree(&subtree_root).is_none());
        maps.record_event(FsEventRecord {
            path: subtree_root.join("after.txt"),
            kind: FsEventKind::Modified,
            observed_at: timestamp(33),
        });
        assert_eq!(maps.pending_event_count(), 1);

        controller
            .try_start_next(
                &mut scheduler,
                &mut workgate,
                ThrottleState::Idle,
                timestamp(33),
            )
            .expect("start reconcile");
        let completion = controller
            .complete_success(&mut maps, &mut scheduler, &mut workgate)
            .expect("complete reconcile");

        assert_eq!(completion.root, subtree_root);
        assert_eq!(completion.disposition, CompletionDisposition::Removed);
        assert!(maps.deferred_reconcile(&subtree_root).is_none());
    }

    fn idle_reconcile_gate() -> ThrottleWorkgate {
        let controller = ThrottleController::default();
        ThrottleWorkgate::new(
            ThrottleState::Idle,
            controller.caps_for(ThrottleState::Idle),
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
                absorbed_busy_event_threshold: 1,
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
