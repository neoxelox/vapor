//! Filesystem reference provider (C8-3, C8-4, C8-8).
//!
//! A full [`Provider`] implementation backed by a local directory that
//! plays the role of the cloud side. When `provider = "filesystem"`,
//! `cloudSyncDirectory` is reinterpreted as an absolute local path (C8-2).
//! The provider is the reference implementation the engine's
//! bidirectional pipeline is validated against before any real cloud
//! backend (Google Drive, C8-48+) goes live, and the backend the
//! provider contract suite runs on.
//!
//! Safety properties:
//! - **Atomic writes**: uploads land in a hidden temp file in the target
//!   directory and are renamed into place after the payload and the
//!   op-id tag are complete.
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
pub fn hash_hex_of_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

/// Streaming SHA-256 of a file's current content.
pub fn hash_hex_of_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Whether `name` is one of the provider's hidden internal files
/// (op-id side-files and in-flight temp files).
pub fn is_internal_file_name(name: &str) -> bool {
    name.starts_with(constants::provider::TEMP_FILE_PREFIX)
        || name.ends_with(constants::provider::OP_ID_SIDE_FILE_SUFFIX)
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
}

impl FilesystemProvider {
    /// A provider that resolves its root when the engine calls
    /// [`Provider::ensure_cloud_sync_directory`] and watches it natively
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
        provider.ensure_cloud_sync_directory(&root.to_string_lossy())?;
        let handle = provider.feed.manual_handle();
        Ok((provider, handle))
    }

    /// Convenience constructor for tests and the daemon bring-up path:
    /// ensures `root` immediately with native capabilities.
    pub fn with_root(root: &Path) -> Result<Self, ProviderError> {
        let provider = Self::new();
        provider.ensure_cloud_sync_directory(&root.to_string_lossy())?;
        Ok(provider)
    }

    fn canonical_root(&self) -> Result<PathBuf, ProviderError> {
        self.root
            .lock()
            .expect("filesystem provider root mutex poisoned")
            .clone()
            .ok_or_else(|| {
                ProviderError::permanent(
                    "filesystem provider has no ensured cloud sync directory yet; \
                     configuration must be validated before sync work starts",
                )
            })
    }

    /// Resolves a remote path under the canonical root and enforces
    /// scope: the deepest existing ancestor must canonicalize inside
    /// the root and live on the same device (C8-3).
    fn resolve_in_scope(&self, remote: &RemotePath) -> Result<PathBuf, ProviderError> {
        let root = self.canonical_root()?;
        let resolved = remote.resolve_under(&root);

        let anchor = deepest_existing_ancestor(&resolved);
        let canonical_anchor = anchor.canonicalize().map_err(|error| {
            ProviderError::transient(format!(
                "cannot canonicalize {} while resolving {remote}: {error}",
                anchor.display()
            ))
        })?;
        if !canonical_anchor.starts_with(&root) {
            return Err(ProviderError::permanent(format!(
                "remote path {remote} escapes the cloud sync root (resolves through {})",
                canonical_anchor.display()
            )));
        }
        enforce_same_device(&root, &canonical_anchor, remote)?;
        Ok(resolved)
    }

    fn remote_entry_for(
        &self,
        remote: &RemotePath,
        resolved: &Path,
    ) -> Result<Option<RemoteEntry>, ProviderError> {
        let metadata = match fs::symlink_metadata(resolved) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
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
        // Capability honesty (C8-43): the native changes feed rides the
        // platform fs-watcher; on hosts where that is still a stub the
        // feed is unadvertised and the engine falls back to reconcile
        // enumeration. Manual feeds (tests) are host-independent.
        if matches!(self.feed_mode, FeedMode::Native)
            && !vapor_platform::fs_watch::native_watcher_available()
        {
            capabilities.supports_remote_changes_feed = false;
        }
        capabilities
    }

    fn content_hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::Sha256
    }

    fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) -> Result<(), ProviderError> {
        let expanded = expand_cloud_directory(cloud_sync_directory)?;
        if let Err(error) = fs::create_dir_all(&expanded) {
            return Err(ProviderError::permanent(format!(
                "cannot create filesystem cloud sync directory {}: {error}. \
                 Set `cloudSyncDirectory` to a writable absolute path when \
                 `provider` is \"filesystem\"",
                expanded.display()
            )));
        }
        let canonical = expanded.canonicalize().map_err(|error| {
            ProviderError::permanent(format!(
                "cannot canonicalize filesystem cloud sync directory {}: {error}",
                expanded.display()
            ))
        })?;
        crate::logging::info(
            "Filesystem provider ensured cloud sync directory",
            &[("path", canonical.display().to_string())],
        );
        *self
            .root
            .lock()
            .expect("filesystem provider root mutex poisoned") = Some(canonical);
        Ok(())
    }

    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
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
                continue;
            };
            if is_internal_file_name(name) {
                continue;
            }
            let Ok(child) = directory.join(name) else {
                continue;
            };
            if let Some(entry) = self.remote_entry_for(&child, &dir_entry.path())? {
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    fn stat(&self, path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_in_scope(path)?;
        self.remote_entry_for(path, &resolved)
    }

    fn content_hash(&self, path: &RemotePath) -> Result<String, ProviderError> {
        let resolved = self.resolve_in_scope(path)?;
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
        fs::create_dir_all(parent).map_err(|error| {
            ProviderError::transient(format!(
                "cannot create remote parent directory for {}: {error}",
                request.remote_path
            ))
        })?;
        let temp_path = parent.join(temp_file_name(&request.op_id));
        let temp = fs::File::create(&temp_path).map_err(|error| {
            ProviderError::transient(format!(
                "cannot create temp file for {}: {error}",
                request.remote_path
            ))
        })?;

        Ok(Box::new(FilesystemUploadSession {
            source,
            temp: Some(temp),
            temp_path,
            target,
            remote_path: request.remote_path,
            op_id: request.op_id,
            precondition: request.precondition,
            caps: self.caps.clone(),
            tags: self.tags.clone(),
            hasher: Sha256::new(),
            bytes_total: 0,
            finished: false,
        }))
    }

    fn begin_download(
        &self,
        request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let resolved = self.resolve_in_scope(&request.remote_path)?;
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

        Ok(Box::new(FilesystemDownloadSession {
            source,
            destination: Some(destination),
            destination_path: request.destination,
            remote_path: request.remote_path,
            hasher: Sha256::new(),
            bytes_total: 0,
            finished: false,
        }))
    }

    fn delete(&self, path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_in_scope(path)?;
        match fs::remove_file(&resolved) {
            Ok(()) => {
                let _ = self.tags.remove(&resolved);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(ProviderError::not_found(
                format!("remote file {path} was already gone"),
            )),
            Err(error) if resolved.is_dir() => {
                // Directories vanish when their last child is removed on
                // the engine side; explicit removal keeps mirrors exact.
                fs::remove_dir_all(&resolved).map_err(|dir_error| {
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

    fn rename(&self, from: &RemotePath, to: &RemotePath, op_id: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_in_scope(from)?;
        let resolved_to = self.resolve_in_scope(to)?;
        if !resolved_from.exists() {
            return Err(ProviderError::not_found(format!(
                "rename source {from} does not exist"
            )));
        }
        if let Some(parent) = resolved_to.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot create rename destination directory for {to}: {error}"
                ))
            })?;
        }
        fs::rename(&resolved_from, &resolved_to).map_err(|error| {
            ProviderError::transient(format!("cannot rename {from} to {to}: {error}"))
        })?;
        self.tags
            .relocate_side_file(&resolved_from, &resolved_to)
            .map_err(|error| {
                ProviderError::transient(format!(
                    "renamed {from} to {to} but could not relocate its tag side-file: {error}"
                ))
            })?;
        let _ = self.tags.write_op_id(&resolved_to, op_id);
        Ok(())
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
/// filesystem provider (C8-2 reinterpretation): `~`-prefixed values
/// expand against the home directory; everything else must already be
/// absolute.
fn expand_cloud_directory(raw: &str) -> Result<PathBuf, ProviderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::permanent(
            "cloudSyncDirectory is empty; the filesystem provider needs an absolute local path",
        ));
    }
    if trimmed == "~" || trimmed.starts_with("~/") {
        let home = vapor_shared::runtime_paths::home_directory().ok_or_else(|| {
            ProviderError::permanent(
                "cannot expand ~ in cloudSyncDirectory: no home directory resolved",
            )
        })?;
        return Ok(match trimmed.strip_prefix("~/") {
            Some(suffix) => home.join(suffix),
            None => home,
        });
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(ProviderError::permanent(format!(
            "cloudSyncDirectory {trimmed} must be an absolute path when `provider` is \"filesystem\""
        )));
    }
    Ok(path)
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

struct FilesystemUploadSession {
    source: fs::File,
    temp: Option<fs::File>,
    temp_path: PathBuf,
    target: PathBuf,
    remote_path: RemotePath,
    op_id: String,
    precondition: RemotePrecondition,
    caps: Arc<dyn FilesystemCapabilities>,
    tags: OpIdTagStore,
    hasher: Sha256,
    bytes_total: u64,
    finished: bool,
}

impl FilesystemUploadSession {
    fn finalize(&mut self) -> Result<TransferOutcome, ProviderError> {
        self.check_precondition()?;

        let temp = self.temp.take().expect("finalize called with live temp");
        temp.sync_all().map_err(|error| {
            ProviderError::transient(format!(
                "cannot sync temp file for {}: {error}",
                self.remote_path
            ))
        })?;
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

        fs::rename(&self.temp_path, &self.target).map_err(|error| {
            ProviderError::transient(format!(
                "cannot move upload into place at {}: {error}",
                self.remote_path
            ))
        })?;
        if needs_side_file {
            self.tags
                .write_op_id(&self.target, &self.op_id)
                .map_err(|error| {
                    ProviderError::transient(format!(
                        "uploaded {} but could not record its op-id tag: {error}",
                        self.remote_path
                    ))
                })?;
        }

        self.finished = true;
        Ok(TransferOutcome {
            bytes_total: self.bytes_total,
            content_hash: hex_encode(&std::mem::take(&mut self.hasher).finalize()),
        })
    }

    fn check_precondition(&self) -> Result<(), ProviderError> {
        match &self.precondition {
            RemotePrecondition::None => Ok(()),
            RemotePrecondition::Absent => {
                if self.target.exists() {
                    Err(ProviderError::precondition_failed(format!(
                        "upload target {} already exists",
                        self.remote_path
                    )))
                } else {
                    Ok(())
                }
            }
            RemotePrecondition::HashEquals(expected) => {
                let current = hash_hex_of_file(&self.target).map_err(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        ProviderError::precondition_failed(format!(
                            "upload target {} vanished while guarded by a hash precondition",
                            self.remote_path
                        ))
                    } else {
                        ProviderError::transient(format!(
                            "cannot verify precondition hash for {}: {error}",
                            self.remote_path
                        ))
                    }
                })?;
                if &current == expected {
                    Ok(())
                } else {
                    Err(ProviderError::precondition_failed(format!(
                        "upload target {} changed since planning",
                        self.remote_path
                    )))
                }
            }
        }
    }
}

impl TransferSession for FilesystemUploadSession {
    fn step(&mut self, max_bytes: u64) -> Result<TransferStep, ProviderError> {
        debug_assert!(!self.finished, "step called after completion");
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
            temp.write_all(&buffer[..read]).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot write temp file for {}: {error}",
                    self.remote_path
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
    finished: bool,
}

impl TransferSession for FilesystemDownloadSession {
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
                destination.sync_all().map_err(|error| {
                    ProviderError::transient(format!(
                        "cannot sync downloaded payload for {}: {error}",
                        self.remote_path
                    ))
                })?;
                self.finished = true;
                return Ok(TransferStep::Completed(TransferOutcome {
                    bytes_total: self.bytes_total,
                    content_hash: hex_encode(&std::mem::take(&mut self.hasher).finalize()),
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

    #[test]
    fn ensure_creates_missing_cloud_root() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path().join("cloud/Vapor");
        let provider = FilesystemProvider::new();
        provider
            .ensure_cloud_sync_directory(&root.to_string_lossy())
            .expect("ensure creates root");
        assert!(root.is_dir());
    }

    #[test]
    fn ensure_rejects_relative_and_empty_paths_with_actionable_error() {
        let provider = FilesystemProvider::new();
        let error = provider
            .ensure_cloud_sync_directory("relative/cloud")
            .expect_err("relative must be rejected");
        assert!(error.message.contains("absolute path"));
        assert!(
            provider.ensure_cloud_sync_directory("  ").is_err(),
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
    fn upload_respects_absent_precondition() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::create_dir_all(cloud.join("docs")).expect("dirs");
        fs::write(cloud.join("docs/hello.txt"), b"pre-existing").expect("seed target");
        let source = dir.path().join("source.txt");
        fs::write(&source, b"new content").expect("seed source");

        let session = provider
            .begin_upload(UploadRequest {
                local_source: source,
                remote_path: RemotePath::new("docs/hello.txt").expect("remote path"),
                op_id: "op-upload-2".to_string(),
                precondition: RemotePrecondition::Absent,
            })
            .expect("upload session");
        let mut session = session;
        let error = loop {
            match session.step(8 * 1024) {
                Ok(TransferStep::Progressed { .. }) => continue,
                Ok(TransferStep::Completed(_)) => panic!("must fail the precondition"),
                Err(error) => break error,
            }
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
    fn rename_moves_payload_and_retags() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cloud = dir.path().join("cloud");
        let provider = provider_at(&cloud);
        fs::write(cloud.join("old.txt"), b"content").expect("seed");

        provider
            .rename(
                &RemotePath::new("old.txt").expect("path"),
                &RemotePath::new("nested/new.txt").expect("path"),
                "op-7",
            )
            .expect("rename succeeds");
        assert!(!cloud.join("old.txt").exists());
        assert_eq!(
            fs::read(cloud.join("nested/new.txt")).expect("moved payload"),
            b"content"
        );
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
