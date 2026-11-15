//! The acknowledgement a one-way mode needs before it runs.
//!
//! `pull-only` and `push-only` make one side an exact copy of the
//! other: they overwrite files that differ and remove files that the
//! source of truth does not have. A profile in such a mode holds, and
//! syncs nothing, until the user has acknowledged that for this mode
//! and this pair of roots. The question is a decision, so every
//! surface shows the same text, and a `vapor.json` edited by hand
//! cannot start a mirror on its own. A change of the mode or of a root
//! is a new mirror and asks again.

use std::path::Path;
use std::time::SystemTime;

use vapor_shared::{SyncMode, constants};

use crate::state_db::{DecisionOption, DurableStateDb, StateDbError};

pub const DECISION_KIND: &str = constants::decision::KIND_ONE_WAY_MODE;
pub const OPTION_ENABLE: &str = constants::decision::OPTION_ENABLE;

/// What an acknowledgement is for: the mode and the two roots. The
/// state entry holds it; a profile whose scope gives another value was
/// not acknowledged.
pub fn fingerprint(mode: SyncMode, local_root: &Path, cloud_root: &str) -> String {
    format!(
        "{}\n{}\n{}",
        mode.as_config_value(),
        local_root.display(),
        cloud_root
    )
}

/// Whether the user acknowledged this mode for these two roots.
pub fn acknowledged(
    state_db: &DurableStateDb,
    mode: SyncMode,
    local_root: &Path,
    cloud_root: &str,
) -> Result<bool, StateDbError> {
    let wanted = fingerprint(mode, local_root, cloud_root);
    Ok(state_db
        .state(constants::state::ONE_WAY_ACKNOWLEDGED_KEY)?
        .is_some_and(|entry| entry.value == wanted))
}

/// Records the acknowledgement. The daemon calls it when the decision
/// is answered `enable`; a setup surface that asked the question
/// itself (`vapor profiles add` on a terminal) calls it before the
/// daemon ever composes the profile.
pub fn acknowledge(
    state_db: &mut DurableStateDb,
    mode: SyncMode,
    local_root: &Path,
    cloud_root: &str,
    now: SystemTime,
) -> Result<(), StateDbError> {
    state_db
        .set_state(
            constants::state::ONE_WAY_ACKNOWLEDGED_KEY,
            &fingerprint(mode, local_root, cloud_root),
            now,
        )
        .map(|_| ())
}

/// The question, in the words every surface shows. It says which side
/// is made to match which, what is overwritten, what is removed, and
/// what has no undo.
pub fn question(mode: SyncMode, local_root: &Path, cloud_root: &str) -> String {
    let local = local_root.display();
    match mode {
        SyncMode::PullOnly => format!(
            "This profile is set to pull-only. Vapor makes {local} an exact copy of the cloud \
             folder {cloud_root}. It overwrites each file that differs on this device, and it \
             removes each file that is only on this device. A removed file goes to the Vapor \
             trash for the retention time. An overwritten file has no undo. Enable pull-only \
             for these two folders?"
        ),
        SyncMode::PushOnly => format!(
            "This profile is set to push-only. Vapor makes the cloud folder {cloud_root} an \
             exact copy of {local}. It overwrites each file that differs in the cloud, and it \
             removes each file that is only in the cloud. A provider with no trash keeps no \
             copy of what is removed or overwritten. Enable push-only for these two folders?"
        ),
        SyncMode::TwoWay => String::new(),
    }
}

pub fn options(mode: SyncMode) -> [DecisionOption; 1] {
    [DecisionOption {
        key: OPTION_ENABLE.to_string(),
        label: format!("Enable {} for these two folders", mode.as_config_value()),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn an_acknowledgement_covers_one_mode_and_one_pair_of_roots() {
        let mut state_db = DurableStateDb::open_in_memory().expect("open");
        let local = Path::new("/home/sam/Photos");
        let cloud = "/Photos";
        assert!(!acknowledged(&state_db, SyncMode::PullOnly, local, cloud).expect("read"));

        acknowledge(&mut state_db, SyncMode::PullOnly, local, cloud, at(1)).expect("ack");
        assert!(acknowledged(&state_db, SyncMode::PullOnly, local, cloud).expect("read"));

        // The other mirror, another local folder, another cloud folder:
        // each is a new mirror that was not acknowledged.
        assert!(!acknowledged(&state_db, SyncMode::PushOnly, local, cloud).expect("read"));
        assert!(
            !acknowledged(
                &state_db,
                SyncMode::PullOnly,
                Path::new("/home/sam/Other"),
                cloud
            )
            .expect("read")
        );
        assert!(!acknowledged(&state_db, SyncMode::PullOnly, local, "/Archive").expect("read"));
    }

    #[test]
    fn the_question_names_the_side_that_is_overwritten() {
        let local = Path::new("/home/sam/Photos");
        let pull = question(SyncMode::PullOnly, local, "/Photos");
        assert!(pull.contains("makes /home/sam/Photos an exact copy of the cloud folder /Photos"));
        assert!(pull.contains("removes each file that is only on this device"));
        let push = question(SyncMode::PushOnly, local, "/Photos");
        assert!(push.contains("makes the cloud folder /Photos an exact copy of /home/sam/Photos"));
        assert!(push.contains("removes each file that is only in the cloud"));
        assert_eq!(options(SyncMode::PushOnly)[0].key, "enable");
    }
}
