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
pub mod root_marker;
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

    pub fn storage_full(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::StorageFull, message)
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
    /// Provider can move a remote object to another path without a
    /// re-upload ([`Provider::move_object`]); the engine turns a
    /// detected local rename into one call when this is set.
    pub supports_server_side_move: bool,
    /// The changes feed is fed by a watcher on this device and a poll
    /// of it costs no request to a cloud: the engine reads it each
    /// second in every throttle state but `Suspended`, so a change is
    /// seen when it happens. A feed that is a network call is polled at
    /// the cadence of the throttle state.
    pub changes_feed_is_watched: bool,
    /// The commit of a transfer is one flush of a drive that the
    /// transfers of the same moment share, so many small transfers at
    /// a time cost the device about what one costs. The engine then
    /// runs small transfers in a lane of their own, wider than the
    /// transfer slots of the throttle state. A provider that pays for
    /// each transfer on its own (a request to a cloud API, which counts
    /// against a rate limit) leaves this off.
    pub small_transfers_share_a_commit: bool,
}

impl ProviderCapabilities {
    pub const FILESYSTEM: Self = Self {
        supports_remote_changes_feed: true,
        supports_server_side_move: true,
        changes_feed_is_watched: true,
        small_transfers_share_a_commit: true,
    };

    pub const GDRIVE_MVP: Self = Self {
        supports_remote_changes_feed: true,
        supports_server_side_move: true,
        changes_feed_is_watched: false,
        small_transfers_share_a_commit: false,
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
    /// The id that the backend has for this object, when it has ids: a
    /// text that stays with the object when the object gets another
    /// name or another place, and that no other object under the root
    /// has. The engine stores it and never reads it. `None` on a
    /// backend without ids, for every object.
    pub id: Option<String>,
}

/// One id that the engine stored for a synced object, handed back to
/// the provider at a start ([`Provider::restore_remote_ids`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteIdentity {
    /// Where the object was when the engine last saw it.
    pub path: RemotePath,
    pub kind: RemoteEntryKind,
    pub id: String,
}

/// A name in a remote directory that cannot be a sync path, so the
/// object stays in the cloud only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsyncableRemoteName {
    /// The name, as closely as text can show it.
    pub name: String,
    /// Why the sync path model cannot carry it, in words for the user.
    pub reason: String,
}

/// What one listing of a remote directory gives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteListing {
    pub entries: Vec<RemoteEntry>,
    pub unsyncable: Vec<UnsyncableRemoteName>,
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
    /// The remote object's modification time as the provider reports
    /// it once the transfer has landed (for an upload, the time of the
    /// object just written; for a download, the time of the object
    /// read). The sync index records it so a later reconcile can tell
    /// a remote edit that kept the byte count from an untouched
    /// object. `None` when the provider cannot say.
    pub remote_modified_at: Option<SystemTime>,
    /// The id of the remote object that the transfer wrote or read
    /// ([`RemoteEntry::id`]). `None` on a backend without ids.
    pub remote_id: Option<String>,
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

    /// The most bytes the session wants one step to move: the
    /// provider's chunk size, which only the provider can choose well
    /// (a resumable upload has its own granularity and its own view of
    /// the link). The engine checks its gates and the bandwidth bucket
    /// between steps, so this is also how soon a transfer can be held
    /// or cancelled.
    fn step_bytes(&self) -> u64 {
        vapor_shared::constants::provider::TRANSFER_STEP_BYTES
    }

    /// The most bytes the next step can move, when the session knows:
    /// what is left of a payload whose size is known, and never zero
    /// (the step that finds the end still has to run). The engine asks
    /// the bandwidth bucket for no more than this, so a small file does
    /// not take the whole bucket from every other transfer for the
    /// moment of its one step. `None` when the session cannot tell.
    fn remaining_bytes(&self) -> Option<u64> {
        None
    }

    /// Abandons the transfer and removes partial artifacts. Idempotent.
    fn abort(&mut self);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteChangeKind {
    CreatedOrModified,
    Removed,
    /// One object, a file or a folder, got another name or another
    /// place. The `path` of the change is where it is now, and `from`
    /// is where it was. For a folder this one entry stands for
    /// everything in it. For a file the change also carries what a
    /// `CreatedOrModified` carries, because a file can get a new name
    /// and new content in one step.
    ///
    /// A backend sends it only when it knows both places, and only
    /// when the object is the one that each of the two paths names.
    /// The engine then renames the same object on the device. A backend
    /// that cannot say where the object was sends `Removed` and
    /// `CreatedOrModified` for a file, and `FolderMoved` for a folder.
    Moved {
        from: RemotePath,
        entry_kind: RemoteEntryKind,
    },
    /// A folder got another name or another place, the feed says so
    /// with one entry for the folder and none for the files in it
    /// (Google Drive), and the backend does not know both places (it
    /// would send [`Self::Moved`] then). `path` is a path of the
    /// folder: the one it has now, or the one it had. The engine walks
    /// the folder that holds that path, and the walk finds what the
    /// change did. With the root as its path it says that the place of
    /// something that changed could not be read, and the engine walks
    /// everything. A feed that reports each file under such a folder
    /// never sends this.
    FolderMoved,
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
    /// The object's size as the feed reports it (`None` for removals
    /// and for feeds without metadata). Loop prevention checks it
    /// against the size the daemon wrote: an op-id tag survives an
    /// in-place edit, so the tag alone does not prove an echo.
    pub size_bytes: Option<u64>,
    /// The object's mtime as the feed reports it, the second half of
    /// that quick check.
    pub modified_at: Option<SystemTime>,
    /// The id of the changed object ([`RemoteEntry::id`]). `None` on a
    /// backend without ids.
    pub id: Option<String>,
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

/// One entry of [`Provider::take_follow_up_changes`].
///
/// A follow-up is work that a provider still owes after a write, and
/// that must not be lost when the daemon stops: Google Drive reads the
/// versions of a file after a guarded overwrite of it landed. The
/// record is text that only the provider reads. The engine stores it
/// and hands it back at the next start. It holds no secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FollowUpChange {
    /// The provider owes this work from now on.
    Started(String),
    /// The work of this record ended: it is done, or it can never be
    /// done. The engine forgets the record.
    Finished(String),
}

/// The provider trait every cloud backend implements.
///
/// Path vocabulary: all remote paths are [`RemotePath`]s; relative to
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
    fn ensure_cloud_root(&self, cloud_root: &str) -> Result<(), ProviderError>;

    /// A stable identity for the cloud root, so a folder that merely
    /// has the same path (a fresh mount, a re-created folder, an
    /// emptied share) is told apart from the one the profile adopted.
    /// Filesystem-backed roots read the `.vapor-root` marker
    /// [`adopt_root`](Self::adopt_root) wrote; Drive-style backends
    /// answer with the folder id. `Ok(None)` when the root exists but
    /// carries no identity (no marker yet, or a backend without one);
    /// `NotFound` when the root is missing. Never creates the root.
    fn root_identity(&self, cloud_root: &str) -> Result<Option<String>, ProviderError> {
        let _ = cloud_root;
        Ok(None)
    }

    /// Adopts the current root as the profile's root: writes the
    /// marker where the backend needs one and returns the identity
    /// [`root_identity`](Self::root_identity) will report from then on
    /// (`None` for a backend without one). Called once, after
    /// [`ensure_cloud_root`](Self::ensure_cloud_root).
    fn adopt_root(
        &self,
        cloud_root: &str,
        device_id: &str,
    ) -> Result<Option<String>, ProviderError> {
        let _ = device_id;
        self.root_identity(cloud_root)
    }

    /// Lists the immediate children of `directory` (non-recursive, so
    /// reconcile walks stay slice-interruptible). `directory` may be
    /// [`RemotePath::root`].
    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError>;

    /// The children of `directory` as [`Self::enumerate`] gives them,
    /// and with them the names in it that cannot be a sync path. Such a
    /// name is no entry: it never syncs. The engine asks the user about
    /// it, so a backend that leaves a name out of its listing says so
    /// here, at every listing, for as long as the name is there. The
    /// default is for a backend that can hold no such name.
    fn enumerate_with_unsyncable(
        &self,
        directory: &RemotePath,
    ) -> Result<RemoteListing, ProviderError> {
        Ok(RemoteListing {
            entries: self.enumerate(directory)?,
            unsyncable: Vec::new(),
        })
    }

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

    /// Moves a remote file from `from` to `to` in one call, keeping its
    /// content and tagging it with `op_id`, when
    /// `supports_server_side_move`. A missing source reports
    /// `NotFound`; an occupied destination reports
    /// `PreconditionFailed` so the engine never overwrites by moving.
    fn move_object(
        &self,
        from: &RemotePath,
        to: &RemotePath,
        op_id: &str,
    ) -> Result<(), ProviderError> {
        let _ = (from, to, op_id);
        Err(ProviderError::permanent(
            "this provider cannot move objects server-side",
        ))
    }

    /// Pulls the next page of remote changes after `cursor`.
    /// `cursor = None` baselines the feed: it returns an empty page
    /// whose `next_cursor` marks "now". Only meaningful when
    /// `supports_remote_changes_feed`.
    fn poll_changes(
        &self,
        cursor: Option<&str>,
        max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError>;

    /// The follow-ups that started or ended since the last call, in
    /// the order they happened. The engine calls this on its tick
    /// thread and writes the answer to its state database, so the call
    /// reads memory only: no request, no file, no lock that a request
    /// is made under.
    ///
    /// The follow-up of a transfer is in this list before the step
    /// that ends the transfer returns, whether the step succeeds or
    /// fails. The engine then stores the record in the write that
    /// stores the outcome of the transfer, and a stop of the daemon
    /// between the two cannot lose it.
    ///
    /// The default is for a backend that owes nothing after a write.
    fn take_follow_up_changes(&self) -> Vec<FollowUpChange> {
        Vec::new()
    }

    /// Takes back the records that an earlier run of the daemon stored
    /// and that no run reported as finished. Called one time for a
    /// provider, before its first transfer, on the tick thread: it
    /// reads memory only. The answer holds the records the provider
    /// does not take (it cannot read them, or their work can no longer
    /// be done), and the engine forgets those at once. A record that
    /// the provider takes is reported as finished through
    /// [`Self::take_follow_up_changes`] when its work ends.
    ///
    /// The work of a record can have run in part before the stop, so
    /// the provider does it in a way that is safe to do twice.
    ///
    /// The default takes none, so a backend that owes nothing leaves
    /// no record of another backend in the database.
    fn resume_follow_ups(&self, records: Vec<String>) -> Vec<String> {
        records
    }

    /// Tells the provider about a write of this device whose outcome
    /// the engine never recorded. The object at `path` holds
    /// `content_hash`, the content of the local file, and carries the
    /// tag `op_id`, which this device gave out for this profile. The
    /// sync index still records `base_hash` and another tag: the
    /// upload landed in a daemon that stopped before it stored the
    /// outcome. The engine moves no bytes for such a file and records
    /// it as in sync.
    ///
    /// The follow-up of that write was never stored
    /// ([`Self::take_follow_up_changes`]), and the upload that runs
    /// again sends nothing. A backend that owes work after a write
    /// takes this call as that write: an overwrite of `base_hash` with
    /// `content_hash`, tagged `op_id`, whose outcome nobody recorded.
    ///
    /// The engine makes no call for content that another device or
    /// another program put there, and none for a path whose index row
    /// is missing or holds no hash: with no base, nothing can have
    /// been replaced. A second call for one write starts no second
    /// piece of work.
    ///
    /// The engine calls this on its tick thread, in the tick that
    /// records the file as in sync. The call reads and writes memory
    /// only: no request, no file. What needs a request waits for a
    /// worker. A follow-up that the call starts is in the answer of
    /// the next [`Self::take_follow_up_changes`], so the engine stores
    /// it with the index row of that tick.
    ///
    /// Both hashes are in [`Self::content_hash_algorithm`].
    ///
    /// The default is for a backend that owes nothing after a write.
    fn note_unrecorded_write(
        &self,
        path: &RemotePath,
        base_hash: &str,
        content_hash: &str,
        op_id: &str,
    ) {
        let _ = (path, base_hash, content_hash, op_id);
    }

    /// Takes back the ids that the engine stored for the synced files
    /// and folders ([`RemoteEntry::id`]), each with the path it had
    /// when the engine last saw it. Called one time for a provider,
    /// before its first transfer, on the tick thread: it reads memory
    /// only.
    ///
    /// A backend whose feed names a changed object by its id needs
    /// this to say which path the entry is about after a stop of the
    /// daemon: the old path of an object that got another name, the
    /// path of one that was removed. A path that comes back is what
    /// the engine believed at its last write. The object can be
    /// somewhere else by now, so the backend confirms it with a
    /// request before it writes through it, as it does for a path it
    /// cached itself.
    ///
    /// The default is for a backend without ids: it keeps nothing.
    fn restore_remote_ids(&self, known: Vec<RemoteIdentity>) {
        let _ = known;
    }
}

pub use filesystem::FilesystemProvider;

pub use gdrive::GoogleDriveProvider;

/// Resolves the configured `provider` value onto a provider instance
///. Unknown values are a configuration error the caller must
/// surface, never a silent fallback, because a wrong provider guess
/// could sync into the wrong place.
pub fn select_provider(kind: &str) -> Result<Box<dyn Provider>, ProviderError> {
    select_provider_for_profile(kind, vapor_shared::constants::profile::DEFAULT_PROFILE_ID)
}

/// Profile-aware provider selection: `gdrive` is now
/// selectable, constructed against the profile's namespaced
/// credentials. Missing credentials do not fail selection: they
/// surface as an actionable `Authentication` error when the engine
/// ensures the cloud root, which blocks sync until
/// `vapor auth login` runs.
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

/// The provider of a profile that does not run: one the daemon
/// suspended, one whose configuration is not valid. The profile needs a
/// provider value and must never sync, so every call is refused. A
/// profile in this state does not tick; if a bug ever let it, a stand-in
/// that answered "the cloud is empty" would read as a cloud where
/// everything was deleted.
#[derive(Debug, Default)]
pub struct SuspendedProvider;

pub fn suspended_provider() -> Box<dyn Provider> {
    Box::new(SuspendedProvider)
}

impl SuspendedProvider {
    fn refuse<T>() -> Result<T, ProviderError> {
        Err(ProviderError::permanent(
            "this profile does not run, so it has no provider to call",
        ))
    }
}

impl Provider for SuspendedProvider {
    fn name(&self) -> &'static str {
        "suspended"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_remote_changes_feed: false,
            supports_server_side_move: false,
            changes_feed_is_watched: false,
            small_transfers_share_a_commit: false,
        }
    }

    fn ensure_cloud_root(&self, _cloud_root: &str) -> Result<(), ProviderError> {
        Self::refuse()
    }

    fn root_identity(&self, _cloud_root: &str) -> Result<Option<String>, ProviderError> {
        Self::refuse()
    }

    fn enumerate(&self, _directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
        Self::refuse()
    }

    fn stat(&self, _path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        Self::refuse()
    }

    fn content_hash(&self, _path: &RemotePath) -> Result<String, ProviderError> {
        Self::refuse()
    }

    fn begin_upload(
        &self,
        _request: UploadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        Self::refuse()
    }

    fn begin_download(
        &self,
        _request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        Self::refuse()
    }

    fn delete(&self, _path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
        Self::refuse()
    }

    fn poll_changes(
        &self,
        _cursor: Option<&str>,
        _max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
        Self::refuse()
    }

    /// The records stay where they are: this stand-in runs in place of
    /// the provider that owes their work, on the state database of the
    /// profile, and that provider takes them when the profile runs
    /// again.
    fn resume_follow_ups(&self, _records: Vec<String>) -> Vec<String> {
        Vec::new()
    }
}

/// Inert provider for engine composition tests: every mutation
/// completes as a successful no-op and every read reports nothing. Not
/// selectable through the `provider` config key, and never the provider
/// of a profile of the daemon; a profile that does not run gets
/// [`SuspendedProvider`].
#[derive(Debug, Default)]
pub struct FilesystemStubProvider;

pub fn inert_stub_provider() -> Box<dyn Provider> {
    Box::new(FilesystemStubProvider)
}

impl Provider for FilesystemStubProvider {
    fn name(&self) -> &'static str {
        "filesystem_stub"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_remote_changes_feed: false,
            supports_server_side_move: false,
            changes_feed_is_watched: false,
            small_transfers_share_a_commit: false,
        }
    }

    fn ensure_cloud_root(&self, cloud_root: &str) -> Result<(), ProviderError> {
        // Inert stub: it has no cloud side, so "ensuring" is a logged
        // no-op by design (not a silent trait default).
        logging::info(
            "Filesystem stub provider treats the cloud root as always present",
            &[("cloud_root", cloud_root.to_string())],
        );
        Ok(())
    }

    fn root_identity(&self, _cloud_root: &str) -> Result<Option<String>, ProviderError> {
        // Always present, never identified: the stub has no cloud side
        // to tell apart from another.
        Ok(None)
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
            remote_modified_at: None,
            remote_id: None,
        }))
    }

    fn abort(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_stub_provider_is_the_filesystem_stub() {
        let provider = inert_stub_provider();
        assert_eq!(provider.name(), "filesystem_stub");
    }

    #[test]
    fn poll_is_blocked_when_throttle_is_suspended() {
        let provider = FilesystemStubProvider;
        assert!(provider.poll_allowed(ThrottleState::Light));
        assert!(!provider.poll_allowed(ThrottleState::Suspended));
    }

    #[test]
    fn the_provider_of_a_profile_that_does_not_run_refuses_every_call() {
        let provider = SuspendedProvider;
        let path = RemotePath::new("notes/a.txt").expect("path");
        // A stand-in that answered would describe a cloud that is empty,
        // which reads as "everything was deleted".
        assert!(provider.ensure_cloud_root("/Vapor").is_err());
        assert!(provider.enumerate(&RemotePath::root()).is_err());
        assert!(provider.stat(&path).is_err());
        assert!(provider.delete(&path, "op").is_err());
        assert!(provider.poll_changes(None, 10).is_err());
        assert!(
            provider
                .begin_upload(UploadRequest {
                    local_source: std::path::PathBuf::from("/tmp/vapor-test-source"),
                    remote_path: path.clone(),
                    op_id: "op".to_string(),
                    precondition: RemotePrecondition::None,
                })
                .is_err()
        );
        assert!(!provider.capabilities().supports_remote_changes_feed);
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
