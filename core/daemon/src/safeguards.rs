//! Lease-order priority for the durable queue.
//!
//! [`intent_priority_rank`] reuses the debounce classification so key
//! config files and code flush ahead of lockfile noise when a single
//! tick drains a mixed batch to the durable queue.
//! [`durable_intent_priority_rank`] adds where an intent came from
//! ([`IntentSource`]): reconcile backlog leases after every fresh
//! change.

use crate::debounce::DebounceClass;

/// Folder/path priority classes for durable-queue flush order.
///
/// Lower rank flushes first. Reuses the debounce classification (the
/// same signal that already encodes "how urgent is a change to this
/// path"): key config files carry user intent, code/text is active
/// work, lockfiles are derived noise.
pub fn intent_priority_rank(class: DebounceClass) -> u8 {
    match class {
        DebounceClass::KeyConfig => 0,
        DebounceClass::CodeText => 1,
        DebounceClass::Document => 2,
        DebounceClass::Other => 3,
        DebounceClass::Lockfile => 4,
    }
}

/// Where a durable queue intent came from, for lease-priority
/// purposes. Reconcile-walk backlog ranks below every fresh intent so
/// a whole-scope reconcile of a large tree can never starve a file the
/// user just edited: background convergence yields to foreground
/// changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntentSource {
    /// Debounced local edits, remote-change polls, conflict follow-ups.
    Fresh,
    /// Comparison-walk output of a whole-scope/subtree reconcile.
    ReconcileBacklog,
}

/// Rank for `ReconcileSubtree` control intents: first. Leasing one is
/// cheap (it only hands the walk to the reconcile controller, which
/// itself defers until Idle) and the startup reconstruction
/// barrier depends on the whole-scope reconcile leasing ahead of any
/// recovered file intents.
pub const RECONCILE_INTENT_PRIORITY_RANK: u8 = 0;

/// Fresh file intents rank `1 + class` (1..=5), preserving the
/// key-config-before-lockfile order within fresh work.
pub const FRESH_INTENT_PRIORITY_RANK_BASE: u8 = 1;

/// Reconcile-backlog file intents rank below every fresh class.
pub const BACKLOG_INTENT_PRIORITY_RANK: u8 = 6;

/// Durable lease-order priority for one queue row (lower leases
/// first): reconcile control intents, then fresh file intents by the
/// path's debounce class, then reconcile-backlog file intents.
pub fn durable_intent_priority_rank(
    path: &std::path::Path,
    kind: crate::event_intents::PendingIntentKind,
    source: IntentSource,
) -> u8 {
    if kind == crate::event_intents::PendingIntentKind::ReconcileSubtree {
        return RECONCILE_INTENT_PRIORITY_RANK;
    }
    if source == IntentSource::ReconcileBacklog {
        return BACKLOG_INTENT_PRIORITY_RANK;
    }
    FRESH_INTENT_PRIORITY_RANK_BASE
        + intent_priority_rank(
            crate::debounce::DebounceWindows::default()
                .classify_path(path)
                .0,
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_ranks_order_config_before_code_before_noise() {
        assert!(
            intent_priority_rank(DebounceClass::KeyConfig)
                < intent_priority_rank(DebounceClass::CodeText)
        );
        assert!(
            intent_priority_rank(DebounceClass::CodeText)
                < intent_priority_rank(DebounceClass::Other)
        );
        assert!(
            intent_priority_rank(DebounceClass::Other)
                < intent_priority_rank(DebounceClass::Lockfile)
        );
    }
}
