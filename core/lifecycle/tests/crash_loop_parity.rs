//! Cross-surface parity contract for the crash-loop backoff schedule.
//!
//! These scenarios are mirrored, case for case, by the Swift tests in
//! `apps/macos/Tests/VaporCoreTests/DaemonLifecycleManagerTests.swift`.
//! If either implementation's schedule drifts, exactly one side of the
//! pair fails and the divergence is visible at PR time. The canonical
//! schedule is documented in `docs/operations/macos/launchagent-policy.md`:
//! with the default policy, crash 1 restarts immediately and backoff
//! starts at crash 2 (`2s → 4s → 8s → Paused on the 5th crash`).

use std::time::{Duration, Instant};

use vapor_lifecycle::{CrashLoopDecision, CrashLoopGuard, CrashLoopPolicy};

#[test]
fn default_policy_schedule_is_nodelay_then_doubling_backoff_then_pause() {
    let mut guard = CrashLoopGuard::new(CrashLoopPolicy::default());
    let t0 = Instant::now();

    assert_eq!(guard.register_crash(t0), CrashLoopDecision::NoDelay);
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(1)),
        CrashLoopDecision::Backoff(Duration::from_secs(2))
    );
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(2)),
        CrashLoopDecision::Backoff(Duration::from_secs(4))
    );
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(3)),
        CrashLoopDecision::Backoff(Duration::from_secs(8))
    );
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(4)),
        CrashLoopDecision::Paused
    );
}

#[test]
fn two_free_crashes_policy_starts_backoff_on_third_crash() {
    // Mirrors the Swift scenario with `delayStartsAfterFailures = 2`.
    let policy = CrashLoopPolicy::new(
        Duration::from_secs(60),
        Duration::from_secs(2),
        Duration::from_secs(32),
        2,
        10,
    );
    let mut guard = CrashLoopGuard::new(policy);
    let t0 = Instant::now();

    assert_eq!(guard.register_crash(t0), CrashLoopDecision::NoDelay);
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(1)),
        CrashLoopDecision::NoDelay
    );
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(2)),
        CrashLoopDecision::Backoff(Duration::from_secs(2))
    );
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(3)),
        CrashLoopDecision::Backoff(Duration::from_secs(4))
    );
}

#[test]
fn crashes_outside_failure_window_reset_the_schedule() {
    let policy = CrashLoopPolicy::new(
        Duration::from_secs(10),
        Duration::from_secs(2),
        Duration::from_secs(30),
        2,
        10,
    );
    let mut guard = CrashLoopGuard::new(policy);
    let t0 = Instant::now();

    assert_eq!(guard.register_crash(t0), CrashLoopDecision::NoDelay);
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(1)),
        CrashLoopDecision::NoDelay
    );
    // 20 seconds later — both prior crashes fell out of the 10 s window,
    // so the count restarts and the crash is free again.
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(20)),
        CrashLoopDecision::NoDelay
    );
}

#[test]
fn pause_state_reports_infinite_delay_until_acknowledged() {
    let policy = CrashLoopPolicy::new(
        Duration::from_secs(600),
        Duration::from_secs(2),
        Duration::from_secs(120),
        1,
        3,
    );
    let mut guard = CrashLoopGuard::new(policy);
    let t0 = Instant::now();

    let _ = guard.register_crash(t0);
    let _ = guard.register_crash(t0 + Duration::from_secs(1));
    assert_eq!(
        guard.register_crash(t0 + Duration::from_secs(2)),
        CrashLoopDecision::Paused
    );
    assert!(guard.is_paused_indefinitely());
    assert_eq!(
        guard.remaining_delay(t0 + Duration::from_secs(3_600)),
        Duration::MAX
    );

    guard.acknowledge_and_resume();
    assert!(!guard.is_paused_indefinitely());
    assert_eq!(
        guard.remaining_delay(t0 + Duration::from_secs(3_600)),
        Duration::ZERO
    );
}

#[test]
fn backoff_is_capped_at_max_delay() {
    let policy = CrashLoopPolicy::new(
        Duration::from_secs(600),
        Duration::from_secs(2),
        Duration::from_secs(8),
        1,
        20,
    );
    let mut guard = CrashLoopGuard::new(policy);
    let t0 = Instant::now();

    let _ = guard.register_crash(t0);
    for step in 1..8 {
        if let CrashLoopDecision::Backoff(delay) =
            guard.register_crash(t0 + Duration::from_secs(step))
        {
            assert!(delay <= Duration::from_secs(8));
        }
    }
}
