//! Content checks of local files, read off the tick thread.
//!
//! The tick thread sometimes has to know whether a local file holds the
//! bytes the sync index recorded: before a cloud deletion removes the
//! file, when a download displaces it, and when a walk finds a file a
//! crash left set aside. Only a worker reads the file. The tick thread
//! uses the answer while the file still has the size and the
//! modification time the worker saw, which is the trust the quick check
//! gives everywhere else. With no such answer the file counts as
//! changed, the side that keeps data.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vapor_providers::HashAlgorithm;
use vapor_shared::constants;

use crate::state_db::SyncIndexEntry;

/// The size and the modification time of a regular file.
type FileState = (u64, SystemTime);

fn file_state(path: &Path) -> Option<FileState> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some((metadata.len(), metadata.modified().ok()?))
}

#[cfg(test)]
thread_local! {
    static CONTENT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Marks a read of file content on the calling thread. Every place in
/// the daemon that hashes a local file calls it, so a test that drives
/// the tick loop over worker threads can assert that the tick thread
/// read nothing.
pub(crate) fn note_content_read() {
    #[cfg(test)]
    CONTENT_READS.with(|reads| reads.set(reads.get() + 1));
}

/// File content reads the calling thread made since it started.
#[cfg(test)]
pub(crate) fn content_reads_on_this_thread() -> usize {
    CONTENT_READS.with(std::cell::Cell::get)
}

/// The hash of a local file, with the size and the modification time
/// the file had when it was read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalFileHash {
    size_bytes: u64,
    modified_at: SystemTime,
    content_hash: String,
}

impl LocalFileHash {
    /// The hash, when the file still has the size and the modification
    /// time it was read with.
    pub fn of_file_with(&self, size_bytes: u64, modified_at: Option<SystemTime>) -> Option<&str> {
        (self.size_bytes == size_bytes && Some(self.modified_at) == modified_at)
            .then_some(self.content_hash.as_str())
    }
}

/// Reads one regular file of at most one hash step. `None` when the
/// file is larger, cannot be read, or changed while it was read.
fn hash_small_file(path: &Path, algorithm: HashAlgorithm) -> Option<LocalFileHash> {
    let (size_bytes, modified_at) = file_state(path)?;
    if size_bytes > constants::engine::HASH_STAGE_STEP_BYTES {
        return None;
    }
    note_content_read();
    let content_hash = vapor_providers::filesystem::hash_hex_of_file_with(path, algorithm).ok()?;
    (file_state(path)? == (size_bytes, modified_at)).then_some(LocalFileHash {
        size_bytes,
        modified_at,
        content_hash,
    })
}

/// What a worker read for one probe, by path.
#[derive(Debug, Default)]
pub(crate) struct LocalHashes {
    hashes: HashMap<PathBuf, LocalFileHash>,
    cut_short: bool,
}

impl LocalHashes {
    /// The reads stopped before every file had its answer, because
    /// hashing went on hold. A file with no answer here is not a file
    /// that changed: the caller asks again later.
    pub fn cut_short(&self) -> bool {
        self.cut_short
    }

    /// The hash of the file at `path`, when a worker read the file that
    /// has this size and modification time.
    pub fn hash_of(
        &self,
        path: &Path,
        size_bytes: u64,
        modified_at: Option<SystemTime>,
    ) -> Option<&str> {
        self.hashes.get(path)?.of_file_with(size_bytes, modified_at)
    }
}

/// The synced files a cloud deletion is about to be checked against.
pub(crate) struct TouchedFileCheck {
    /// The index row of the file, or every row under the folder.
    pub rows: Vec<SyncIndexEntry>,
    pub algorithm: HashAlgorithm,
}

impl TouchedFileCheck {
    /// Reads each file that has the size its row recorded and another
    /// modification time: the one case where only the content says
    /// whether the file changed since the last sync. `may_read` is
    /// asked before each file: a folder can hold many such files, and
    /// the throttle state can put hashing on hold part of the way.
    pub fn run(&self, may_read: &dyn Fn() -> bool) -> LocalHashes {
        let mut hashes = HashMap::new();
        let mut cut_short = false;
        for row in &self.rows {
            let Some((size_bytes, modified_at)) = file_state(&row.path) else {
                continue;
            };
            if size_bytes != row.size_bytes || row.matches_local(size_bytes, Some(modified_at)) {
                continue;
            }
            if !may_read() {
                cut_short = true;
                break;
            }
            if let Some(hash) = hash_small_file(&row.path, self.algorithm) {
                hashes.insert(row.path.clone(), hash);
            }
        }
        LocalHashes { hashes, cut_short }
    }
}

/// The local file a two-way download is about to displace.
pub(crate) struct DisplacedFileCheck {
    pub local_path: PathBuf,
    /// The index row of the path when the download was dispatched.
    pub index: Option<SyncIndexEntry>,
    pub algorithm: HashAlgorithm,
}

impl DisplacedFileCheck {
    /// Reads the file when only its content can say that it equals the
    /// downloaded payload or the bytes last synced: the quick check
    /// does not clear it, and its size is the size of one of the two.
    pub fn run(&self, incoming_size_bytes: u64) -> Option<LocalFileHash> {
        let (size_bytes, modified_at) = file_state(&self.local_path)?;
        let index = self.index.as_ref();
        if index.is_some_and(|index| index.matches_local(size_bytes, Some(modified_at))) {
            return None;
        }
        let size_could_match = size_bytes == incoming_size_bytes
            || index.is_some_and(|index| index.size_bytes == size_bytes);
        if !size_could_match {
            return None;
        }
        hash_small_file(&self.local_path, self.algorithm)
    }
}

/// A file a reconcile walk found under a set-aside name.
pub(crate) struct SetAsideFile {
    pub aside: PathBuf,
    /// The name the file had, when the set-aside name carries it.
    pub original_name: Option<String>,
}

/// A set-aside file, and whether a worker found its bytes equal to the
/// file now at its original name.
pub(crate) struct SetAsideVerdict {
    pub file: SetAsideFile,
    /// The states of the set-aside file and of the original when both
    /// were read and held the same bytes.
    same_as_original: Option<(FileState, FileState)>,
}

impl SetAsideVerdict {
    /// Whether the set-aside file can be dropped: a worker found its
    /// bytes at the original name, and neither file changed since.
    pub fn still_same_as(&self, original: &Path) -> bool {
        self.same_as_original
            .is_some_and(|(aside_state, original_state)| {
                file_state(&self.file.aside) == Some(aside_state)
                    && file_state(original) == Some(original_state)
            })
    }
}

/// The path a set-aside file gives its bytes back to.
pub(crate) fn set_aside_original_path(file: &SetAsideFile) -> PathBuf {
    file.aside
        .with_file_name(file.original_name.as_deref().unwrap_or("recovered-file"))
}

/// Compares each set-aside file with the file at its original name.
pub(crate) fn compare_set_aside_files(found: Vec<SetAsideFile>) -> Vec<SetAsideVerdict> {
    found
        .into_iter()
        .map(|file| {
            let same_as_original = file
                .original_name
                .is_some()
                .then(|| set_aside_original_path(&file))
                .and_then(|original| same_bytes(&file.aside, &original));
            SetAsideVerdict {
                file,
                same_as_original,
            }
        })
        .collect()
}

fn same_bytes(left: &Path, right: &Path) -> Option<(FileState, FileState)> {
    if file_state(left)?.0 != file_state(right)?.0 {
        return None;
    }
    let left = hash_small_file(left, HashAlgorithm::Sha256)?;
    let right = hash_small_file(right, HashAlgorithm::Sha256)?;
    (left.content_hash == right.content_hash).then_some((
        (left.size_bytes, left.modified_at),
        (right.size_bytes, right.modified_at),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use vapor_providers::filesystem::hash_hex_of_bytes;

    fn index_row(
        path: &Path,
        body: &[u8],
        local_modified_at: Option<SystemTime>,
    ) -> SyncIndexEntry {
        SyncIndexEntry {
            path: path.to_path_buf(),
            content_hash: hash_hex_of_bytes(body),
            size_bytes: body.len() as u64,
            local_modified_at,
            remote_modified_at: None,
            last_op_id: String::new(),
            updated_at: SystemTime::UNIX_EPOCH,
            remote_id: None,
        }
    }

    fn state_of(path: &Path) -> (u64, Option<SystemTime>) {
        let metadata = fs::symlink_metadata(path).expect("metadata");
        (metadata.len(), metadata.modified().ok())
    }

    fn set_modified(path: &Path, modified_at: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(modified_at)
            .expect("set mtime");
    }

    #[test]
    fn a_touched_file_is_read_and_a_file_the_quick_check_clears_is_not() {
        let temp = tempfile::TempDir::new().expect("temp");
        let touched = temp.path().join("touched.txt");
        let untouched = temp.path().join("untouched.txt");
        let resized = temp.path().join("resized.txt");
        fs::write(&touched, b"same bytes").expect("seed");
        fs::write(&untouched, b"same bytes").expect("seed");
        fs::write(&resized, b"more bytes than before").expect("seed");
        let synced_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_modified(&untouched, synced_at);

        let check = TouchedFileCheck {
            rows: vec![
                index_row(&touched, b"same bytes", Some(synced_at)),
                index_row(&untouched, b"same bytes", Some(synced_at)),
                index_row(&resized, b"same bytes", Some(synced_at)),
                index_row(
                    &temp.path().join("gone.txt"),
                    b"same bytes",
                    Some(synced_at),
                ),
            ],
            algorithm: HashAlgorithm::Sha256,
        };
        let reads_before = content_reads_on_this_thread();
        let hashes = check.run(&|| true);
        assert_eq!(
            content_reads_on_this_thread() - reads_before,
            1,
            "only the touched file is read"
        );
        assert!(!hashes.cut_short());

        // With hashing on hold nothing is read, and the answer says so.
        let reads_before = content_reads_on_this_thread();
        let held = check.run(&|| false);
        assert_eq!(content_reads_on_this_thread() - reads_before, 0);
        assert!(held.cut_short());

        let (size, modified_at) = state_of(&touched);
        assert_eq!(
            hashes.hash_of(&touched, size, modified_at),
            Some(hash_hex_of_bytes(b"same bytes").as_str())
        );
        let (size, modified_at) = state_of(&untouched);
        assert_eq!(hashes.hash_of(&untouched, size, modified_at), None);
        let (size, modified_at) = state_of(&resized);
        assert_eq!(hashes.hash_of(&resized, size, modified_at), None);
    }

    #[test]
    fn a_hash_is_not_used_for_a_file_that_changed_after_it_was_read() {
        let temp = tempfile::TempDir::new().expect("temp");
        let file = temp.path().join("notes.txt");
        fs::write(&file, b"first").expect("seed");
        let hashes = TouchedFileCheck {
            rows: vec![index_row(&file, b"first", None)],
            algorithm: HashAlgorithm::Sha256,
        }
        .run(&|| true);
        let (size, modified_at) = state_of(&file);
        assert!(hashes.hash_of(&file, size, modified_at).is_some());

        // Same size, later time: the bytes may be other bytes now.
        let later = modified_at.expect("mtime") + Duration::from_secs(5);
        assert_eq!(hashes.hash_of(&file, size, Some(later)), None);
        assert_eq!(hashes.hash_of(&file, size, None), None);
    }

    #[test]
    fn a_displaced_file_is_read_only_when_its_size_could_match() {
        let temp = tempfile::TempDir::new().expect("temp");
        let file = temp.path().join("report.md");
        fs::write(&file, b"local edit").expect("seed");
        let check = |index| DisplacedFileCheck {
            local_path: file.clone(),
            index,
            algorithm: HashAlgorithm::Sha256,
        };

        // Another size than the payload and no index row: it diverged,
        // and nobody has to read it to know.
        assert!(check(None).run(4).is_none());
        // The size of the payload: only the content can say.
        assert!(check(None).run(10).is_some());
        // The size the index recorded, touched since.
        let row = index_row(&file, b"last sync!", None);
        assert!(check(Some(row)).run(4).is_some());
        // Untouched since the last sync: the quick check clears it.
        let (_, modified_at) = state_of(&file);
        let row = index_row(&file, b"last sync!", modified_at);
        assert!(check(Some(row)).run(10).is_none());
    }

    #[test]
    fn a_set_aside_file_is_dropped_only_while_it_equals_the_original() {
        let temp = tempfile::TempDir::new().expect("temp");
        let original = temp.path().join("plan.md");
        let same = temp.path().join(".vapor-tmp-aside-same");
        let other = temp.path().join(".vapor-tmp-aside-other");
        fs::write(&original, b"the plan").expect("seed");
        fs::write(&same, b"the plan").expect("seed");
        fs::write(&other, b"the edit").expect("seed");

        let verdicts = compare_set_aside_files(vec![
            SetAsideFile {
                aside: same.clone(),
                original_name: Some("plan.md".to_string()),
            },
            SetAsideFile {
                aside: other,
                original_name: Some("plan.md".to_string()),
            },
            SetAsideFile {
                aside: same,
                original_name: None,
            },
        ]);

        assert!(verdicts[0].still_same_as(&original));
        assert!(!verdicts[1].still_same_as(&original));
        assert!(!verdicts[2].still_same_as(&original));

        // The original is written again after the comparison.
        fs::write(&original, b"new plan").expect("rewrite");
        set_modified(&original, SystemTime::now() + Duration::from_secs(5));
        assert!(!verdicts[0].still_same_as(&original));
    }
}
