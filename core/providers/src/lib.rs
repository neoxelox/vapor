#![forbid(unsafe_code)]

//! Provider trait surface + provider-neutral types.
//!
//! The engine talks to every cloud backend through [`Provider`]. The
//! trait is deliberately synchronous and chunk-oriented: long transfers
//! are performed through [`TransferSession`]s that the staged executor
//! steps under slice budgets, so throttle transitions and shutdown
//! requests interrupt work at bounded checkpoints instead of waiting for
//! whole files (`AGENTS.md §3`).

use std::error::Error;
use std::fmt::{self, Display};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use vapor_shared::{ProviderErrorKind, ThrottleState};

pub mod bandwidth;
pub mod filesystem;
pub mod gdrive;
pub mod http;
pub mod logging;
mod paths;
pub mod tags;

pub use bandwidth::BandwidthShaper;
pub use paths::{RemotePath, RemotePathError};

/// Typed provider failure carrying the provider-neutral taxonomy from
/// `core/shared`. The engine maps `kind.retry_classification`
/// onto the retry policy and keeps the full kind for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Transient, message)
    }

    pub fn rate_limited(retry_after: Option<Duration>, message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::RateLimited { retry_after }, message)
    }

    pub fn authentication(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Authentication, message)
    }

    pub fn precondition_failed(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::PreconditionFailed, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::NotFound, message)
    }

    pub fn cloud_root_unavailable(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::CloudRootUnavailable, message)
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Permanent, message)
    }
}

impl Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} provider failure: {}",
            self.kind.label(),
            self.message
        )
    }
}

impl Error for ProviderError {}

/// Content-hash algorithm a provider reports and compares with. The
/// engine's hash stage must produce hashes with the provider's
/// algorithm so local/remote comparisons and the self-write-cache hash
/// fallback are meaningful.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HashAlgorithm {
    #[default]
    Sha256,
    /// Google Drive reports MD5 checksums in file metadata.
    Md5,
}

/// Finalized provider capability model. Capabilities gate
/// engine behavior; a provider must never advertise a capability its
/// implementation does not honor (enforced by the contract suite in
/// `tests/provider_contract.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Provider can produce an incremental remote-changes feed via
    /// [`Provider::poll_changes`]. Without it the engine falls back to
    /// periodic reconcile enumeration.
    pub supports_remote_changes_feed: bool,
    /// Provider can rename a remote object in place without a
    /// delete + re-upload round trip.
    pub supports_server_side_rename: bool,
    /// Provider honors [`RemotePrecondition`] guards on uploads.
    pub supports_write_preconditions: bool,
    /// Provider persists the engine's op-id tag on remote objects and
    /// echoes it back through entries and changes (loop prevention's
    /// primary correlator).
    pub supports_op_id_tags: bool,
    /// Provider reports content hashes in enumeration / changes
    /// metadata without a separate expensive call.
    pub supports_content_hashes_in_metadata: bool,
}

impl ProviderCapabilities {
    pub const FILESYSTEM: Self = Self {
        supports_remote_changes_feed: true,
        supports_server_side_rename: true,
        supports_write_preconditions: true,
        supports_op_id_tags: true,
        supports_content_hashes_in_metadata: false,
    };

    pub const GDRIVE_MVP: Self = Self {
        supports_remote_changes_feed: true,
        supports_server_side_rename: true,
        supports_write_preconditions: true,
        supports_op_id_tags: true,
        supports_content_hashes_in_metadata: true,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteEntryKind {
    File,
    Directory,
}

/// One remote object as reported by enumeration or stat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub path: RemotePath,
    pub kind: RemoteEntryKind,
    pub size_bytes: u64,
    pub modified_at: SystemTime,
    /// Present when the provider reports hashes in metadata
    /// (`supports_content_hashes_in_metadata`); otherwise fetch via
    /// [`Provider::content_hash`].
    pub content_hash: Option<String>,
    /// The engine op-id tag if this object was written by Vapor and the
    /// provider supports tags.
    pub op_id: Option<String>,
}

/// Write guard for uploads (deterministic race resolution). A
/// failed guard surfaces as [`ProviderErrorKind::PreconditionFailed`],
/// which the engine treats as "re-plan against fresh remote state".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemotePrecondition {
    /// No guard: last write wins at the provider level.
    None,
    /// Target must not exist (fresh create).
    Absent,
    /// Target's current content hash must equal this value.
    HashEquals(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRequest {
    /// Absolute local file the provider reads from.
    pub local_source: PathBuf,
    /// Destination relative to the cloud sync root.
    pub remote_path: RemotePath,
    /// Opaque operation id the provider must attach to the written
    /// object when `supports_op_id_tags` (loop prevention).
    pub op_id: String,
    pub precondition: RemotePrecondition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRequest {
    /// Source relative to the cloud sync root.
    pub remote_path: RemotePath,
    /// Absolute local path the provider writes the payload to. The
    /// engine owns the atomic move into the sync root afterwards.
    pub destination: PathBuf,
}

/// Progress of one [`TransferSession::step`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferStep {
    Progressed { bytes_transferred: u64 },
    Completed(TransferOutcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferOutcome {
    pub bytes_total: u64,
    /// Hash of the transferred content in the provider's
    /// [`HashAlgorithm`], hex-encoded.
    pub content_hash: String,
}

/// A chunked upload or download in flight. Sessions hold whatever
/// provider-side state resuming needs (temp file, resumable upload URL)
/// and must clean up partial artifacts on [`TransferSession::abort`] or
/// drop.
pub trait TransferSession: Send {
    /// Advances by at most `max_bytes`. Callers keep invoking until
    /// [`TransferStep::Completed`]; a returned error ends the session
    /// (the engine's retry machinery re-plans from scratch).
    fn step(&mut self, max_bytes: u64) -> Result<TransferStep, ProviderError>;

    /// Abandons the transfer and removes partial artifacts. Idempotent.
    fn abort(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteChangeKind {
    CreatedOrModified,
    Removed,
}

/// One entry of the remote changes feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteChange {
    pub path: RemotePath,
    pub kind: RemoteChangeKind,
    pub observed_at: SystemTime,
    /// Engine op-id tag when the changed object carries one (primary
    /// self-write correlator; `None` for removals).
    pub op_id: Option<String>,
    /// Content hash when available in feed metadata (fallback
    /// correlator).
    pub content_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteChangesPage {
    pub changes: Vec<RemoteChange>,
    /// Opaque cursor for the next poll. The engine persists it durably
    /// only after every change in this page reached the durable queue
    /// (cursor advance on durable intent completion).
    pub next_cursor: String,
}

/// Outcome of a changes poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangesPoll {
    Page(RemoteChangesPage),
    /// The supplied cursor is no longer replayable (daemon restart with
    /// an in-memory feed, ring overflow, provider-side expiry). The
    /// caller must schedule a whole-scope reconcile and re-baseline by
    /// polling with `cursor = None`.
    CursorExpired,
}

/// The provider trait every cloud backend implements.
///
/// Path vocabulary: all remote paths are [`RemotePath`]s — relative to
/// the configured cloud sync root, forward-slash separated, no
/// traversal. Scope safety is the provider's responsibility: an
/// implementation must refuse to touch anything outside its root.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> ProviderCapabilities;

    fn content_hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::Sha256
    }

    fn poll_allowed(&self, throttle_state: ThrottleState) -> bool {
        let allowed = !matches!(throttle_state, ThrottleState::Suspended);
        if !allowed {
            logging::debug(
                "Blocked remote polling because throttle state is suspended",
                &[("throttle_state", format!("{:?}", throttle_state))],
            );
        }
        allowed
    }

    /// Ensures the cloud-side sync root exists, creating it when the
    /// backend allows. Required (no silent-Ok default): every
    /// provider must state explicitly whether it can honor this
    /// safety-relevant operation. A failure blocks regular sync work
    /// with an actionable configuration error.
    fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) -> Result<(), ProviderError>;

    /// Lists the immediate children of `directory` (non-recursive, so
    /// reconcile walks stay slice-interruptible). `directory` may be
    /// [`RemotePath::root`].
    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError>;

    /// Metadata for a single remote path; `Ok(None)` when absent.
    fn stat(&self, path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError>;

    /// Content hash of a remote file in the provider's algorithm.
    /// Potentially expensive (full read on filesystem-backed
    /// providers); callers keep it out of hot paths.
    fn content_hash(&self, path: &RemotePath) -> Result<String, ProviderError>;

    fn begin_upload(
        &self,
        request: UploadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError>;

    fn begin_download(
        &self,
        request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError>;

    /// Deletes a remote file. Deleting an absent path reports
    /// [`ProviderErrorKind::NotFound`]; the engine treats that as
    /// convergence, not failure.
    fn delete(&self, path: &RemotePath, op_id: &str) -> Result<(), ProviderError>;

    /// Renames a remote file in place. Only meaningful when
    /// `supports_server_side_rename`.
    fn rename(&self, from: &RemotePath, to: &RemotePath, op_id: &str) -> Result<(), ProviderError>;

    /// Pulls the next page of remote changes after `cursor`.
    /// `cursor = None` baselines the feed: it returns an empty page
    /// whose `next_cursor` marks "now". Only meaningful when
    /// `supports_remote_changes_feed`.
    fn poll_changes(
        &self,
        cursor: Option<&str>,
        max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError>;
}

pub use filesystem::FilesystemProvider;

pub use gdrive::GoogleDriveProvider;

/// Resolves the configured `provider` value onto a provider instance
///. Unknown values are a configuration error the caller must
/// surface — never a silent fallback, because a wrong provider guess
/// could sync into the wrong place.
pub fn select_provider(kind: &str) -> Result<Box<dyn Provider>, ProviderError> {
    select_provider_for_profile(
        kind,
        vapor_shared::constants::provider::DEFAULT_PROFILE_FALLBACK,
    )
}

/// Profile-aware provider selection: `gdrive` is now
/// selectable, constructed against the profile's namespaced
/// credentials. Missing credentials do not fail selection — they
/// surface as an actionable `Authentication` error when the engine
/// ensures the cloud root, which blocks sync until
/// `vapor auth login gdrive` runs.
pub fn select_provider_for_profile(
    kind: &str,
    profile_id: &str,
) -> Result<Box<dyn Provider>, ProviderError> {
    match kind.trim() {
        value if value == vapor_shared::constants::provider::FILESYSTEM => {
            Ok(Box::new(FilesystemProvider::new()))
        }
        value if value == vapor_shared::constants::provider::GDRIVE => {
            Ok(Box::new(GoogleDriveProvider::for_profile(profile_id)?))
        }
        other => Err(ProviderError::permanent(format!(
            "unknown provider '{other}'; accepted values: {}",
            vapor_shared::constants::provider::ALL.join(", ")
        ))),
    }
}

/// Inert stub provider used by engine composition tests and as the
/// explicit "no provider configured" placeholder. Every mutation
/// completes as a successful no-op so pipeline tests can drive intents
/// end-to-end without a real backend; it is not selectable via the
/// `provider` config key and must never ship as a production default
/// beyond the pre-GA bring-up (a later task wires the real
/// [`FilesystemProvider`] as the default).
#[derive(Debug, Default)]
pub struct FilesystemStubProvider;

pub fn default_provider() -> Box<dyn Provider> {
    Box::new(FilesystemStubProvider)
}

impl Provider for FilesystemStubProvider {
    fn name(&self) -> &'static str {
        "filesystem_stub"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_remote_changes_feed: false,
            supports_server_side_rename: false,
            supports_write_preconditions: false,
            supports_op_id_tags: false,
            supports_content_hashes_in_metadata: false,
        }
    }

    fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) -> Result<(), ProviderError> {
        // Inert stub: it has no cloud side, so "ensuring" is a logged
        // no-op by design (not a silent trait default).
        logging::info(
            "Filesystem stub provider treats the cloud sync directory as always present",
            &[("cloud_sync_directory", cloud_sync_directory.to_string())],
        );
        Ok(())
    }

    fn enumerate(&self, _directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
        Ok(Vec::new())
    }

    fn stat(&self, _path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        Ok(None)
    }

    fn content_hash(&self, path: &RemotePath) -> Result<String, ProviderError> {
        Err(ProviderError::not_found(format!(
            "stub provider has no remote object at {path}"
        )))
    }

    fn begin_upload(
        &self,
        _request: UploadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        Ok(Box::new(NoopTransferSession))
    }

    fn begin_download(
        &self,
        request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        Err(ProviderError::not_found(format!(
            "stub provider has no remote object at {}",
            request.remote_path
        )))
    }

    fn delete(&self, _path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    fn rename(
        &self,
        _from: &RemotePath,
        _to: &RemotePath,
        _op_id: &str,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    fn poll_changes(
        &self,
        _cursor: Option<&str>,
        _max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
        Ok(ChangesPoll::Page(RemoteChangesPage {
            changes: Vec::new(),
            next_cursor: "0".to_string(),
        }))
    }
}

/// Upload session of the stub provider: completes immediately without
/// side effects. The zero-length hash keeps outcomes well-formed for
/// composition tests.
struct NoopTransferSession;

impl TransferSession for NoopTransferSession {
    fn step(&mut self, _max_bytes: u64) -> Result<TransferStep, ProviderError> {
        Ok(TransferStep::Completed(TransferOutcome {
            bytes_total: 0,
            content_hash: filesystem::hash_hex_of_bytes(b""),
        }))
    }

    fn abort(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_provider_returns_filesystem_stub_pre_ga() {
        let provider = default_provider();
        assert_eq!(provider.name(), "filesystem_stub");
    }

    #[test]
    fn poll_is_blocked_when_throttle_is_suspended() {
        let provider = FilesystemStubProvider;
        assert!(provider.poll_allowed(ThrottleState::Light));
        assert!(!provider.poll_allowed(ThrottleState::Suspended));
    }

    #[test]
    fn filesystem_stub_reports_no_capabilities() {
        let capabilities = FilesystemStubProvider.capabilities();
        assert!(!capabilities.supports_remote_changes_feed);
        assert!(!capabilities.supports_server_side_rename);
        assert!(!capabilities.supports_op_id_tags);
    }

    #[test]
    fn filesystem_stub_upload_completes_as_noop() {
        let provider = FilesystemStubProvider;
        let mut session = provider
            .begin_upload(UploadRequest {
                local_source: PathBuf::from("/tmp/source.txt"),
                remote_path: RemotePath::new("docs/source.txt").expect("remote path"),
                op_id: "op-1".to_string(),
                precondition: RemotePrecondition::None,
            })
            .expect("stub upload session");
        match session.step(1024).expect("step") {
            TransferStep::Completed(outcome) => assert_eq!(outcome.bytes_total, 0),
            other => panic!("expected immediate completion, got {other:?}"),
        }
    }

    #[test]
    fn provider_error_maps_taxonomy_to_retry_classification() {
        let error = ProviderError::precondition_failed("etag mismatch");
        assert_eq!(
            error.kind.retry_classification(),
            vapor_shared::RetryFailureKind::Transient
        );
        let not_found = ProviderError::not_found("gone");
        assert_eq!(
            not_found.kind.retry_classification(),
            vapor_shared::RetryFailureKind::Permanent
        );
    }
}
