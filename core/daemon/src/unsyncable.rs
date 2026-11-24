//! The `unsyncable-name` decision: a file whose name cannot be
//! carried to the other side, so it never syncs. On this device that
//! is a name that is not valid UTF-8 (possible on Linux; macOS refuses
//! to create one) and a name with a backslash in it (macOS allows one;
//! the sync path model keeps `\` for the Windows separator and refuses
//! it in a name). In the cloud it is whatever the provider's listing
//! reports as no sync path: the same two in a cloud folder on a drive,
//! a `/` in a Google Drive name.
//! Nothing is at stake but the user's awareness, so the question
//! has one answer, `skip`, which stops the asking; the fix is a rename,
//! after which the walk finds the name gone and withdraws the
//! question. Design: `docs/architecture/data-flow.md` §Decisions.

use std::path::{Path, PathBuf};

use crate::state_db::DecisionOption;

pub use vapor_shared::constants::decision::{KIND_UNSYNCABLE_NAME as DECISION_KIND, OPTION_SKIP};

/// A name the walk could not carry, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsyncableName {
    /// The path, spelled as closely as the name allows (a byte that is
    /// not UTF-8 shows as the replacement character); the decision's
    /// scope key.
    pub shown_path: PathBuf,
    pub reason: String,
    /// The name is in the cloud folder, not on this device.
    pub in_the_cloud: bool,
}

impl UnsyncableName {
    /// A name with a backslash in it.
    pub fn backslash(path: &Path) -> Self {
        Self {
            shown_path: path.to_path_buf(),
            reason: "its name has a backslash in it, which the sync path model cannot carry"
                .to_string(),
            in_the_cloud: false,
        }
    }

    /// A name that the provider found in the cloud folder that
    /// `local_directory` syncs with, and that cannot be a sync path.
    /// The key is where the file would be on this device. A `/` cannot
    /// be in a name there, so it shows as the division slash.
    pub fn in_the_cloud(
        local_directory: &Path,
        name: &vapor_providers::UnsyncableRemoteName,
    ) -> Self {
        Self {
            shown_path: local_directory.join(name.name.replace('/', "\u{2215}")),
            reason: name.reason.clone(),
            in_the_cloud: true,
        }
    }

    pub fn not_utf8(path: &Path) -> Self {
        Self {
            shown_path: PathBuf::from(path.to_string_lossy().into_owned()),
            reason: "its name is not valid UTF-8, which the sync path model cannot carry"
                .to_string(),
            in_the_cloud: false,
        }
    }
}

pub fn question(name: &UnsyncableName) -> String {
    if name.in_the_cloud {
        return format!(
            "{} in the cloud folder cannot be synced: {}. It stays in the cloud only. Rename it \
             there and it syncs; choose skip to stop being asked about it.",
            name.shown_path
                .file_name()
                .unwrap_or(name.shown_path.as_os_str())
                .to_string_lossy(),
            name.reason
        );
    }
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
