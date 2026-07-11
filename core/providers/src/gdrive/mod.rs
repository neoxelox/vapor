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
//! - **Root handling**: `ensure_cloud_sync_directory`
//!   resolves or creates the configured folder chain; failures surface
//!   as actionable errors and the engine blocks sync until it works.
//! - **Uploads**: multipart for small payloads, resumable
//!   chunked sessions for large ones, chunk size auto-tuned across
//!   sessions, rate-limit aware classification throughout.
//! - **Changes**: `changes.list` with a persisted page token;
//!   a 410 maps to `ChangesPoll::CursorExpired`, which the engine
//!   answers with a whole-scope reconcile + re-baseline.
//! - **Deletes** move files to the Drive trash (recoverable) rather
//!   than purging — consistent with Vapor's data-preservation posture.
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
const OP_ID_PROPERTY: &str = "vaporOpId";
/// Payloads at or under this size upload in one multipart request.
const SIMPLE_UPLOAD_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Resumable chunks must be multiples of 256 KiB per the API contract.
const CHUNK_GRANULARITY: u64 = 256 * 1024;
const CHUNK_MIN: u64 = 256 * 1024;
const CHUNK_MAX: u64 = 64 * 1024 * 1024;
const CHUNK_START: u64 = 8 * 1024 * 1024;
/// Refresh this long before the recorded expiry.
const TOKEN_REFRESH_MARGIN_MS: u64 = 60_000;

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

struct TokenManager {
    config: GdriveConfig,
    secrets: Arc<dyn SecretStore>,
    transport: Arc<dyn HttpTransport>,
    cached: Mutex<Option<StoredTokens>>,
}

impl TokenManager {
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }

    fn load(&self) -> Result<StoredTokens, ProviderError> {
        if let Some(tokens) = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return Ok(tokens);
        }
        let raw = self
            .secrets
            .get(&self.config.token_secret_name())
            .map_err(|_| {
                ProviderError::authentication(format!(
                    "no Google Drive credentials for profile '{}'; run `vapor auth login gdrive --profile {}`",
                    self.config.profile_id, self.config.profile_id
                ))
            })?;
        let tokens: StoredTokens = serde_json::from_str(&raw).map_err(|_| {
            ProviderError::authentication(
                "stored Google Drive credentials are unreadable; run `vapor auth login gdrive`",
            )
        })?;
        *self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tokens.clone());
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
        self.force_refresh(&tokens)
    }

    fn force_refresh(&self, current: &StoredTokens) -> Result<String, ProviderError> {
        let Some(refresh_token) = current.refresh_token.as_deref() else {
            return Err(ProviderError::authentication(
                "Google Drive access token expired and no refresh token is stored; run `vapor auth login gdrive`",
            ));
        };
        let refreshed = oauth::refresh_tokens(
            self.transport.as_ref(),
            &self.config.client_id,
            self.config.client_secret.as_deref(),
            refresh_token,
            Self::now_ms(),
        )?;
        self.store(&refreshed);
        Ok(refreshed.access_token)
    }

    fn invalidate(&self) {
        *self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
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

// ---------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------

pub struct GoogleDriveProvider {
    tokens: TokenManager,
    transport: Arc<dyn HttpTransport>,
    /// Resolved id of the configured cloud root folder.
    root_id: Mutex<Option<String>>,
    /// Path → file-id cache; pruned on deletes/renames.
    id_by_path: Mutex<BTreeMap<String, String>>,
    /// File-id → path cache for changes mapping.
    path_by_id: Mutex<BTreeMap<String, String>>,
    /// Learned resumable chunk size, shared across sessions.
    chunk_hint: Arc<Mutex<u64>>,
}

impl GoogleDriveProvider {
    pub fn new(
        config: GdriveConfig,
        secrets: Arc<dyn SecretStore>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            tokens: TokenManager {
                config,
                secrets,
                transport: transport.clone(),
                cached: Mutex::new(None),
            },
            transport,
            root_id: Mutex::new(None),
            id_by_path: Mutex::new(BTreeMap::new()),
            path_by_id: Mutex::new(BTreeMap::new()),
            chunk_hint: Arc::new(Mutex::new(CHUNK_START)),
        }
    }

    /// Production constructor: native transport + native secret store,
    /// client credentials from the environment (installed-app PKCE has
    /// no embeddable secret; the id is per-deployment, see
    /// `docs/operations/provider-auth-operations.md`).
    pub fn for_profile(profile_id: &str) -> Result<Self, ProviderError> {
        let client_id = std::env::var(vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_ID)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ProviderError::authentication(format!(
                    "{} is not set; the Google Drive provider needs an OAuth client id",
                    vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_ID
                ))
            })?;
        let client_secret = std::env::var(vapor_shared::constants::env::VAPOR_GDRIVE_CLIENT_SECRET)
            .ok()
            .filter(|value| !value.trim().is_empty());
        let secrets = vapor_platform::NativeSecretStore::for_current_user()
            .map(|store| Arc::new(store) as Arc<dyn SecretStore>)
            .map_err(|error| {
                ProviderError::authentication(format!("secret store unavailable: {error}"))
            })?;
        Ok(Self::new(
            GdriveConfig {
                client_id,
                client_secret,
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
        let mut attempt = 0;
        loop {
            let token = self.tokens.access_token()?;
            let mut all_headers = headers.clone();
            all_headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            let response = self
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
                // Stale token despite the expiry margin: force one
                // refresh and retry.
                attempt += 1;
                self.tokens.invalidate();
                let current = self.tokens.load()?;
                self.tokens.force_refresh(&current)?;
                continue;
            }
            return Ok(response);
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

    fn evict_path(&self, path: &str) {
        let id = self
            .id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(path);
        if let Some(id) = id {
            self.path_by_id
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
        }
    }

    fn find_child(&self, parent_id: &str, name: &str) -> Result<Option<GdFile>, ProviderError> {
        let query = format!(
            "'{}' in parents and name = '{}' and trashed = false",
            escape_query(parent_id),
            escape_query(name)
        );
        let url = format!(
            "{API_BASE}/files?q={}&fields=files({FILE_FIELDS})&pageSize=2",
            oauth::url_encode(&query)
        );
        let list: GdFileList = self.api_json("GET", url, None)?;
        Ok(list.files.into_iter().next())
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
        if let Some(id) = self
            .id_by_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path.as_str())
            .cloned()
        {
            // Cached ids may be stale after external deletes; the
            // caller-facing operations verify via the API where it
            // matters (stat re-fetches, deletes surface 404s).
            let url = format!("{API_BASE}/files/{id}?fields={FILE_FIELDS}");
            match self.api_json::<GdFile>("GET", url, None) {
                Ok(file) if !file.trashed => return Ok(Some(file)),
                Ok(_) => {
                    self.evict_path(path.as_str());
                    return Ok(None);
                }
                Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                    self.evict_path(path.as_str());
                    return Ok(None);
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
        let mut parent_id = root_id;
        let mut walked = String::new();
        for segment in parent.as_str().split('/') {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(segment);
            parent_id = match self.find_child(&parent_id, segment)? {
                Some(existing) => existing.id,
                None => self.create_folder(&parent_id, segment)?,
            };
            self.cache_mapping(&walked, &parent_id);
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
    fn path_for_changed_file(&self, file: &GdFile) -> Option<String> {
        if let Some(path) = self
            .path_by_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&file.id)
            .cloned()
        {
            return Some(path);
        }
        let root_id = self
            .root_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()?;
        // Walk up through parents until the root (bounded depth).
        let mut segments = vec![file.name.clone()];
        let mut current_parent = file.parents.first().cloned()?;
        for _ in 0..64 {
            if current_parent == root_id {
                segments.reverse();
                let path = segments.join("/");
                self.cache_mapping(&path, &file.id);
                return Some(path);
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
                let path = format!("{parent_path}/{}", segments.join("/"));
                self.cache_mapping(&path, &file.id);
                return Some(path);
            }
            let url = format!("{API_BASE}/files/{current_parent}?fields={FILE_FIELDS}");
            let parent: GdFile = self.api_json("GET", url, None).ok()?;
            segments.push(parent.name.clone());
            current_parent = parent.parents.first().cloned()?;
        }
        None
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

    fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) -> Result<(), ProviderError> {
        let segments: Vec<&str> = cloud_sync_directory
            .trim()
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        if segments.is_empty() {
            return Err(ProviderError::permanent(
                "cloudSyncDirectory must name a folder inside Google Drive (for example \"/Vapor\")",
            ));
        }
        let mut parent_id = "root".to_string();
        for segment in &segments {
            parent_id = match self.find_child(&parent_id, segment)? {
                Some(existing) if existing.mime_type == FOLDER_MIME => existing.id,
                Some(_) => {
                    return Err(ProviderError::permanent(format!(
                        "cloudSyncDirectory component '{segment}' exists in Drive but is not a folder"
                    )));
                }
                None => self.create_folder(&parent_id, segment)?,
            };
        }
        logging::info(
            "Google Drive sync root is ready",
            &[("cloud_sync_directory", cloud_sync_directory.to_string())],
        );
        *self
            .root_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(parent_id);
        Ok(())
    }

    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
        let Some(folder) = self.resolve(directory)? else {
            return Err(ProviderError::not_found(format!(
                "remote directory {directory} does not exist"
            )));
        };
        let mut entries = Vec::new();
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
                let Ok(child) = directory.join(&file.name) else {
                    continue;
                };
                entries.push(self.entry_from_file(&child, &file));
            }
            match list.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    fn stat(&self, path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        Ok(self
            .resolve(path)?
            .map(|file| self.entry_from_file(path, &file)))
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

        Ok(Box::new(GdriveUploadSession {
            state: UploadState::NotStarted,
            provider_transport: self.transport.clone(),
            token_manager_config: self.tokens.config.clone(),
            secrets: self.tokens.secrets.clone(),
            file_id: existing.map(|file| file.id),
            metadata,
            source,
            total_bytes,
            sent_bytes: 0,
            chunk_hint: self.chunk_hint.clone(),
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
            provider: ProviderHandle {
                transport: self.transport.clone(),
                config: self.tokens.config.clone(),
                secrets: self.tokens.secrets.clone(),
            },
            file_id: file.id,
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

    fn rename(&self, from: &RemotePath, to: &RemotePath, op_id: &str) -> Result<(), ProviderError> {
        let file = self.resolve(from)?.ok_or_else(|| {
            ProviderError::not_found(format!("rename source {from} does not exist"))
        })?;
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

    fn poll_changes(
        &self,
        cursor: Option<&str>,
        max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
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
                });
                continue;
            }
            let Some(file) = &change.file else { continue };
            if file.mime_type == FOLDER_MIME {
                continue;
            }
            let Some(path) = self.path_for_changed_file(file) else {
                // Outside the sync root (or unmappable): not ours.
                continue;
            };
            let Ok(remote_path) = RemotePath::new(path) else {
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
            });
        }

        let next_cursor = list
            .next_page_token
            .or(list.new_start_page_token)
            .unwrap_or_else(|| cursor.to_string());
        Ok(ChangesPoll::Page(RemoteChangesPage {
            changes,
            next_cursor,
        }))
    }
}

/// Minimal token+transport handle for sessions (they outlive the
/// borrow of the provider).
struct ProviderHandle {
    transport: Arc<dyn HttpTransport>,
    config: GdriveConfig,
    secrets: Arc<dyn SecretStore>,
}

impl ProviderHandle {
    fn execute_authed(
        &self,
        method: &'static str,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<HttpResponse, ProviderError> {
        let manager = TokenManager {
            config: self.config.clone(),
            secrets: self.secrets.clone(),
            transport: self.transport.clone(),
            cached: Mutex::new(None),
        };
        let token = manager.access_token()?;
        let mut all_headers = headers;
        all_headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        self.transport
            .execute(HttpRequest {
                method,
                url,
                headers: all_headers,
                body,
            })
            .map_err(|error| {
                ProviderError::transient(format!("Drive API unreachable: {}", error.message))
            })
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
    provider_transport: Arc<dyn HttpTransport>,
    token_manager_config: GdriveConfig,
    secrets: Arc<dyn SecretStore>,
    /// `Some` when updating an existing file.
    file_id: Option<String>,
    metadata: serde_json::Value,
    source: std::fs::File,
    total_bytes: u64,
    sent_bytes: u64,
    chunk_hint: Arc<Mutex<u64>>,
}

impl GdriveUploadSession {
    fn handle(&self) -> ProviderHandle {
        ProviderHandle {
            transport: self.provider_transport.clone(),
            config: self.token_manager_config.clone(),
            secrets: self.secrets.clone(),
        }
    }

    fn simple_multipart(&mut self) -> Result<TransferOutcome, ProviderError> {
        let mut content = Vec::with_capacity(self.total_bytes as usize);
        self.source.read_to_end(&mut content).map_err(|error| {
            ProviderError::transient(format!("cannot read upload source: {error}"))
        })?;
        let boundary = "vapor-multipart-boundary";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
        body.extend_from_slice(self.metadata.to_string().as_bytes());
        body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(&content);
        body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());

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
        let response = self.handle().execute_authed(
            method,
            url,
            vec![(
                "Content-Type".to_string(),
                format!("multipart/related; boundary={boundary}"),
            )],
            body,
        )?;
        if response.status >= 300 {
            return Err(classify_api_failure(&response));
        }
        let file: GdFile = serde_json::from_slice(&response.body).map_err(|error| {
            ProviderError::transient(format!("unparsable upload response: {error}"))
        })?;
        self.state = UploadState::Done;
        Ok(TransferOutcome {
            bytes_total: self.total_bytes,
            content_hash: file
                .md5_checksum
                .unwrap_or_else(|| md5_hex_of_bytes(&content)),
        })
    }

    fn initiate_resumable(&mut self) -> Result<String, ProviderError> {
        let (method, url): (&'static str, String) = match &self.file_id {
            Some(id) => (
                "PATCH",
                format!("{UPLOAD_BASE}/files/{id}?uploadType=resumable"),
            ),
            None => ("POST", format!("{UPLOAD_BASE}/files?uploadType=resumable")),
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

                let range_end = self.sent_bytes + chunk_len - 1;
                let response = self.handle().execute_authed(
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
                )?;
                match response.status {
                    // 308: chunk accepted, upload incomplete.
                    308 => {
                        self.sent_bytes += chunk_len;
                        self.grow_chunk_hint();
                        Ok(TransferStep::Progressed {
                            bytes_transferred: chunk_len,
                        })
                    }
                    200 | 201 => {
                        self.sent_bytes += chunk_len;
                        self.state = UploadState::Done;
                        let file: GdFile =
                            serde_json::from_slice(&response.body).unwrap_or_default();
                        Ok(TransferStep::Completed(TransferOutcome {
                            bytes_total: self.total_bytes,
                            content_hash: file.md5_checksum.unwrap_or_default(),
                        }))
                    }
                    _ => {
                        let error = classify_api_failure(&response);
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
    destination: Option<std::fs::File>,
    destination_path: std::path::PathBuf,
    total_bytes: u64,
    received_bytes: u64,
    hasher: Md5,
    finished: bool,
}

impl TransferSession for GdriveDownloadSession {
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
        // A 200 (full body) or a short remainder completes the payload.
        if response.status == 200 || self.received_bytes >= self.total_bytes {
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
            destination.sync_all().map_err(|error| {
                ProviderError::transient(format!("cannot sync downloaded payload: {error}"))
            })?;
        }
        self.finished = true;
        let digest = std::mem::replace(&mut self.hasher, Md5::new()).finalize();
        Ok(TransferStep::Completed(TransferOutcome {
            bytes_total: self.received_bytes,
            content_hash: hex_encode(&digest),
        }))
    }
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
            "Google Drive rejected the credentials; run `vapor auth login gdrive`",
        ),
        403 if body_text.contains("ateLimitExceeded") || body_text.contains("quotaExceeded") => {
            ProviderError::rate_limited(retry_after, "Drive rate limit exceeded")
        }
        403 => ProviderError::permanent(format!(
            "Drive denied the operation (403): {}",
            truncate(&body_text, 200)
        )),
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

/// Minimal RFC 3339 UTC parser (`YYYY-MM-DDTHH:MM:SS(.mmm)Z`) — Drive
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
