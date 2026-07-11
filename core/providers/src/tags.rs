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
        match fs::remove_file(Self::side_file_path(path)) {
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
        if !source.exists() {
            return Ok(());
        }
        fs::rename(source, Self::side_file_path(to))
    }

    fn write_side_file(&self, path: &Path, op_id: &str) -> io::Result<()> {
        let payload = serde_json::to_string(&SideFilePayload {
            op_id: op_id.to_string(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let side_file = Self::side_file_path(path);
        let temp = side_file.with_extension("json.tmp");
        fs::write(&temp, payload)?;
        fs::rename(&temp, &side_file)
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
    fn side_file_detection_matches_suffix() {
        assert!(OpIdTagStore::is_side_file(Path::new(
            "/x/a.txt.vapor-meta.json"
        )));
        assert!(!OpIdTagStore::is_side_file(Path::new("/x/a.txt")));
    }
}
