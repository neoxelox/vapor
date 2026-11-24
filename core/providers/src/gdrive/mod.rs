//! Google Drive provider.
//!
//! Implements the full [`Provider`] contract against the Drive v3 API
//! through the injectable HTTP transport (`crate::http`), so every flow
//! is testable offline. Highlights:
//!
//! - **Auth**: profile-scoped tokens from the `SecretStore`
//!   (`auth.{profile}.gdrive.token`), proactive refresh 60s
//!   before expiry, `invalid_grant` → `Authentication` (user
//!   re-consent), one forced-refresh retry on a 401.
//! - **Root handling**: `ensure_cloud_root`
//!   resolves or creates the configured folder chain; failures surface
//!   as actionable errors and the engine blocks sync until it works.
//! - **Uploads**: multipart for small payloads, resumable
//!   chunked sessions for large ones, chunk size tuned across
//!   sessions, rate-limit aware classification throughout.
//! - **Changes**: `changes.list` with a persisted page token;
//!   a 410 maps to `ChangesPoll::CursorExpired`, which the engine
//!   answers with a whole-scope reconcile + re-baseline.
//! - **Deletes** move files to the Drive trash (recoverable) rather
//!   than purging: consistent with Vapor's data-preservation posture.
//!
//! Op-ids ride in `appProperties.vaporOpId`; content hashes use Drive's
//! metadata `md5Checksum` (the engine's hash stage follows
//! [`HashAlgorithm::Md5`] for this provider).

pub mod oauth;

use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use md5::{Digest as Md5Digest, Md5};
use serde::Deserialize;
use vapor_platform::SecretStore;

use crate::http::{HttpRequest, HttpResponse, HttpTransport};
use crate::{
    ChangesPoll, DownloadRequest, HashAlgorithm, Provider, ProviderCapabilities, ProviderError,
    RemoteChange, RemoteChangeKind, RemoteChangesPage, RemoteEntry, RemoteEntryKind, RemotePath,
    RemotePrecondition, TransferOutcome, TransferSession, TransferStep, UploadRequest, logging,
};
use oauth::StoredTokens;

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const GOOGLE_APPS_MIME_PREFIX: &str = "application/vnd.google-apps.";
const OP_ID_PROPERTY: &str = "vaporOpId";

/// Where a file or a folder of the changes feed is.
enum ChangedPath {
    InScope(ResolvedChangePath),
    /// Outside the sync root.
    OutOfScope,
    /// A folder above it could not be read (the cloud did not answer),
    /// so its place is not known. The feed does not say it again.
    Unknown,
}

/// A changed file's freshly-resolved path, plus the stale cached path it
/// moved from (a rename/move) when that differs.
struct ResolvedChangePath {
    new_path: String,
    old_path: Option<String>,
    /// Whether the changed file is the one its path names. Drive lets
    /// two files in one folder have one name; the path names one of
    /// them, and a change of the other one is not a change of the path.
    names_the_path: bool,
}

/// Google-native objects (Docs, Sheets, Slides, shortcuts, …) other than
/// folders. They have no byte content or md5, so they cannot sync as
/// files: enumeration/stat/the changes feed must exclude them, otherwise
/// each one materializes as a phantom 0-byte local file (and deleting
/// that phantom would trash the real Doc).
fn is_google_native_non_folder(mime_type: &str) -> bool {
    mime_type.starts_with(GOOGLE_APPS_MIME_PREFIX) && mime_type != FOLDER_MIME
}
/// Payloads at or under this size upload in one multipart request.
const SIMPLE_UPLOAD_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Resumable chunks must be multiples of 256 KiB per the API contract.
const CHUNK_GRANULARITY: u64 = 256 * 1024;
const CHUNK_MIN: u64 = 256 * 1024;
const CHUNK_MAX: u64 = 64 * 1024 * 1024;
const CHUNK_START: u64 = 8 * 1024 * 1024;
/// Refresh this long before the recorded expiry.
const TOKEN_REFRESH_MARGIN_MS: u64 = 60_000;
/// How long a file id stays remembered as outside the sync root before
/// the changes feed re-checks it (in case it was moved into scope). The
/// changes feed is Drive-wide, so a busy out-of-scope file (a colleague's
/// doc edited every minute) would otherwise re-walk its whole parent
/// chain on every poll.
const OUT_OF_SCOPE_TTL: std::time::Duration = std::time::Duration::from_secs(300);
/// Cap on the out-of-scope negative cache so a churny wider Drive cannot
/// grow it without bound; oldest entries are pruned first.
const OUT_OF_SCOPE_MAX_ENTRIES: usize = 4096;

#[derive(Clone)]
pub struct GdriveConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub profile_id: String,
}

impl GdriveConfig {
    /// The secret-store entry holding this profile's tokens
    /// (profile-scoped namespacing).
    pub fn token_secret_name(&self) -> String {
        format!("auth.{}.gdrive.token", self.profile_id)
    }
}

/// How long the tokens in memory stand with no look at the secret
/// store. A sign-out removes the tokens from the store and a new
/// sign-in replaces them; a daemon that runs follows both this soon.
const TOKEN_STORE_CHECK: Duration = Duration::from_secs(30);
/// How long a store with no tokens must go on saying so before the
/// profile counts as signed out. One read is not enough: the keychain
/// replaces an item by a delete and an add, and a read between the two
/// finds nothing.
const TOKEN_SIGN_OUT_CONFIRM: Duration = Duration::from_secs(2);

/// What the provider knows of the secret store behind the tokens it
/// holds in memory.
struct StoreLook {
    /// When the store is read next. `None`: at the next call.
    due: Option<std::time::Instant>,
    /// Since when the store says that it holds no tokens.
    missing_since: Option<std::time::Instant>,
    /// [`TOKEN_STORE_CHECK`] and [`TOKEN_SIGN_OUT_CONFIRM`], which a
    /// test shortens.
    check: Duration,
    confirm: Duration,
}

struct TokenManager {
    config: GdriveConfig,
    secrets: Arc<dyn SecretStore>,
    transport: Arc<dyn HttpTransport>,
    cached: Mutex<Option<StoredTokens>>,
    look: Mutex<StoreLook>,
    /// The refresh token Google last refused. While the secret store
    /// still holds it, a call fails without another token request; a
    /// new sign-in stores a different one and the next call uses it.
    rejected_refresh_token: Mutex<Option<String>>,
    /// Held for the length of a token refresh, so only one runs.
    refreshing: Mutex<()>,
}

impl TokenManager {
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }

    fn signed_out(&self) -> ProviderError {
        ProviderError::authentication(format!(
            "no Google Drive credentials for profile '{}'; {}",
            self.config.profile_id,
            self.sign_in_hint()
        ))
    }

    /// The store holds the tokens in memory: the next look is a whole
    /// check time away.
    fn note_confirmed(&self) {
        let mut look = self
            .look
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        look.due = Some(std::time::Instant::now() + look.check);
        look.missing_since = None;
    }

    /// The tokens in memory, when the secret store still stands behind
    /// them. The store is the record of the sign-in: tokens that left
    /// it are not used again, and tokens of another sign-in take the
    /// place of the ones in memory.
    fn cached_and_still_stored(&self) -> Result<Option<StoredTokens>, ProviderError> {
        let Some(tokens) = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        else {
            return Ok(None);
        };
        let now = std::time::Instant::now();
        let mut look = self
            .look
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if look.due.is_some_and(|due| now < due) {
            return Ok(Some(tokens));
        }
        match self.secrets.get(&self.config.token_secret_name()) {
            Err(vapor_platform::SecretStoreError::NotFound(_)) => match look.missing_since {
                // The store said so before, and says so still: the user
                // signed this profile out.
                Some(since) if since.elapsed() >= look.confirm => {
                    drop(look);
                    self.invalidate();
                    Err(self.signed_out())
                }
                Some(since) => {
                    look.due = Some(since + look.confirm);
                    Ok(Some(tokens))
                }
                None => {
                    look.missing_since = Some(now);
                    look.due = Some(now + look.confirm);
                    Ok(Some(tokens))
                }
            },
            Ok(raw) => {
                look.due = Some(now + look.check);
                look.missing_since = None;
                match serde_json::from_str::<StoredTokens>(&raw) {
                    // Another sign-in: the next read takes it.
                    Ok(stored) if stored.refresh_token != tokens.refresh_token => {
                        drop(look);
                        self.invalidate();
                        Ok(None)
                    }
                    _ => Ok(Some(tokens)),
                }
            }
            // The store did not answer (a locked keychain). It did not
            // say that the sign-in is gone.
            Err(_) => {
                look.due = Some(now + look.check);
                Ok(Some(tokens))
            }
        }
    }

    fn load(&self) -> Result<StoredTokens, ProviderError> {
        if let Some(tokens) = self.cached_and_still_stored()? {
            return Ok(tokens);
        }
        let raw = self
            .secrets
            .get(&self.config.token_secret_name())
            .map_err(|error| match error {
                vapor_platform::SecretStoreError::NotFound(_) => self.signed_out(),
                // The store holds something and did not give it: the
                // keychain would have asked, or it failed. A new
                // sign-in writes an item this build can read.
                refused => ProviderError::authentication(format!(
                    "the Google Drive credentials for profile '{}' could not be read ({refused}); {}",
                    self.config.profile_id,
                    self.sign_in_hint()
                )),
            })?;
        let tokens: StoredTokens = serde_json::from_str(&raw).map_err(|_| {
            ProviderError::authentication(format!(
                "stored Google Drive credentials are unreadable; {}",
                self.sign_in_hint()
            ))
        })?;
        *self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tokens.clone());
        self.note_confirmed();
        Ok(tokens)
    }

    fn store(&self, tokens: &StoredTokens) {
        if let Ok(serialized) = serde_json::to_string(tokens)
            && let Err(error) = self
                .secrets
                .set(&self.config.token_secret_name(), &serialized)
        {
            logging::warning(
                "Could not persist refreshed Google Drive tokens",
                &[("error", error.to_string())],
            );
        }
        *self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tokens.clone());
    }

    fn access_token(&self) -> Result<String, ProviderError> {
        let tokens = self.load()?;
        let now_ms = Self::now_ms();
        if tokens.expires_at_ms > now_ms + TOKEN_REFRESH_MARGIN_MS {
            return Ok(tokens.access_token);
        }
        // One refresh at a time. Several workers find the token near its
        // end at the same moment; the first one asks, and the others
        // take what it stored.
        let _refreshing = self
            .refreshing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tokens = self.load()?;
        if tokens.expires_at_ms > Self::now_ms() + TOKEN_REFRESH_MARGIN_MS {
            return Ok(tokens.access_token);
        }
        self.force_refresh(&tokens)
    }

    /// A request with `rejected` as its access token got a 401. One
    /// refresh runs at a time: workers that were refused together wait
    /// here, and each one after the first takes the token the first
    /// one stored.
    fn refresh_after_rejection(&self, rejected: &str) -> Result<String, ProviderError> {
        let _refreshing = self
            .refreshing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.invalidate();
        let current = self.load()?;
        if current.access_token != rejected
            && current.expires_at_ms > Self::now_ms() + TOKEN_REFRESH_MARGIN_MS
        {
            return Ok(current.access_token);
        }
        self.force_refresh(&current)
    }

    /// Refreshes the access token. When Google refuses the refresh
    /// token, the cache is dropped so the next call reads the secret
    /// store again: that is how a daemon picks up a new
    /// `vapor auth login` without a restart.
    fn force_refresh(&self, current: &StoredTokens) -> Result<String, ProviderError> {
        let Some(refresh_token) = current.refresh_token.as_deref() else {
            self.invalidate();
            return Err(ProviderError::authentication(format!(
                "Google Drive access token expired and no refresh token is stored; {}",
                self.sign_in_hint()
            )));
        };
        if self
            .rejected_refresh_token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_deref()
            == Some(refresh_token)
        {
            self.invalidate();
            return Err(ProviderError::authentication(format!(
                "Google Drive refused this sign-in; {}",
                self.sign_in_hint()
            )));
        }
        let refreshed = match oauth::refresh_tokens(
            self.transport.as_ref(),
            &self.config.client_id,
            self.config.client_secret.as_deref(),
            refresh_token,
            Self::now_ms(),
        ) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                if error.kind == vapor_shared::ProviderErrorKind::Authentication {
                    *self
                        .rejected_refresh_token
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(refresh_token.to_string());
                    self.invalidate();
                }
                return Err(error);
            }
        };
        // The store is read again before the write: the sign-in can
        // have changed while the refresh ran.
        match self.secrets.get(&self.config.token_secret_name()) {
            // No tokens in the store: a sign-out, or a write of another
            // process that is half done. To write the new tokens would
            // sign the profile in again. They stay in memory until the
            // next look at the store says which of the two it was.
            Err(vapor_platform::SecretStoreError::NotFound(_)) => {
                *self
                    .cached
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(refreshed.clone());
                let mut look = self
                    .look
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let now = std::time::Instant::now();
                let since = *look.missing_since.get_or_insert(now);
                look.due = Some(since + look.confirm);
                return Ok(refreshed.access_token);
            }
            // The store holds another sign-in. The tokens of this
            // refresh belong to the one the user replaced.
            Ok(raw)
                if serde_json::from_str::<StoredTokens>(&raw)
                    .is_ok_and(|stored| stored.refresh_token.as_deref() != Some(refresh_token)) =>
            {
                self.invalidate();
                return Err(ProviderError::transient(
                    "the sign-in of this profile changed during a token refresh",
                ));
            }
            _ => {}
        }
        self.store(&refreshed);
        self.note_confirmed();
        Ok(refreshed.access_token)
    }

    fn sign_in_hint(&self) -> String {
        format!(
            "sign in again with `vapor auth login --profile {}`",
            self.config.profile_id
        )
    }

    fn invalidate(&self) {
        *self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let mut look = self
            .look
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        look.due = None;
        look.missing_since = None;
    }
}

// ---------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
struct GdFile {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "mimeType")]
    mime_type: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default, rename = "md5Checksum")]
    md5_checksum: Option<String>,
    #[serde(default, rename = "appProperties")]
    app_properties: Option<BTreeMap<String, String>>,
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    trashed: bool,
    #[serde(default, rename = "modifiedTime")]
    modified_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GdFileList {
    #[serde(default)]
    files: Vec<GdFile>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GdStartPageToken {
    #[serde(default, rename = "startPageToken")]
    start_page_token: String,
}

#[derive(Debug, Deserialize)]
struct GdChange {
    #[serde(default, rename = "fileId")]
    file_id: String,
    #[serde(default)]
    removed: bool,
    #[serde(default)]
    file: Option<GdFile>,
}

#[derive(Debug, Deserialize)]
struct GdChangeList {
    #[serde(default)]
    changes: Vec<GdChange>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(default, rename = "newStartPageToken")]
    new_start_page_token: Option<String>,
}

const FILE_FIELDS: &str =
    "id,name,mimeType,size,md5Checksum,appProperties,parents,trashed,modifiedTime";

/// One stored version of a file's content.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
struct GdRevision {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "md5Checksum")]
    md5_checksum: Option<String>,
    #[serde(default, rename = "modifiedTime")]
    modified_time: Option<String>,
    #[serde(default)]
    size: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GdRevisionList {
    #[serde(default)]
    revisions: Vec<GdRevision>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

const REVISION_FIELDS: &str = "nextPageToken,revisions(id,md5Checksum,modifiedTime,size)";
/// The most pages of a file's versions one check reads. Drive keeps
/// 100 versions of a file, 200 with the ones kept for good.
const REVISION_PAGES_MAX: usize = 5;

/// Work that keeps a version an upload replaced without having seen
/// it. Drive has no conditional write of content: the guard of an
/// upload is a read followed by a write, and a second device can write
/// between the two. Both writes then land, and the first is a version
/// of the file that no device holds under the name any more. Drive
/// still has it, as a stored version, and this work gives it a name of
/// its own.
#[derive(Clone, Debug)]
enum ReplacedWork {
    /// Read the versions of a file after a guarded upload of it landed:
    /// `base_md5` is what the upload meant to replace, `own_md5` what
    /// it wrote.
    Check {
        file_id: String,
        parent_id: String,
        file_name: String,
        base_md5: String,
        own_md5: String,
        /// From the re-verification before the commit to the answer of
        /// the commit: the time in which another write could land
        /// unseen.
        commit_took: Duration,
        /// The tag the upload put on the file. A file that still has
        /// it was not written again by a device that runs Vapor.
        op_id: String,
    },
    /// Copy one version into a file next to the one it belongs to.
    Copy(RevisionCopy),
}

#[derive(Clone, Debug)]
struct RevisionCopy {
    file_id: String,
    revision: GdRevision,
    parent_id: String,
    /// The name of the file the version belongs to.
    file_name: String,
    /// The name of the copy, from the moment it is chosen.
    copy_name: String,
    /// The resumable session of the copy and how far it is, for a
    /// version too large for one request.
    upload: Option<(String, u64)>,
}

/// How many times one piece of that work is tried after a failure that
/// time alone does not mend, before it is given up with a warning that
/// says where the version is. A cloud that cannot be reached, a rate
/// limit, and a sign-in that is needed spend none: the work waits for
/// as long as they last.
const REPLACED_WORK_ATTEMPTS: u32 = 20;
/// The most checks one poll reads: the poll is the feed, and it must
/// stay quick. A check is one request. A copy moves bytes, so a poll
/// makes one step of one copy and no more.
const REPLACED_WORK_PER_POLL: usize = 8;

/// Room on top of what the time of a commit says about the versions
/// another device wrote at the same moment. A version that the upload
/// replaced was committed after the upload last looked at the file, and
/// Drive gives a version its time when its bytes are in, some time
/// before it commits the write: about a second for a small file.
const REPLACED_VERSION_SLACK_BEFORE: Duration = Duration::from_secs(30);
/// A version that carries a later time than the upload's own and was
/// replaced by it got that time before the upload committed. This is
/// room for the clocks of Drive only: each second of it is a second in
/// which the next save of another device reads as a write of this
/// moment.
const REPLACED_VERSION_SLACK_AFTER: Duration = Duration::from_secs(1);

/// How many uploads this provider remembers as its own
/// ([`GoogleDriveProvider::own_writes`]).
const OWN_WRITES_MAX: usize = 512;

/// What the versions of a file say about the moment of a guarded
/// upload of it.
#[derive(Debug, Default, PartialEq, Eq)]
struct SameMoment {
    /// The version the upload wrote.
    own: Option<GdRevision>,
    /// The versions another writer put in at that moment. One of them
    /// can hold the name; each of the others is on no device under the
    /// name any more.
    others: Vec<GdRevision>,
}

impl SameMoment {
    /// The versions to keep as copies, given the content that holds
    /// the name now.
    ///
    /// Which of the writes of one moment holds the name is not in the
    /// list of the versions: the one that committed last does, and it
    /// can carry the older time. That one is not lost. Every other
    /// write of another device is.
    ///
    /// The upload's own version is lost too when another write of that
    /// moment holds the name. The device that wrote the head keeps it,
    /// when that device runs Vapor: its upload put its own tag on the
    /// file. `head_carries_own_tag` says that the file still has the
    /// tag of this upload, so the head came from a program that does
    /// not check what it replaces, and nobody else keeps this version.
    fn into_kept(self, head_md5: Option<&str>, head_carries_own_tag: bool) -> Vec<GdRevision> {
        let another_write_holds_the_name = head_md5.is_some_and(|head| {
            self.others
                .iter()
                .any(|revision| revision.md5_checksum.as_deref() == Some(head))
        });
        let mut keep: Vec<GdRevision> = self
            .others
            .into_iter()
            .filter(|revision| revision.md5_checksum.as_deref() != head_md5)
            .collect();
        if another_write_holds_the_name
            && head_carries_own_tag
            && let Some(own) = self.own
        {
            keep.push(own);
        }
        keep
    }
}

/// Reads the list of a file's versions after a guarded upload of it:
/// `base_md5` is what the upload meant to replace, `own_md5` what it
/// wrote, and `commit_took` the time from its last look at the file to
/// the answer of its write. `own_writes` holds the content of the other
/// uploads of this file that this provider made: its next save is no
/// write of another device, however soon it came. Versions with the
/// content of the base or of the upload are not listed: nothing of them
/// is missing.
fn versions_of_the_same_moment(
    revisions: &[GdRevision],
    base_md5: &str,
    own_md5: &str,
    commit_took: Duration,
    own_writes: &[String],
) -> SameMoment {
    // Drive lists the versions by their times, oldest first. A version
    // gets the time at which Drive had its bytes, or the time its
    // writer set (Drive for desktop sets the time the file has on its
    // disk).
    let md5 = |revision: &GdRevision| revision.md5_checksum.clone().unwrap_or_default();
    let time_ms = |revision: &GdRevision| {
        revision
            .modified_time
            .as_deref()
            .and_then(parse_rfc3339_millis)
            .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|age| age.as_millis())
    };
    let Some(own) = revisions
        .iter()
        .rposition(|revision| md5(revision) == own_md5)
    else {
        return SameMoment::default();
    };
    // With no version of the base in the list (Drive dropped it), the
    // list cannot say what came after it.
    let Some(base) = revisions[..own]
        .iter()
        .rposition(|revision| md5(revision) == base_md5)
    else {
        return SameMoment::default();
    };
    // The place in the list is not enough. When the base carries a
    // time its writer set, every version Drive stored before it is
    // listed after it; and of two writes of one moment, the one that
    // committed second can carry the older time. The times bound the
    // moment: a write that the upload did not see was committed inside
    // `commit_took`, so its time is at most that long after the time
    // of the upload's own version, and not much longer before it.
    let own_ms = time_ms(&revisions[own]);
    let took = commit_took.as_millis();
    let of_the_moment = |revision: &GdRevision| match (own_ms, time_ms(revision)) {
        (Some(own), Some(at)) => {
            at + 2 * took + REPLACED_VERSION_SLACK_BEFORE.as_millis() >= own
                && at <= own + took + REPLACED_VERSION_SLACK_AFTER.as_millis()
        }
        _ => true,
    };
    let mut others: Vec<GdRevision> = Vec::new();
    for (index, revision) in revisions.iter().enumerate().skip(base + 1) {
        let content = md5(revision);
        if index == own
            || content.is_empty()
            || content == base_md5
            || content == own_md5
            || own_writes.contains(&content)
            || !of_the_moment(revision)
        {
            continue;
        }
        others.retain(|earlier| md5(earlier) != content);
        others.push(revision.clone());
    }
    SameMoment {
        own: Some(revisions[own].clone()),
        others,
    }
}

// ---------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------

/// How many files of one name in one folder a lookup reads. The lowest
/// id among them is the file the name stands for.
const DUPLICATE_NAME_PAGE: usize = 100;

/// How long a folder id that a request confirmed stands without another
/// request. A path lookup would otherwise confirm every folder above it
/// again: a stat five folders deep cost six requests, and an upload
/// into that folder about fifteen. The changes feed drops a folder from
/// the cache as soon as it reports the folder changed, so the window
/// only covers a change the feed has not delivered yet.
const FOLDER_ID_TRUST: Duration = Duration::from_secs(60);

/// The same trust for a write, and much shorter. An upload into a
/// folder that another device trashed a moment ago would land in the
/// trash and then be removed here as part of that deletion. A lookup
/// finds no folder there and creates one, so the new file survives.
const FOLDER_WRITE_TRUST: Duration = Duration::from_secs(5);

pub struct GoogleDriveProvider {
    tokens: Arc<TokenManager>,
    /// Resolved id of the configured cloud root folder.
    root_id: Mutex<Option<String>>,
    /// Path → file-id cache; pruned on deletes/renames.
    id_by_path: Mutex<BTreeMap<String, String>>,
    /// File-id → path cache for changes mapping.
    path_by_id: Mutex<BTreeMap<String, String>>,
    /// File ids the changes feed has resolved to be outside the sync root,
    /// with the time they were last confirmed so the entry can expire.
    out_of_scope: Mutex<BTreeMap<String, SystemTime>>,
    /// Learned resumable chunk size, shared across sessions.
    chunk_hint: Arc<Mutex<u64>>,
    /// Ids of files this provider leaves in the cloud and has said so
    /// about: one that shares its name with another file in the same
    /// folder and lost the choice, and one whose name cannot be a sync
    /// path. Each is reported one time.
    reported_duplicates: Mutex<std::collections::BTreeSet<String>>,
    /// When each cached folder path was last confirmed by a request
    /// ([`FOLDER_ID_TRUST`]).
    folder_confirmed_at: Mutex<BTreeMap<String, std::time::Instant>>,
    /// Held while a missing folder is looked up and created. Drive
    /// allows two folders with one name, so two uploads into one new
    /// folder must not both find it missing and both create it.
    folder_creation: Mutex<()>,
    /// What is left to do for versions that uploads replaced, with the
    /// tries each piece has had. An upload adds to it when it lands;
    /// the changes poll works it off.
    replaced_work: Arc<Mutex<std::collections::VecDeque<(ReplacedWork, u32)>>>,
    /// The file and the content of the last uploads this provider
    /// made. The check for a replaced version reads it: the next save
    /// of this device is in the list of the versions too, and it is no
    /// write of another device.
    own_writes: Arc<Mutex<std::collections::VecDeque<(String, String)>>>,
}

impl GoogleDriveProvider {
    pub fn new(
        config: GdriveConfig,
        secrets: Arc<dyn SecretStore>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            tokens: Arc::new(TokenManager {
                config,
                secrets,
                transport,
                cached: Mutex::new(None),
                look: Mutex::new(StoreLook {
                    due: None,
                    missing_since: None,
                    check: TOKEN_STORE_CHECK,
                    confirm: TOKEN_SIGN_OUT_CONFIRM,
                }),
                rejected_refresh_token: Mutex::new(None),
                refreshing: Mutex::new(()),
            }),
            root_id: Mutex::new(None),
            id_by_path: Mutex::new(BTreeMap::new()),
            path_by_id: Mutex::new(BTreeMap::new()),
            reported_duplicates: Mutex::new(std::collections::BTreeSet::new()),
            folder_confirmed_at: Mutex::new(BTreeMap::new()),
            folder_creation: Mutex::new(()),
            out_of_scope: Mutex::new(BTreeMap::new()),
            chunk_hint: Arc::new(Mutex::new(CHUNK_START)),
            replaced_work: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            own_writes: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        }
    }

    /// Production constructor: native transport + native secret store,
    /// and the OAuth client from [`oauth::client_credentials`] (built
    /// in, or overridden by the environment; see
    /// `docs/operations/provider-auth-operations.md`).
    pub fn for_profile(profile_id: &str) -> Result<Self, ProviderError> {
        let client = oauth::client_credentials().ok_or_else(|| {
            ProviderError::authentication(format!(
                "this build has no Google Drive OAuth client and {} is not set",
                vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_ID
            ))
        })?;
        // The provider runs inside the daemon: a read of the tokens that
        // would open a keychain dialog fails instead, and the profile
        // holds for a sign-in.
        let secrets = vapor_platform::open_store(vapor_platform::SecretAccess::Background)
            .map(Arc::<dyn SecretStore>::from)
            .map_err(|error| {
                ProviderError::authentication(format!("secret store unavailable: {error}"))
            })?;
        Ok(Self::new(
            GdriveConfig {
                client_id: client.client_id,
                client_secret: client.client_secret,
                profile_id: profile_id.to_string(),
            },
            secrets,
            Arc::new(crate::http::NativeHttpTransport),
        ))
    }

    // -- HTTP plumbing ------------------------------------------------

    fn execute_authed(
        &self,
        method: &'static str,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<HttpResponse, ProviderError> {
        execute_authed_with(&self.tokens, method, url, headers, body)
    }

    fn handle(&self) -> ProviderHandle {
        ProviderHandle {
            tokens: self.tokens.clone(),
        }
    }

    fn api_json<T: for<'de> Deserialize<'de>>(
        &self,
        method: &'static str,
        url: String,
        body: Option<serde_json::Value>,
    ) -> Result<T, ProviderError> {
        let (headers, body) = match body {
            Some(value) => (
                vec![("Content-Type".to_string(), "application/json".to_string())],
                serde_json::to_vec(&value).expect("json body serializes"),
            ),
            None => (Vec::new(), Vec::new()),
        };
        let response = self.execute_authed(method, url, headers, body)?;
        if response.status >= 300 {
            return Err(classify_api_failure(&response));
        }
        serde_json::from_slice(&response.body).map_err(|error| {
            ProviderError::transient(format!("unparsable Drive API response: {error}"))
        })
    }

    // -- Versions that uploads replaced --------------------------------

    /// Works off a part of [`Self::replaced_work`]. A piece that fails
    /// goes to the end of the line and is tried at a later poll.
    fn keep_replaced_versions(&self) {
        for _ in 0..REPLACED_WORK_PER_POLL {
            let next = self
                .replaced_work
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front();
            let Some((work, attempts)) = next else {
                return;
            };
            let moved_bytes = matches!(&work, ReplacedWork::Copy(_));
            let mut requeue: Vec<(ReplacedWork, u32)> = Vec::new();
            let mut failed = false;
            match self.advance_replaced_work(work.clone()) {
                // What follows keeps the tries of the piece it follows:
                // a copy that opens a session each time and never ends
                // must run out of them too.
                Ok(follow_up) => requeue.extend(follow_up.into_iter().map(|work| (work, attempts))),
                // The file or the version is gone: nothing is left to
                // keep, and no try will find it.
                Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                    crate::logging::debug(
                        "A version that an upload replaced is gone from Google Drive",
                        &[("error", error.message)],
                    );
                }
                Err(error) => {
                    failed = true;
                    let waits_for_the_cloud = matches!(
                        error.kind,
                        vapor_shared::ProviderErrorKind::Transient
                            | vapor_shared::ProviderErrorKind::RateLimited { .. }
                            | vapor_shared::ProviderErrorKind::Authentication
                            | vapor_shared::ProviderErrorKind::CloudRootUnavailable
                            | vapor_shared::ProviderErrorKind::StorageFull
                    );
                    let attempts = if waits_for_the_cloud {
                        attempts
                    } else {
                        attempts + 1
                    };
                    if attempts < REPLACED_WORK_ATTEMPTS {
                        crate::logging::debug(
                            "Could not keep a version that an upload replaced yet; trying again",
                            &[("error", error.message)],
                        );
                        // A copy starts again from its first byte:
                        // nothing of a resumable session is a file
                        // until its end.
                        let work = match work {
                            ReplacedWork::Copy(mut copy) => {
                                copy.upload = None;
                                ReplacedWork::Copy(copy)
                            }
                            check => check,
                        };
                        requeue.push((work, attempts));
                    } else {
                        match &work {
                            // Nothing says that a version was replaced.
                            ReplacedWork::Check { file_name, .. } => crate::logging::debug(
                                "Gave up the check for a version that an upload replaced",
                                &[("file", file_name.clone()), ("error", error.message)],
                            ),
                            ReplacedWork::Copy(copy) => crate::logging::warning(
                                "Two writes of one file landed in Google Drive at the same moment, and the version that lost the name could not be kept as a copy; it is in the version history of the file in Google Drive",
                                &[("file", copy.file_name.clone()), ("error", error.message)],
                            ),
                        }
                    }
                }
            }
            let mut queue = self
                .replaced_work
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if failed {
                queue.extend(requeue);
            } else {
                for work in requeue.into_iter().rev() {
                    queue.push_front(work);
                }
            }
            drop(queue);
            // One failure is enough for one poll, and so is one step of
            // a copy: the bytes it moves are outside the transfer
            // budgets of the engine.
            if failed || moved_bytes {
                return;
            }
        }
    }

    /// Does one piece of the work and answers with what follows it.
    fn advance_replaced_work(
        &self,
        work: ReplacedWork,
    ) -> Result<Vec<ReplacedWork>, ProviderError> {
        match work {
            ReplacedWork::Check {
                file_id,
                parent_id,
                file_name,
                base_md5,
                own_md5,
                commit_took,
                op_id,
            } => {
                let revisions = match self.list_revisions(&file_id) {
                    Ok(revisions) => revisions,
                    // The file is gone, and its versions with it.
                    Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                        return Ok(Vec::new());
                    }
                    Err(error) => return Err(error),
                };
                let own_writes: Vec<String> = self
                    .own_writes
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .iter()
                    .filter(|(file, content)| file == &file_id && content != &own_md5)
                    .map(|(_, content)| content.clone())
                    .collect();
                let same_moment = versions_of_the_same_moment(
                    &revisions,
                    &base_md5,
                    &own_md5,
                    commit_took,
                    &own_writes,
                );
                // The common answer, and it costs the one request.
                if same_moment.others.is_empty() {
                    return Ok(Vec::new());
                }
                let url = format!("{API_BASE}/files/{file_id}?fields={FILE_FIELDS}");
                let head: GdFile = match self.api_json("GET", url, None) {
                    Ok(head) => head,
                    Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                        return Ok(Vec::new());
                    }
                    Err(error) => return Err(error),
                };
                let head_carries_own_tag = head
                    .app_properties
                    .as_ref()
                    .and_then(|properties| properties.get(OP_ID_PROPERTY))
                    .is_some_and(|tag| tag == &op_id);
                Ok(same_moment
                    .into_kept(head.md5_checksum.as_deref(), head_carries_own_tag)
                    .into_iter()
                    .map(|revision| {
                        ReplacedWork::Copy(RevisionCopy {
                            file_id: file_id.clone(),
                            revision,
                            parent_id: parent_id.clone(),
                            file_name: file_name.clone(),
                            copy_name: String::new(),
                            upload: None,
                        })
                    })
                    .collect())
            }
            ReplacedWork::Copy(copy) => self.advance_revision_copy(copy),
        }
    }

    fn list_revisions(&self, file_id: &str) -> Result<Vec<GdRevision>, ProviderError> {
        let mut revisions = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..REVISION_PAGES_MAX {
            let mut url = format!(
                "{API_BASE}/files/{file_id}/revisions?pageSize=1000&fields={REVISION_FIELDS}"
            );
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={}", oauth::url_encode(token)));
            }
            let page: GdRevisionList = self.api_json("GET", url, None)?;
            revisions.extend(page.revisions);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(revisions)
    }

    /// Copies one stored version into a file of its own, named as a
    /// conflict copy of the file it belongs to. A small version goes
    /// in one request; a large one moves one chunk per call through a
    /// resumable session.
    fn advance_revision_copy(
        &self,
        mut copy: RevisionCopy,
    ) -> Result<Vec<ReplacedWork>, ProviderError> {
        let replaced_at_ms = copy
            .revision
            .modified_time
            .as_deref()
            .and_then(parse_rfc3339_millis)
            .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|age| age.as_millis() as u64)
            .unwrap_or(0);
        // Two versions of one millisecond, or two with no time, would
        // have one name. A name that other content holds already is
        // not this version's copy: the next number is tried.
        let mut name = copy.copy_name.clone();
        if copy.upload.is_none() {
            let mut sequence = None;
            loop {
                name = vapor_shared::conflict::copy_name(
                    &copy.file_name,
                    vapor_shared::constants::conflict::REPLACED_VERSION,
                    replaced_at_ms,
                    sequence,
                );
                match self.find_child(&copy.parent_id, &name)? {
                    None => break,
                    // A copy of this version that is there already
                    // (another device kept it, or a try of this one
                    // got that far) is enough.
                    Some(existing) if existing.md5_checksum == copy.revision.md5_checksum => {
                        return Ok(Vec::new());
                    }
                    Some(_) if sequence.unwrap_or(1) >= 8 => {
                        return Err(ProviderError::permanent(
                            "every name for the copy of a replaced version is taken",
                        ));
                    }
                    Some(_) => sequence = Some(sequence.map_or(2, |number| number + 1)),
                }
            }
            copy.copy_name = name.clone();
        }
        let size: u64 = copy
            .revision
            .size
            .as_deref()
            .and_then(|size| size.parse().ok())
            .unwrap_or(0);
        let media = format!(
            "{API_BASE}/files/{}/revisions/{}?alt=media",
            copy.file_id, copy.revision.id
        );
        let metadata = serde_json::json!({ "name": name, "parents": [copy.parent_id] });

        if copy.upload.is_none() {
            if size <= SIMPLE_UPLOAD_MAX_BYTES {
                let response = self.execute_authed("GET", media, Vec::new(), Vec::new())?;
                if response.status >= 300 {
                    return Err(classify_api_failure(&response));
                }
                let content = response.body;
                if let Some(expected) = copy.revision.md5_checksum.as_deref()
                    && md5_hex_of_bytes(&content) != expected
                {
                    return Err(ProviderError::transient(
                        "a stored version came down with other bytes than Drive described",
                    ));
                }
                let boundary = multipart_boundary_absent_in(&content);
                let mut body = Vec::new();
                body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
                body.extend_from_slice(metadata.to_string().as_bytes());
                body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
                body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
                body.extend_from_slice(&content);
                body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
                let response = self.execute_authed(
                    "POST",
                    format!("{UPLOAD_BASE}/files?uploadType=multipart&fields=id"),
                    vec![(
                        "Content-Type".to_string(),
                        format!("multipart/related; boundary={boundary}"),
                    )],
                    body,
                )?;
                if response.status >= 300 {
                    return Err(classify_api_failure(&response));
                }
                self.report_kept_copy(&copy, &name);
                return Ok(Vec::new());
            }
            let response = self.execute_authed(
                "POST",
                format!("{UPLOAD_BASE}/files?uploadType=resumable&fields=id"),
                vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("X-Upload-Content-Length".to_string(), size.to_string()),
                ],
                metadata.to_string().into_bytes(),
            )?;
            if response.status >= 300 {
                return Err(classify_api_failure(&response));
            }
            let session_url = response
                .header("Location")
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ProviderError::transient("resumable initiation returned no session URI")
                })?;
            copy.upload = Some((session_url, 0));
            return Ok(vec![ReplacedWork::Copy(copy)]);
        }

        let (session_url, sent) = copy.upload.clone().expect("checked above");
        if sent >= size {
            return Err(ProviderError::transient(
                "the copy of a stored version sent every byte and did not finish",
            ));
        }
        let chunk = CHUNK_START.min(size - sent);
        let last = sent + chunk - 1;
        let response = self.execute_authed(
            "GET",
            media,
            vec![("Range".to_string(), format!("bytes={sent}-{last}"))],
            Vec::new(),
        )?;
        if response.status >= 300 {
            return Err(classify_api_failure(&response));
        }
        if response.body.len() as u64 != chunk {
            return Err(ProviderError::transient(format!(
                "a stored version answered {} bytes for a range of {chunk}",
                response.body.len()
            )));
        }
        let response = self.execute_authed(
            "PUT",
            session_url.clone(),
            vec![(
                "Content-Range".to_string(),
                format!("bytes {sent}-{last}/{size}"),
            )],
            response.body,
        )?;
        match response.status {
            308 => {
                let acked = response
                    .header("Range")
                    .and_then(parse_resumable_range_end)
                    .map(|end| end + 1)
                    .unwrap_or(last + 1);
                copy.upload = Some((session_url, acked.min(size)));
                Ok(vec![ReplacedWork::Copy(copy)])
            }
            200 | 201 => {
                self.report_kept_copy(&copy, &name);
                Ok(Vec::new())
            }
            _ => Err(classify_api_failure(&response)),
        }
    }

    fn report_kept_copy(&self, copy: &RevisionCopy, name: &str) {
        crate::logging::warning(
            "Two writes of one file landed in Google Drive at the same moment; kept the version that lost the name as a copy",
            &[("file", copy.file_name.clone()), ("copy", name.to_string())],
        );
    }

    // -- Path/id resolution --------------------------------------------

    fn require_root_id(&self) -> Result<String, ProviderError> {
        self.root_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                ProviderError::permanent(
                    "the Google Drive sync root has not been ensured yet; configuration must be validated before sync work starts",
                )
            })
    }

    fn cache_mapping(&self, path: &str, id: &str) {
        self.id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.to_string(), id.to_string());
        self.path_by_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), path.to_string());
    }

    /// Whether the cache binds `path` to the file `id`.
    fn path_is_bound_to(&self, path: &str, id: &str) -> bool {
        self.id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .is_some_and(|bound| bound == id)
    }

    /// Forgets where the file `id` lives, and leaves the path it had to
    /// the file that path is bound to.
    fn forget_file(&self, id: &str) {
        self.path_by_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }

    /// Records where a file the changes feed reported lives, and says
    /// whether that file is the one the path names. Drive lets two
    /// files in one folder have one name. A path is bound to the file
    /// with the lower id (the rule `find_child` applies). A change of
    /// the other file must not rebind the path, must not read as a
    /// change of the path, and its removal later must not read as the
    /// removal of the file the path names.
    fn bind_changed_file(&self, path: &str, file: &GdFile) -> bool {
        let bound = self
            .id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .cloned();
        match bound {
            Some(bound) if bound == file.id => true,
            Some(bound) if bound < file.id => {
                self.forget_file(&file.id);
                false
            }
            Some(_) => {
                self.cache_mapping(path, &file.id);
                true
            }
            None => {
                // No binding yet: one lookup says which file the path
                // names. A lookup that fails binds the changed file, as
                // a folder with one file of that name would.
                let kept = file
                    .parents
                    .first()
                    .and_then(|parent| self.find_child(parent, &file.name).ok().flatten())
                    .filter(|kept| kept.id != file.id);
                match kept {
                    Some(kept) => {
                        self.cache_mapping(path, &kept.id);
                        self.forget_file(&file.id);
                        self.report_duplicate(path, &kept, file);
                        false
                    }
                    None => {
                        self.cache_mapping(path, &file.id);
                        true
                    }
                }
            }
        }
    }

    /// Says one time that a file shares its name with another one in
    /// the same folder and is left in the cloud.
    fn report_duplicate(&self, path: &str, kept: &GdFile, left: &GdFile) {
        let first_report = self
            .reported_duplicates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(left.id.clone());
        if first_report {
            logging::warning(
                "Two files in the cloud folder share one name; Vapor syncs one and leaves the other in the cloud. Rename one of them in Google Drive",
                &[
                    ("path", path.to_string()),
                    ("synced_file_id", kept.id.clone()),
                    ("left_file_id", left.id.clone()),
                ],
            );
        }
    }

    fn evict_path(&self, path: &str) {
        let id = self
            .id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(path);
        if let Some(id) = id {
            // Only when the file is still known under this path: a
            // listing can have found it under another one since, and
            // that knowledge stays.
            let mut path_by_id = self
                .path_by_id
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if path_by_id.get(&id).is_some_and(|known| known == path) {
                path_by_id.remove(&id);
            }
        }
        // A folder that changed takes the trust of everything under it
        // with it: those paths hang off the old place.
        let below = format!("{path}/");
        self.folder_confirmed_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|folder, _| folder != path && !folder.starts_with(&below));
    }

    /// Forgets every file and folder the cache holds under `path`. A
    /// folder that got another name or another place takes them along,
    /// and what the cache says about them names the old place: a lookup
    /// of an old path would find a file that is not there, and a file
    /// made later at that path would count as the second of its name.
    fn forget_under(&self, path: &str) {
        let below = format!("{path}/");
        self.id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|known, _| !known.starts_with(&below));
        self.path_by_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|_, known| !known.starts_with(&below));
    }

    /// The path a feed entry gives its file, when the cache knows the
    /// folder that holds it. No request.
    fn path_from_the_cache(&self, file: &GdFile) -> Option<String> {
        let parent = file.parents.first()?;
        let root_id = self
            .root_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()?;
        if parent == &root_id {
            return Some(file.name.clone());
        }
        let parent_path = self
            .path_by_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(parent)
            .cloned()?;
        Some(format!("{parent_path}/{}", file.name))
    }

    /// Notes that a request just confirmed `path` as a folder with the
    /// id the cache holds for it.
    fn trust_folder(&self, path: &str) {
        self.folder_confirmed_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.to_string(), std::time::Instant::now());
    }

    /// The cached id of the folder at `path`, when a request confirmed
    /// it within [`FOLDER_ID_TRUST`].
    fn trusted_folder_id(&self, path: &str) -> Option<String> {
        self.folder_id_confirmed_within(path, FOLDER_ID_TRUST)
    }

    /// The cached id of the folder at `path`, when a request confirmed
    /// it no longer than `trust` ago.
    fn folder_id_confirmed_within(&self, path: &str, trust: Duration) -> Option<String> {
        let confirmed_at = self
            .folder_confirmed_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .copied()?;
        if confirmed_at.elapsed() > trust {
            return None;
        }
        self.id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .cloned()
    }

    /// The id of the folder at `path`: the trusted one, or the one a
    /// lookup finds, which is then trusted. `None` when there is no
    /// folder there.
    fn resolve_folder_id(&self, path: &RemotePath) -> Result<Option<String>, ProviderError> {
        if let Some(id) = self.trusted_folder_id(path.as_str()) {
            return Ok(Some(id));
        }
        match self.resolve(path)? {
            Some(file) if file.mime_type == FOLDER_MIME => {
                self.cache_mapping(path.as_str(), &file.id);
                self.trust_folder(path.as_str());
                Ok(Some(file.id))
            }
            _ => Ok(None),
        }
    }

    fn find_child(&self, parent_id: &str, name: &str) -> Result<Option<GdFile>, ProviderError> {
        let query = format!(
            "'{}' in parents and name = '{}' and trashed = false",
            escape_query(parent_id),
            escape_query(name)
        );
        let url = format!(
            "{API_BASE}/files?q={}&fields=files({FILE_FIELDS})&pageSize={DUPLICATE_NAME_PAGE}",
            oauth::url_encode(&query)
        );
        let list: GdFileList = self.api_json("GET", url, None)?;
        // Drive allows multiple children with the same name in one folder.
        // Pick the smallest id deterministically so the binding cannot flip
        // across calls/restarts: an unstable binding manufactures endless
        // keep-both conflicts (two-way) and a never-converging mirror
        // (push-only). The page is large enough to hold every file of
        // that name: with a page of two, a third file made the choice
        // depend on the order Drive answered in.
        //
        // Only what the listing would show under this name counts. The
        // query may answer for another spelling of the name, and for a
        // Google Doc, which has no bytes and which the listing skips: a
        // lookup that bound a path to either one would act on a file
        // the engine never saw under that path.
        Ok(list
            .files
            .into_iter()
            .filter(|file| file.name == name && !is_google_native_non_folder(&file.mime_type))
            .min_by(|left, right| left.id.cmp(&right.id)))
    }

    /// Whether a cache-hit file still names `path`: identical leaf name and
    /// parent folder. An external rename or move leaves the cached id
    /// valid but pointing at a different name/parent, so a stale hit would
    /// otherwise upload local edits onto the wrong file (or one moved
    /// outside the sync root) and never recreate the original.
    fn cached_hit_still_matches(
        &self,
        path: &RemotePath,
        file: &GdFile,
        root_id: &str,
    ) -> Result<bool, ProviderError> {
        let leaf = path.as_str().rsplit('/').next().unwrap_or("");
        if file.name != leaf {
            return Ok(false);
        }
        let expected_parent = match path.parent() {
            Some(parent) if !parent.is_root() => match self.resolve_folder_id(&parent)? {
                Some(parent_id) => parent_id,
                None => return Ok(false),
            },
            _ => root_id.to_string(),
        };
        Ok(file.parents.iter().any(|parent| parent == &expected_parent))
    }

    /// Resolves a remote path to a Drive file, walking (and caching)
    /// each segment under the ensured root.
    fn resolve(&self, path: &RemotePath) -> Result<Option<GdFile>, ProviderError> {
        let root_id = self.require_root_id()?;
        if path.is_root() {
            return Ok(Some(GdFile {
                id: root_id,
                mime_type: FOLDER_MIME.to_string(),
                ..GdFile::default()
            }));
        }
        let cached_id = self
            .id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path.as_str())
            .cloned();
        if let Some(id) = cached_id {
            let url = format!("{API_BASE}/files/{id}?fields={FILE_FIELDS}");
            match self.api_json::<GdFile>("GET", url, None) {
                Ok(file)
                    if !file.trashed && self.cached_hit_still_matches(path, &file, &root_id)? =>
                {
                    return Ok(Some(file));
                }
                // Trashed, renamed/moved away, or 404: the cached mapping is
                // stale. Evict and fall through to the fresh segment walk so
                // we never write into (or read) whatever file the id now
                // names.
                Ok(_) => self.evict_path(path.as_str()),
                Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                    self.evict_path(path.as_str());
                }
                Err(error) => return Err(error),
            }
        }

        let mut parent_id = root_id;
        let mut walked = String::new();
        let segments: Vec<&str> = path.as_str().split('/').collect();
        for (index, segment) in segments.iter().enumerate() {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(segment);
            let Some(file) = self.find_child(&parent_id, segment)? else {
                return Ok(None);
            };
            self.cache_mapping(&walked, &file.id);
            if index == segments.len() - 1 {
                return Ok(Some(file));
            }
            parent_id = file.id;
        }
        Ok(None)
    }

    /// Resolves the parent folder id for `path`, creating intermediate
    /// folders as needed (uploads into fresh directories).
    fn ensure_parent_id(&self, path: &RemotePath) -> Result<String, ProviderError> {
        let root_id = self.require_root_id()?;
        let Some(parent) = path.parent() else {
            return Ok(root_id);
        };
        if parent.is_root() {
            return Ok(root_id);
        }
        // The common case: another upload into this folder a moment ago.
        if let Some(id) = self.folder_id_confirmed_within(parent.as_str(), FOLDER_WRITE_TRUST) {
            return Ok(id);
        }
        // One walk at a time: the second one finds what the first made.
        let _creating = self
            .folder_creation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(id) = self.folder_id_confirmed_within(parent.as_str(), FOLDER_WRITE_TRUST) {
            return Ok(id);
        }
        let mut parent_id = root_id;
        let mut walked = String::new();
        for segment in parent.as_str().split('/') {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(segment);
            if let Some(id) = self.folder_id_confirmed_within(&walked, FOLDER_WRITE_TRUST) {
                parent_id = id;
                continue;
            }
            parent_id = match self.find_child(&parent_id, segment)? {
                Some(existing) => existing.id,
                None => self.create_folder(&parent_id, segment)?,
            };
            self.cache_mapping(&walked, &parent_id);
            self.trust_folder(&walked);
        }
        Ok(parent_id)
    }

    fn create_folder(&self, parent_id: &str, name: &str) -> Result<String, ProviderError> {
        let created: GdFile = self.api_json(
            "POST",
            format!("{API_BASE}/files?fields=id"),
            Some(serde_json::json!({
                "name": name,
                "mimeType": FOLDER_MIME,
                "parents": [parent_id],
            })),
        )?;
        Ok(created.id)
    }

    fn entry_from_file(&self, path: &RemotePath, file: &GdFile) -> RemoteEntry {
        self.cache_mapping(path.as_str(), &file.id);
        RemoteEntry {
            path: path.clone(),
            kind: if file.mime_type == FOLDER_MIME {
                RemoteEntryKind::Directory
            } else {
                RemoteEntryKind::File
            },
            size_bytes: file
                .size
                .as_deref()
                .and_then(|size| size.parse().ok())
                .unwrap_or(0),
            modified_at: file
                .modified_time
                .as_deref()
                .and_then(parse_rfc3339_millis)
                .unwrap_or(SystemTime::UNIX_EPOCH),
            content_hash: file.md5_checksum.clone(),
            op_id: file
                .app_properties
                .as_ref()
                .and_then(|properties| properties.get(OP_ID_PROPERTY))
                .cloned(),
        }
    }

    /// Best-effort path for a changed file id: cache first, then a
    /// parent-chain walk toward the ensured root.
    fn path_for_changed_file(&self, file: &GdFile) -> ChangedPath {
        // Zero-request reject for a file already resolved to be outside the
        // sync root, until its TTL lapses (in case it moved into scope).
        if self.is_known_out_of_scope(&file.id) {
            return ChangedPath::OutOfScope;
        }
        // Any previously-cached path for this id is the OLD path: it must
        // NOT be trusted as the current one: a remote rename/move keeps the
        // id but changes the name/parent. Recompute the fresh path from the
        // change's own name + parents; the parent short-circuit against
        // cached parent paths is still fine (parents rarely move).
        let cached = self
            .path_by_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&file.id)
            .cloned();
        let Some(root_id) = self
            .root_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        else {
            return ChangedPath::OutOfScope;
        };
        // Walk up through parents until the root (bounded depth).
        let mut segments = vec![file.name.clone()];
        let Some(mut current_parent) = file.parents.first().cloned() else {
            // A parentless file cannot be under the root.
            self.mark_out_of_scope(&file.id);
            return ChangedPath::OutOfScope;
        };
        let mut new_path = None;
        for _ in 0..64 {
            if current_parent == root_id {
                segments.reverse();
                new_path = Some(segments.join("/"));
                break;
            }
            // Known parent path short-circuits the walk.
            if let Some(parent_path) = self
                .path_by_id
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&current_parent)
                .cloned()
            {
                segments.reverse();
                new_path = Some(format!("{parent_path}/{}", segments.join("/")));
                break;
            }
            let url = format!("{API_BASE}/files/{current_parent}?fields={FILE_FIELDS}");
            let parent: GdFile = match self.api_json("GET", url, None) {
                Ok(parent) => parent,
                // The folder above is gone: nothing under it is in
                // the sync root.
                Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                    return ChangedPath::OutOfScope;
                }
                // The cloud did not answer. No verdict is remembered,
                // and the caller must not take the file for one that
                // is outside the root.
                Err(_) => return ChangedPath::Unknown,
            };
            segments.push(parent.name.clone());
            let Some(next) = parent.parents.first().cloned() else {
                // Walked up to a top-level parent that is not our root.
                self.mark_out_of_scope(&file.id);
                return ChangedPath::OutOfScope;
            };
            current_parent = next;
        }
        let Some(new_path) = new_path else {
            // Exhausted the depth bound without reaching the root.
            self.mark_out_of_scope(&file.id);
            return ChangedPath::OutOfScope;
        };
        // Reconcile caches to the fresh path; a stale mapping at a
        // different path is a rename/move the caller must surface as a
        // Removed(old) + CreatedOrModified(new) pair so the local side
        // converges (the old file is removed, the new one downloaded).
        // Only the file a path names left that path when it moved.
        let old_path = cached
            .filter(|cached_path| cached_path != &new_path)
            .filter(|cached_path| self.path_is_bound_to(cached_path, &file.id));
        if let Some(old) = &old_path {
            self.evict_path(old);
        }
        let names_the_path = self.bind_changed_file(&new_path, file);
        // It resolved in-scope, so drop any stale out-of-scope verdict
        // (the file may have just been moved into the sync root).
        self.out_of_scope
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&file.id);
        ChangedPath::InScope(ResolvedChangePath {
            new_path,
            old_path,
            names_the_path,
        })
    }

    /// Whether `file_id` is remembered as outside the sync root and the
    /// memory has not yet expired.
    fn is_known_out_of_scope(&self, file_id: &str) -> bool {
        let cache = self
            .out_of_scope
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match cache.get(file_id) {
            Some(recorded) => recorded
                .elapsed()
                .map(|age| age < OUT_OF_SCOPE_TTL)
                .unwrap_or(false),
            None => false,
        }
    }

    /// Records `file_id` as outside the sync root, pruning expired entries
    /// (and, if still over the cap, the oldest) so the cache stays bounded.
    fn mark_out_of_scope(&self, file_id: &str) {
        let now = SystemTime::now();
        let mut cache = self
            .out_of_scope
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|_, recorded| {
            recorded
                .elapsed()
                .map(|age| age < OUT_OF_SCOPE_TTL)
                .unwrap_or(false)
        });
        if cache.len() >= OUT_OF_SCOPE_MAX_ENTRIES
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, recorded)| **recorded)
                .map(|(id, _)| id.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(file_id.to_string(), now);
    }
}

impl Provider for GoogleDriveProvider {
    fn name(&self) -> &'static str {
        vapor_shared::constants::provider::GDRIVE
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::GDRIVE_MVP
    }

    fn content_hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::Md5
    }

    fn ensure_cloud_root(&self, cloud_root: &str) -> Result<(), ProviderError> {
        let segments = cloud_root_segments(cloud_root)?;
        let mut parent_id = "root".to_string();
        for segment in &segments {
            parent_id = match self.find_child(&parent_id, segment)? {
                Some(existing) if existing.mime_type == FOLDER_MIME => existing.id,
                Some(_) => {
                    return Err(ProviderError::permanent(format!(
                        "cloudRoot component '{segment}' exists in Drive but is not a folder"
                    )));
                }
                None => self.create_folder(&parent_id, segment)?,
            };
        }
        logging::info(
            "Google Drive sync root is ready",
            &[("cloud_root", cloud_root.to_string())],
        );
        *self
            .root_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(parent_id);
        Ok(())
    }

    fn move_object(
        &self,
        from: &RemotePath,
        to: &RemotePath,
        op_id: &str,
    ) -> Result<(), ProviderError> {
        let file = self.resolve(from)?.ok_or_else(|| {
            ProviderError::not_found(format!("move source {from} does not exist"))
        })?;
        if self.resolve(to)?.is_some() {
            return Err(ProviderError::precondition_failed(format!(
                "move destination {to} already exists"
            )));
        }
        let new_parent = self.ensure_parent_id(to)?;
        let old_parent = file.parents.first().cloned().unwrap_or_default();
        let mut url = format!("{API_BASE}/files/{}?fields=id", file.id);
        if new_parent != old_parent {
            url.push_str(&format!(
                "&addParents={}&removeParents={}",
                oauth::url_encode(&new_parent),
                oauth::url_encode(&old_parent)
            ));
        }
        let _: GdFile = self.api_json(
            "PATCH",
            url,
            Some(serde_json::json!({
                "name": to.file_name().unwrap_or_default(),
                "appProperties": { OP_ID_PROPERTY: op_id },
            })),
        )?;
        self.evict_path(from.as_str());
        self.cache_mapping(to.as_str(), &file.id);
        Ok(())
    }

    fn root_identity(&self, cloud_root: &str) -> Result<Option<String>, ProviderError> {
        // The folder id is the identity: a folder re-created at the
        // same path gets a new id, and a renamed folder keeps its own.
        let mut parent_id = "root".to_string();
        for segment in cloud_root_segments(cloud_root)? {
            parent_id = match self.find_child(&parent_id, segment)? {
                Some(existing) if existing.mime_type == FOLDER_MIME => existing.id,
                Some(_) => {
                    return Err(ProviderError::permanent(format!(
                        "cloudRoot component '{segment}' exists in Drive but is not a folder"
                    )));
                }
                None => {
                    return Err(ProviderError::not_found(format!(
                        "Google Drive folder {cloud_root} is missing"
                    )));
                }
            };
        }
        Ok(Some(parent_id))
    }

    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
        let Some(folder) = self.resolve(directory)? else {
            return Err(ProviderError::not_found(format!(
                "remote directory {directory} does not exist"
            )));
        };
        // One entry for each name: the file with the lowest id, which is
        // the one every lookup of that path finds.
        let mut by_name: BTreeMap<String, (RemotePath, GdFile)> = BTreeMap::new();
        let mut page_token: Option<String> = None;
        loop {
            let query = format!(
                "'{}' in parents and trashed = false",
                escape_query(&folder.id)
            );
            let mut url = format!(
                "{API_BASE}/files?q={}&fields=nextPageToken,files({FILE_FIELDS})&pageSize=1000",
                oauth::url_encode(&query)
            );
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={}", oauth::url_encode(token)));
            }
            let list: GdFileList = self.api_json("GET", url, None)?;
            for file in list.files {
                if is_google_native_non_folder(&file.mime_type) {
                    continue;
                }
                let Ok(child) = directory.join(&file.name) else {
                    // A Drive name may contain `/`, which the sync path
                    // model cannot represent; say so, or the file would
                    // simply never sync with no trace of why. One time
                    // for each file: a listing runs at every scan.
                    let first_report = self
                        .reported_duplicates
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(file.id.clone());
                    if first_report {
                        logging::warning(
                            "A file in Google Drive has a name that cannot be a sync path; it stays in the cloud. Rename it in Google Drive",
                            &[
                                ("directory", directory.as_str().to_string()),
                                ("name", file.name.clone()),
                            ],
                        );
                    }
                    continue;
                };
                match by_name.remove(&file.name) {
                    None => {
                        by_name.insert(file.name.clone(), (child, file));
                    }
                    Some((path, other)) => {
                        let (kept, left) = if other.id <= file.id {
                            (other, file)
                        } else {
                            (file, other)
                        };
                        self.report_duplicate(path.as_str(), &kept, &left);
                        by_name.insert(kept.name.clone(), (path, kept));
                    }
                }
            }
            match list.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        let mut entries: Vec<RemoteEntry> = by_name
            .values()
            .map(|(path, file)| self.entry_from_file(path, file))
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    fn stat(&self, path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        Ok(self.resolve(path)?.and_then(|file| {
            // A Google-native object cannot sync as a file; report it as
            // absent so the engine never plans a transfer for it.
            if is_google_native_non_folder(&file.mime_type) {
                None
            } else {
                Some(self.entry_from_file(path, &file))
            }
        }))
    }

    fn content_hash(&self, path: &RemotePath) -> Result<String, ProviderError> {
        let entry = self.stat(path)?.ok_or_else(|| {
            ProviderError::not_found(format!("remote file {path} does not exist"))
        })?;
        entry.content_hash.ok_or_else(|| {
            ProviderError::permanent(format!(
                "Drive reports no md5 for {path} (Google-native document types cannot sync as files)"
            ))
        })
    }

    fn begin_upload(
        &self,
        request: UploadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let existing = self.resolve(&request.remote_path)?;
        match &request.precondition {
            RemotePrecondition::None => {}
            RemotePrecondition::Absent => {
                if existing.is_some() {
                    return Err(ProviderError::precondition_failed(format!(
                        "upload target {} already exists",
                        request.remote_path
                    )));
                }
            }
            RemotePrecondition::HashEquals(expected) => {
                let current = existing.as_ref().and_then(|file| file.md5_checksum.clone());
                if current.as_ref() != Some(expected) {
                    return Err(ProviderError::precondition_failed(format!(
                        "upload target {} changed since planning",
                        request.remote_path
                    )));
                }
            }
        }

        let metadata = match &existing {
            // Updating content keeps identity; only appProperties move.
            Some(_) => serde_json::json!({
                "appProperties": { OP_ID_PROPERTY: request.op_id },
            }),
            None => serde_json::json!({
                "name": request.remote_path.file_name().unwrap_or_default(),
                "parents": [self.ensure_parent_id(&request.remote_path)?],
                "appProperties": { OP_ID_PROPERTY: request.op_id },
            }),
        };
        let source = std::fs::File::open(&request.local_source).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProviderError::not_found(format!(
                    "upload source {} disappeared",
                    request.local_source.display()
                ))
            } else {
                ProviderError::transient(format!("cannot open upload source: {error}"))
            }
        })?;
        let total_bytes = source
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0);

        let overwritten = existing
            .as_ref()
            .and_then(|file| Some((file.parents.first()?.clone(), file.name.clone())));
        Ok(Box::new(GdriveUploadSession {
            state: UploadState::NotStarted,
            tokens: self.tokens.clone(),
            overwritten,
            file_id: existing.map(|file| file.id),
            metadata,
            source,
            total_bytes,
            sent_bytes: 0,
            chunk_hint: self.chunk_hint.clone(),
            precondition: request.precondition.clone(),
            hasher: Md5::new(),
            hashed_bytes: 0,
            replaced_work: self.replaced_work.clone(),
            own_writes: self.own_writes.clone(),
            op_id: request.op_id.clone(),
            reverified_at: None,
        }))
    }

    fn begin_download(
        &self,
        request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let file = self.resolve(&request.remote_path)?.ok_or_else(|| {
            ProviderError::not_found(format!(
                "remote file {} does not exist",
                request.remote_path
            ))
        })?;
        let total_bytes: u64 = file
            .size
            .as_deref()
            .and_then(|size| size.parse().ok())
            .unwrap_or(0);
        if let Some(parent) = request.destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ProviderError::transient(format!("cannot create download directory: {error}"))
            })?;
        }
        let destination = std::fs::File::create(&request.destination).map_err(|error| {
            ProviderError::transient(format!("cannot create download destination: {error}"))
        })?;
        Ok(Box::new(GdriveDownloadSession {
            provider: self.handle(),
            expected_md5: file.md5_checksum.clone().filter(|md5| !md5.is_empty()),
            file_id: file.id,
            remote_modified_at: file.modified_time.as_deref().and_then(parse_rfc3339_millis),
            destination: Some(destination),
            destination_path: request.destination,
            total_bytes,
            received_bytes: 0,
            hasher: Md5::new(),
            finished: false,
        }))
    }

    fn delete(&self, path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
        let file = self.resolve(path)?.ok_or_else(|| {
            ProviderError::not_found(format!("remote file {path} was already gone"))
        })?;
        // Trash, don't purge: recoverable removal fits the
        // data-preservation posture.
        let _: GdFile = self.api_json(
            "PATCH",
            format!("{API_BASE}/files/{}?fields=id,trashed", file.id),
            Some(serde_json::json!({ "trashed": true })),
        )?;
        self.evict_path(path.as_str());
        Ok(())
    }

    fn poll_changes(
        &self,
        cursor: Option<&str>,
        max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
        // Before the read of the feed, so that a copy made here is in
        // the answer of this poll or of the next.
        self.keep_replaced_versions();
        let Some(cursor) = cursor else {
            let start: GdStartPageToken =
                self.api_json("GET", format!("{API_BASE}/changes/startPageToken"), None)?;
            return Ok(ChangesPoll::Page(RemoteChangesPage {
                changes: Vec::new(),
                next_cursor: start.start_page_token,
            }));
        };

        let url = format!(
            "{API_BASE}/changes?pageToken={}&pageSize={}&fields=newStartPageToken,nextPageToken,changes(fileId,removed,file({FILE_FIELDS}))",
            oauth::url_encode(cursor),
            max_changes.clamp(1, 1000),
        );
        let response = self.execute_authed("GET", url, Vec::new(), Vec::new())?;
        if response.status == 410 {
            // The page token expired server-side: the engine reconciles
            // and re-baselines.
            return Ok(ChangesPoll::CursorExpired);
        }
        if response.status >= 300 {
            return Err(classify_api_failure(&response));
        }
        let list: GdChangeList = serde_json::from_slice(&response.body).map_err(|error| {
            ProviderError::transient(format!("unparsable changes response: {error}"))
        })?;

        let now = SystemTime::now();
        // Set when the place of a changed file or folder could not be
        // read. The feed does not report it again, so the engine is
        // told to compare everything.
        let mut place_unknown = false;
        let mut changes = Vec::new();
        for change in &list.changes {
            let removed = change.removed
                || change
                    .file
                    .as_ref()
                    .map(|file| file.trashed)
                    .unwrap_or(true);
            if removed {
                let known_path = self
                    .path_by_id
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&change.file_id)
                    .cloned();
                let Some(path) = known_path else {
                    // A deletion of a file we never mapped cannot be
                    // applied by path; reconcile owns that convergence.
                    logging::debug(
                        "Skipping Drive deletion of an unmapped file id",
                        &[("file_id", change.file_id.clone())],
                    );
                    continue;
                };
                if !self.path_is_bound_to(&path, &change.file_id) {
                    // Another file has this name now. The path still
                    // names that one.
                    self.forget_file(&change.file_id);
                    continue;
                }
                self.evict_path(&path);
                let Ok(remote_path) = RemotePath::new(path) else {
                    continue;
                };
                changes.push(RemoteChange {
                    path: remote_path,
                    kind: RemoteChangeKind::Removed,
                    observed_at: now,
                    op_id: None,
                    content_hash: None,
                    size_bytes: None,
                    modified_at: None,
                });
                continue;
            }
            let Some(file) = &change.file else { continue };
            if file.mime_type == FOLDER_MIME {
                let path_before = self
                    .path_by_id
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&file.id)
                    .cloned();
                let Some(path_before) = path_before else {
                    // A folder this provider did not know: a new one,
                    // whose files come with entries of their own.
                    continue;
                };
                // The entry of a folder that is where it was (the echo
                // of a folder this device made, a change of something
                // else of it) says nothing new, and costs no request.
                if self.path_from_the_cache(file).as_deref() == Some(path_before.as_str()) {
                    continue;
                }
                // Whatever the cache holds for a folder that got
                // another name or another place, and for everything
                // under it, names the old place.
                self.evict_path(&path_before);
                self.forget_under(&path_before);
                // Drive reports such a folder with this one entry, and
                // nothing for the files in it. The engine walks the
                // folder that held it and the one that holds it now.
                let path_now = match self.path_for_changed_file(file) {
                    ChangedPath::InScope(resolved) => Some(resolved.new_path),
                    ChangedPath::OutOfScope => None,
                    ChangedPath::Unknown => {
                        place_unknown = true;
                        None
                    }
                };
                if path_now.as_deref() == Some(path_before.as_str()) {
                    continue;
                }
                for path in [Some(path_before), path_now].into_iter().flatten() {
                    let Ok(path) = RemotePath::new(path) else {
                        continue;
                    };
                    changes.push(RemoteChange {
                        path,
                        kind: RemoteChangeKind::FolderMoved,
                        observed_at: now,
                        op_id: None,
                        content_hash: None,
                        size_bytes: None,
                        modified_at: None,
                    });
                }
                continue;
            }
            if is_google_native_non_folder(&file.mime_type) {
                continue;
            }
            let resolved = match self.path_for_changed_file(file) {
                ChangedPath::InScope(resolved) => resolved,
                // Not ours.
                ChangedPath::OutOfScope => continue,
                ChangedPath::Unknown => {
                    place_unknown = true;
                    continue;
                }
            };
            // A rename/move surfaces as Removed(old) + CreatedOrModified(new)
            // so the local side deletes the old path and downloads the new
            // one. Reported at the cached path alone, the change would
            // never converge.
            if let Some(old_path) = resolved.old_path
                && let Ok(old_remote) = RemotePath::new(old_path)
            {
                changes.push(RemoteChange {
                    path: old_remote,
                    kind: RemoteChangeKind::Removed,
                    observed_at: now,
                    op_id: None,
                    content_hash: None,
                    size_bytes: None,
                    modified_at: None,
                });
            }
            if !resolved.names_the_path {
                // A second file under a name that another file has: it
                // stays in the cloud, and the path did not change.
                continue;
            }
            let Ok(remote_path) = RemotePath::new(resolved.new_path) else {
                continue;
            };
            changes.push(RemoteChange {
                path: remote_path,
                kind: RemoteChangeKind::CreatedOrModified,
                observed_at: now,
                op_id: file
                    .app_properties
                    .as_ref()
                    .and_then(|properties| properties.get(OP_ID_PROPERTY))
                    .cloned(),
                content_hash: file.md5_checksum.clone(),
                size_bytes: file.size.as_deref().and_then(|size| size.parse().ok()),
                modified_at: file.modified_time.as_deref().and_then(parse_rfc3339_millis),
            });
        }

        if place_unknown {
            changes.push(RemoteChange {
                path: RemotePath::root(),
                kind: RemoteChangeKind::FolderMoved,
                observed_at: now,
                op_id: None,
                content_hash: None,
                size_bytes: None,
                modified_at: None,
            });
        }

        // Drive must return a cursor to advance on: `nextPageToken` for more
        // pages, else `newStartPageToken` for the next baseline. A parsable
        // response missing both is not progress: reusing the old cursor
        // would durably re-poll the identical page forever. Surface it as
        // transient so the poll retries instead of wedging.
        let Some(next_cursor) = list.next_page_token.or(list.new_start_page_token) else {
            return Err(ProviderError::transient(
                "Drive changes response advanced no cursor (no nextPageToken/newStartPageToken)",
            ));
        };
        Ok(ChangesPoll::Page(RemoteChangesPage {
            changes,
            next_cursor,
        }))
    }
}

/// A cheap, cloneable handle over the provider's shared `TokenManager`
/// that upload/download sessions carry for their hot-path requests. It
/// clones an `Arc`, so it neither re-reads the SecretStore per request
/// nor loses the token cache (a fresh manager per chunk would do both).
struct ProviderHandle {
    tokens: Arc<TokenManager>,
}

impl ProviderHandle {
    fn execute_authed(
        &self,
        method: &'static str,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<HttpResponse, ProviderError> {
        execute_authed_with(&self.tokens, method, url, headers, body)
    }
}

/// The one authenticated-request path, shared by the provider and every
/// session handle: attach a bearer token from the shared cache and, on a
/// single `401`, force one refresh and resend. Without the retry a chunk
/// PUT whose token expired mid-upload would fail the whole intent
/// terminally instead of refreshing and resending that one chunk.
fn execute_authed_with(
    tokens: &TokenManager,
    method: &'static str,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<HttpResponse, ProviderError> {
    let mut attempt = 0;
    // Shared with the one retry after a 401, never copied: a body is up
    // to one upload chunk.
    let body = Arc::new(body);
    loop {
        let token = tokens.access_token()?;
        let mut all_headers = headers.clone();
        all_headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        let response = tokens
            .transport
            .execute(HttpRequest {
                method,
                url: url.clone(),
                headers: all_headers,
                body: body.clone(),
            })
            .map_err(|error| {
                ProviderError::transient(format!("Drive API unreachable: {}", error.message))
            })?;
        if response.status == 401 && attempt == 0 {
            attempt += 1;
            tokens.refresh_after_rejection(&token)?;
            continue;
        }
        return Ok(response);
    }
}

// ---------------------------------------------------------------------
// Upload session
// ---------------------------------------------------------------------

enum UploadState {
    NotStarted,
    Resumable { session_url: String },
    Done,
}

struct GdriveUploadSession {
    state: UploadState,
    tokens: Arc<TokenManager>,
    /// `Some` when updating an existing file.
    file_id: Option<String>,
    metadata: serde_json::Value,
    source: std::fs::File,
    total_bytes: u64,
    sent_bytes: u64,
    chunk_hint: Arc<Mutex<u64>>,
    /// The precondition, retained so it can be re-verified immediately
    /// before the committing request: begin_upload's one-time check is a
    /// check-then-act across the whole (possibly long) transfer.
    precondition: RemotePrecondition,
    /// MD5 of the bytes of a resumable upload, taken as the chunks go
    /// out. It is the content hash when Drive's final answer carries
    /// none, so the sync index never records an empty one.
    hasher: Md5,
    /// How far into the file `hasher` has read. A chunk that Drive
    /// acknowledged only in part is sent again from the acknowledged
    /// offset, and those bytes must not be hashed twice.
    hashed_bytes: u64,
    /// Where a guarded overwrite that landed leaves the check for a
    /// version it replaced ([`ReplacedWork`]).
    replaced_work: Arc<Mutex<std::collections::VecDeque<(ReplacedWork, u32)>>>,
    /// Shared with the provider ([`GoogleDriveProvider::own_writes`]).
    own_writes: Arc<Mutex<std::collections::VecDeque<(String, String)>>>,
    /// The tag this upload puts on the file.
    op_id: String,
    /// When the last re-verification before a commit began, on the
    /// clock that does not jump and on the one that counts a sleep of
    /// the device.
    reverified_at: Option<(std::time::Instant, SystemTime)>,
    /// The folder and the name of the file that is overwritten, which
    /// a copy of a replaced version is placed and named by.
    overwritten: Option<(String, String)>,
}

impl GdriveUploadSession {
    fn handle(&self) -> ProviderHandle {
        ProviderHandle {
            tokens: self.tokens.clone(),
        }
    }

    /// Re-verifies a `HashEquals` precondition immediately before the
    /// committing request, shrinking the check-then-act window from the
    /// whole (possibly hours-long, throttle-paused) transfer to one
    /// round-trip. A divergence surfaces as PreconditionFailed, which the
    /// engine resolves as keep-both instead of a silent overwrite.
    fn reverify_precondition_before_commit(&mut self) -> Result<(), ProviderError> {
        let RemotePrecondition::HashEquals(expected) = &self.precondition else {
            return Ok(());
        };
        let Some(file_id) = &self.file_id else {
            return Ok(());
        };
        self.reverified_at = Some((std::time::Instant::now(), SystemTime::now()));
        let url = format!("{API_BASE}/files/{file_id}?fields={FILE_FIELDS}");
        let response = self
            .handle()
            .execute_authed("GET", url, Vec::new(), Vec::new())?;
        if response.status == 404 {
            // The file we meant to overwrite vanished mid-transfer. Surface a
            // conflict rather than resurrecting it with our bytes.
            return Err(ProviderError::precondition_failed(
                "upload target was deleted during the transfer; keeping both",
            ));
        }
        if response.status >= 300 {
            return Err(classify_api_failure(&response));
        }
        let current: GdFile = serde_json::from_slice(&response.body).map_err(|error| {
            ProviderError::transient(format!("unparsable Drive API response: {error}"))
        })?;
        if current.trashed || current.md5_checksum.as_ref() != Some(expected) {
            return Err(ProviderError::precondition_failed(
                "upload target changed during the transfer; keeping both",
            ));
        }
        Ok(())
    }

    /// The re-verification before the commit and the commit are two
    /// requests, and another device can write between them. After a
    /// guarded overwrite landed, the versions of the file say whether
    /// one did; the changes poll reads them, so the upload does not
    /// wait for the answer.
    ///
    /// A commit whose answer did not come gets the check too: the write
    /// can have landed, and the retry then finds its own bytes in the
    /// cloud and sends nothing. A check for a write that did not land
    /// finds no version of it and ends.
    fn check_for_a_replaced_version(&self, own_md5: &str) {
        let (RemotePrecondition::HashEquals(base_md5), Some(file_id), Some((parent_id, file_name))) =
            (&self.precondition, &self.file_id, &self.overwritten)
        else {
            return;
        };
        if own_md5.is_empty() || own_md5 == base_md5 {
            return;
        }
        let mut own_writes = self
            .own_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        own_writes.push_back((file_id.clone(), own_md5.to_string()));
        while own_writes.len() > OWN_WRITES_MAX {
            own_writes.pop_front();
        }
        drop(own_writes);
        let commit_took = self
            .reverified_at
            .map(|(at, wall)| at.elapsed().max(wall.elapsed().unwrap_or_default()))
            .unwrap_or_default();
        self.replaced_work
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back((
                ReplacedWork::Check {
                    file_id: file_id.clone(),
                    parent_id: parent_id.clone(),
                    file_name: file_name.clone(),
                    base_md5: base_md5.clone(),
                    own_md5: own_md5.to_string(),
                    commit_took,
                    op_id: self.op_id.clone(),
                },
                0,
            ));
    }

    /// Whether a failed commit can have landed all the same: the
    /// request went out, and no answer says that Drive refused it.
    fn commit_can_have_landed(error: &ProviderError) -> bool {
        matches!(error.kind, vapor_shared::ProviderErrorKind::Transient)
    }

    /// The check for the last chunk of a resumable upload whose answer
    /// did not come, with the hash of the bytes that went out.
    fn check_after_a_lost_answer(&self) {
        if self.hashed_bytes == self.total_bytes {
            let sent = hex_encode(&self.hasher.clone().finalize());
            self.check_for_a_replaced_version(&sent);
        }
    }

    fn simple_multipart(&mut self) -> Result<TransferOutcome, ProviderError> {
        let mut content = Vec::with_capacity(self.total_bytes as usize);
        self.source.read_to_end(&mut content).map_err(|error| {
            ProviderError::transient(format!("cannot read upload source: {error}"))
        })?;
        // A fixed boundary corrupts (or bounces with 400) any file whose
        // bytes contain the delimiter line. Generate a random per-request
        // boundary and, defensively, regenerate until it does not occur in
        // the payload, so no file content can ever collide with it.
        let boundary = multipart_boundary_absent_in(&content);
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
        body.extend_from_slice(self.metadata.to_string().as_bytes());
        body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(&content);
        body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
        // The body holds the bytes now; the file's own copy is not kept
        // for the length of the request.
        let content_md5 = md5_hex_of_bytes(&content);
        drop(content);

        let (method, url): (&'static str, String) = match &self.file_id {
            Some(id) => (
                "PATCH",
                format!("{UPLOAD_BASE}/files/{id}?uploadType=multipart&fields={FILE_FIELDS}"),
            ),
            None => (
                "POST",
                format!("{UPLOAD_BASE}/files?uploadType=multipart&fields={FILE_FIELDS}"),
            ),
        };
        self.reverify_precondition_before_commit()?;
        let committed = self
            .handle()
            .execute_authed(
                method,
                url,
                vec![(
                    "Content-Type".to_string(),
                    format!("multipart/related; boundary={boundary}"),
                )],
                body,
            )
            .and_then(|response| {
                if response.status >= 300 {
                    return Err(classify_api_failure(&response));
                }
                serde_json::from_slice::<GdFile>(&response.body).map_err(|error| {
                    ProviderError::transient(format!("unparsable upload response: {error}"))
                })
            });
        let file = match committed {
            Ok(file) => file,
            Err(error) => {
                if Self::commit_can_have_landed(&error) {
                    self.check_for_a_replaced_version(&content_md5);
                }
                return Err(error);
            }
        };
        self.state = UploadState::Done;
        let content_hash = file
            .md5_checksum
            .filter(|md5| !md5.is_empty())
            .unwrap_or(content_md5);
        self.check_for_a_replaced_version(&content_hash);
        Ok(TransferOutcome {
            bytes_total: self.total_bytes,
            remote_modified_at: file.modified_time.as_deref().and_then(parse_rfc3339_millis),
            content_hash,
        })
    }

    fn initiate_resumable(&mut self) -> Result<String, ProviderError> {
        // `fields` on the request that opens the session decides what
        // the final answer holds. Without it Drive returns its default
        // set for the method, which has no checksum and no modified
        // time.
        let (method, url): (&'static str, String) = match &self.file_id {
            Some(id) => (
                "PATCH",
                format!("{UPLOAD_BASE}/files/{id}?uploadType=resumable&fields={FILE_FIELDS}"),
            ),
            None => (
                "POST",
                format!("{UPLOAD_BASE}/files?uploadType=resumable&fields={FILE_FIELDS}"),
            ),
        };
        let response = self.handle().execute_authed(
            method,
            url,
            vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "X-Upload-Content-Length".to_string(),
                    self.total_bytes.to_string(),
                ),
            ],
            self.metadata.to_string().into_bytes(),
        )?;
        if response.status >= 300 {
            return Err(classify_api_failure(&response));
        }
        response
            .header("Location")
            .map(ToOwned::to_owned)
            .ok_or_else(|| ProviderError::transient("resumable initiation returned no session URI"))
    }

    fn chunk_size(&self, max_bytes: u64) -> u64 {
        let hint = *self
            .chunk_hint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let budget = hint.min(max_bytes.max(CHUNK_GRANULARITY));
        // All chunks except the final one must be 256 KiB multiples.
        let aligned = (budget / CHUNK_GRANULARITY).max(1) * CHUNK_GRANULARITY;
        aligned.min(CHUNK_MAX)
    }

    fn grow_chunk_hint(&self) {
        let mut hint = self
            .chunk_hint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *hint = (*hint * 2).min(CHUNK_MAX);
    }

    fn shrink_chunk_hint(&self) {
        let mut hint = self
            .chunk_hint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *hint = (*hint / 2).max(CHUNK_MIN);
    }
}

impl TransferSession for GdriveUploadSession {
    /// The resumable chunk size this provider tunes for itself: it
    /// grows while chunks land and halves when one fails.
    fn step_bytes(&self) -> u64 {
        *self
            .chunk_hint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn remaining_bytes(&self) -> Option<u64> {
        Some(self.total_bytes.saturating_sub(self.sent_bytes).max(1))
    }

    fn step(&mut self, max_bytes: u64) -> Result<TransferStep, ProviderError> {
        match &self.state {
            UploadState::Done => unreachable!("step called after completion"),
            UploadState::NotStarted => {
                if self.total_bytes <= SIMPLE_UPLOAD_MAX_BYTES {
                    return Ok(TransferStep::Completed(self.simple_multipart()?));
                }
                let session_url = self.initiate_resumable()?;
                self.state = UploadState::Resumable { session_url };
                Ok(TransferStep::Progressed {
                    bytes_transferred: 0,
                })
            }
            UploadState::Resumable { session_url } => {
                let session_url = session_url.clone();
                let remaining = self.total_bytes - self.sent_bytes;
                let chunk_len = self.chunk_size(max_bytes).min(remaining);
                let mut chunk = vec![0_u8; chunk_len as usize];
                self.source
                    .seek(std::io::SeekFrom::Start(self.sent_bytes))
                    .and_then(|_| self.source.read_exact(&mut chunk))
                    .map_err(|error| {
                        ProviderError::transient(format!("cannot read upload chunk: {error}"))
                    })?;
                // Each byte is hashed one time, in order.
                let chunk_end = self.sent_bytes + chunk_len;
                if chunk_end > self.hashed_bytes {
                    let already_hashed = (self.hashed_bytes - self.sent_bytes) as usize;
                    self.hasher.update(&chunk[already_hashed..]);
                    self.hashed_bytes = chunk_end;
                }

                let range_end = self.sent_bytes + chunk_len - 1;
                // The resumable update only materializes when the final chunk
                // commits, so re-verify the precondition immediately before it:
                // begin_upload's check could be arbitrarily stale by now.
                let commits = range_end + 1 == self.total_bytes;
                if commits {
                    self.reverify_precondition_before_commit()?;
                }
                let response = match self.handle().execute_authed(
                    "PUT",
                    session_url,
                    vec![(
                        "Content-Range".to_string(),
                        format!(
                            "bytes {}-{}/{}",
                            self.sent_bytes, range_end, self.total_bytes
                        ),
                    )],
                    chunk,
                ) {
                    Ok(response) => response,
                    Err(error) => {
                        if commits && Self::commit_can_have_landed(&error) {
                            self.check_after_a_lost_answer();
                        }
                        return Err(error);
                    }
                };
                match response.status {
                    // 308: chunk accepted, upload incomplete. Trust the
                    // server's `Range: bytes=0-N` header (the authoritative
                    // committed offset) over the local counter: if the
                    // server acknowledged fewer bytes than we sent, resume
                    // from there instead of leaving a gap that the next PUT
                    // would reject.
                    308 => {
                        let acked = response
                            .header("Range")
                            .and_then(parse_resumable_range_end)
                            .map(|last| last + 1)
                            .unwrap_or(self.sent_bytes + chunk_len);
                        let advanced = acked.min(self.total_bytes);
                        let bytes_transferred = advanced.saturating_sub(self.sent_bytes);
                        self.sent_bytes = advanced;
                        self.grow_chunk_hint();
                        Ok(TransferStep::Progressed { bytes_transferred })
                    }
                    200 | 201 => {
                        self.sent_bytes += chunk_len;
                        self.state = UploadState::Done;
                        let file: GdFile =
                            serde_json::from_slice(&response.body).unwrap_or_default();
                        // The hash of the bytes this session sent, when
                        // it read every one of them.
                        let sent_hash = (self.hashed_bytes == self.total_bytes)
                            .then(|| hex_encode(&std::mem::take(&mut self.hasher).finalize()));
                        let stored_hash = file.md5_checksum.filter(|checksum| !checksum.is_empty());
                        // A completed upload is a verified upload: what
                        // Drive stored is what was sent. A difference
                        // fails the transfer, and the retry sends the
                        // file again.
                        if let (Some(sent), Some(stored)) = (&sent_hash, &stored_hash)
                            && !sent.eq_ignore_ascii_case(stored)
                        {
                            return Err(ProviderError::transient(
                                "Google Drive stored other bytes than the upload sent",
                            ));
                        }
                        let content_hash = stored_hash.or(sent_hash).unwrap_or_default();
                        self.check_for_a_replaced_version(&content_hash);
                        Ok(TransferStep::Completed(TransferOutcome {
                            bytes_total: self.total_bytes,
                            content_hash,
                            remote_modified_at: file
                                .modified_time
                                .as_deref()
                                .and_then(parse_rfc3339_millis),
                        }))
                    }
                    _ => {
                        let error = classify_api_failure(&response);
                        if commits && Self::commit_can_have_landed(&error) {
                            self.check_after_a_lost_answer();
                        }
                        if matches!(
                            error.kind,
                            vapor_shared::ProviderErrorKind::Transient
                                | vapor_shared::ProviderErrorKind::RateLimited { .. }
                        ) {
                            // Adaptive sizing: back off the
                            // learned chunk before the retry re-plans.
                            self.shrink_chunk_hint();
                        }
                        Err(error)
                    }
                }
            }
        }
    }

    fn abort(&mut self) {
        self.state = UploadState::Done;
    }
}

// ---------------------------------------------------------------------
// Download session
// ---------------------------------------------------------------------

struct GdriveDownloadSession {
    provider: ProviderHandle,
    file_id: String,
    /// Drive's checksum of the file when the download began. The
    /// download reads the file in ranges, and nothing pins a range to a
    /// revision: a file that is replaced while it comes down gives the
    /// head of one version and the tail of another.
    expected_md5: Option<String>,
    remote_modified_at: Option<SystemTime>,
    destination: Option<std::fs::File>,
    destination_path: std::path::PathBuf,
    total_bytes: u64,
    received_bytes: u64,
    hasher: Md5,
    finished: bool,
}

impl TransferSession for GdriveDownloadSession {
    fn remaining_bytes(&self) -> Option<u64> {
        Some(self.total_bytes.saturating_sub(self.received_bytes).max(1))
    }

    fn step(&mut self, max_bytes: u64) -> Result<TransferStep, ProviderError> {
        debug_assert!(!self.finished, "step called after completion");
        if self.received_bytes >= self.total_bytes {
            return self.finish();
        }
        let end = (self.received_bytes + max_bytes.max(1) - 1).min(self.total_bytes - 1);
        let response = self.provider.execute_authed(
            "GET",
            format!("{API_BASE}/files/{}?alt=media", self.file_id),
            vec![(
                "Range".to_string(),
                format!("bytes={}-{}", self.received_bytes, end),
            )],
            Vec::new(),
        )?;
        if response.status >= 300 {
            return Err(classify_api_failure(&response));
        }
        if response.body.is_empty() {
            // The range asked for at least one byte; an empty success
            // body would report zero progress forever instead of failing
            // the transfer into retry backoff.
            return Err(ProviderError::transient(format!(
                "download range {}-{end} returned an empty body",
                self.received_bytes
            )));
        }
        use std::io::Write;
        let destination = self
            .destination
            .as_mut()
            .expect("destination lives until completion");
        destination.write_all(&response.body).map_err(|error| {
            ProviderError::transient(format!("cannot write download destination: {error}"))
        })?;
        self.hasher.update(&response.body);
        self.received_bytes += response.body.len() as u64;
        // A 200 means the server ignored the Range and returned the full
        // body. Verify it actually delivered the whole file before
        // finishing, otherwise a truncated/transcoded body (or the
        // transport's size cap) would complete as a self-consistent but
        // corrupt local file.
        if response.status == 200 {
            if self.received_bytes != self.total_bytes {
                return Err(ProviderError::transient(format!(
                    "download returned {} bytes for a {}-byte file (Range ignored or truncated)",
                    self.received_bytes, self.total_bytes
                )));
            }
            return self.finish();
        }
        if self.received_bytes >= self.total_bytes {
            return self.finish();
        }
        Ok(TransferStep::Progressed {
            bytes_transferred: response.body.len() as u64,
        })
    }

    fn abort(&mut self) {
        self.destination.take();
        if !self.finished {
            let _ = std::fs::remove_file(&self.destination_path);
            self.finished = true;
        }
    }
}

impl GdriveDownloadSession {
    fn finish(&mut self) -> Result<TransferStep, ProviderError> {
        if let Some(destination) = self.destination.take() {
            vapor_platform::fs_ops::make_durable(&destination).map_err(|error| {
                ProviderError::transient(format!("cannot sync downloaded payload: {error}"))
            })?;
        }
        let digest = std::mem::replace(&mut self.hasher, Md5::new()).finalize();
        let content_hash = hex_encode(&digest);
        if let Some(expected) = &self.expected_md5
            && *expected != content_hash
        {
            // What came down is not the file Drive described: it changed
            // while it was read. The payload goes, and the retry reads
            // the file as it is then.
            self.abort();
            return Err(ProviderError::transient(
                "the file changed in Google Drive while it was downloaded; it is read again",
            ));
        }
        self.finished = true;
        Ok(TransferStep::Completed(TransferOutcome {
            bytes_total: self.received_bytes,
            content_hash,
            remote_modified_at: self.remote_modified_at,
        }))
    }
}

/// The path components of the configured Drive root.
fn cloud_root_segments(cloud_root: &str) -> Result<Vec<&str>, ProviderError> {
    let segments: Vec<&str> = cloud_root
        .trim()
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return Err(ProviderError::permanent(
            "cloudRoot must name a folder inside Google Drive (for example \"/Vapor\")",
        ));
    }
    Ok(segments)
}

// ---------------------------------------------------------------------
// Failure classification + small helpers
// ---------------------------------------------------------------------

/// Maps a Drive API failure response onto the provider taxonomy
/// (rate-limit-aware).
fn classify_api_failure(response: &HttpResponse) -> ProviderError {
    let body_text = String::from_utf8_lossy(&response.body);
    let retry_after = response
        .header("Retry-After")
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs);
    match response.status {
        401 => ProviderError::authentication(
            "Google Drive rejected the credentials; run `vapor auth login`",
        ),
        403 => match extract_403_reason(&body_text).as_deref() {
            // Rate/quota limits are transient: back off, do not drop the
            // intent. `dailyLimitExceeded` and `storageQuotaExceeded` (note
            // the capital Q) belong here too.
            Some(
                "rateLimitExceeded"
                | "userRateLimitExceeded"
                | "sharingRateLimitExceeded"
                | "dailyLimitExceeded"
                | "quotaExceeded",
            ) => ProviderError::rate_limited(retry_after, "Drive rate/quota limit exceeded"),
            Some("storageQuotaExceeded") => ProviderError::storage_full(
                "Google Drive storage is full; free space or upgrade the account, then uploads resume",
            ),
            // Fall back to substring sniffing for bodies that don't parse
            // to a structured reason.
            _ if body_text.contains("ateLimitExceeded")
                || (body_text.contains("quotaExceeded")
                    && !body_text.contains("storageQuotaExceeded")) =>
            {
                ProviderError::rate_limited(retry_after, "Drive rate limit exceeded")
            }
            _ => ProviderError::permanent(format!(
                "Drive denied the operation (403): {}",
                truncate(&body_text, 200)
            )),
        },
        404 => ProviderError::not_found("Drive object not found"),
        410 => ProviderError::precondition_failed("Drive resource is gone (410)"),
        412 => ProviderError::precondition_failed("Drive precondition failed (412)"),
        429 => ProviderError::rate_limited(retry_after, "Drive rate limit exceeded (429)"),
        status if status >= 500 => {
            ProviderError::transient(format!("Drive server error ({status})"))
        }
        status => ProviderError::permanent(format!(
            "unexpected Drive response ({status}): {}",
            truncate(&body_text, 200)
        )),
    }
}

/// A random multipart boundary guaranteed not to appear in `payload`.
fn multipart_boundary_absent_in(payload: &[u8]) -> String {
    loop {
        let mut octets = [0_u8; 16];
        getrandom::fill(&mut octets).expect("OS CSPRNG unavailable");
        let boundary = format!("vapor-{}", hex_encode(&octets));
        // The delimiter that could collide is "--<boundary>".
        if !contains_subslice(payload, format!("--{boundary}").as_bytes()) {
            return boundary;
        }
    }
}

/// Parses the last committed byte from a resumable-upload `Range` header
/// of the form `bytes=0-N`. Returns `None` when absent/unparsable.
fn parse_resumable_range_end(range: &str) -> Option<u64> {
    range
        .trim()
        .strip_prefix("bytes=")
        .and_then(|value| value.rsplit('-').next())
        .and_then(|end| end.trim().parse::<u64>().ok())
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Extracts `error.errors[0].reason` from a Drive JSON error body.
fn extract_403_reason(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("error")?
        .get("errors")?
        .get(0)?
        .get("reason")?
        .as_str()
        .map(ToString::to_string)
}

fn escape_query(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('\'', "\\'")
}

fn truncate(raw: &str, max: usize) -> String {
    raw.chars().take(max).collect()
}

fn hex_encode(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn md5_hex_of_bytes(bytes: &[u8]) -> String {
    hex_encode(&Md5::digest(bytes))
}

/// Minimal RFC 3339 UTC parser (`YYYY-MM-DDTHH:MM:SS(.mmm)Z`): Drive
/// always emits this shape; anything else reads as `None`.
fn parse_rfc3339_millis(raw: &str) -> Option<SystemTime> {
    let bytes = raw.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let year: i64 = raw.get(0..4)?.parse().ok()?;
    let month: i64 = raw.get(5..7)?.parse().ok()?;
    let day: i64 = raw.get(8..10)?.parse().ok()?;
    let hour: i64 = raw.get(11..13)?.parse().ok()?;
    let minute: i64 = raw.get(14..16)?.parse().ok()?;
    let second: i64 = raw.get(17..19)?.parse().ok()?;
    let millis: i64 = match raw.get(19..20) {
        Some(".") => {
            let fraction: String = raw[20..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let padded = format!("{fraction:0<3}");
            padded[..3].parse().ok()?
        }
        _ => 0,
    };

    // Days since the UNIX epoch (civil-days algorithm).
    let years = if month <= 2 { year - 1 } else { year };
    let era = if years >= 0 { years } else { years - 399 } / 400;
    let year_of_era = years - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    let total_millis = ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000 + millis;
    if total_millis < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(total_millis as u64))
}

#[cfg(test)]
mod tests;
