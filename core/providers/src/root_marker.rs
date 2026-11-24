//! The root identity marker: a hidden `.vapor-root` file written once
//! into a sync root when a profile adopts it. Its id is what the
//! profile records and checks at every start, so a folder that merely
//! has the same path (a fresh mount, a re-created folder, another
//! profile's root) is told apart from the one the sync index
//! describes. The local root and a filesystem-backed cloud root carry
//! the same marker; Drive-style backends use their folder id instead.
//! Design: `docs/architecture/data-flow.md` §Root identity.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use vapor_shared::constants;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RootMarker {
    pub root_id: String,
    pub created_at_ms: u64,
    /// The device that adopted the root; informational.
    pub created_by: String,
}

pub fn marker_path(root: &Path) -> PathBuf {
    root.join(constants::provider::ROOT_MARKER_FILE_NAME)
}

/// The marker in `root`, `Ok(None)` when there is none or it is
/// unreadable as a marker (a foreign file of that name is not an
/// identity).
pub fn read_marker(root: &Path) -> io::Result<Option<RootMarker>> {
    match std::fs::read(marker_path(root)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn fresh_marker(created_by: &str, now: SystemTime) -> RootMarker {
    RootMarker {
        root_id: new_root_id(now),
        created_at_ms: now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        created_by: created_by.to_string(),
    }
}

/// Writes the whole marker to a file of its own under the internal
/// temp prefix. The name holds the id of the marker: two adopters of
/// one root, in one process or on two devices, never share it.
fn stage_marker(root: &Path, marker: &RootMarker) -> io::Result<PathBuf> {
    let bytes = serde_json::to_vec_pretty(marker).map_err(io::Error::other)?;
    let staging = root.join(format!(
        "{}root-{}",
        constants::provider::TEMP_FILE_PREFIX,
        marker.root_id
    ));
    std::fs::write(&staging, bytes)?;
    Ok(staging)
}

/// Writes a fresh marker into `root` (which must exist) and returns it.
/// It takes the place of a marker that is there. Atomic: staged under
/// the internal temp prefix, then renamed.
pub fn write_marker(root: &Path, created_by: &str, now: SystemTime) -> io::Result<RootMarker> {
    let marker = fresh_marker(created_by, now);
    let staging = stage_marker(root, &marker)?;
    std::fs::rename(&staging, marker_path(root))?;
    Ok(marker)
}

/// Reads the marker or writes one: the adoption step.
///
/// Two adopters can find no marker at the same moment: two profiles of
/// one daemon on one cloud folder, or two devices on a shared one. The
/// first marker stays and both get its id. With a marker for each, the
/// one whose marker was replaced would find another identity at its
/// next check and take the root for a folder that was replaced.
pub fn adopt(root: &Path, created_by: &str, now: SystemTime) -> io::Result<RootMarker> {
    if let Some(marker) = read_marker(root)? {
        return Ok(marker);
    }
    let marker = fresh_marker(created_by, now);
    let staging = stage_marker(root, &marker)?;
    // A hard link fails when the name is taken, and what it publishes
    // is the whole file.
    match std::fs::hard_link(&staging, marker_path(root)) {
        Ok(()) => {
            let _ = std::fs::remove_file(&staging);
            Ok(marker)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match read_marker(root) {
                Ok(Some(first)) => {
                    let _ = std::fs::remove_file(&staging);
                    Ok(first)
                }
                // A foreign file of that name is not an identity: the
                // marker takes its place.
                Ok(None) => {
                    std::fs::rename(&staging, marker_path(root))?;
                    Ok(marker)
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&staging);
                    Err(error)
                }
            }
        }
        // A volume with no hard links: the rename, where the last of
        // two adopters wins.
        Err(_) => {
            std::fs::rename(&staging, marker_path(root))?;
            Ok(marker)
        }
    }
}

/// A 128-bit id, hex: a hash of the time, the process, the thread, and
/// a counter. Unique on one machine by construction and wide enough
/// that two machines do not collide in practice.
fn new_root_id(now: SystemTime) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let material = format!(
        "{nanos}:{}:{counter}:{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let digest = crate::filesystem::hash_hex_of_bytes(material.as_bytes());
    digest[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adopt_writes_once_and_reads_the_same_id_afterwards() {
        let temp = tempfile::TempDir::new().expect("temp");
        let root = temp.path();
        assert_eq!(read_marker(root).expect("read"), None);
        let first = adopt(root, "dev-a", SystemTime::UNIX_EPOCH).expect("adopt");
        assert_eq!(first.root_id.len(), 32);
        let again = adopt(root, "dev-b", SystemTime::UNIX_EPOCH).expect("adopt again");
        assert_eq!(again, first, "a second adoption keeps the marker");
        assert!(marker_path(root).is_file());
        assert!(
            !root.read_dir().expect("list").flatten().any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(constants::provider::TEMP_FILE_PREFIX)),
            "no staging file is left behind"
        );
    }

    #[test]
    fn adopters_of_one_root_at_the_same_moment_get_one_identity() {
        // Two profiles of one daemon on one cloud folder reach it in
        // the same tick. Each must end with the id the marker holds, or
        // one of them finds "another folder" at its next check.
        for _ in 0..50 {
            let temp = tempfile::TempDir::new().expect("temp");
            let root = temp.path().to_path_buf();
            let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
            let adopters: Vec<_> = (0..4)
                .map(|n| {
                    let (root, gate) = (root.clone(), gate.clone());
                    std::thread::spawn(move || {
                        gate.wait();
                        adopt(&root, &format!("dev-{n}"), SystemTime::UNIX_EPOCH)
                            .expect("adopt")
                            .root_id
                    })
                })
                .collect();
            let ids: Vec<String> = adopters
                .into_iter()
                .map(|adopter| adopter.join().expect("thread"))
                .collect();
            let on_disk = read_marker(&root).expect("read").expect("a marker").root_id;
            assert!(
                ids.iter().all(|id| id == &on_disk),
                "every adopter holds the id of the marker: {ids:?} against {on_disk}"
            );
            assert_eq!(
                root.read_dir().expect("list").count(),
                1,
                "no staging file is left behind"
            );
        }
    }

    #[test]
    fn adoption_takes_the_place_of_a_foreign_file_under_the_marker_name() {
        let temp = tempfile::TempDir::new().expect("temp");
        std::fs::write(marker_path(temp.path()), b"not json").expect("seed");
        let marker = adopt(temp.path(), "dev", SystemTime::UNIX_EPOCH).expect("adopt");
        assert_eq!(read_marker(temp.path()).expect("read"), Some(marker));
    }

    #[test]
    fn a_foreign_file_under_the_marker_name_is_not_an_identity() {
        let temp = tempfile::TempDir::new().expect("temp");
        std::fs::write(marker_path(temp.path()), b"not json").expect("seed");
        assert_eq!(read_marker(temp.path()).expect("read"), None);
    }

    #[test]
    fn two_markers_written_back_to_back_differ() {
        let temp = tempfile::TempDir::new().expect("temp");
        let a = write_marker(temp.path(), "dev", SystemTime::UNIX_EPOCH).expect("a");
        let b = write_marker(temp.path(), "dev", SystemTime::UNIX_EPOCH).expect("b");
        assert_ne!(a.root_id, b.root_id);
    }
}
