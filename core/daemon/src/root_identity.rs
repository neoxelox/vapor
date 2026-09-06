//! Root identity: the check that the folders a profile syncs are the
//! folders it adopted, so an unplugged volume, an empty folder at a
//! mount point, or a re-created cloud folder is never mirrored as
//! "everything was deleted".
//!
//! Each side carries an identity: the local root a hidden
//! `.vapor-root` marker, the cloud root whatever the provider offers
//! (the same marker on a filesystem-backed root, the folder id on
//! Google Drive). The profile records both at adoption and compares at
//! every start and on a cadence. A root that is missing is waited for,
//! never re-created; a root that is present without the recorded
//! identity opens a `root-replaced` decision, and the profile holds
//! until the user answers `reattach` (merge without deleting anything)
//! or the original root returns. Design: `docs/architecture/data-flow.md`
//! §Root identity.

use std::path::Path;
use std::time::SystemTime;

use vapor_providers::root_marker;
use vapor_shared::constants;

use crate::state_db::{DurableStateDb, StateDbError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootStatus {
    /// Present and carrying the recorded identity (or just adopted).
    Ready,
    /// Not there. Waited for; never re-created once adopted.
    Missing,
    /// Present, but not the root the profile adopted.
    Replaced {
        expected: String,
        found: Option<String>,
    },
    /// Could not be asked (a network failure, a permission); nothing
    /// changes until it can be.
    Unreachable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootSide {
    Local,
    Cloud,
}

impl RootSide {
    pub fn label(self) -> &'static str {
        match self {
            RootSide::Local => "local",
            RootSide::Cloud => "cloud",
        }
    }

    fn state_key(self) -> &'static str {
        match self {
            RootSide::Local => constants::state::LOCAL_ROOT_IDENTITY_KEY,
            RootSide::Cloud => constants::state::CLOUD_ROOT_IDENTITY_KEY,
        }
    }
}

/// The identity a profile recorded for one side. An empty string means
/// the side was adopted but offers no identity (a backend without one),
/// so no check is possible there.
pub fn recorded_identity(
    state_db: &DurableStateDb,
    side: RootSide,
) -> Result<Option<String>, StateDbError> {
    Ok(state_db.state(side.state_key())?.map(|entry| entry.value))
}

pub fn record_identity(
    state_db: &mut DurableStateDb,
    side: RootSide,
    identity: &str,
    now: SystemTime,
) -> Result<(), StateDbError> {
    state_db
        .set_state(side.state_key(), identity, now)
        .map(|_| ())
}

/// Checks the local root and adopts it when the profile has no
/// recorded identity yet (creating the directory then, and only then).
pub fn check_local_root(
    state_db: &mut DurableStateDb,
    root: &Path,
    device_id: &str,
    now: SystemTime,
) -> Result<RootStatus, StateDbError> {
    let recorded = recorded_identity(state_db, RootSide::Local)?;
    let present = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata.is_dir(),
        Err(_) => false,
    };
    match (recorded, present) {
        (None, false) => {
            if let Err(error) = std::fs::create_dir_all(root) {
                crate::logging::error(
                    "Failed to create missing local sync directory",
                    &[
                        ("path", root.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                return Ok(RootStatus::Missing);
            }
            crate::logging::info(
                "Created missing local sync directory",
                &[("path", root.display().to_string())],
            );
            adopt_local(state_db, root, device_id, now)
        }
        (None, true) => adopt_local(state_db, root, device_id, now),
        (Some(_), false) => Ok(RootStatus::Missing),
        (Some(expected), true) => {
            let found = root_marker::read_marker(root)
                .ok()
                .flatten()
                .map(|marker| marker.root_id);
            if found.as_deref() == Some(expected.as_str()) {
                Ok(RootStatus::Ready)
            } else {
                Ok(RootStatus::Replaced { expected, found })
            }
        }
    }
}

fn adopt_local(
    state_db: &mut DurableStateDb,
    root: &Path,
    device_id: &str,
    now: SystemTime,
) -> Result<RootStatus, StateDbError> {
    match root_marker::adopt(root, device_id, now) {
        Ok(marker) => {
            record_identity(state_db, RootSide::Local, &marker.root_id, now)?;
            crate::logging::info(
                "Adopted the local sync directory",
                &[
                    ("path", root.display().to_string()),
                    ("root_id", marker.root_id),
                ],
            );
            Ok(RootStatus::Ready)
        }
        Err(error) => {
            // A root that cannot hold a marker (read-only, an exotic
            // filesystem) is still usable; it just cannot be told apart
            // from a replacement later. Recorded as identity-less.
            crate::logging::warning(
                "Cannot write the root marker into the local sync directory; replacement detection is off for it",
                &[
                    ("path", root.display().to_string()),
                    ("error", error.to_string()),
                ],
            );
            record_identity(state_db, RootSide::Local, "", now)?;
            Ok(RootStatus::Ready)
        }
    }
}

/// Re-adopts the local root after a `reattach` answer: the current
/// folder becomes the profile's root, whatever marker it carried.
pub fn reattach_local(
    state_db: &mut DurableStateDb,
    root: &Path,
    device_id: &str,
    now: SystemTime,
) -> Result<(), StateDbError> {
    let marker = match root_marker::read_marker(root).ok().flatten() {
        Some(marker) => marker,
        None => match root_marker::write_marker(root, device_id, now) {
            Ok(marker) => marker,
            Err(error) => {
                crate::logging::warning(
                    "Cannot write the root marker into the reattached local sync directory",
                    &[
                        ("path", root.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                return record_identity(state_db, RootSide::Local, "", now);
            }
        },
    };
    record_identity(state_db, RootSide::Local, &marker.root_id, now)
}

/// Compares what the provider reports for the cloud root against the
/// recorded identity. `probe` is the provider's answer:
/// `Ok(identity)` when the root is there, `Err` for a missing root.
pub fn classify_cloud_probe(
    recorded: &str,
    probe: Result<Option<String>, vapor_providers::ProviderError>,
) -> RootStatus {
    if recorded.is_empty() {
        // Adopted without an identity: nothing to compare.
        return match probe {
            Ok(_) => RootStatus::Ready,
            Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                RootStatus::Missing
            }
            Err(error) => RootStatus::Unreachable(error.message),
        };
    }
    match probe {
        Ok(Some(found)) if found == recorded => RootStatus::Ready,
        Ok(found) => RootStatus::Replaced {
            expected: recorded.to_string(),
            found,
        },
        Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
            RootStatus::Missing
        }
        Err(error) => RootStatus::Unreachable(error.message),
    }
}

pub const DECISION_KIND: &str = "root-replaced";
pub const OPTION_REATTACH: &str = "reattach";
pub const MISSING_DECISION_KIND: &str = "root-missing";
pub const OPTION_RECREATE: &str = "recreate";

/// The question a missing root asks.
pub fn missing_question(side: RootSide, root: &str) -> String {
    match side {
        RootSide::Local => format!(
            "The local sync directory {root} is missing (a volume unplugged, a folder moved or \
             deleted). Vapor is waiting for it and will not create a folder there on its own, \
             so nothing gets deleted anywhere. Put it back and Vapor resumes on its own, or \
             re-create it empty and let the cloud fill it: nothing is deleted on either side."
        ),
        RootSide::Cloud => format!(
            "The cloud sync directory {root} is missing (deleted, moved, or on a volume that is \
             not mounted). Vapor is waiting for it and will not create it on its own, so nothing \
             gets deleted anywhere. Restore it and Vapor resumes on its own, or re-create it \
             empty and let this device fill it: nothing is deleted on either side."
        ),
    }
}

/// The question a replaced root asks.
pub fn question(side: RootSide, root: &str, found: Option<&str>) -> String {
    let what = match found {
        Some(_) => "a different sync folder",
        None => "a folder that Vapor never adopted",
    };
    match side {
        RootSide::Local => format!(
            "The local sync directory {root} now holds {what}. Vapor is not syncing it, so \
             nothing gets deleted anywhere. Put the original folder back and Vapor resumes on \
             its own, or reattach this folder: its files and the cloud's are merged and nothing \
             is deleted on either side."
        ),
        RootSide::Cloud => format!(
            "The cloud sync directory {root} now holds {what}. Vapor is not syncing it, so \
             nothing gets deleted anywhere. Restore the original folder and Vapor resumes on \
             its own, or reattach this folder: its files and this device's are merged and \
             nothing is deleted on either side."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn db(temp: &TempDir) -> DurableStateDb {
        DurableStateDb::open(temp.path().join("state/vapor.sqlite")).expect("open")
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds)
    }

    #[test]
    fn a_missing_root_is_created_and_adopted_only_the_first_time() {
        let temp = TempDir::new().expect("temp");
        let mut state_db = db(&temp);
        let root = temp.path().join("Vapor");
        assert_eq!(
            check_local_root(&mut state_db, &root, "dev", at(1)).expect("check"),
            RootStatus::Ready
        );
        assert!(root.is_dir(), "first adoption creates the folder");
        let recorded = recorded_identity(&state_db, RootSide::Local)
            .expect("read")
            .expect("recorded");
        assert_eq!(recorded.len(), 32);
        assert_eq!(
            root_marker::read_marker(&root)
                .expect("marker")
                .expect("written")
                .root_id,
            recorded
        );

        // The volume goes away: Vapor waits, and does not re-create it.
        std::fs::remove_dir_all(&root).expect("unplug");
        assert_eq!(
            check_local_root(&mut state_db, &root, "dev", at(2)).expect("check"),
            RootStatus::Missing
        );
        assert!(!root.exists(), "a once-adopted root is never re-created");
    }

    #[test]
    fn a_folder_without_the_recorded_marker_is_a_replacement() {
        let temp = TempDir::new().expect("temp");
        let mut state_db = db(&temp);
        let root = temp.path().join("Vapor");
        check_local_root(&mut state_db, &root, "dev", at(1)).expect("adopt");
        let recorded = recorded_identity(&state_db, RootSide::Local)
            .expect("read")
            .expect("recorded");

        // An empty folder appears at the mount point.
        std::fs::remove_dir_all(&root).expect("unplug");
        std::fs::create_dir_all(&root).expect("empty folder at the path");
        assert_eq!(
            check_local_root(&mut state_db, &root, "dev", at(2)).expect("check"),
            RootStatus::Replaced {
                expected: recorded.clone(),
                found: None
            }
        );

        // Another profile's root (a different marker) is a replacement too.
        root_marker::write_marker(&root, "other", at(3)).expect("foreign marker");
        let status = check_local_root(&mut state_db, &root, "dev", at(3)).expect("check");
        assert!(
            matches!(&status, RootStatus::Replaced { expected, found: Some(found) } if *expected == recorded && *found != recorded),
            "{status:?}"
        );

        // Reattaching adopts what is there now.
        reattach_local(&mut state_db, &root, "dev", at(4)).expect("reattach");
        assert_eq!(
            check_local_root(&mut state_db, &root, "dev", at(5)).expect("check"),
            RootStatus::Ready
        );
    }

    #[test]
    fn the_original_root_returning_is_ready_again_without_an_answer() {
        let temp = TempDir::new().expect("temp");
        let mut state_db = db(&temp);
        let root = temp.path().join("Vapor");
        check_local_root(&mut state_db, &root, "dev", at(1)).expect("adopt");
        let marker = std::fs::read(root_marker::marker_path(&root)).expect("marker bytes");
        std::fs::remove_dir_all(&root).expect("unplug");
        std::fs::create_dir_all(&root).expect("empty folder");
        assert!(matches!(
            check_local_root(&mut state_db, &root, "dev", at(2)).expect("check"),
            RootStatus::Replaced { .. }
        ));
        std::fs::write(root_marker::marker_path(&root), marker).expect("the volume is back");
        assert_eq!(
            check_local_root(&mut state_db, &root, "dev", at(3)).expect("check"),
            RootStatus::Ready
        );
    }

    #[test]
    fn cloud_probes_classify_against_the_recorded_identity() {
        assert_eq!(
            classify_cloud_probe("abc", Ok(Some("abc".into()))),
            RootStatus::Ready
        );
        assert_eq!(
            classify_cloud_probe("abc", Ok(Some("xyz".into()))),
            RootStatus::Replaced {
                expected: "abc".into(),
                found: Some("xyz".into())
            }
        );
        assert_eq!(
            classify_cloud_probe("abc", Ok(None)),
            RootStatus::Replaced {
                expected: "abc".into(),
                found: None
            }
        );
        assert_eq!(
            classify_cloud_probe(
                "abc",
                Err(vapor_providers::ProviderError::not_found("gone"))
            ),
            RootStatus::Missing
        );
        // A backend without an identity can only be present or missing.
        assert_eq!(classify_cloud_probe("", Ok(None)), RootStatus::Ready);
        assert_eq!(
            classify_cloud_probe("", Err(vapor_providers::ProviderError::not_found("gone"))),
            RootStatus::Missing
        );
        assert_eq!(
            classify_cloud_probe("abc", Err(vapor_providers::ProviderError::transient("net"))),
            RootStatus::Unreachable("net".into())
        );
    }
}
