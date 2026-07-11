use std::collections::BTreeMap;

use vapor_shared::ThrottleState;

use crate::throttle::ThrottleCaps;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkClass {
    Planner,
    Hash,
    Upload,
    Download,
    Reconcile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkPermit {
    pub id: u64,
    pub class: WorkClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActivePermit {
    class: WorkClass,
    uses_planner_slot: bool,
    uses_read_token: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkPermitDeniedReason {
    PlannerWorkersExhausted,
    HashWorkersExhausted,
    ReadTokensExhausted,
    UploadConcurrencyExhausted,
    DownloadConcurrencyExhausted,
    HashingDisabled,
    UploadsDisabled,
    DownloadsDisabled,
    ReconcileDisabled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkPermitDenied {
    pub class: WorkClass,
    pub reason: WorkPermitDeniedReason,
    pub throttle_state: ThrottleState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkgateSnapshot {
    pub throttle_state: ThrottleState,
    pub caps: ThrottleCaps,
    pub active_planner_workers: usize,
    pub active_hash_workers: usize,
    pub active_uploads: usize,
    pub active_downloads: usize,
    pub active_reconciles: usize,
    pub active_read_tokens: usize,
}

impl WorkgateSnapshot {
    pub fn available_planner_workers(&self) -> usize {
        self.caps
            .planner_workers
            .saturating_sub(self.active_planner_workers)
    }

    pub fn available_hash_workers(&self) -> usize {
        self.caps
            .hash_workers
            .saturating_sub(self.active_hash_workers)
    }

    pub fn available_uploads(&self) -> usize {
        self.caps
            .upload_concurrency
            .saturating_sub(self.active_uploads)
    }

    pub fn available_downloads(&self) -> usize {
        self.caps
            .download_concurrency
            .saturating_sub(self.active_downloads)
    }

    pub fn available_read_tokens(&self) -> usize {
        self.caps
            .read_tokens
            .saturating_sub(self.active_read_tokens)
    }
}

#[derive(Debug)]
pub struct ThrottleWorkgate {
    throttle_state: ThrottleState,
    caps: ThrottleCaps,
    active_planner_workers: usize,
    active_hash_workers: usize,
    active_uploads: usize,
    active_downloads: usize,
    active_reconciles: usize,
    active_read_tokens: usize,
    next_permit_id: u64,
    active_permits: BTreeMap<u64, ActivePermit>,
}

impl ThrottleWorkgate {
    pub fn new(throttle_state: ThrottleState, caps: ThrottleCaps) -> Self {
        Self {
            throttle_state,
            caps,
            active_planner_workers: 0,
            active_hash_workers: 0,
            active_uploads: 0,
            active_downloads: 0,
            active_reconciles: 0,
            active_read_tokens: 0,
            next_permit_id: 1,
            active_permits: BTreeMap::new(),
        }
    }

    pub fn reconfigure(&mut self, throttle_state: ThrottleState, caps: ThrottleCaps) {
        self.throttle_state = throttle_state;
        self.caps = caps;
    }

    pub fn snapshot(&self) -> WorkgateSnapshot {
        WorkgateSnapshot {
            throttle_state: self.throttle_state,
            caps: self.caps,
            active_planner_workers: self.active_planner_workers,
            active_hash_workers: self.active_hash_workers,
            active_uploads: self.active_uploads,
            active_downloads: self.active_downloads,
            active_reconciles: self.active_reconciles,
            active_read_tokens: self.active_read_tokens,
        }
    }

    pub fn try_acquire(&mut self, class: WorkClass) -> Result<WorkPermit, WorkPermitDenied> {
        let active_permit = match class {
            WorkClass::Planner => {
                self.ensure_planner_capacity(class)?;
                ActivePermit {
                    class,
                    uses_planner_slot: true,
                    uses_read_token: false,
                }
            }
            WorkClass::Hash => {
                self.ensure_hash_capacity()?;
                ActivePermit {
                    class,
                    uses_planner_slot: false,
                    uses_read_token: true,
                }
            }
            WorkClass::Upload => {
                self.ensure_upload_capacity()?;
                ActivePermit {
                    class,
                    uses_planner_slot: false,
                    uses_read_token: false,
                }
            }
            WorkClass::Download => {
                self.ensure_download_capacity()?;
                ActivePermit {
                    class,
                    uses_planner_slot: false,
                    uses_read_token: false,
                }
            }
            WorkClass::Reconcile => {
                self.ensure_reconcile_capacity()?;
                ActivePermit {
                    class,
                    uses_planner_slot: true,
                    uses_read_token: true,
                }
            }
        };

        let id = self.allocate_permit_id();
        let permit = WorkPermit { id, class };
        self.active_permits.insert(permit.id, active_permit);
        self.increment_counts(active_permit);
        Ok(permit)
    }

    /// Returns a permit id that does not collide with any currently-active
    /// permit. Uses wrapping arithmetic so the engine survives
    /// `u64::MAX` saturation without locking up on a single id (the previous
    /// `saturating_add` impl would have re-issued `u64::MAX` for every
    /// subsequent allocation, breaking the active-permits map invariants).
    fn allocate_permit_id(&mut self) -> u64 {
        // Active permits are bounded by the workgate caps (single digits),
        // so the worst-case loop length is bounded by `caps.planner_workers
        // + caps.hash_workers + caps.upload_concurrency + caps.read_tokens`,
        // which is far below `u64::MAX`. The loop is guaranteed to find a
        // free slot before exhausting the address space.
        loop {
            let id = self.next_permit_id;
            self.next_permit_id = self.next_permit_id.wrapping_add(1);
            if !self.active_permits.contains_key(&id) {
                return id;
            }
        }
    }

    pub fn release(&mut self, permit: WorkPermit) -> bool {
        let Some(active_permit) = self.active_permits.remove(&permit.id) else {
            return false;
        };

        if active_permit.class != permit.class {
            self.active_permits.insert(permit.id, active_permit);
            return false;
        }

        self.decrement_counts(active_permit);
        true
    }

    fn ensure_planner_capacity(&self, class: WorkClass) -> Result<(), WorkPermitDenied> {
        if self.active_planner_workers >= self.caps.planner_workers {
            return Err(self.denied(class, WorkPermitDeniedReason::PlannerWorkersExhausted));
        }
        Ok(())
    }

    fn ensure_hash_capacity(&self) -> Result<(), WorkPermitDenied> {
        if !self.caps.allow_hashing {
            return Err(self.denied(WorkClass::Hash, WorkPermitDeniedReason::HashingDisabled));
        }
        if self.active_hash_workers >= self.caps.hash_workers {
            return Err(self.denied(
                WorkClass::Hash,
                WorkPermitDeniedReason::HashWorkersExhausted,
            ));
        }
        if self.active_read_tokens >= self.caps.read_tokens {
            return Err(self.denied(WorkClass::Hash, WorkPermitDeniedReason::ReadTokensExhausted));
        }
        Ok(())
    }

    fn ensure_upload_capacity(&self) -> Result<(), WorkPermitDenied> {
        if !self.caps.allow_uploads {
            return Err(self.denied(WorkClass::Upload, WorkPermitDeniedReason::UploadsDisabled));
        }
        if self.active_uploads >= self.caps.upload_concurrency {
            return Err(self.denied(
                WorkClass::Upload,
                WorkPermitDeniedReason::UploadConcurrencyExhausted,
            ));
        }
        Ok(())
    }

    fn ensure_download_capacity(&self) -> Result<(), WorkPermitDenied> {
        if !self.caps.allow_downloads {
            return Err(self.denied(
                WorkClass::Download,
                WorkPermitDeniedReason::DownloadsDisabled,
            ));
        }
        if self.active_downloads >= self.caps.download_concurrency {
            return Err(self.denied(
                WorkClass::Download,
                WorkPermitDeniedReason::DownloadConcurrencyExhausted,
            ));
        }
        Ok(())
    }

    fn ensure_reconcile_capacity(&self) -> Result<(), WorkPermitDenied> {
        if !self.caps.allow_reconcile {
            return Err(self.denied(
                WorkClass::Reconcile,
                WorkPermitDeniedReason::ReconcileDisabled,
            ));
        }
        self.ensure_planner_capacity(WorkClass::Reconcile)?;
        if self.active_read_tokens >= self.caps.read_tokens {
            return Err(self.denied(
                WorkClass::Reconcile,
                WorkPermitDeniedReason::ReadTokensExhausted,
            ));
        }
        Ok(())
    }

    fn increment_counts(&mut self, permit: ActivePermit) {
        match permit.class {
            WorkClass::Planner => {
                self.active_planner_workers += 1;
            }
            WorkClass::Hash => {
                self.active_hash_workers += 1;
            }
            WorkClass::Upload => {
                self.active_uploads += 1;
            }
            WorkClass::Download => {
                self.active_downloads += 1;
            }
            WorkClass::Reconcile => {
                self.active_reconciles += 1;
            }
        }

        if permit.uses_planner_slot && permit.class != WorkClass::Planner {
            self.active_planner_workers += 1;
        }
        if permit.uses_read_token {
            self.active_read_tokens += 1;
        }
    }

    fn decrement_counts(&mut self, permit: ActivePermit) {
        match permit.class {
            WorkClass::Planner => {
                self.active_planner_workers = self.active_planner_workers.saturating_sub(1);
            }
            WorkClass::Hash => {
                self.active_hash_workers = self.active_hash_workers.saturating_sub(1);
            }
            WorkClass::Upload => {
                self.active_uploads = self.active_uploads.saturating_sub(1);
            }
            WorkClass::Download => {
                self.active_downloads = self.active_downloads.saturating_sub(1);
            }
            WorkClass::Reconcile => {
                self.active_reconciles = self.active_reconciles.saturating_sub(1);
            }
        }

        if permit.uses_planner_slot && permit.class != WorkClass::Planner {
            self.active_planner_workers = self.active_planner_workers.saturating_sub(1);
        }
        if permit.uses_read_token {
            self.active_read_tokens = self.active_read_tokens.saturating_sub(1);
        }
    }

    fn denied(&self, class: WorkClass, reason: WorkPermitDeniedReason) -> WorkPermitDenied {
        WorkPermitDenied {
            class,
            reason,
            throttle_state: self.throttle_state,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_next_permit_id_for_testing(&mut self, id: u64) {
        self.next_permit_id = id;
    }

    #[cfg(test)]
    pub(crate) fn next_permit_id_for_testing(&self) -> u64 {
        self.next_permit_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::throttle::ThrottleController;

    #[test]
    fn idle_drain_allows_planner_and_uploads_up_to_cap() {
        let mut gate = idle_drain_gate();

        let planner_permits: Vec<WorkPermit> = (0..4)
            .map(|_| {
                gate.try_acquire(WorkClass::Planner)
                    .expect("planner permit")
            })
            .collect();
        let upload_permits: Vec<WorkPermit> = (0..4)
            .map(|_| gate.try_acquire(WorkClass::Upload).expect("upload permit"))
            .collect();

        let planner_denied = gate
            .try_acquire(WorkClass::Planner)
            .expect_err("planner cap should be enforced");
        let upload_denied = gate
            .try_acquire(WorkClass::Upload)
            .expect_err("upload cap should be enforced");

        assert_eq!(
            planner_denied.reason,
            WorkPermitDeniedReason::PlannerWorkersExhausted
        );
        assert_eq!(
            upload_denied.reason,
            WorkPermitDeniedReason::UploadConcurrencyExhausted
        );

        let snapshot = gate.snapshot();
        assert_eq!(snapshot.active_planner_workers, 4);
        assert_eq!(snapshot.active_uploads, 4);
        assert_eq!(snapshot.available_planner_workers(), 0);
        assert_eq!(snapshot.available_uploads(), 0);

        for permit in planner_permits.into_iter().chain(upload_permits) {
            assert!(gate.release(permit));
        }
        assert_eq!(gate.snapshot().active_planner_workers, 0);
        assert_eq!(gate.snapshot().active_uploads, 0);
    }

    #[test]
    fn hash_work_is_limited_by_hash_workers_and_read_tokens() {
        let mut gate = idle_drain_gate();

        let first = gate
            .try_acquire(WorkClass::Hash)
            .expect("first hash permit");
        let second = gate
            .try_acquire(WorkClass::Hash)
            .expect("second hash permit");
        let denied = gate
            .try_acquire(WorkClass::Hash)
            .expect_err("read token cap should block third hash permit");

        assert_eq!(denied.reason, WorkPermitDeniedReason::ReadTokensExhausted);
        let snapshot = gate.snapshot();
        assert_eq!(snapshot.active_hash_workers, 2);
        assert_eq!(snapshot.active_read_tokens, 2);
        assert_eq!(snapshot.available_hash_workers(), 2);
        assert_eq!(snapshot.available_read_tokens(), 0);

        assert!(gate.release(first));
        assert!(gate.release(second));
    }

    #[test]
    fn reconcile_consumes_planner_capacity_and_read_tokens() {
        let mut gate = idle_drain_gate();

        let reconcile = gate
            .try_acquire(WorkClass::Reconcile)
            .expect("reconcile permit should be allowed in idle drain");
        let hash = gate.try_acquire(WorkClass::Hash).expect("hash permit");
        let denied = gate
            .try_acquire(WorkClass::Hash)
            .expect_err("read tokens should be shared with reconcile");

        assert_eq!(denied.reason, WorkPermitDeniedReason::ReadTokensExhausted);
        let snapshot = gate.snapshot();
        assert_eq!(snapshot.active_reconciles, 1);
        assert_eq!(snapshot.active_planner_workers, 1);
        assert_eq!(snapshot.active_read_tokens, 2);

        assert!(gate.release(reconcile));
        assert!(gate.release(hash));
    }

    #[test]
    fn suspended_state_blocks_hash_upload_and_reconcile_work() {
        let controller = ThrottleController::default();
        let mut gate = ThrottleWorkgate::new(
            ThrottleState::Suspended,
            controller.caps_for(ThrottleState::Suspended),
        );

        let planner_denied = gate
            .try_acquire(WorkClass::Planner)
            .expect_err("planner workers should be zero when suspended");
        let hash_denied = gate
            .try_acquire(WorkClass::Hash)
            .expect_err("hashing should be disabled when suspended");
        let upload_denied = gate
            .try_acquire(WorkClass::Upload)
            .expect_err("uploads should be disabled when suspended");
        let reconcile_denied = gate
            .try_acquire(WorkClass::Reconcile)
            .expect_err("reconcile should be disabled when suspended");

        assert_eq!(
            planner_denied.reason,
            WorkPermitDeniedReason::PlannerWorkersExhausted
        );
        assert_eq!(hash_denied.reason, WorkPermitDeniedReason::HashingDisabled);
        assert_eq!(
            upload_denied.reason,
            WorkPermitDeniedReason::UploadsDisabled
        );
        assert_eq!(
            reconcile_denied.reason,
            WorkPermitDeniedReason::ReconcileDisabled
        );
    }

    #[test]
    fn downshifting_caps_keeps_running_work_but_blocks_new_acquires() {
        let controller = ThrottleController::default();
        let mut gate = idle_drain_gate();
        let first = gate
            .try_acquire(WorkClass::Upload)
            .expect("first upload permit");
        let second = gate
            .try_acquire(WorkClass::Upload)
            .expect("second upload permit");
        let third = gate
            .try_acquire(WorkClass::Upload)
            .expect("third upload permit");

        gate.reconfigure(
            ThrottleState::Throttled,
            controller.caps_for(ThrottleState::Throttled),
        );

        let denied = gate
            .try_acquire(WorkClass::Upload)
            .expect_err("new uploads must respect lowered cap");
        assert_eq!(
            denied.reason,
            WorkPermitDeniedReason::UploadConcurrencyExhausted
        );
        assert_eq!(gate.snapshot().active_uploads, 3);
        assert_eq!(gate.snapshot().available_uploads(), 0);

        assert!(gate.release(first));
        assert!(gate.release(second));
        assert!(gate.release(third));

        let resumed = gate
            .try_acquire(WorkClass::Upload)
            .expect("single throttled upload should be allowed once active count drops");
        assert_eq!(gate.snapshot().active_uploads, 1);
        assert!(gate.release(resumed));
    }

    #[test]
    fn invalid_or_duplicate_release_is_rejected() {
        let mut gate = idle_drain_gate();
        let permit = gate
            .try_acquire(WorkClass::Planner)
            .expect("planner permit");
        let mismatched = WorkPermit {
            id: permit.id,
            class: WorkClass::Upload,
        };

        assert!(!gate.release(mismatched));
        assert!(gate.release(permit));
        assert!(!gate.release(permit));
        assert!(!gate.release(WorkPermit {
            id: 999,
            class: WorkClass::Upload,
        }));
    }

    fn idle_drain_gate() -> ThrottleWorkgate {
        let controller = ThrottleController::default();
        ThrottleWorkgate::new(
            ThrottleState::IdleDrain,
            controller.caps_for(ThrottleState::IdleDrain),
        )
    }

    #[test]
    fn permit_id_wraps_past_u64_max_and_skips_already_active_ids() {
        // the workgate must keep allocating
        // unique permit ids even after the counter saturates. We seed the
        // allocator one shy of u64::MAX, then walk it past the boundary
        // while holding a permit at id 0 to force the wrap-around branch
        // to skip a colliding slot.
        let mut gate = idle_drain_gate();

        // Drive the allocator near saturation.
        gate.set_next_permit_id_for_testing(u64::MAX);
        let max_permit = gate
            .try_acquire(WorkClass::Planner)
            .expect("permit at u64::MAX");
        assert_eq!(max_permit.id, u64::MAX);
        // After the wrap, the next allocation lands at id 0.
        assert_eq!(gate.next_permit_id_for_testing(), 0);

        // Hold a synthetic active permit at id 0 to force the allocator to
        // skip it. We do this by acquiring + remembering id 0 first.
        let zero_permit = gate
            .try_acquire(WorkClass::Planner)
            .expect("permit at id 0");
        assert_eq!(zero_permit.id, 0);

        // The allocator must keep walking past 0 / 1 if those collide.
        // Re-set the counter to 0 to *force* a collision; the allocator
        // should walk past the active permits and land at the next free id.
        gate.set_next_permit_id_for_testing(0);
        let next_permit = gate
            .try_acquire(WorkClass::Upload)
            .expect("permit after forced collision");
        assert!(
            next_permit.id != u64::MAX && next_permit.id != 0,
            "wrap-around allocator must skip already-active permit ids; got {}",
            next_permit.id
        );

        assert!(gate.release(max_permit));
        assert!(gate.release(zero_permit));
        assert!(gate.release(next_permit));
    }

    #[test]
    fn permit_ids_remain_unique_under_alternating_acquire_release_through_wrap() {
        // Stress check: walk the counter through the boundary while
        // alternating acquire/release. Every issued permit must hold a
        // unique id, and the workgate's bookkeeping must agree on each
        // release (returns `false` for an id that is not currently active
        // or has the wrong class). We release each permit before acquiring
        // the next so we don't bump into the upload-concurrency cap.
        let mut gate = idle_drain_gate();
        gate.set_next_permit_id_for_testing(u64::MAX - 2);

        let mut observed_ids = Vec::new();
        for _ in 0..6 {
            let permit = gate
                .try_acquire(WorkClass::Upload)
                .expect("upload permit during wrap walk");
            observed_ids.push(permit.id);
            assert!(gate.release(permit));
        }

        // After alternating acquire/release through the wrap, every id we
        // saw must be distinct — i.e., the wrap-around branch never re-
        // issued an id while it was still active. Adjacent ids may match
        // post-release because the active-permits map is now empty, but the
        // *issued sequence* must still be monotonic-with-wrap (no repeats
        // within the in-flight window of one). Verifying distinct ids
        // across all six acquisitions is the strict version of that check.
        let mut sorted = observed_ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            observed_ids.len(),
            "permit ids must stay unique across the wrap boundary; observed {observed_ids:?}"
        );
    }
}
