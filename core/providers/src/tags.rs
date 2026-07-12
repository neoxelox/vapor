//! Op-id tagging with xattr-primary + side-file fallback.
//!
//! Semantics per `docs/architecture/data-flow.md §Loop prevention`:
//! writes attempt the platform metadata tag (xattr / ADS) first; when
//! the filesystem cannot hold one (`Unsupported`, `PermissionDenied`,
//! read-only), the fallback side-file `{path}.vapor-meta.json` is
//! written atomically next to the payload. Reads check the xattr first,
//! then the side-file; when both exist the xattr wins.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vapor_platform::fs_caps::FilesystemCapabilities;
use vapor_shared::constants;

/// Where a tag write ended up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpIdTagLocation {
    Xattr,
    SideFile,
}

/// Side-file wire shape. JSON so future fields (content hash, device
/// id) can be added without a format break.
#[derive(Debug, Serialize, Deserialize)]
struct SideFilePayload {
    #[serde(rename = "opId")]
    op_id: String,
}

#[derive(Clone)]
pub struct OpIdTagStore {
    caps: Arc<dyn FilesystemCapabilities>,
}

impl OpIdTagStore {
    pub fn new(caps: Arc<dyn FilesystemCapabilities>) -> Self {
        Self { caps }
    }

    /// The side-file path for `path`: `{path}.vapor-meta.json`.
    pub fn side_file_path(path: &Path) -> PathBuf {
        let mut file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        file_name.push_str(constants::provider::OP_ID_SIDE_FILE_SUFFIX);
        path.with_file_name(file_name)
    }

    /// Whether `path` names a Vapor op-id side-file. Providers hide
    /// these from enumeration and changes feeds.
    pub fn is_side_file(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(constants::provider::OP_ID_SIDE_FILE_SUFFIX))
    }

    /// Writes the op-id tag for `path`, falling back to the side-file
    /// when the filesystem cannot hold an xattr.
    pub fn write_op_id(&self, path: &Path, op_id: &str) -> io::Result<OpIdTagLocation> {
        match self
            .caps
            .write_tag(path, constants::provider::OP_ID_XATTR_NAME, op_id)
        {
            Ok(()) => Ok(OpIdTagLocation::Xattr),
            Err(error) if xattr_fallback_applies(&error) => {
                self.write_side_file(path, op_id)?;
                Ok(OpIdTagLocation::SideFile)
            }
            Err(error) => Err(error),
        }
    }

    /// Reads the op-id tag for `path`. Xattr wins over the side-file;
    /// unreadable/absent tags read as `None` (loop prevention then
    /// falls back to content-hash correlation).
    pub fn read_op_id(&self, path: &Path) -> Option<String> {
        if let Ok(Some(value)) = self
            .caps
            .read_tag(path, constants::provider::OP_ID_XATTR_NAME)
            && !value.is_empty()
        {
            return Some(value);
        }
        let side_file = Self::side_file_path(path);
        let contents = fs::read_to_string(side_file).ok()?;
        let payload: SideFilePayload = serde_json::from_str(&contents).ok()?;
        if payload.op_id.is_empty() {
            None
        } else {
            Some(payload.op_id)
        }
    }

    /// Removes both tag locations. Best-effort: absent tags are fine,
    /// real I/O failures on the side-file surface as errors.
    pub fn remove(&self, path: &Path) -> io::Result<()> {
        // The xattr disappears with the file itself; removal only
        // matters when the file still exists.
        let _ = self
            .caps
            .remove_tag(path, constants::provider::OP_ID_XATTR_NAME);
        let side_file = Self::side_file_path(path);
        // Never delete a real user file that merely shares the reserved
        // suffix: only remove a path that actually parses as our metadata.
        if !Self::is_owned_side_file(&side_file) {
            return Ok(());
        }
        match fs::remove_file(&side_file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Moves any side-file tag alongside a payload rename. The xattr
    /// travels with the file inode; the side-file does not, so renames
    /// must relocate it explicitly.
    pub fn relocate_side_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        let source = Self::side_file_path(from);
        if !Self::is_owned_side_file(&source) {
            return Ok(());
        }
        let destination = Self::side_file_path(to);
        // Refuse to clobber a user file that happens to sit at the
        // destination's reserved-suffix path.
        if Self::collides_with_user_file(&destination) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "cannot relocate op-id side-file onto existing non-metadata file {}",
                    destination.display()
                ),
            ));
        }
        fs::rename(source, destination)
    }

    fn write_side_file(&self, path: &Path, op_id: &str) -> io::Result<()> {
        let side_file = Self::side_file_path(path);
        // The atomic rename below would overwrite whatever sits here; a
        // user file that merely shares the reserved suffix must not be
        // destroyed silently (keep-both / never-silent-overwrite policy).
        if Self::collides_with_user_file(&side_file) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "op-id side-file path {} is occupied by a non-metadata user file",
                    side_file.display()
                ),
            ));
        }
        let payload = serde_json::to_string(&SideFilePayload {
            op_id: op_id.to_string(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        // Stage inside the reserved internal namespace (TEMP_FILE_PREFIX) so
        // a crash between write and rename leaves an orphan that stays
        // hidden from sync by the filter's unconditional internal-artifact
        // check — not a `*.tmp` name that relies on a user-editable rule.
        let staged_name = side_file
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("{}{name}", constants::provider::TEMP_FILE_PREFIX))
            .unwrap_or_else(|| {
                format!(
                    "{}side.vapor-meta.json",
                    constants::provider::TEMP_FILE_PREFIX
                )
            });
        let temp = side_file.with_file_name(staged_name);
        fs::write(&temp, payload)?;
        fs::rename(&temp, &side_file)
    }

    /// Whether `path` exists and parses as one of our side-file payloads
    /// (so it is safe to overwrite/delete as our own tag).
    fn is_owned_side_file(path: &Path) -> bool {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str::<SideFilePayload>(&contents).is_ok(),
            Err(_) => false,
        }
    }

    /// Whether a real, non-metadata file occupies `path` — i.e. it exists
    /// but is not one of our side-file payloads.
    fn collides_with_user_file(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok() && !Self::is_owned_side_file(path)
    }
}

/// Errors that route a tag write onto the side-file fallback
/// (`ENOTSUP` / `EACCES` / `EROFS` per the data-flow contract).
fn xattr_fallback_applies(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::ReadOnlyFilesystem
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vapor_platform::fs_caps::{CaseSensitivity, InMemoryFilesystemCapabilities};

    fn store_with_xattr(supported: bool) -> OpIdTagStore {
        OpIdTagStore::new(Arc::new(InMemoryFilesystemCapabilities::new(
            supported,
            CaseSensitivity::Sensitive,
        )))
    }

    #[test]
    fn xattr_write_wins_when_supported() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"payload").expect("seed");

        let store = store_with_xattr(true);
        let location = store.write_op_id(&file, "op-1").expect("write");
        assert_eq!(location, OpIdTagLocation::Xattr);
        assert_eq!(store.read_op_id(&file), Some("op-1".to_string()));
        assert!(
            !OpIdTagStore::side_file_path(&file).exists(),
            "no side-file when xattr succeeded"
        );
    }

    #[test]
    fn side_file_fallback_when_xattr_unsupported() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"payload").expect("seed");

        let store = store_with_xattr(false);
        let location = store.write_op_id(&file, "op-2").expect("write");
        assert_eq!(location, OpIdTagLocation::SideFile);
        assert_eq!(store.read_op_id(&file), Some("op-2".to_string()));
        assert!(OpIdTagStore::side_file_path(&file).exists());
    }

    #[test]
    fn xattr_wins_over_side_file_when_both_present() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"payload").expect("seed");

        let caps = Arc::new(InMemoryFilesystemCapabilities::default());
        let store = OpIdTagStore::new(caps.clone());
        // Seed a stale side-file, then write a fresh xattr.
        caps.set_supports_xattr(false);
        store.write_op_id(&file, "stale-side").expect("side write");
        caps.set_supports_xattr(true);
        store
            .write_op_id(&file, "fresh-xattr")
            .expect("xattr write");

        assert_eq!(store.read_op_id(&file), Some("fresh-xattr".to_string()));
    }

    #[test]
    fn remove_clears_both_locations() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"payload").expect("seed");

        let caps = Arc::new(InMemoryFilesystemCapabilities::default());
        let store = OpIdTagStore::new(caps.clone());
        caps.set_supports_xattr(false);
        store.write_op_id(&file, "side").expect("side write");
        caps.set_supports_xattr(true);
        store.write_op_id(&file, "xattr").expect("xattr write");

        store.remove(&file).expect("remove");
        assert_eq!(store.read_op_id(&file), None);
    }

    #[test]
    fn relocate_side_file_follows_renames() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let from = dir.path().join("a.txt");
        let to = dir.path().join("b.txt");
        std::fs::write(&from, b"payload").expect("seed");

        let store = store_with_xattr(false);
        store.write_op_id(&from, "op-3").expect("write");
        std::fs::rename(&from, &to).expect("rename payload");
        store.relocate_side_file(&from, &to).expect("relocate");

        assert_eq!(store.read_op_id(&to), Some("op-3".to_string()));
        assert!(!OpIdTagStore::side_file_path(&from).exists());
    }

    #[test]
    fn write_refuses_to_clobber_a_user_file_sharing_the_reserved_suffix() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"payload").expect("seed");
        // A real user file legally named notes.txt.vapor-meta.json.
        let collision = OpIdTagStore::side_file_path(&file);
        std::fs::write(&collision, b"the user's important notes").expect("seed collision");

        let store = store_with_xattr(false);
        let error = store
            .write_op_id(&file, "op")
            .expect_err("must not overwrite the user's file");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&collision).expect("user file intact"),
            b"the user's important notes"
        );
    }

    #[test]
    fn remove_leaves_a_user_file_that_only_shares_the_suffix() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"payload").expect("seed");
        let collision = OpIdTagStore::side_file_path(&file);
        std::fs::write(&collision, b"not our metadata").expect("seed collision");

        let store = store_with_xattr(true);
        store.remove(&file).expect("remove is best-effort");
        assert!(
            collision.exists(),
            "a non-metadata user file must survive tag removal"
        );
    }

    #[test]
    fn side_file_detection_matches_suffix() {
        assert!(OpIdTagStore::is_side_file(Path::new(
            "/x/a.txt.vapor-meta.json"
        )));
        assert!(!OpIdTagStore::is_side_file(Path::new("/x/a.txt")));
    }
}
