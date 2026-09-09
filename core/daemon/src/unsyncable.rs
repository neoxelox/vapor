//! The `unsyncable-name` decision: a local file whose name cannot be
//! carried to the other side, so it never syncs. Today that is a name
//! that is not valid UTF-8 (possible on Linux; macOS refuses to create
//! one). Nothing is at stake but the user's awareness, so the question
//! has one answer, `skip`, which stops the asking; the fix is a rename,
//! after which the walk finds the name gone and withdraws the
//! question. Design: `docs/architecture/data-flow.md` §Decisions.

use std::path::{Path, PathBuf};

use crate::state_db::DecisionOption;

pub const DECISION_KIND: &str = "unsyncable-name";
pub const OPTION_SKIP: &str = "skip";

/// A name the walk could not carry, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsyncableName {
    /// The path, spelled as closely as the name allows (a byte that is
    /// not UTF-8 shows as the replacement character); the decision's
    /// scope key.
    pub shown_path: PathBuf,
    pub reason: String,
}

impl UnsyncableName {
    pub fn not_utf8(path: &Path) -> Self {
        Self {
            shown_path: PathBuf::from(path.to_string_lossy().into_owned()),
            reason: "its name is not valid UTF-8, which the sync path model cannot carry"
                .to_string(),
        }
    }
}

pub fn question(name: &UnsyncableName) -> String {
    format!(
        "{} cannot be synced: {}. It stays on this device only. Rename it and it syncs; choose \
         skip to stop being asked about it.",
        name.shown_path.display(),
        name.reason
    )
}

pub fn options() -> [DecisionOption; 1] {
    [DecisionOption {
        key: OPTION_SKIP.to_string(),
        label: "Skip this file; stop asking".to_string(),
    }]
}
