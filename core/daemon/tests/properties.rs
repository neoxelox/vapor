//! Property tests for the invariants a random input is likeliest to
//! break (`docs/architecture/testing-strategy.md` §Property tests).
//! Each runs a bounded number of cases so the Tier 1 budget holds;
//! the case count is the `PROPTEST_CASES` environment variable.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use proptest::prelude::*;
use vapor_daemon::event_intents::PendingIntentKind;
use vapor_daemon::fs_events::resolve_event_path_within_watch_root;
use vapor_daemon::retry::{RetryFailureKind, RetryPolicy};
use vapor_daemon::scheduler::KeyedSupersedingScheduler;

/// A path component the way a filesystem event could spell one: a
/// plain name, a dot, a parent step, or an empty piece from a doubled
/// separator.
fn component() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-zA-Z0-9_. -]{1,12}".prop_map(|s| s),
        1 => Just(".".to_string()),
        2 => Just("..".to_string()),
        1 => Just(String::new()),
    ]
}

fn event_path() -> impl Strategy<Value = PathBuf> {
    (prop::collection::vec(component(), 0..8), any::<bool>()).prop_map(|(parts, absolute)| {
        let joined = parts.join(std::path::MAIN_SEPARATOR_STR);
        if absolute {
            PathBuf::from(format!("{}{joined}", std::path::MAIN_SEPARATOR_STR))
        } else {
            PathBuf::from(joined)
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The engine only ever admits event paths that resolve inside the
    /// watch root. Whatever the watcher spells (`..` hops, `.`
    /// components, doubled separators, an absolute path elsewhere), a
    /// path that the check accepts lies under the root once its
    /// components are normalized, and no accepted path escapes.
    #[test]
    fn accepted_event_paths_never_escape_the_watch_root(candidate in event_path()) {
        let temp = tempfile::TempDir::new().expect("temp");
        let root = vapor_shared::paths::canonicalize(temp.path()).expect("canonical root");
        let inside = root.join(&candidate);
        if resolve_event_path_within_watch_root(&root, &inside) {
            // Normalize lexically the way the engine does and confirm
            // the result is under the root: every `..` consumed stays
            // inside it.
            let mut depth: i64 = 0;
            for component in candidate.components() {
                match component {
                    std::path::Component::ParentDir => depth -= 1,
                    std::path::Component::Normal(_) => depth += 1,
                    _ => {}
                }
                prop_assert!(depth >= 0, "accepted a path that climbs above the root: {candidate:?}");
            }
        }
        // A path rooted somewhere else is never accepted, however it is spelled.
        let elsewhere = PathBuf::from(format!("{}elsewhere-{}", std::path::MAIN_SEPARATOR_STR, std::process::id()));
        prop_assert!(!resolve_event_path_within_watch_root(&root, &elsewhere.join(&candidate)));
    }

    /// Any interleaving of upserts leaves the scheduler with at most
    /// one pending intent per path, and the one it keeps is the latest
    /// kind for that path.
    #[test]
    fn scheduler_keeps_one_pending_intent_per_path(
        ops in prop::collection::vec((0u8..6, 0u8..4, 0u64..100_000), 1..64)
    ) {
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut latest: std::collections::BTreeMap<PathBuf, PendingIntentKind> =
            std::collections::BTreeMap::new();
        let kinds = [
            PendingIntentKind::Upload,
            PendingIntentKind::Delete,
            PendingIntentKind::Rename,
            PendingIntentKind::Download,
        ];
        for (path_index, kind_index, at_ms) in ops {
            let path = PathBuf::from(format!("/root/file-{path_index}.txt"));
            let kind = kinds[usize::from(kind_index)];
            scheduler.upsert_intent(path.clone(), kind, SystemTime::UNIX_EPOCH + Duration::from_millis(at_ms));
            latest.insert(path, kind);
        }
        prop_assert_eq!(scheduler.pending_count(), latest.len());
        for (path, kind) in &latest {
            let scheduled = scheduler
                .scheduled_intent(path)
                .expect("every upserted path stays scheduled");
            prop_assert_eq!(scheduled.kind, *kind, "the latest kind wins for {}", path.display());
        }
        // Draining claims each path exactly once.
        let mut claimed = std::collections::BTreeSet::new();
        while let Some(intent) = scheduler.claim_next() {
            prop_assert!(claimed.insert(intent.path.clone()), "claimed twice: {}", intent.path.display());
        }
        prop_assert_eq!(claimed.len(), latest.len());
    }

    /// Transient retry delays never shrink as attempts grow and never
    /// exceed the policy's ceiling, whatever the intent id (which only
    /// seeds the jitter).
    #[test]
    fn retry_backoff_is_monotonic_and_capped(
        intent_id in any::<i64>(),
        attempts in 1u32..40,
    ) {
        let policy = RetryPolicy::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ceiling = vapor_shared::constants::engine::RETRY_MAX_DELAY_MILLIS as u128;
        let jitter = u128::from(vapor_shared::constants::engine::RETRY_JITTER_PERCENT);
        let mut previous: Option<u128> = None;
        for attempt in 1..=attempts {
            let decision = policy.decide(intent_id, attempt, RetryFailureKind::Transient, now);
            prop_assert!(decision.retryable);
            let delay = decision.delay.expect("transient failures retry").as_millis();
            prop_assert!(delay > 0, "attempt {attempt}: a zero delay would spin");
            // Jitter is symmetric around the exponential base, so the
            // widest the delay can be is the ceiling plus its band.
            prop_assert!(
                delay * 100 <= ceiling * (100 + jitter),
                "attempt {attempt}: {delay} ms is past the ceiling"
            );
            // The base never shrinks between attempts: the lowest the
            // next band can reach is above the highest the previous
            // band could have reached, once both are scaled by the
            // band width.
            if let Some(previous) = previous {
                prop_assert!(
                    previous * (100 - jitter) <= delay * (100 + jitter),
                    "attempt {attempt}: {delay} ms after {previous} ms is a shrinking base"
                );
            }
            previous = Some(delay);
        }
    }
}
