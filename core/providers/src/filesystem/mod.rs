//! Filesystem reference provider.
//!
//! A full [`Provider`] implementation backed by a local directory that
//! plays the role of the cloud side. When `provider = "filesystem"`,
//! `cloudRoot` is reinterpreted as an absolute local path.
//! The provider is the reference implementation the engine's
//! bidirectional pipeline is validated against before any real cloud
//! backend (Google Drive) goes live, and the backend the
//! provider contract suite runs on.
//!
//! Safety properties:
//! - **Atomic writes**: uploads land in a hidden temp file in the target
//!   directory and are committed after the payload and the op-id tag are
//!   complete: renamed into place for the overwrite modes, or
//!   hard-linked (atomic no-clobber) for an `Absent` guard so a
//!   concurrent writer cannot be silently overwritten. A `HashEquals`
//!   guard re-checks the target's `(size, mtime)` just before the commit
//!   (a residual sub-stat TOCTOU on a shared network mount is a
//!   documented limitation of this reference provider).
//! - **Strict scope enforcement**: every operation resolves its
//!   [`RemotePath`] under the canonical root and refuses symlink
//!   escapes, traversal, and device crossings.
//! - **Op-id tagging**: xattr-primary with side-file fallback via
//!   [`OpIdTagStore`]; side-files and temp files are hidden from
//!   enumeration and the changes feed.

mod feed;

pub use feed::ManualFeedHandle;

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use vapor_platform::fs_caps::{FilesystemCapabilities, NativeFilesystemCapabilities};
use vapor_shared::constants;

use crate::tags::OpIdTagStore;
use crate::{
    ChangesPoll, DownloadRequest, HashAlgorithm, Provider, ProviderCapabilities, ProviderError,
    RemoteEntry, RemoteEntryKind, RemotePath, RemotePrecondition, TransferOutcome, TransferSession,
    TransferStep, UploadRequest,
};

/// Hex-encoded SHA-256 of a byte slice. Shared by the provider, the
/// engine's hash stage tests, and the stub provider.
/// Whether `path` is there and is neither a regular file nor a folder
/// nor a link: a pipe, a socket, or a device. Such an entry is not an
/// object of the cloud folder, and it is never opened.
pub(crate) fn is_special_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        let kind = metadata.file_type();
        !kind.is_file() && !kind.is_dir() && !kind.is_symlink()
    })
}

pub fn hash_hex_of_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

/// Streaming SHA-256 of a file's current content.
pub fn hash_hex_of_file(path: &Path) -> io::Result<String> {
    hash_hex_of_file_with(path, HashAlgorithm::Sha256)
}

/// Streaming hash of a file's current content in the given algorithm.
/// The single algorithm-aware file hasher: any local hash compared
/// against a provider-produced hash (sync index, transfer outcome, echo
/// record) must go through here so SHA-256 and MD5 providers compare
/// like against like.
pub fn hash_hex_of_file_with(path: &Path, algorithm: HashAlgorithm) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    match algorithm {
        HashAlgorithm::Sha256 => {
            let mut hasher = Sha256::new();
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            Ok(hex_encode(&hasher.finalize()))
        }
        HashAlgorithm::Md5 => {
            let mut hasher = md5::Md5::new();
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            Ok(hex_encode(&hasher.finalize()))
        }
    }
}

fn hex_encode(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Whether `name` is one of Vapor's hidden internal names: op-id
/// side-files, in-flight temp files, the root marker, and the `.vapor`
/// runtime directory.
pub fn is_internal_file_name(name: &str) -> bool {
    name.starts_with(constants::provider::TEMP_FILE_PREFIX)
        || name.ends_with(constants::provider::OP_ID_SIDE_FILE_SUFFIX)
        || name == constants::provider::ROOT_MARKER_FILE_NAME
        || constants::filtering::INTERNAL_IGNORE_DIRECTORY_NAMES.contains(&name)
}

/// Whether any segment of `remote` is an internal name: a path under
/// a `.vapor` directory is Vapor's own state, never an object to sync.
pub fn has_internal_segment(remote: &RemotePath) -> bool {
    remote.as_str().split('/').any(is_internal_file_name)
}

/// Best-effort reap of an orphaned staging temp file. A `TEMP_FILE_PREFIX`
/// file older than the stale-age threshold is crash residue (an
/// interrupted upload/download stage): hidden from sync but never
/// otherwise removed, so it would accumulate across unclean shutdowns.
/// A young temp file (an in-flight, possibly throttle-paused transfer) is
/// left alone. Errors are ignored: this is cleanup, never load-bearing.
pub fn reap_if_stale_temp_file(path: &Path, name: &str, now: SystemTime) {
    if !name.starts_with(constants::provider::TEMP_FILE_PREFIX) {
        return;
    }
    let stale = fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| {
            age.as_millis() as u64 >= constants::provider::STALE_TEMP_FILE_MAX_AGE_MILLIS
        });
    if stale {
        let _ = fs::remove_file(path);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FeedMode {
    Native,
    Manual,
}

pub struct FilesystemProvider {
    caps: Arc<dyn FilesystemCapabilities>,
    tags: OpIdTagStore,
    root: Mutex<Option<PathBuf>>,
    feed: feed::ChangesFeed,
    feed_mode: FeedMode,
    /// Entries of the cloud folder whose name cannot be a sync path and
    /// that this provider has said so about, so each is reported one
    /// time and not at every listing.
    reported_names: Mutex<std::collections::BTreeSet<PathBuf>>,
}

impl FilesystemProvider {
    /// A provider that resolves its root when the engine calls
    /// [`Provider::ensure_cloud_root`] and watches it natively
    /// for the changes feed.
    pub fn new() -> Self {
        Self::with_capabilities(Arc::new(NativeFilesystemCapabilities::for_current_host()))
    }

    pub fn with_capabilities(caps: Arc<dyn FilesystemCapabilities>) -> Self {
        Self {
            tags: OpIdTagStore::new(caps.clone()),
            caps,
            root: Mutex::new(None),
            feed: feed::ChangesFeed::new(),
            feed_mode: FeedMode::Native,
            reported_names: Mutex::new(std::collections::BTreeSet::new()),
        }
    }

    /// Test constructor: the changes feed is driven manually through the
    /// returned handle instead of a native watcher, so tests stay
    /// deterministic. The root is ensured immediately.
    pub fn with_manual_feed(
        root: &Path,
        caps: Arc<dyn FilesystemCapabilities>,
    ) -> Result<(Self, ManualFeedHandle), ProviderError> {
        let mut provider = Self::with_capabilities(caps);
        provider.feed_mode = FeedMode::Manual;
        provider.ensure_cloud_root(&root.to_string_lossy())?;
        let handle = provider.feed.manual_handle();
        Ok((provider, handle))
    }

    /// Convenience constructor for tests and the daemon bring-up path:
    /// ensures `root` immediately with native capabilities.
    pub fn with_root(root: &Path) -> Result<Self, ProviderError> {
        let provider = Self::new();
        provider.ensure_cloud_root(&root.to_string_lossy())?;
        Ok(provider)
    }

    fn canonical_root(&self) -> Result<PathBuf, ProviderError> {
        self.root
            .lock()
            .expect("filesystem provider root mutex poisoned")
            .clone()
            .ok_or_else(|| {
                ProviderError::permanent(
                    "filesystem provider has no ensured cloud root yet; \
                     configuration must be validated before sync work starts",
                )
            })
    }

    /// Resolves a remote path under the canonical root and enforces
    /// scope: the deepest existing ancestor must canonicalize inside
    /// the root and live on the same device.
    fn resolve_in_scope(&self, remote: &RemotePath) -> Result<PathBuf, ProviderError> {
        let root = self.canonical_root()?;
        let resolved = remote.resolve_under(&root);

        let canonical_anchor =
            canonical_existing_ancestor(&resolved, &vapor_shared::paths::canonicalize)
                .map_err(|error| scope_lookup_failure(&root, remote, &error))?;
        if !canonical_anchor.starts_with(&root) {
            return Err(classify_scope_failure(
                &root,
                format!(
                    "remote path {remote} escapes the cloud sync root (resolves through {})",
                    canonical_anchor.display()
                ),
            ));
        }
        enforce_same_device(&root, &canonical_anchor, remote)?;
        Ok(resolved)
    }

    fn remote_entry_for(
        &self,
        remote: &RemotePath,
        resolved: &Path,
    ) -> Result<Option<RemoteEntry>, ProviderError> {
        if stored_under_another_spelling(resolved).is_some() {
            return Ok(None);
        }
        let metadata = match fs::symlink_metadata(resolved) {
            Ok(metadata) => metadata,
            // `NotADirectory`: a folder on the way to the path is a
            // file now, so the object is gone as surely as with
            // `NotFound`.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(ProviderError::transient(format!(
                    "cannot stat remote path {remote}: {error}"
                )));
            }
        };
        if metadata.file_type().is_symlink() {
            // Symlinks are outside the provider's contract: following
            // them risks scope escape, so they are invisible.
            return Ok(None);
        }
        if !metadata.is_dir() && !metadata.is_file() {
            // A pipe, a socket, a device: no content to transfer, and
            // opening a pipe waits for a writer that may never come.
            return Ok(None);
        }
        let kind = if metadata.is_dir() {
            RemoteEntryKind::Directory
        } else {
            RemoteEntryKind::File
        };
        let op_id = if kind == RemoteEntryKind::File {
            self.tags.read_op_id(resolved)
        } else {
            None
        };
        Ok(Some(RemoteEntry {
            path: remote.clone(),
            kind,
            size_bytes: if kind == RemoteEntryKind::File {
                metadata.len()
            } else {
                0
            },
            modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            content_hash: None,
            op_id,
        }))
    }

    /// Gives each folder on the way to `directory` the spelling the
    /// path asks for. On a volume that folds names, a folder renamed
    /// in spelling only resolves to the old folder, so a move into
    /// `Photos/` would land in `photos/` and the rename of the folder
    /// would never reach the cloud.
    fn respell_folders(&self, directory: &Path) -> io::Result<()> {
        let root = self
            .canonical_root()
            .map_err(|error| io::Error::other(error.message))?;
        let Ok(below_root) = directory.strip_prefix(&root) else {
            return Ok(());
        };
        let mut folder = root;
        for name in below_root.components() {
            folder.push(name);
            if let Some(stored) = stored_under_another_spelling(&folder)
                && stored.parent() == folder.parent()
            {
                fs::rename(&stored, &folder)?;
            }
        }
        Ok(())
    }

    /// Whether the nearest folder that exists on the way to `target`
    /// is stored under another spelling than the path asks for.
    fn folder_stored_under_another_spelling(&self, target: &Path) -> bool {
        let Ok(root) = self.canonical_root() else {
            return false;
        };
        let mut folder = target.parent();
        while let Some(current) = folder {
            if current == root || !current.starts_with(&root) {
                return false;
            }
            match vapor_shared::paths::stored_spelling(current) {
                vapor_shared::paths::StoredSpelling::Other(_) => return true,
                vapor_shared::paths::StoredSpelling::Exact => return false,
                vapor_shared::paths::StoredSpelling::Absent => folder = current.parent(),
            }
        }
        false
    }

    fn ensure_feed_started(&self) -> Result<(), ProviderError> {
        if self.feed_mode == FeedMode::Manual {
            return Ok(());
        }
        let root = self.canonical_root()?;
        self.feed.ensure_native_watcher(&root)
    }

    /// Read access for the engine's local apply path and loop
    /// prevention (shared tag semantics).
    pub fn tag_store(&self) -> &OpIdTagStore {
        &self.tags
    }
}

impl Default for FilesystemProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for FilesystemProvider {
    fn name(&self) -> &'static str {
        constants::provider::FILESYSTEM
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let mut capabilities = ProviderCapabilities::FILESYSTEM;
        // Capability honesty: the native changes feed rides the
        // platform fs-watcher; on hosts where that is still a stub the
        // feed is unadvertised and the engine falls back to reconcile
        // enumeration. Manual feeds (tests) are host-independent.
        if matches!(self.feed_mode, FeedMode::Native)
            && !vapor_platform::fs_watch::native_watcher_available()
        {
            capabilities.supports_remote_changes_feed = false;
            capabilities.changes_feed_is_watched = false;
        }
        capabilities
    }

    fn content_hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::Sha256
    }

    fn ensure_cloud_root(&self, cloud_root: &str) -> Result<(), ProviderError> {
        let expanded = expand_cloud_directory(cloud_root)?;
        if let Err(error) = fs::create_dir_all(&expanded) {
            return Err(ProviderError::permanent(format!(
                "cannot create filesystem cloud root {}: {error}. \
                 Set `cloudRoot` to a writable absolute path when \
                 `provider` is \"filesystem\"",
                expanded.display()
            )));
        }
        let canonical = vapor_shared::paths::canonicalize(&expanded).map_err(|error| {
            ProviderError::permanent(format!(
                "cannot canonicalize filesystem cloud root {}: {error}",
                expanded.display()
            ))
        })?;
        // The root is ensured again at every check of the root; only
        // a root that is new to this provider is news.
        let mut root = self
            .root
            .lock()
            .expect("filesystem provider root mutex poisoned");
        if root.as_ref() != Some(&canonical) {
            crate::logging::info(
                "Filesystem provider ensured cloud root",
                &[("path", canonical.display().to_string())],
            );
        }
        *root = Some(canonical);
        drop(root);
        Ok(())
    }

    fn move_object(
        &self,
        from: &RemotePath,
        to: &RemotePath,
        op_id: &str,
    ) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_in_scope(from)?;
        let resolved_to = self.resolve_in_scope(to)?;
        // Only an answer of the volume says that the source is gone. A
        // folder that cannot be read (a lost permission, a share that
        // went away) answers nothing, and the caller that took that for
        // "gone" would act on a deletion nobody made.
        match resolved_from.try_exists() {
            Ok(true) if stored_under_another_spelling(&resolved_from).is_none() => {}
            Ok(_) => {
                return Err(ProviderError::not_found(format!(
                    "move source {from} does not exist"
                )));
            }
            // A folder on the way that is a file now holds no source.
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                return Err(ProviderError::not_found(format!(
                    "move source {from} does not exist"
                )));
            }
            Err(error) => {
                return Err(ProviderError::transient(format!(
                    "cannot read the move source {from}: {error}"
                )));
            }
        }
        // On a volume that folds names, a destination that is another
        // spelling of the source's own name resolves to the source:
        // that is the rename asked for, not an occupied name.
        let respelling_the_source = stored_under_another_spelling(&resolved_to)
            .is_some_and(|stored| stored == resolved_from);
        if fs::symlink_metadata(&resolved_to).is_ok() && !respelling_the_source {
            return Err(ProviderError::precondition_failed(format!(
                "move destination {to} already exists"
            )));
        }
        if let Some(parent) = resolved_to.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot create the destination directory for {to}: {error}"
                ))
            })?;
            self.respell_folders(parent).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot rename the destination directory for {to}: {error}"
                ))
            })?;
        }
        fs::rename(&resolved_from, &resolved_to).map_err(|error| {
            ProviderError::transient(format!("cannot move {from} to {to}: {error}"))
        })?;
        self.tags
            .relocate_side_file(&resolved_from, &resolved_to)
            .map_err(|error| {
                ProviderError::transient(format!(
                    "moved {from} to {to} but could not relocate its tag side-file: {error}"
                ))
            })?;
        let _ = self.tags.write_op_id(&resolved_to, op_id);
        Ok(())
    }

    fn root_identity(&self, cloud_root: &str) -> Result<Option<String>, ProviderError> {
        let expanded = expand_cloud_directory(cloud_root)?;
        match fs::symlink_metadata(&expanded) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(ProviderError::permanent(format!(
                    "filesystem cloud root {} is not a directory",
                    expanded.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ProviderError::not_found(format!(
                    "filesystem cloud root {} is missing",
                    expanded.display()
                )));
            }
            Err(error) => {
                return Err(ProviderError::transient(format!(
                    "cannot stat filesystem cloud root {}: {error}",
                    expanded.display()
                )));
            }
        }
        crate::root_marker::read_marker(&expanded)
            .map(|marker| marker.map(|marker| marker.root_id))
            .map_err(|error| {
                ProviderError::transient(format!(
                    "cannot read the root marker in {}: {error}",
                    expanded.display()
                ))
            })
    }

    fn adopt_root(
        &self,
        cloud_root: &str,
        device_id: &str,
    ) -> Result<Option<String>, ProviderError> {
        let expanded = expand_cloud_directory(cloud_root)?;
        crate::root_marker::adopt(&expanded, device_id, SystemTime::now())
            .map(|marker| Some(marker.root_id))
            .map_err(|error| {
                ProviderError::transient(format!(
                    "cannot write the root marker in {}: {error}",
                    expanded.display()
                ))
            })
    }

    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
        Ok(self.enumerate_with_unsyncable(directory)?.entries)
    }

    fn enumerate_with_unsyncable(
        &self,
        directory: &RemotePath,
    ) -> Result<crate::RemoteListing, ProviderError> {
        let mut unsyncable = Vec::new();
        let resolved = self.resolve_in_scope(directory)?;
        let reader = fs::read_dir(&resolved).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ProviderError::not_found(format!("remote directory {directory} does not exist"))
            } else {
                ProviderError::transient(format!(
                    "cannot enumerate remote directory {directory}: {error}"
                ))
            }
        })?;

        let mut entries = Vec::new();
        for dir_entry in reader {
            let dir_entry = dir_entry.map_err(|error| {
                ProviderError::transient(format!(
                    "cannot read remote directory entry under {directory}: {error}"
                ))
            })?;
            let name = dir_entry.file_name();
            let Some(name) = name.to_str() else {
                // Unrepresentable in the UTF-8 RemotePath: log so a file
                // that never syncs is diagnosable rather than silently
                // absent (macOS enforces UTF-8 names, so this is rare).
                let first_report = self
                    .reported_names
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(dir_entry.path());
                if first_report {
                    crate::logging::warning(
                        "A file in the cloud folder has a name that is not valid UTF-8; it cannot be synced. Rename it",
                        &[("path", dir_entry.path().to_string_lossy().into_owned())],
                    );
                }
                unsyncable.push(crate::UnsyncableRemoteName {
                    name: dir_entry.file_name().to_string_lossy().into_owned(),
                    reason: "its name is not valid UTF-8, which the sync path model cannot carry"
                        .to_string(),
                });
                continue;
            };
            if is_internal_file_name(name) {
                // Opportunistically reap orphaned staging temp files left
                // by an unclean crash so they cannot accumulate hidden in
                // the cloud folder; in-flight (young) temps are untouched.
                reap_if_stale_temp_file(&dir_entry.path(), name, SystemTime::now());
                continue;
            }
            let Ok(child) = directory.join(name) else {
                // A name the path model refuses (a backslash, which is
                // a separator on Windows). It stays in the cloud folder
                // only, and the log says so one time.
                let first_report = self
                    .reported_names
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(dir_entry.path());
                if first_report {
                    crate::logging::warning(
                        "A file in the cloud folder has a name that cannot be synced. Rename it",
                        &[("path", dir_entry.path().to_string_lossy().into_owned())],
                    );
                }
                unsyncable.push(crate::UnsyncableRemoteName {
                    name: name.to_string(),
                    reason:
                        "its name has a backslash in it, which the sync path model cannot carry"
                            .to_string(),
                });
                continue;
            };
            if let Some(entry) = self.remote_entry_for(&child, &dir_entry.path())? {
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        unsyncable.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(crate::RemoteListing {
            entries,
            unsyncable,
        })
    }

    fn stat(&self, path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_in_scope(path)?;
        self.remote_entry_for(path, &resolved)
    }

    fn content_hash(&self, path: &RemotePath) -> Result<String, ProviderError> {
        let resolved = self.resolve_in_scope(path)?;
        if stored_under_another_spelling(&resolved).is_some() || is_special_file(&resolved) {
            return Err(ProviderError::not_found(format!(
                "remote file {path} does not exist"
            )));
        }
        hash_hex_of_file(&resolved).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ProviderError::not_found(format!("remote file {path} does not exist"))
            } else {
                ProviderError::transient(format!("cannot hash remote file {path}: {error}"))
            }
        })
    }

    fn begin_upload(
        &self,
        request: UploadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let target = self.resolve_in_scope(&request.remote_path)?;
        if stored_under_another_spelling(&target).is_some()
            || self.folder_stored_under_another_spelling(&target)
        {
            // The folder holds this name, or one of the folders on the
            // way to it, under another spelling. Writing here would
            // replace that object, or land inside a folder that then
            // does not answer for this path. Its own deletion or move
            // frees the name; until then the upload waits.
            return Err(ProviderError::transient(format!(
                "the cloud folder holds {} under another spelling; waiting for that name to go",
                request.remote_path
            )));
        }
        let source = fs::File::open(&request.local_source).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ProviderError::not_found(format!(
                    "upload source {} disappeared",
                    request.local_source.display()
                ))
            } else {
                ProviderError::transient(format!(
                    "cannot open upload source {}: {error}",
                    request.local_source.display()
                ))
            }
        })?;

        let parent = target.parent().ok_or_else(|| {
            ProviderError::permanent(format!(
                "upload target {} has no parent directory",
                request.remote_path
            ))
        })?;
        // A full volume can refuse the folder or the temp file, before
        // the first byte is written. That is the cloud being full too.
        fs::create_dir_all(parent).map_err(|error| {
            if error.kind() == io::ErrorKind::StorageFull {
                return upload_write_error(&request.remote_path, "create", &error);
            }
            ProviderError::transient(format!(
                "cannot create remote parent directory for {}: {error}",
                request.remote_path
            ))
        })?;
        let temp_path = parent.join(temp_file_name(&request.op_id));
        let temp = fs::File::create(&temp_path)
            .map_err(|error| upload_write_error(&request.remote_path, "create", &error))?;

        // Resolve the precondition into a phase. `HashEquals` opens the
        // target now (cheap) and defers the actual read to budgeted
        // `step()` slices; `Absent`/`None` stream immediately (the real
        // Absent guard is the no-clobber create at finalize).
        let (require_absent, hash_guard, phase) = match request.precondition {
            RemotePrecondition::None => (false, None, UploadPhase::Streaming),
            RemotePrecondition::Absent => {
                if target.exists() {
                    return Err(ProviderError::precondition_failed(format!(
                        "upload target {} already exists",
                        request.remote_path
                    )));
                }
                (true, None, UploadPhase::Streaming)
            }
            RemotePrecondition::HashEquals(expected) => {
                let target_file = fs::File::open(&target).map_err(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        ProviderError::precondition_failed(format!(
                            "upload target {} vanished while guarded by a hash precondition",
                            request.remote_path
                        ))
                    } else {
                        ProviderError::transient(format!(
                            "cannot open upload target {} to verify precondition: {error}",
                            request.remote_path
                        ))
                    }
                })?;
                (
                    false,
                    Some(HashGuard {
                        expected,
                        verified_size_mtime: None,
                        verified_node: target_file
                            .metadata()
                            .ok()
                            .and_then(|metadata| file_node(&metadata)),
                    }),
                    UploadPhase::VerifyingHash {
                        target: target_file,
                        hasher: Sha256::new(),
                    },
                )
            }
        };

        let source_len = source.metadata().map(|metadata| metadata.len()).ok();
        Ok(Box::new(FilesystemUploadSession {
            source,
            temp: Some(temp),
            temp_path,
            target,
            remote_path: request.remote_path,
            op_id: request.op_id,
            require_absent,
            hash_guard,
            phase,
            caps: self.caps.clone(),
            tags: self.tags.clone(),
            hasher: Sha256::new(),
            bytes_total: 0,
            source_len,
            finished: false,
        }))
    }

    fn begin_download(
        &self,
        request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let resolved = self.resolve_in_scope(&request.remote_path)?;
        if stored_under_another_spelling(&resolved).is_some() || is_special_file(&resolved) {
            return Err(ProviderError::not_found(format!(
                "remote file {} does not exist",
                request.remote_path
            )));
        }
        let source = fs::File::open(&resolved).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ProviderError::not_found(format!(
                    "remote file {} does not exist",
                    request.remote_path
                ))
            } else {
                ProviderError::transient(format!(
                    "cannot open remote file {}: {error}",
                    request.remote_path
                ))
            }
        })?;
        if let Some(parent) = request.destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot create download destination directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let destination = fs::File::create(&request.destination).map_err(|error| {
            ProviderError::transient(format!(
                "cannot create download destination {}: {error}",
                request.destination.display()
            ))
        })?;

        let source_len = source.metadata().map(|metadata| metadata.len()).ok();
        Ok(Box::new(FilesystemDownloadSession {
            source,
            destination: Some(destination),
            destination_path: request.destination,
            remote_path: request.remote_path,
            hasher: Sha256::new(),
            bytes_total: 0,
            source_len,
            finished: false,
        }))
    }

    fn delete(&self, path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_in_scope(path)?;
        if stored_under_another_spelling(&resolved).is_some() {
            // Another spelling of the name is another object; removing
            // through this one would delete it.
            return Err(ProviderError::not_found(format!(
                "remote file {path} was already gone"
            )));
        }
        match fs::remove_file(&resolved) {
            Ok(()) => {
                let _ = self.tags.remove(&resolved);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(ProviderError::not_found(
                format!("remote file {path} was already gone"),
            )),
            Err(error) if resolved.is_dir() => {
                // Non-recursive on purpose: `remove_dir` fails on a
                // non-empty directory. A recursive `remove_dir_all` here
                // would destroy children the engine never observed,
                // violating the never-silent-overwrite guarantee. The one
                // exception is Vapor's own bookkeeping: a staging temp an
                // aborted transfer left behind, or the op-id side-file of
                // a file that is already gone. Those are invisible to
                // listings, so the engine can never delete them itself,
                // and a directory holding nothing else is empty as far as
                // the user is concerned.
                reap_internal_files(&resolved);
                fs::remove_dir(&resolved).map_err(|dir_error| {
                    ProviderError::transient(format!(
                        "cannot delete remote directory {path}: {dir_error} (file path error: {error})"
                    ))
                })
            }
            Err(error) => Err(ProviderError::transient(format!(
                "cannot delete remote file {path}: {error}"
            ))),
        }
    }

    fn poll_changes(
        &self,
        cursor: Option<&str>,
        max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
        self.ensure_feed_started()?;
        let root = self.canonical_root()?;
        self.feed.poll(&root, &self.tags, cursor, max_changes)
    }
}

/// Removes Vapor's internal files (staging temps, op-id side-files)
/// directly inside `directory` and nothing else. Used before deleting
/// a directory the engine believes is empty.
/// The stored path when the cloud folder holds the entry `resolved`
/// names under another spelling. A cloud keeps `Report.txt` and
/// `report.txt` apart, and the engine relies on that: a folder on a
/// volume that folds names must not answer for a spelling it does not
/// store.
fn stored_under_another_spelling(resolved: &Path) -> Option<PathBuf> {
    match vapor_shared::paths::stored_spelling(resolved) {
        vapor_shared::paths::StoredSpelling::Other(stored) => Some(stored),
        _ => None,
    }
}

fn reap_internal_files(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_internal_file_name(name) && entry.path().is_file() {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn temp_file_name(op_id: &str) -> String {
    let sanitized: String = op_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{}{sanitized}", constants::provider::TEMP_FILE_PREFIX)
}

/// Expands and validates the configured cloud directory for the
/// filesystem provider (reinterpretation): `~`-prefixed values
/// expand against the home directory; everything else must already be
/// absolute.
fn expand_cloud_directory(raw: &str) -> Result<PathBuf, ProviderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::permanent(
            "cloudRoot is empty; the filesystem provider needs an absolute local path",
        ));
    }
    if trimmed == "~" || trimmed.starts_with("~/") {
        let home = vapor_shared::runtime_paths::home_directory().ok_or_else(|| {
            ProviderError::permanent("cannot expand ~ in cloudRoot: no home directory resolved")
        })?;
        return Ok(match trimmed.strip_prefix("~/") {
            Some(suffix) => home.join(suffix),
            None => home,
        });
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(ProviderError::permanent(format!(
            "cloudRoot {trimmed} must be an absolute path when `provider` is \"filesystem\""
        )));
    }
    Ok(path)
}

/// Best-effort POSIX-mode carry-over from an open source handle onto a
/// staged file. The handle's metadata stays readable even after the
/// source path is renamed away mid-transfer. Mode is metadata, not
/// content: a failure here must never fail the transfer (the bytes are
/// correct), so it only logs. No-op where the platform has no POSIX
/// modes.
#[cfg(unix)]
fn copy_permissions_from_handle(source: &fs::File, staged: &Path) {
    match source.metadata() {
        Ok(metadata) => {
            if let Err(error) = fs::set_permissions(staged, metadata.permissions()) {
                let fields = [
                    ("path", staged.display().to_string()),
                    ("error", error.to_string()),
                ];
                if error.kind() == io::ErrorKind::NotFound {
                    // The staging file's directory was removed underneath
                    // the transfer (a concurrent remote delete); the
                    // commit that follows fails and the intent retries
                    // against fresh remote state. Nothing to act on here.
                    crate::logging::debug(
                        "Staged transfer payload vanished before its permissions could be set",
                        &fields,
                    );
                } else {
                    crate::logging::warning(
                        "Could not carry file permissions onto the staged transfer payload",
                        &fields,
                    );
                }
            }
        }
        Err(error) => {
            crate::logging::warning(
                "Could not read source file permissions for the staged transfer payload",
                &[
                    ("path", staged.display().to_string()),
                    ("error", error.to_string()),
                ],
            );
        }
    }
}

#[cfg(not(unix))]
fn copy_permissions_from_handle(_source: &fs::File, _staged: &Path) {}

/// A scope-resolution failure has two very different causes: a genuinely
/// out-of-scope path (permanent, never retry) and the cloud sync root
/// itself having vanished under a running daemon (deleted, unmounted).
/// In the latter case the deepest-existing ancestor of *every* legal
/// path resolves through the root's parent, so the escape check fires
/// for paths that are perfectly in scope. Report that as the
/// self-healing `CloudRootUnavailable` so the engine blocks admission
/// and re-ensures the root instead of spinning on a permanent error.
fn classify_scope_failure(root: &Path, message: String) -> ProviderError {
    if !root.is_dir() {
        return ProviderError::cloud_root_unavailable(format!(
            "cloud root {} no longer exists ({message})",
            root.display()
        ));
    }
    ProviderError::permanent(message)
}

/// The canonical form of the deepest ancestor of `path` that exists.
/// One look finds that ancestor and a second one canonicalizes it. An
/// entry that goes between the two (another writer of the cloud folder
/// removed it, or a rename that changes only its spelling is under
/// way, which a volume may carry out in two steps) is looked for
/// again, a few times.
fn canonical_existing_ancestor(
    path: &Path,
    canonicalize: &dyn Fn(&Path) -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    const LOOKS: usize = 4;
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for _ in 0..LOOKS {
        match canonicalize(&deepest_existing_ancestor(path)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => last = error,
            outcome => return outcome,
        }
    }
    Err(last)
}

/// What a failed look for the anchor of `remote` means. An entry that
/// kept going away while it was looked for changed under the daemon:
/// the next attempt finds what is there then. Anything else cannot be
/// placed inside the root, which is permanent, unless the root itself
/// is gone.
fn scope_lookup_failure(root: &Path, remote: &RemotePath, error: &io::Error) -> ProviderError {
    let message = format!("cannot resolve {remote} under the cloud sync root: {error}");
    if error.kind() == io::ErrorKind::NotFound && root.is_dir() {
        return ProviderError::transient(message);
    }
    classify_scope_failure(root, message)
}

fn deepest_existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    while !current.exists() {
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return current,
        }
    }
    current
}

#[cfg(unix)]
fn enforce_same_device(
    root: &Path,
    anchor: &Path,
    remote: &RemotePath,
) -> Result<(), ProviderError> {
    use std::os::unix::fs::MetadataExt;

    let root_dev = fs::metadata(root)
        .map_err(|error| ProviderError::transient(format!("cannot stat cloud sync root: {error}")))?
        .dev();
    let anchor_dev = fs::metadata(anchor)
        .map_err(|error| {
            ProviderError::transient(format!("cannot stat {}: {error}", anchor.display()))
        })?
        .dev();
    if root_dev != anchor_dev {
        return Err(ProviderError::permanent(format!(
            "remote path {remote} crosses onto a different device; \
             the filesystem provider refuses device crossings"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_same_device(
    _root: &Path,
    _anchor: &Path,
    _remote: &RemotePath,
) -> Result<(), ProviderError> {
    Ok(())
}

/// A HashEquals guard: the expected content hash plus the (size, mtime)
/// of the target observed at the end of the budgeted verification pass,
/// so `finalize` can cheaply re-stat immediately before the rename and
/// refuse the write if the target moved under us in the interim.
struct HashGuard {
    expected: String,
    verified_size_mtime: Option<(u64, SystemTime)>,
    /// Which file the verification read. A writer that replaces the
    /// target puts another file under the name, with a size and a time
    /// that may be the same.
    verified_node: Option<FileNode>,
}

/// The identity of a file on its volume, where the host has one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileNode {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn file_node(metadata: &fs::Metadata) -> Option<FileNode> {
    use std::os::unix::fs::MetadataExt;
    Some(FileNode {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn file_node(_metadata: &fs::Metadata) -> Option<FileNode> {
    None
}

/// Upload proceeds in two phases so a `HashEquals` precondition never
/// stalls the tick loop: the old remote copy is hashed in budgeted
/// chunks across `step()` calls before any payload is streamed.
enum UploadPhase {
    /// Reading the existing target to verify a `HashEquals` precondition.
    VerifyingHash { target: fs::File, hasher: Sha256 },
    /// Streaming the source payload into the temp file.
    Streaming,
}

struct FilesystemUploadSession {
    source: fs::File,
    temp: Option<fs::File>,
    temp_path: PathBuf,
    target: PathBuf,
    remote_path: RemotePath,
    op_id: String,
    /// `Absent` requires an atomic no-clobber create; `HashEquals` carries
    /// its guard here; `None` is absent.
    require_absent: bool,
    hash_guard: Option<HashGuard>,
    phase: UploadPhase,
    caps: Arc<dyn FilesystemCapabilities>,
    tags: OpIdTagStore,
    hasher: Sha256,
    bytes_total: u64,
    /// The source's size when the upload began.
    source_len: Option<u64>,
    finished: bool,
}

/// A failed write of an upload's temp file. A volume with no room left
/// is the cloud being full: the upload waits for space, it does not
/// burn its retries.
fn upload_write_error(remote_path: &RemotePath, action: &str, error: &io::Error) -> ProviderError {
    if error.kind() == io::ErrorKind::StorageFull {
        return ProviderError::storage_full(format!(
            "the cloud folder's volume is full ({remote_path}); free space, then uploads resume"
        ));
    }
    ProviderError::transient(format!(
        "cannot {action} temp file for {remote_path}: {error}"
    ))
}

impl FilesystemUploadSession {
    fn finalize(&mut self) -> Result<TransferOutcome, ProviderError> {
        let temp = self.temp.take().expect("finalize called with live temp");
        vapor_platform::fs_ops::make_durable(&temp)
            .map_err(|error| upload_write_error(&self.remote_path, "sync", &error))?;
        drop(temp);

        // Tag the temp file first so the rename lands a fully-tagged
        // object; fall back to a target-side side-file when the
        // filesystem cannot hold an xattr.
        let needs_side_file = self
            .caps
            .write_tag(
                &self.temp_path,
                constants::provider::OP_ID_XATTR_NAME,
                &self.op_id,
            )
            .is_err();

        // Re-check the HashEquals guard immediately before committing:
        // the name must still hold the file that was verified, with the
        // size and the time it had. A full re-hash here would bring back
        // the unbounded read this session exists to avoid. A writer that
        // lands after this check is caught by the commit itself, which
        // exchanges the two files.
        if let Some(guard) = &self.hash_guard {
            let current = fs::metadata(&self.target).ok();
            let same_file = current.as_ref().is_some_and(|metadata| {
                guard.verified_node.is_none() || file_node(metadata) == guard.verified_node
            });
            let current =
                current.and_then(|metadata| Some((metadata.len(), metadata.modified().ok()?)));
            if !same_file || current != guard.verified_size_mtime {
                let _ = fs::remove_file(&self.temp_path);
                self.finished = true;
                return Err(ProviderError::precondition_failed(format!(
                    "upload target {} changed after its hash was verified",
                    self.remote_path
                )));
            }
        }

        // Carry the source file's permissions onto the object landing
        // at the target (executable bits especially): the temp was
        // created with the process umask, and losing the mode turns a
        // 0755 script into a 0644 file on the other replica.
        copy_permissions_from_handle(&self.source, &self.temp_path);

        // Every write that can fail happens before the rename. An object
        // that lands must land tagged: a tag written after the commit
        // can fail on a full disk, and the retry then finds the bytes
        // already there, so the untagged object comes back through the
        // changes feed as another writer's edit.
        if needs_side_file && let Err(error) = self.tags.write_side_file(&self.target, &self.op_id)
        {
            let _ = fs::remove_file(&self.temp_path);
            self.finished = true;
            return Err(ProviderError::transient(format!(
                "cannot record the op-id tag for {}: {error}",
                self.remote_path
            )));
        }
        if let Err(error) = self.commit() {
            if needs_side_file {
                let _ = self.tags.remove_side_file(&self.target);
            }
            return Err(error);
        }

        self.finished = true;
        Ok(TransferOutcome {
            bytes_total: self.bytes_total,
            content_hash: hex_encode(&std::mem::take(&mut self.hasher).finalize()),
            remote_modified_at: current_size_mtime(&self.target).map(|(_, mtime)| mtime),
        })
    }

    /// Move the staged temp into place. `Absent` uses an atomic
    /// no-clobber *rename* so a concurrent writer that lands the target
    /// between the check and here is not silently overwritten.
    /// `HashEquals` exchanges the temp with the target, so the file it
    /// replaces is in its hands: when that is not the file the guard
    /// verified, another writer came in between, and its version stays
    /// as a copy. `None` renames unconditionally.
    /// The no-clobber commit must be a rename, never a hard-link +
    /// unlink pair: FSEvents tracks file events by node, and a link-
    /// created target stays bound to the deleted temp name: every
    /// later external edit or deletion of the uploaded file would then
    /// be invisible to the changes feed (observed live as a cloud-side
    /// `rm` that never propagated).
    fn commit(&mut self) -> Result<(), ProviderError> {
        if self.require_absent {
            match vapor_platform::fs_ops::atomic_noclobber_rename(&self.temp_path, &self.target) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&self.temp_path);
                    Err(ProviderError::precondition_failed(format!(
                        "upload target {} was created by another writer during the transfer",
                        self.remote_path
                    )))
                }
                Err(error) => Err(ProviderError::transient(format!(
                    "cannot move upload into place at {}: {error}",
                    self.remote_path
                ))),
            }
        } else if self
            .hash_guard
            .as_ref()
            .is_some_and(|guard| guard.verified_node.is_some())
        {
            self.commit_over_verified()
        } else {
            self.rename_into_place()
        }
    }

    fn rename_into_place(&self) -> Result<(), ProviderError> {
        fs::rename(&self.temp_path, &self.target).map_err(|error| {
            ProviderError::transient(format!(
                "cannot move upload into place at {}: {error}",
                self.remote_path
            ))
        })
    }

    fn commit_over_verified(&mut self) -> Result<(), ProviderError> {
        let guard = self
            .hash_guard
            .as_ref()
            .expect("the caller checked the guard");
        match exchange_into_place(&self.temp_path, &self.target, guard, self.caps.as_ref()) {
            Ok(None) => Ok(()),
            Ok(Some(copy)) => {
                crate::logging::warning(
                    "An upload replaced a version another writer had just put in the cloud; kept that version as a copy",
                    &[
                        ("path", self.remote_path.to_string()),
                        ("copy", copy.display().to_string()),
                    ],
                );
                Ok(())
            }
            Err(ExchangeError::NoExchange) => self.rename_into_place(),
            Err(ExchangeError::TargetGone) => {
                let _ = fs::remove_file(&self.temp_path);
                Err(ProviderError::precondition_failed(format!(
                    "upload target {} was deleted during the transfer; keeping both",
                    self.remote_path
                )))
            }
            Err(ExchangeError::Commit(error)) => Err(ProviderError::transient(format!(
                "cannot move upload into place at {}: {error}",
                self.remote_path
            ))),
            Err(ExchangeError::LostTheRace) => {
                // The two files are back as they were: the other
                // writer's version under the name, this upload under
                // the temp name.
                let _ = fs::remove_file(&self.temp_path);
                Err(ProviderError::precondition_failed(format!(
                    "upload target {} changed at the moment of the commit; keeping both",
                    self.remote_path
                )))
            }
            Err(ExchangeError::Kept { error, left_at }) => {
                // The upload is in place, and the version it replaced
                // is a file that nothing may remove: the temp name is
                // not this session's to clean up any more.
                self.finished = true;
                crate::logging::error(
                    "An upload replaced a version another writer had just put in the cloud, and that version could not be given the name of a copy",
                    &[
                        ("path", self.remote_path.to_string()),
                        ("left_at", left_at.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                Ok(())
            }
        }
    }

    /// One budgeted read of the existing target during hash verification.
    /// Returns `true` once the target reaches EOF (verification complete).
    fn verify_step(&mut self, max_bytes: u64) -> Result<(u64, bool), ProviderError> {
        let UploadPhase::VerifyingHash { target, hasher } = &mut self.phase else {
            unreachable!("verify_step called outside the verification phase");
        };
        let mut remaining = max_bytes.max(1);
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut consumed = 0_u64;
        while remaining > 0 {
            let chunk = buffer.len().min(remaining as usize);
            let read = target.read(&mut buffer[..chunk]).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot read upload target {} to verify precondition: {error}",
                    self.remote_path
                ))
            })?;
            if read == 0 {
                let current = hex_encode(&std::mem::take(hasher).finalize());
                let guard = self
                    .hash_guard
                    .as_mut()
                    .expect("verification phase implies a hash guard");
                if current != guard.expected {
                    return Err(ProviderError::precondition_failed(format!(
                        "upload target {} changed since planning",
                        self.remote_path
                    )));
                }
                // The size and the time of the file that was read, not
                // of whatever is under the name by now.
                guard.verified_size_mtime = target
                    .metadata()
                    .ok()
                    .and_then(|metadata| Some((metadata.len(), metadata.modified().ok()?)));
                return Ok((consumed, true));
            }
            hasher.update(&buffer[..read]);
            consumed += read as u64;
            remaining -= read as u64;
        }
        Ok((consumed, false))
    }
}

/// Why a guarded overwrite did not land as an exchange.
enum ExchangeError {
    /// The host or the volume has no exchange of two files.
    NoExchange,
    /// Nothing is under the target's name any more.
    TargetGone,
    /// The exchange itself failed; the target is untouched.
    Commit(io::Error),
    /// The file under the name was not the verified one and could not
    /// be kept as a copy, so the exchange was undone: the name holds
    /// that file again, and the temp name holds the upload.
    LostTheRace,
    /// The upload is in place. The file it replaced could not be given
    /// the name of a copy, and the exchange could not be undone: that
    /// file is at `left_at`, under a name that no cleanup removes.
    Kept { error: io::Error, left_at: PathBuf },
}

/// The commit of a guarded overwrite. The check of the guard and a
/// rename are two steps, and another writer can land between them. The
/// exchange closes that: whatever was under the name at the moment of
/// the commit is under the temp name afterwards, and it is removed only
/// when it is the file the guard verified. Any other file is a version
/// nobody compared: it stays next to the target as a conflict copy,
/// whose path is the answer.
fn exchange_into_place(
    temp_path: &Path,
    target: &Path,
    guard: &HashGuard,
    caps: &dyn FilesystemCapabilities,
) -> Result<Option<PathBuf>, ExchangeError> {
    let upload = fs::symlink_metadata(temp_path)
        .ok()
        .and_then(|metadata| file_node(&metadata));
    match vapor_platform::fs_ops::atomic_swap(temp_path, target) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            return Err(ExchangeError::NoExchange);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ExchangeError::TargetGone);
        }
        Err(error) => return Err(ExchangeError::Commit(error)),
    }
    let replaced = temp_path;
    let is_the_verified_file = fs::symlink_metadata(replaced).ok().is_some_and(|metadata| {
        file_node(&metadata) == guard.verified_node
            && metadata
                .modified()
                .ok()
                .map(|modified| (metadata.len(), modified))
                == guard.verified_size_mtime
    });
    if is_the_verified_file {
        let _ = fs::remove_file(replaced);
        return Ok(None);
    }
    let error = match keep_replaced_version(replaced, target, caps) {
        Ok(copy) => return Ok(Some(copy)),
        Err(error) => error,
    };
    // The version must not stay under a temp name: the session removes
    // that name when it ends, and the sweep of stale temp files removes
    // it later. Undo the exchange, so that the version is under the
    // name again and the upload loses the race in the usual way.
    if vapor_platform::fs_ops::atomic_swap(temp_path, target).is_ok() {
        let holds_the_upload = fs::symlink_metadata(temp_path)
            .ok()
            .is_some_and(|metadata| upload.is_some() && file_node(&metadata) == upload);
        if holds_the_upload {
            return Err(ExchangeError::LostTheRace);
        }
        // A third writer landed between the two exchanges, and its
        // file is under the temp name now. It is kept like the first.
        if let Ok(copy) = keep_replaced_version(temp_path, target, caps) {
            return Ok(Some(copy));
        }
    }
    // Last resort: a name outside the temp prefix, so that no cleanup
    // takes the file and the next scan syncs it as a file.
    let left_at = target.with_file_name(format!(
        "replaced-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|age| age.as_nanos())
            .unwrap_or(0)
    ));
    match fs::rename(temp_path, &left_at) {
        Ok(()) => Err(ExchangeError::Kept { error, left_at }),
        Err(_) => Err(ExchangeError::Kept {
            error,
            left_at: temp_path.to_path_buf(),
        }),
    }
}

/// Gives the file at `replaced` the name of a conflict copy of
/// `target`, next to it. The copy is nobody's upload: it carries no
/// op-id.
fn keep_replaced_version(
    replaced: &Path,
    target: &Path,
    caps: &dyn FilesystemCapabilities,
) -> io::Result<PathBuf> {
    let _ = caps.remove_tag(replaced, constants::provider::OP_ID_XATTR_NAME);
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let replaced_at_ms = fs::symlink_metadata(replaced)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|age| age.as_millis() as u64)
        .unwrap_or(0);
    let mut sequence = None;
    loop {
        let copy = target.with_file_name(copy_name_that_fits(&name, replaced_at_ms, sequence));
        match vapor_platform::fs_ops::atomic_noclobber_rename(replaced, &copy) {
            Ok(()) => return Ok(copy),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                sequence = Some(sequence.map_or(2, |sequence| sequence + 1));
            }
            Err(error) => return Err(error),
        }
    }
}

/// The name of the copy of a replaced version of `name`. A copy name is
/// longer than the name it comes from, and a file name has a limit:
/// the stem is cut, at a character, by what the copy name has too
/// much. The copy of such a name no longer says which file it belongs
/// to, and it is still a file that syncs.
fn copy_name_that_fits(name: &str, replaced_at_ms: u64, sequence: Option<u64>) -> String {
    const MAX_FILE_NAME_BYTES: usize = 255;
    let build = |name: &str| {
        vapor_shared::conflict::copy_name(
            name,
            constants::conflict::REPLACED_VERSION,
            replaced_at_ms,
            sequence,
        )
    };
    let full = build(name);
    let Some(excess) = full
        .len()
        .checked_sub(MAX_FILE_NAME_BYTES)
        .filter(|n| *n > 0)
    else {
        return full;
    };
    let (stem, extension) = match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(index) => name.split_at(index),
    };
    let mut keep = stem.len().saturating_sub(excess);
    while keep > 0 && !stem.is_char_boundary(keep) {
        keep -= 1;
    }
    build(&format!("{}{extension}", &stem[..keep]))
}

/// The target's (size, mtime) if it can be stat'd, for the pre-commit
/// TOCTOU re-check. `None` (e.g. the file vanished) is itself a change.
fn current_size_mtime(path: &Path) -> Option<(u64, SystemTime)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

impl TransferSession for FilesystemUploadSession {
    fn remaining_bytes(&self) -> Option<u64> {
        // The check of the old remote copy reads a file of another size.
        if matches!(self.phase, UploadPhase::VerifyingHash { .. }) {
            return None;
        }
        self.source_len
            .map(|len| len.saturating_sub(self.bytes_total).max(1))
    }

    fn step(&mut self, max_bytes: u64) -> Result<TransferStep, ProviderError> {
        debug_assert!(!self.finished, "step called after completion");

        // Phase 1: verify the HashEquals precondition by reading the old
        // remote copy in budgeted slices, so overwriting a multi-GB file
        // never holds the tick thread for one unbounded hash.
        if matches!(self.phase, UploadPhase::VerifyingHash { .. }) {
            let (bytes, done) = self.verify_step(max_bytes)?;
            if done {
                self.phase = UploadPhase::Streaming;
            }
            return Ok(TransferStep::Progressed {
                bytes_transferred: bytes,
            });
        }

        let mut remaining = max_bytes;
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut transferred = 0_u64;

        while remaining > 0 {
            let chunk = buffer.len().min(remaining as usize);
            let read = self.source.read(&mut buffer[..chunk]).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot read upload source for {}: {error}",
                    self.remote_path
                ))
            })?;
            if read == 0 {
                let outcome = self.finalize()?;
                return Ok(TransferStep::Completed(outcome));
            }
            let temp = self.temp.as_mut().expect("temp lives until finalize");
            temp.write_all(&buffer[..read])
                .map_err(|error| upload_write_error(&self.remote_path, "write", &error))?;
            self.hasher.update(&buffer[..read]);
            self.bytes_total += read as u64;
            transferred += read as u64;
            remaining -= read as u64;
        }
        Ok(TransferStep::Progressed {
            bytes_transferred: transferred,
        })
    }

    fn abort(&mut self) {
        self.temp.take();
        if !self.finished {
            let _ = fs::remove_file(&self.temp_path);
            self.finished = true;
        }
    }
}

impl Drop for FilesystemUploadSession {
    fn drop(&mut self) {
        if !self.finished {
            self.temp.take();
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}

struct FilesystemDownloadSession {
    source: fs::File,
    destination: Option<fs::File>,
    destination_path: PathBuf,
    remote_path: RemotePath,
    hasher: Sha256,
    bytes_total: u64,
    /// The remote file's size when the download began.
    source_len: Option<u64>,
    finished: bool,
}

impl TransferSession for FilesystemDownloadSession {
    fn remaining_bytes(&self) -> Option<u64> {
        self.source_len
            .map(|len| len.saturating_sub(self.bytes_total).max(1))
    }

    fn step(&mut self, max_bytes: u64) -> Result<TransferStep, ProviderError> {
        debug_assert!(!self.finished, "step called after completion");
        let mut remaining = max_bytes;
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut transferred = 0_u64;

        while remaining > 0 {
            let chunk = buffer.len().min(remaining as usize);
            let read = self.source.read(&mut buffer[..chunk]).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot read remote file {}: {error}",
                    self.remote_path
                ))
            })?;
            if read == 0 {
                let destination = self
                    .destination
                    .take()
                    .expect("destination lives until completion");
                vapor_platform::fs_ops::make_durable(&destination).map_err(|error| {
                    ProviderError::transient(format!(
                        "cannot sync downloaded payload for {}: {error}",
                        self.remote_path
                    ))
                })?;
                // Carry the remote file's permissions onto the staged
                // payload; the engine's rename into place preserves them.
                copy_permissions_from_handle(&self.source, &self.destination_path);
                self.finished = true;
                return Ok(TransferStep::Completed(TransferOutcome {
                    bytes_total: self.bytes_total,
                    content_hash: hex_encode(&std::mem::take(&mut self.hasher).finalize()),
                    remote_modified_at: self
                        .source
                        .metadata()
                        .ok()
                        .and_then(|metadata| metadata.modified().ok()),
                }));
            }
            let destination = self
                .destination
                .as_mut()
                .expect("destination lives until completion");
            destination.write_all(&buffer[..read]).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot write download destination {}: {error}",
                    self.destination_path.display()
                ))
            })?;
            self.hasher.update(&buffer[..read]);
            self.bytes_total += read as u64;
            transferred += read as u64;
            remaining -= read as u64;
        }
        Ok(TransferStep::Progressed {
            bytes_transferred: transferred,
        })
    }

    fn abort(&mut self) {
        self.destination.take();
        if !self.finished {
            let _ = fs::remove_file(&self.destination_path);
            self.finished = true;
        }
    }
}

impl Drop for FilesystemDownloadSession {
    fn drop(&mut self) {
        if !self.finished {
            self.destination.take();
            let _ = fs::remove_file(&self.destination_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vapor_platform::fs_caps::{CaseSensitivity, InMemoryFilesystemCapabilities};

    fn drive_to_completion(mut session: Box<dyn TransferSession>) -> TransferOutcome {
        loop {
            match session.step(8 * 1024).expect("transfer step") {
                TransferStep::Progressed { .. } => continue,
                TransferStep::Completed(outcome) => return outcome,
            }
        }
    }

    fn provider_at(dir: &Path) -> FilesystemProvider {
        FilesystemProvider::with_root(dir).expect("provider root")
    }

    #[cfg(unix)]
    #[test]
    fn transfers_preserve_posix_file_modes_in_both_directions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path().join("cloud");
        std::fs::create_dir_all(&root).expect("root");
        let provider = provider_at(&root);

        // Upload: a 0755 local script must land 0755 at the target.
        let local_script = dir.path().join("run.sh");
        std::fs::write(&local_script, b"#!/bin/sh\necho hi\n").expect("seed script");
        std::fs::set_permissions(&local_script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod source");
        let remote = RemotePath::new("run.sh").expect("remote path");
        let session = provider
            .begin_upload(UploadRequest {
                local_source: local_script,
                remote_path: remote.clone(),
                op_id: "op-mode".to_string(),
                precondition: RemotePrecondition::None,
            })
            .expect("begin upload");
        drive_to_completion(session);
        let uploaded_mode = std::fs::metadata(root.join("run.sh"))
            .expect("uploaded")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(uploaded_mode, 0o755, "upload must carry the mode");

        // Download: the staged payload must carry the remote's mode so
        // the engine's rename lands an executable file.
        let staging = dir.path().join("staged-download");
        let session = provider
            .begin_download(DownloadRequest {
                remote_path: remote,
                destination: staging.clone(),
            })
            .expect("begin download");
        drive_to_completion(session);
        let staged_mode = std::fs::metadata(&staging)
            .expect("staged")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(staged_mode, 0o755, "download must carry the mode");
    }

    #[test]
    fn vanished_cloud_root_classifies_as_cloud_root_unavailable() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path().join("cloud");
        std::fs::create_dir_all(&root).expect("root");
        let provider = provider_at(&root);
        let remote = RemotePath::new("docs/file.txt").expect("remote path");

        // Root alive: an in-scope path resolves fine.
        assert!(provider.stat(&remote).expect("stat").is_none());

        // Root deleted out from under the provider: every scope
        // resolution must report the self-healing kind, not Permanent.
        std::fs::remove_dir_all(&root).expect("delete root");
        let error = provider.stat(&remote).expect_err("stat must fail");
        assert_eq!(
            error.kind,
            vapor_shared::ProviderErrorKind::CloudRootUnavailable,
            "unexpected classification: {}",
            error.message
        );
    }

    #[test]
    fn ensure_creates_missing_cloud_root() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path().join("cloud/Vapor");
        let provider = FilesystemProvider::new();
        provider
            .ensure_cloud_root(&root.to_string_lossy())
            .expect("ensure creates root");
        assert!(root.is_dir());
    }

    #[test]
    fn ensure_rejects_relative_and_empty_paths_with_actionable_error() {
        let provider = FilesystemProvider::new();
        let error = provider
            .ensure_cloud_root("relative/cloud")
            .expect_err("relative must be rejected");
        assert!(error.message.contains("absolute path"));
        assert!(
            provider.ensure_cloud_root("  ").is_err(),
            "empty path must be rejected"
        );
    }

    #[test]
    fn upload_lands_atomically_with_op_id_tag_and_hash() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        let source = dir.path().join("source.txt");
        fs::write(&source, b"hello vapor").expect("seed source");

        let session = provider
            .begin_upload(UploadRequest {
                local_source: source,
                remote_path: RemotePath::new("docs/hello.txt").expect("remote path"),
                op_id: "op-upload-1".to_string(),
                precondition: RemotePrecondition::Absent,
            })
            .expect("upload session");
        let outcome = drive_to_completion(session);

        let target = cloud.join("docs/hello.txt");
        assert_eq!(fs::read(&target).expect("uploaded payload"), b"hello vapor");
        assert_eq!(outcome.bytes_total, 11);
        assert_eq!(outcome.content_hash, hash_hex_of_bytes(b"hello vapor"));
        assert_eq!(
            provider.tag_store().read_op_id(&target),
            Some("op-upload-1".to_string())
        );
        // No temp file remains.
        let leftovers: Vec<_> = fs::read_dir(cloud.join("docs"))
            .expect("read docs")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(constants::provider::TEMP_FILE_PREFIX)
            })
            .collect();
        assert!(leftovers.is_empty(), "temp files must not survive");
    }

    #[test]
    fn upload_side_file_tag_lands_before_the_rename_and_leaves_with_a_failed_commit() {
        // Without xattr support the op-id lives in a side-file. It is
        // written before the payload is renamed into place, so a tag
        // write that fails (a full disk) aborts the upload and no
        // object ever lands untagged; a commit that fails takes the
        // side-file with it.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        fs::create_dir_all(&cloud).expect("cloud root");
        let caps = Arc::new(InMemoryFilesystemCapabilities::new(
            false,
            CaseSensitivity::Sensitive,
        ));
        let (provider, _feed) =
            FilesystemProvider::with_manual_feed(&cloud, caps).expect("provider");
        let source = dir.path().join("source.txt");
        fs::write(&source, b"payload").expect("seed source");
        let request = |name: &str| UploadRequest {
            local_source: source.clone(),
            remote_path: RemotePath::new(name).expect("remote path"),
            op_id: "op-side".to_string(),
            precondition: RemotePrecondition::Absent,
        };

        let outcome = drive_to_completion(
            provider
                .begin_upload(request("landed.txt"))
                .expect("session"),
        );
        assert_eq!(outcome.bytes_total, 7);
        let landed = cloud.join("landed.txt");
        assert!(
            crate::tags::OpIdTagStore::side_file_path(&landed).exists(),
            "the tag lands with the object"
        );
        assert_eq!(
            provider.tag_store().read_op_id(&landed),
            Some("op-side".to_string())
        );

        // Another writer lands the target during the transfer: the
        // no-clobber commit refuses, and nothing of ours stays behind.
        let mut session = provider
            .begin_upload(request("taken.txt"))
            .expect("session");
        let taken = cloud.join("taken.txt");
        fs::write(&taken, b"theirs").expect("other writer");
        let error = loop {
            match session.step(8 * 1024) {
                Ok(TransferStep::Progressed { .. }) => continue,
                Ok(TransferStep::Completed(_)) => {
                    panic!("the commit must refuse an occupied target")
                }
                Err(error) => break error,
            }
        };
        assert_eq!(
            error.kind,
            vapor_shared::ProviderErrorKind::PreconditionFailed
        );
        assert_eq!(
            fs::read(&taken).expect("the other writer's file"),
            b"theirs"
        );
        assert!(
            !crate::tags::OpIdTagStore::side_file_path(&taken).exists(),
            "a failed commit leaves no side-file"
        );
        assert!(provider.tag_store().read_op_id(&taken).is_none());
    }

    #[test]
    fn upload_respects_absent_precondition() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::create_dir_all(cloud.join("docs")).expect("dirs");
        fs::write(cloud.join("docs/hello.txt"), b"pre-existing").expect("seed target");
        let source = dir.path().join("source.txt");
        fs::write(&source, b"new content").expect("seed source");

        // An Absent guard against an already-present target fails fast at
        // begin_upload (or, if it somehow slipped past, while stepping).
        let error = match provider.begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("docs/hello.txt").expect("remote path"),
            op_id: "op-upload-2".to_string(),
            precondition: RemotePrecondition::Absent,
        }) {
            Ok(mut session) => loop {
                match session.step(8 * 1024) {
                    Ok(TransferStep::Progressed { .. }) => continue,
                    Ok(TransferStep::Completed(_)) => panic!("must fail the precondition"),
                    Err(error) => break error,
                }
            },
            Err(error) => error,
        };
        assert_eq!(
            error.kind,
            vapor_shared::ProviderErrorKind::PreconditionFailed
        );
        assert_eq!(
            fs::read(cloud.join("docs/hello.txt")).expect("target intact"),
            b"pre-existing",
            "failed precondition must not touch the target"
        );
    }

    #[test]
    fn absent_precondition_is_atomic_no_clobber_when_target_appears_mid_transfer() {
        // The begin-time exists() check is a fast path, not the guard: a
        // concurrent writer that lands the target between begin_upload and
        // the commit must not be silently overwritten last-write-wins.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        let source = dir.path().join("source.txt");
        fs::write(&source, b"our upload").expect("seed source");

        let mut session = provider
            .begin_upload(UploadRequest {
                local_source: source,
                remote_path: RemotePath::new("race.txt").expect("remote path"),
                op_id: "op-race".to_string(),
                precondition: RemotePrecondition::Absent,
            })
            .expect("session opens: target absent at begin");

        // Another writer lands the target after the begin-time check.
        fs::write(cloud.join("race.txt"), b"someone else won").expect("racing write");

        let error = loop {
            match session.step(8 * 1024) {
                Ok(TransferStep::Progressed { .. }) => continue,
                Ok(TransferStep::Completed(_)) => panic!("no-clobber create must refuse"),
                Err(error) => break error,
            }
        };
        assert_eq!(
            error.kind,
            vapor_shared::ProviderErrorKind::PreconditionFailed
        );
        assert_eq!(
            fs::read(cloud.join("race.txt")).expect("racing content intact"),
            b"someone else won",
            "the concurrent writer's content must survive"
        );
    }

    #[test]
    fn upload_hash_precondition_guards_divergence() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::write(cloud.join("a.txt"), b"version-1").expect("seed");
        let source = dir.path().join("source.txt");
        fs::write(&source, b"version-2").expect("seed source");

        // Guard against the hash of a *different* version: must fail.
        let session = provider
            .begin_upload(UploadRequest {
                local_source: source.clone(),
                remote_path: RemotePath::new("a.txt").expect("remote path"),
                op_id: "op-3".to_string(),
                precondition: RemotePrecondition::HashEquals(hash_hex_of_bytes(b"other")),
            })
            .expect("session");
        let mut session = session;
        let error = loop {
            match session.step(1024) {
                Ok(TransferStep::Progressed { .. }) => continue,
                Ok(TransferStep::Completed(_)) => panic!("must fail"),
                Err(error) => break error,
            }
        };
        assert_eq!(
            error.kind,
            vapor_shared::ProviderErrorKind::PreconditionFailed
        );

        // Guard against the actual current hash: must succeed.
        let session = provider
            .begin_upload(UploadRequest {
                local_source: source,
                remote_path: RemotePath::new("a.txt").expect("remote path"),
                op_id: "op-4".to_string(),
                precondition: RemotePrecondition::HashEquals(hash_hex_of_bytes(b"version-1")),
            })
            .expect("session");
        let outcome = drive_to_completion(session);
        assert_eq!(outcome.content_hash, hash_hex_of_bytes(b"version-2"));
    }

    #[test]
    fn hash_precondition_is_verified_in_budgeted_slices() {
        // Overwriting a large existing file must not hash the whole old copy
        // in one unbudgeted step(): the verification is sliced so throttle
        // transitions and shutdown are honored between reads.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        let old = vec![3_u8; 200_000];
        fs::write(cloud.join("big.bin"), &old).expect("seed target");
        let source = dir.path().join("source.txt");
        fs::write(&source, b"replacement").expect("seed source");

        let mut session = provider
            .begin_upload(UploadRequest {
                local_source: source,
                remote_path: RemotePath::new("big.bin").expect("remote path"),
                op_id: "op-budget".to_string(),
                precondition: RemotePrecondition::HashEquals(hash_hex_of_bytes(&old)),
            })
            .expect("session");

        let mut steps = 0_u32;
        let outcome = loop {
            steps += 1;
            match session.step(1024).expect("step") {
                TransferStep::Progressed { .. } => continue,
                TransferStep::Completed(outcome) => break outcome,
            }
        };
        // 200 KB verified 1 KB at a time cannot complete in a handful of steps.
        assert!(
            steps > 100,
            "verification must be sliced, took only {steps} steps"
        );
        assert_eq!(outcome.content_hash, hash_hex_of_bytes(b"replacement"));
        assert_eq!(
            fs::read(cloud.join("big.bin")).expect("target"),
            b"replacement"
        );
    }

    #[test]
    fn hash_precondition_rechecks_target_just_before_commit() {
        // The target passes hash verification, then a concurrent writer
        // changes it before the rename lands. The pre-commit (size, mtime)
        // re-check must refuse rather than silently overwrite.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::write(cloud.join("c.txt"), b"planned").expect("seed target");
        let source = dir.path().join("source.txt");
        fs::write(&source, b"our new bytes").expect("seed source");

        let mut session = provider
            .begin_upload(UploadRequest {
                local_source: source,
                remote_path: RemotePath::new("c.txt").expect("remote path"),
                op_id: "op-recheck".to_string(),
                precondition: RemotePrecondition::HashEquals(hash_hex_of_bytes(b"planned")),
            })
            .expect("session");

        // First step verifies the (tiny) target's hash and transitions to
        // streaming. Mutate the target now, before the commit re-check.
        assert!(matches!(
            session.step(64 * 1024).expect("verify step"),
            TransferStep::Progressed { .. }
        ));
        fs::write(cloud.join("c.txt"), b"changed underneath us").expect("racing write");

        let error = loop {
            match session.step(64 * 1024) {
                Ok(TransferStep::Progressed { .. }) => continue,
                Ok(TransferStep::Completed(_)) => panic!("commit re-check must refuse"),
                Err(error) => break error,
            }
        };
        assert_eq!(
            error.kind,
            vapor_shared::ProviderErrorKind::PreconditionFailed
        );
        assert_eq!(
            fs::read(cloud.join("c.txt")).expect("target intact"),
            b"changed underneath us",
            "the concurrent write must survive the refused commit"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_version_that_lands_at_the_moment_of_the_commit_stays_as_a_copy() {
        // The guard verified one file. Another writer puts its own file
        // under the name after the last check and before the commit.
        // The commit must not be the end of that file.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        let target = cloud.join("doc.txt");
        fs::write(&target, b"base").expect("seed target");
        let verified = fs::metadata(&target).expect("stat");
        let guard = HashGuard {
            expected: hash_hex_of_bytes(b"base"),
            verified_size_mtime: Some((verified.len(), verified.modified().expect("mtime"))),
            verified_node: file_node(&verified),
        };

        let theirs = cloud.join("their-upload.tmp");
        fs::write(&theirs, b"their edit").expect("stage theirs");
        fs::rename(&theirs, &target).expect("their commit");
        let ours = cloud.join("our-upload.tmp");
        fs::write(&ours, b"our edit").expect("stage ours");

        let copy = exchange_into_place(&ours, &target, &guard, provider.caps.as_ref())
            .ok()
            .expect("the commit lands")
            .expect("the replaced version is kept");

        assert_eq!(fs::read(&target).expect("target"), b"our edit");
        assert_eq!(fs::read(&copy).expect("copy"), b"their edit");
        let name = copy
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        assert!(
            name.starts_with("doc~conflict-replaced-") && name.ends_with(".txt"),
            "{name} is not the name of a copy of doc.txt"
        );
        assert!(!ours.exists(), "nothing stays under the temp name");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_replaced_version_of_a_file_with_a_long_name_is_kept_too() {
        // A copy name is longer than the name: for a name near the
        // limit of the volume the copy would have none. The version
        // must stay a file all the same.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        let name = format!("{}.txt", "n".repeat(245));
        let target = cloud.join(&name);
        fs::write(&target, b"base").expect("seed target");
        let verified = fs::metadata(&target).expect("stat");
        let guard = HashGuard {
            expected: hash_hex_of_bytes(b"base"),
            verified_size_mtime: Some((verified.len(), verified.modified().expect("mtime"))),
            verified_node: file_node(&verified),
        };
        let theirs = cloud.join("their-upload.tmp");
        fs::write(&theirs, b"their edit").expect("stage theirs");
        fs::rename(&theirs, &target).expect("their commit");
        let ours = cloud.join("our-upload.tmp");
        fs::write(&ours, b"our edit").expect("stage ours");

        let copy = exchange_into_place(&ours, &target, &guard, provider.caps.as_ref())
            .ok()
            .expect("the commit lands")
            .expect("the replaced version is kept");

        assert_eq!(fs::read(&target).expect("target"), b"our edit");
        assert_eq!(fs::read(&copy).expect("copy"), b"their edit");
        let copy_name = copy.file_name().and_then(|n| n.to_str()).expect("name");
        assert!(
            copy_name.len() <= 255 && copy_name.ends_with(".txt"),
            "{copy_name}"
        );
        assert!(copy_name.contains("~conflict-replaced-"), "{copy_name}");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_file_the_guard_verified_is_replaced_with_no_copy() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        let target = cloud.join("doc.txt");
        fs::write(&target, b"base").expect("seed target");
        let verified = fs::metadata(&target).expect("stat");
        let guard = HashGuard {
            expected: hash_hex_of_bytes(b"base"),
            verified_size_mtime: Some((verified.len(), verified.modified().expect("mtime"))),
            verified_node: file_node(&verified),
        };
        let ours = cloud.join("our-upload.tmp");
        fs::write(&ours, b"our edit").expect("stage ours");

        let copy = exchange_into_place(&ours, &target, &guard, provider.caps.as_ref())
            .ok()
            .expect("the commit lands");

        assert_eq!(copy, None);
        assert_eq!(fs::read(&target).expect("target"), b"our edit");
        assert_eq!(
            fs::read_dir(&cloud).expect("list").count(),
            1,
            "only the target is left"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn uploads_of_one_file_at_the_same_moment_lose_no_version() {
        // Every writer planned against the same base. Whatever the
        // order of their commits, each version ends under the name, as
        // a copy next to it, or with its writer, who was refused and
        // still has the file.
        const WRITERS: usize = 8;
        for _round in 0..20 {
            let dir = tempfile::TempDir::new().expect("temp dir");
            let cloud = dir.path().join("cloud");
            let provider = Arc::new(provider_at(&cloud));
            fs::write(cloud.join("doc.txt"), b"base").expect("seed target");
            let barrier = Arc::new(std::sync::Barrier::new(WRITERS));

            let writers: Vec<_> = (0..WRITERS)
                .map(|writer| {
                    let provider = provider.clone();
                    let barrier = barrier.clone();
                    let source = dir.path().join(format!("source-{writer}.txt"));
                    fs::write(&source, format!("edit of writer {writer}")).expect("seed source");
                    std::thread::spawn(move || {
                        let mut session = provider
                            .begin_upload(UploadRequest {
                                local_source: source,
                                remote_path: RemotePath::new("doc.txt").expect("remote path"),
                                op_id: format!("op-{writer}"),
                                precondition: RemotePrecondition::HashEquals(hash_hex_of_bytes(
                                    b"base",
                                )),
                            })
                            .ok()?;
                        // The first step verifies the base; the commit
                        // is in the last one.
                        session.step(64 * 1024).ok()?;
                        barrier.wait();
                        loop {
                            match session.step(64 * 1024) {
                                Ok(TransferStep::Progressed { .. }) => continue,
                                Ok(TransferStep::Completed(_)) => return Some(writer),
                                Err(error) => {
                                    assert_eq!(
                                        error.kind,
                                        vapor_shared::ProviderErrorKind::PreconditionFailed,
                                        "{}",
                                        error.message
                                    );
                                    return None;
                                }
                            }
                        }
                    })
                })
                .collect();
            let committed: Vec<usize> = writers
                .into_iter()
                .filter_map(|writer| writer.join().expect("writer thread"))
                .collect();
            assert!(!committed.is_empty(), "one writer at least commits");

            let in_the_cloud: std::collections::BTreeSet<String> = fs::read_dir(&cloud)
                .expect("list")
                .flatten()
                .map(|entry| fs::read_to_string(entry.path()).expect("read"))
                .collect();
            for writer in committed {
                assert!(
                    in_the_cloud.contains(&format!("edit of writer {writer}")),
                    "the upload of writer {writer} was reported as done and is gone: {in_the_cloud:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_move_whose_source_cannot_be_read_is_not_a_source_that_is_gone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::create_dir_all(cloud.join("docs")).expect("folder");
        fs::write(cloud.join("docs/report.txt"), b"quarterly").expect("seed");
        let (from, to) = (
            RemotePath::new("docs/report.txt").expect("path"),
            RemotePath::new("docs/Report.txt").expect("path"),
        );

        let readable = fs::metadata(cloud.join("docs"))
            .expect("stat")
            .permissions();
        fs::set_permissions(cloud.join("docs"), fs::Permissions::from_mode(0o000))
            .expect("lock the folder");
        let refused = provider.move_object(&from, &to, "op-move");
        fs::set_permissions(cloud.join("docs"), readable).expect("unlock");

        let error = refused.expect_err("the folder cannot be read");
        assert_eq!(
            error.kind,
            vapor_shared::ProviderErrorKind::Transient,
            "{}",
            error.message
        );
        // A source that is gone is still said to be gone.
        let gone = provider
            .move_object(
                &RemotePath::new("docs/none.txt").expect("path"),
                &to,
                "op-move",
            )
            .expect_err("no such file");
        assert_eq!(gone.kind, vapor_shared::ProviderErrorKind::NotFound);
    }

    #[test]
    fn stale_staging_temp_files_are_reaped_but_in_flight_ones_survive() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let stale = dir
            .path()
            .join(format!("{}dl-old", constants::provider::TEMP_FILE_PREFIX));
        let fresh = dir
            .path()
            .join(format!("{}dl-new", constants::provider::TEMP_FILE_PREFIX));
        let user_file = dir.path().join("keep.txt");
        std::fs::write(&stale, b"orphan").expect("seed stale");
        std::fs::write(&fresh, b"in flight").expect("seed fresh");
        std::fs::write(&user_file, b"user data").expect("seed user file");

        let now = SystemTime::now();
        let stale_age = now
            + std::time::Duration::from_millis(
                constants::provider::STALE_TEMP_FILE_MAX_AGE_MILLIS + 60_000,
            );

        // The stale temp is older than the threshold relative to `stale_age`.
        reap_if_stale_temp_file(&stale, "dl-old", now); // name lacks prefix here
        assert!(stale.exists(), "guard only reaps prefixed names");

        reap_if_stale_temp_file(
            &stale,
            &format!("{}dl-old", constants::provider::TEMP_FILE_PREFIX),
            stale_age,
        );
        assert!(!stale.exists(), "an aged staging temp must be reaped");

        reap_if_stale_temp_file(
            &fresh,
            &format!("{}dl-new", constants::provider::TEMP_FILE_PREFIX),
            now,
        );
        assert!(fresh.exists(), "a young in-flight temp must survive");

        // A real user file is never a candidate (no reserved prefix).
        reap_if_stale_temp_file(&user_file, "keep.txt", stale_age);
        assert!(user_file.exists());
    }

    #[test]
    fn download_streams_payload_and_hash() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::create_dir_all(cloud.join("docs")).expect("dirs");
        fs::write(cloud.join("docs/data.bin"), vec![7_u8; 200_000]).expect("seed remote");
        let destination = dir.path().join("staging/data.bin");

        let session = provider
            .begin_download(DownloadRequest {
                remote_path: RemotePath::new("docs/data.bin").expect("remote path"),
                destination: destination.clone(),
            })
            .expect("download session");
        let outcome = drive_to_completion(session);

        assert_eq!(outcome.bytes_total, 200_000);
        assert_eq!(
            fs::read(&destination).expect("downloaded payload"),
            vec![7_u8; 200_000]
        );
        assert_eq!(
            outcome.content_hash,
            hash_hex_of_bytes(&vec![7_u8; 200_000])
        );
    }

    #[test]
    fn download_of_missing_remote_reports_not_found() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let provider = provider_at(&dir.path().join("cloud"));
        let result = provider.begin_download(DownloadRequest {
            remote_path: RemotePath::new("missing.txt").expect("remote path"),
            destination: dir.path().join("staging/missing.txt"),
        });
        let error = match result {
            Ok(_) => panic!("missing remote must fail"),
            Err(error) => error,
        };
        assert_eq!(error.kind, vapor_shared::ProviderErrorKind::NotFound);
    }

    #[test]
    fn delete_removes_payload_and_reports_not_found_when_gone() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::write(cloud.join("a.txt"), b"x").expect("seed");

        provider
            .delete(&RemotePath::new("a.txt").expect("path"), "op-5")
            .expect("delete succeeds");
        assert!(!cloud.join("a.txt").exists());

        let error = provider
            .delete(&RemotePath::new("a.txt").expect("path"), "op-6")
            .expect_err("second delete reports not found");
        assert_eq!(error.kind, vapor_shared::ProviderErrorKind::NotFound);
    }

    #[test]
    fn deleting_a_directory_reaps_only_vapor_internal_leftovers() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        // A crash mid-upload left a staging temp; a deleted file left its
        // side-file. Neither shows in listings, so the engine cannot
        // delete them, and neither is user content.
        fs::create_dir_all(cloud.join("gone")).expect("dirs");
        fs::write(cloud.join("gone/.vapor-tmp-orphan"), b"partial").expect("temp");
        fs::write(cloud.join("gone/old.txt.vapor-meta.json"), b"{}").expect("side");
        provider
            .delete(&RemotePath::new("gone").expect("path"), "op-1")
            .expect("an empty-but-for-internals directory deletes");
        assert!(!cloud.join("gone").exists());

        // User content is never reaped.
        fs::create_dir_all(cloud.join("kept")).expect("dirs");
        fs::write(cloud.join("kept/.vapor-tmp-orphan"), b"partial").expect("temp");
        fs::write(cloud.join("kept/user.txt"), b"mine").expect("user");
        let error = provider
            .delete(&RemotePath::new("kept").expect("path"), "op-2")
            .expect_err("a directory with user content stays");
        assert_eq!(error.kind, vapor_shared::ProviderErrorKind::Transient);
        assert!(cloud.join("kept/user.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_listing_reports_the_names_that_cannot_be_a_sync_path() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::create_dir_all(cloud.join("docs")).expect("dirs");
        fs::write(cloud.join("docs/fine.txt"), b"a").expect("seed");
        fs::write(cloud.join("docs/back\\slash.txt"), b"b").expect("seed");

        let docs = RemotePath::new("docs").expect("path");
        // It says so at every listing: the engine asks while the name
        // is there and stops when it is gone.
        for _ in 0..2 {
            let listing = provider.enumerate_with_unsyncable(&docs).expect("listing");
            let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
            assert_eq!(names, vec!["docs/fine.txt"], "it is no entry");
            assert_eq!(listing.unsyncable.len(), 1);
            assert_eq!(listing.unsyncable[0].name, "back\\slash.txt");
            assert!(listing.unsyncable[0].reason.contains("backslash"));
        }
        assert_eq!(provider.enumerate(&docs).expect("entries").len(), 1);

        fs::rename(
            cloud.join("docs/back\\slash.txt"),
            cloud.join("docs/slash.txt"),
        )
        .expect("rename");
        let listing = provider.enumerate_with_unsyncable(&docs).expect("listing");
        assert!(listing.unsyncable.is_empty());
        assert_eq!(listing.entries.len(), 2);
    }

    #[test]
    fn enumerate_lists_files_and_directories_but_hides_internal_files() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::create_dir_all(cloud.join("sub")).expect("dirs");
        fs::write(cloud.join("a.txt"), b"a").expect("seed");
        fs::write(cloud.join(".vapor-tmp-inflight"), b"tmp").expect("seed temp");
        fs::write(cloud.join("a.txt.vapor-meta.json"), b"{}").expect("seed side-file");

        let entries = provider.enumerate(&RemotePath::root()).expect("enumerate");
        let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "sub"]);
        assert_eq!(entries[0].kind, RemoteEntryKind::File);
        assert_eq!(entries[1].kind, RemoteEntryKind::Directory);
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_in_the_cloud_folder_is_not_an_object() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::write(cloud.join("a.txt"), b"a").expect("seed");
        let status = std::process::Command::new("mkfifo")
            .arg(cloud.join("pipe"))
            .status()
            .expect("mkfifo");
        assert!(status.success(), "mkfifo must succeed");
        let pipe = RemotePath::new("pipe").expect("path");

        let entries = provider.enumerate(&RemotePath::root()).expect("enumerate");
        let names: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(names, vec!["a.txt"], "the listing skips the pipe");
        assert_eq!(provider.stat(&pipe).expect("stat"), None);
        // Neither call opens the pipe: an open would wait for a writer
        // and never return.
        let download = provider.begin_download(DownloadRequest {
            remote_path: pipe.clone(),
            destination: dir.path().join("staging"),
        });
        assert_eq!(
            download.err().map(|error| error.kind),
            Some(vapor_shared::ProviderErrorKind::NotFound)
        );
        assert_eq!(
            provider.content_hash(&pipe).err().map(|error| error.kind),
            Some(vapor_shared::ProviderErrorKind::NotFound)
        );
    }

    // Symlink scope tests are Unix-only: creating symlinks on Windows
    // requires elevation, and the escape vector under test is a Unix
    // filesystem shape.
    #[cfg(unix)]
    #[test]
    fn scope_enforcement_rejects_symlink_escape() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).expect("outside dir");
        fs::write(outside.join("secret.txt"), b"secret").expect("seed outside");
        let provider = provider_at(&cloud);

        std::os::unix::fs::symlink(&outside, cloud.join("escape")).expect("symlink");
        let error = provider
            .stat(&RemotePath::new("escape/secret.txt").expect("path"))
            .expect_err("symlink escape must be rejected");
        assert_eq!(error.kind, vapor_shared::ProviderErrorKind::Permanent);
        assert!(error.message.contains("escapes"));
    }

    #[cfg(unix)]
    #[test]
    fn stat_treats_symlinks_inside_root_as_invisible() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::write(cloud.join("real.txt"), b"real").expect("seed");

        {
            std::os::unix::fs::symlink(cloud.join("real.txt"), cloud.join("link.txt"))
                .expect("symlink");
            assert_eq!(
                provider
                    .stat(&RemotePath::new("link.txt").expect("path"))
                    .expect("stat"),
                None,
                "symlinks are invisible to the provider"
            );
        }
    }

    #[test]
    fn manual_feed_provider_round_trips_operations() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        fs::create_dir_all(&cloud).expect("cloud root");
        let caps = Arc::new(InMemoryFilesystemCapabilities::new(
            true,
            CaseSensitivity::Insensitive,
        ));
        let (provider, _handle) =
            FilesystemProvider::with_manual_feed(&cloud, caps).expect("manual provider");
        assert_eq!(provider.name(), "filesystem");
        assert!(provider.capabilities().supports_remote_changes_feed);
    }

    #[test]
    fn another_spelling_of_a_name_is_another_object() {
        let dir = tempfile::TempDir::new().expect("dir");
        let provider = provider_at(dir.path());
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        fs::write(root.join("Report.txt"), b"payload").expect("seed");
        if fs::symlink_metadata(root.join("report.txt")).is_err() {
            // This volume keeps the two spellings apart by itself.
            return;
        }
        let other = RemotePath::new("report.txt").expect("path");

        assert_eq!(provider.stat(&other).expect("stat"), None);
        assert_eq!(
            provider.content_hash(&other).expect_err("hash").kind,
            vapor_shared::ProviderErrorKind::NotFound
        );
        assert_eq!(
            provider.delete(&other, "op").expect_err("delete").kind,
            vapor_shared::ProviderErrorKind::NotFound
        );
        assert!(root.join("Report.txt").exists(), "the stored object stays");
        let download = provider.begin_download(DownloadRequest {
            remote_path: other.clone(),
            destination: dir.path().join("out"),
        });
        assert_eq!(
            download.err().expect("download").kind,
            vapor_shared::ProviderErrorKind::NotFound
        );
    }

    #[test]
    fn an_entry_that_goes_while_its_path_is_resolved_is_looked_for_again() {
        let dir = tempfile::TempDir::new().expect("dir");
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        let file = root.join("a.txt");
        fs::write(&file, b"x").expect("seed");
        let remote = RemotePath::new("a.txt").expect("path");

        // The entry is gone for the first two looks, as in the middle
        // of a rename, and there for the third.
        let looks = std::cell::Cell::new(0);
        let flaky = |path: &Path| {
            looks.set(looks.get() + 1);
            if looks.get() <= 2 {
                Err(io::Error::from(io::ErrorKind::NotFound))
            } else {
                vapor_shared::paths::canonicalize(path)
            }
        };
        assert_eq!(
            canonical_existing_ancestor(&file, &flaky).expect("found"),
            file
        );

        // It never comes back: that is a change under the daemon, to
        // try again later, and never a permanent failure of the path.
        let gone = |_: &Path| Err(io::Error::from(io::ErrorKind::NotFound));
        let error = canonical_existing_ancestor(&file, &gone).expect_err("gone");
        assert_eq!(
            scope_lookup_failure(&root, &remote, &error).kind,
            vapor_shared::ProviderErrorKind::Transient
        );
        // Another failure cannot place the path inside the root.
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            scope_lookup_failure(&root, &remote, &denied).kind,
            vapor_shared::ProviderErrorKind::Permanent
        );
        // A root that is gone is its own state, whatever the error.
        assert_eq!(
            scope_lookup_failure(&root.join("no-root"), &remote, &error).kind,
            vapor_shared::ProviderErrorKind::CloudRootUnavailable
        );
    }

    #[test]
    fn an_upload_waits_while_its_folder_is_stored_under_another_spelling() {
        let dir = tempfile::TempDir::new().expect("dir");
        let provider = provider_at(dir.path());
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        fs::create_dir(root.join("Docs")).expect("dir");
        if fs::symlink_metadata(root.join("docs")).is_err() {
            // This volume keeps the two spellings apart by itself.
            return;
        }
        let source = dir.path().join("source.bin");
        fs::write(&source, b"payload").expect("source");
        let upload = |remote: &str| {
            provider.begin_upload(UploadRequest {
                local_source: source.clone(),
                remote_path: RemotePath::new(remote).expect("path"),
                op_id: "op".to_string(),
                precondition: RemotePrecondition::None,
            })
        };

        // Written through, the file would land in `Docs/`, and `docs/a`
        // would then read as absent.
        for remote in ["docs/a.txt", "docs/new/deeper/a.txt"] {
            let refused = upload(remote).err().expect("the upload waits");
            assert_eq!(
                refused.kind,
                vapor_shared::ProviderErrorKind::Transient,
                "{remote}"
            );
        }
        assert!(upload("Docs/a.txt").is_ok(), "the stored spelling uploads");
        assert!(upload("Docs/new/a.txt").is_ok());
    }

    #[test]
    fn a_move_can_change_only_the_spelling_of_a_file_or_of_its_folder() {
        let dir = tempfile::TempDir::new().expect("dir");
        let provider = provider_at(dir.path());
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        fs::create_dir(root.join("photos")).expect("dir");
        fs::write(root.join("photos/report.txt"), b"payload").expect("seed");
        let names = |directory: &Path| -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(directory)
                .expect("list")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                // A host without xattr keeps the op-id tag in a side-file.
                .filter(|name| !name.ends_with(".vapor-meta.json"))
                .collect();
            names.sort();
            names
        };

        provider
            .move_object(
                &RemotePath::new("photos/report.txt").expect("path"),
                &RemotePath::new("photos/Report.txt").expect("path"),
                "op-1",
            )
            .expect("a change of spelling is a move");
        assert_eq!(names(&root.join("photos")), vec!["Report.txt"]);

        provider
            .move_object(
                &RemotePath::new("photos/Report.txt").expect("path"),
                &RemotePath::new("Photos/Report.txt").expect("path"),
                "op-2",
            )
            .expect("a move into the folder's new spelling");
        if fs::symlink_metadata(root.join("PHOTOS")).is_ok() {
            // A volume that folds names holds one folder; it took the
            // new spelling.
            assert_eq!(names(&root), vec!["Photos"]);
        }
        assert_eq!(
            fs::read(root.join("Photos/Report.txt")).expect("moved"),
            b"payload"
        );
    }

    #[test]
    fn content_hash_matches_streaming_hash() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::write(cloud.join("h.txt"), b"hash me").expect("seed");
        assert_eq!(
            provider
                .content_hash(&RemotePath::new("h.txt").expect("path"))
                .expect("hash"),
            hash_hex_of_bytes(b"hash me")
        );
    }
}
