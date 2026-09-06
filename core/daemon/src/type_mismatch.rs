//! The `type-mismatch` decision: a path that is a file on one side and
//! a directory on the other. Two-way sync has no rule that picks a
//! winner without guessing, so both sides stay untouched and the user
//! is asked. Design: `docs/architecture/data-flow.md` §Decisions.

use std::path::Path;

use crate::state_db::DecisionOption;

pub const DECISION_KIND: &str = "type-mismatch";
pub const OPTION_KEEP_BOTH: &str = "keep-both";
pub const OPTION_PREFER_LOCAL: &str = "prefer-local";
pub const OPTION_PREFER_CLOUD: &str = "prefer-cloud";

pub fn question(path: &Path, local_is_dir: bool) -> String {
    let (here, there) = if local_is_dir {
        ("a folder", "a file")
    } else {
        ("a file", "a folder")
    };
    format!(
        "{} is {here} on this device and {there} in the cloud. Vapor is leaving both untouched. \
         Keep both (the local one moves to a conflict name and the cloud one comes down under \
         the original name), keep this device's (the cloud one is removed), or keep the cloud's \
         (the local one goes to the trash)?",
        path.display()
    )
}

pub fn options(local_is_dir: bool) -> [DecisionOption; 3] {
    let local = if local_is_dir { "folder" } else { "file" };
    let cloud = if local_is_dir { "file" } else { "folder" };
    [
        DecisionOption {
            key: OPTION_KEEP_BOTH.to_string(),
            label: format!(
                "Keep both: move this device's {local} to a conflict name, bring the cloud's {cloud} down"
            ),
        },
        DecisionOption {
            key: OPTION_PREFER_LOCAL.to_string(),
            label: format!("Keep this device's {local}; remove the cloud's {cloud}"),
        },
        DecisionOption {
            key: OPTION_PREFER_CLOUD.to_string(),
            label: format!("Keep the cloud's {cloud}; move this device's {local} to the trash"),
        },
    ]
}
