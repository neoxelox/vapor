//! Provider contract suite.
//!
//! Every provider implementation must satisfy the same behavioral
//! contract behind the `Provider` trait. The suite runs each contract
//! case against:
//!
//! - the real [`FilesystemProvider`] (the reference implementation),
//! - the same provider on a filesystem WITHOUT xattr support (the
//!   side-file constraint mode of FAT/network mounts),
//! - an in-memory mock with object-store-style constraints (no op-id
//!   tags, no changes feed: the S3/R2-shaped capability profile).
//!
//! Capability honesty is the core rule: a provider must implement what
//! it advertises and error loudly on what it does not, never silently
//! no-op.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use vapor_platform::fs_caps::{CaseSensitivity, InMemoryFilesystemCapabilities};
use vapor_providers::filesystem::hash_hex_of_bytes;
use vapor_providers::{
    ChangesPoll, DownloadRequest, FilesystemProvider, Provider, ProviderCapabilities,
    ProviderError, RemoteEntry, RemoteEntryKind, RemoteIdentity, RemotePath, RemotePrecondition,
    TransferOutcome, TransferSession, TransferStep, UploadRequest,
};
use vapor_shared::ProviderErrorKind;

/// One provider under contract, plus the sandbox it lives in.
struct ProviderUnderTest {
    name: &'static str,
    provider: Box<dyn Provider>,
    /// Whether this provider stores the engine's op-id tag on an object
    /// and gives it back. An entry of one that does not reads `None`,
    /// and the engine then tells its own writes by their hash.
    stores_op_ids: bool,
    _sandbox: Option<tempfile::TempDir>,
}

fn providers_under_test() -> Vec<ProviderUnderTest> {
    let mut providers = Vec::new();

    let sandbox = tempfile::TempDir::new().expect("sandbox");
    let cloud = sandbox.path().join("cloud");
    std::fs::create_dir_all(&cloud).expect("cloud root");
    providers.push(ProviderUnderTest {
        name: "filesystem",
        provider: Box::new(FilesystemProvider::with_root(&cloud).expect("provider")),
        stores_op_ids: true,
        _sandbox: Some(sandbox),
    });

    // Constraint mode: no xattr support → op-id tags land in
    // side-files, which must stay invisible.
    let sandbox = tempfile::TempDir::new().expect("sandbox");
    let cloud = sandbox.path().join("cloud");
    std::fs::create_dir_all(&cloud).expect("cloud root");
    let no_xattr_caps = Arc::new(InMemoryFilesystemCapabilities::new(
        false,
        CaseSensitivity::Sensitive,
    ));
    let (provider, _feed) =
        FilesystemProvider::with_manual_feed(&cloud, no_xattr_caps).expect("no-xattr provider");
    providers.push(ProviderUnderTest {
        name: "filesystem-no-xattr",
        provider: Box::new(provider),
        stores_op_ids: true,
        _sandbox: Some(sandbox),
    });

    providers.push(ProviderUnderTest {
        name: "object-store-mock",
        provider: Box::new(MockObjectStoreProvider::new()),
        stores_op_ids: false,
        _sandbox: None,
    });

    providers
}

fn drive(mut session: Box<dyn TransferSession>) -> Result<TransferOutcome, ProviderError> {
    loop {
        match session.step(64 * 1024)? {
            TransferStep::Progressed { .. } => continue,
            TransferStep::Completed(outcome) => return Ok(outcome),
        }
    }
}

fn upload(
    provider: &dyn Provider,
    scratch: &Path,
    remote: &str,
    content: &[u8],
    precondition: RemotePrecondition,
) -> Result<TransferOutcome, ProviderError> {
    let source = scratch.join("upload-source.tmp");
    std::fs::write(&source, content).expect("seed source");
    let session = provider.begin_upload(UploadRequest {
        local_source: source,
        remote_path: RemotePath::new(remote).expect("remote path"),
        op_id: format!("op-{remote}"),
        precondition,
    })?;
    drive(session)
}

#[test]
fn contract_upload_stat_download_delete_round_trip() {
    for under_test in providers_under_test() {
        let scratch = tempfile::TempDir::new().expect("scratch");
        let provider = under_test.provider.as_ref();
        let name = under_test.name;

        let outcome = upload(
            provider,
            scratch.path(),
            "docs/contract.txt",
            b"contract payload",
            RemotePrecondition::Absent,
        )
        .unwrap_or_else(|error| panic!("{name}: upload failed: {error}"));
        assert_eq!(
            outcome.content_hash,
            hash_hex_of_bytes(b"contract payload"),
            "{name}: upload outcome hash"
        );

        let entry = provider
            .stat(&RemotePath::new("docs/contract.txt").expect("path"))
            .unwrap_or_else(|error| panic!("{name}: stat failed: {error}"))
            .unwrap_or_else(|| panic!("{name}: uploaded object must stat"));
        assert_eq!(entry.kind, RemoteEntryKind::File, "{name}");
        assert_eq!(entry.size_bytes, 16, "{name}");

        // Op-id honesty: a provider that stores tags gives the tag
        // back; one that does not reads `None`, never a made-up value.
        if under_test.stores_op_ids {
            assert_eq!(
                entry.op_id.as_deref(),
                Some("op-docs/contract.txt"),
                "{name}: op-id must echo through stat"
            );
        } else {
            assert_eq!(entry.op_id, None, "{name}");
        }

        // Enumeration lists the parent and hides internals.
        let listed = provider
            .enumerate(&RemotePath::new("docs").expect("path"))
            .unwrap_or_else(|error| panic!("{name}: enumerate failed: {error}"));
        assert_eq!(listed.len(), 1, "{name}: exactly the uploaded file");

        // The listing that the walk asks for holds the same entries,
        // and a folder of names that all sync reports no other name.
        let listing = provider
            .enumerate_with_unsyncable(&RemotePath::new("docs").expect("path"))
            .unwrap_or_else(|error| panic!("{name}: listing failed: {error}"));
        assert_eq!(listing.entries, listed, "{name}");
        assert!(listing.unsyncable.is_empty(), "{name}");

        // Download round-trips the bytes.
        let destination = scratch.path().join("downloaded.bin");
        let outcome = provider
            .begin_download(DownloadRequest {
                remote_path: RemotePath::new("docs/contract.txt").expect("path"),
                destination: destination.clone(),
            })
            .and_then(drive)
            .unwrap_or_else(|error| panic!("{name}: download failed: {error}"));
        assert_eq!(outcome.bytes_total, 16, "{name}");
        assert_eq!(
            std::fs::read(&destination).expect("download destination"),
            b"contract payload",
            "{name}"
        );

        // Delete converges; a second delete reports NotFound.
        provider
            .delete(&RemotePath::new("docs/contract.txt").expect("path"), "op-d")
            .unwrap_or_else(|error| panic!("{name}: delete failed: {error}"));
        assert!(
            provider
                .stat(&RemotePath::new("docs/contract.txt").expect("path"))
                .unwrap_or_else(|error| panic!("{name}: stat failed: {error}"))
                .is_none(),
            "{name}: deleted object must not stat"
        );
        let second = provider
            .delete(
                &RemotePath::new("docs/contract.txt").expect("path"),
                "op-d2",
            )
            .expect_err("second delete must report NotFound");
        assert_eq!(second.kind, ProviderErrorKind::NotFound, "{name}");
    }
}

#[test]
fn contract_write_preconditions_are_honored() {
    // Every provider honours them: the engine guards each overwrite with
    // one, and a provider that ignored it would overwrite in silence.
    for under_test in providers_under_test() {
        let provider = under_test.provider.as_ref();
        let scratch = tempfile::TempDir::new().expect("scratch");
        let name = under_test.name;

        upload(
            provider,
            scratch.path(),
            "guarded.txt",
            b"version-1",
            RemotePrecondition::Absent,
        )
        .unwrap_or_else(|error| panic!("{name}: initial upload failed: {error}"));

        // Absent guard on an existing object must fail.
        let error = upload(
            provider,
            scratch.path(),
            "guarded.txt",
            b"version-2",
            RemotePrecondition::Absent,
        )
        .expect_err("absent precondition must fail on existing object");
        assert_eq!(error.kind, ProviderErrorKind::PreconditionFailed, "{name}");

        // Hash guard against the wrong version must fail; against the
        // right version must succeed.
        let error = upload(
            provider,
            scratch.path(),
            "guarded.txt",
            b"version-2",
            RemotePrecondition::HashEquals(hash_hex_of_bytes(b"not the content")),
        )
        .expect_err("wrong hash precondition must fail");
        assert_eq!(error.kind, ProviderErrorKind::PreconditionFailed, "{name}");

        upload(
            provider,
            scratch.path(),
            "guarded.txt",
            b"version-2",
            RemotePrecondition::HashEquals(hash_hex_of_bytes(b"version-1")),
        )
        .unwrap_or_else(|error| panic!("{name}: guarded overwrite failed: {error}"));
    }
}

#[test]
fn contract_changes_feed_reports_new_writes_or_is_unadvertised() {
    for under_test in providers_under_test() {
        let provider = under_test.provider.as_ref();
        let name = under_test.name;
        if !provider.capabilities().supports_remote_changes_feed {
            assert!(
                provider.poll_changes(None, 10).is_err(),
                "{name}: unadvertised feed must error"
            );
            continue;
        }
        // Feed correctness for the filesystem provider is covered by
        // its own suite (native watcher + manual feed); here we assert
        // the baseline poll shape only.
        match provider.poll_changes(None, 10) {
            Ok(ChangesPoll::Page(page)) => {
                assert!(page.changes.is_empty(), "{name}: baseline must be empty");
                assert!(!page.next_cursor.is_empty(), "{name}");
            }
            Ok(ChangesPoll::CursorExpired) => {
                panic!("{name}: a baseline poll must never expire")
            }
            Err(error) => panic!("{name}: baseline poll failed: {error}"),
        }
    }
}

#[test]
fn contract_a_provider_that_owes_no_work_after_a_write_reports_none_and_takes_no_record() {
    for under_test in providers_under_test() {
        let provider = under_test.provider.as_ref();
        let name = under_test.name;
        let scratch = tempfile::TempDir::new().expect("scratch");

        // A fresh write and a guarded overwrite: each of these backends
        // makes the check and the write one step, or holds the replaced
        // file in its hands at once, so nothing is left to do later.
        let first = upload(
            provider,
            scratch.path(),
            "notes/owed.txt",
            b"first",
            RemotePrecondition::Absent,
        )
        .unwrap_or_else(|error| panic!("{name}: upload failed: {error}"));
        upload(
            provider,
            scratch.path(),
            "notes/owed.txt",
            b"second",
            RemotePrecondition::HashEquals(first.content_hash),
        )
        .unwrap_or_else(|error| panic!("{name}: guarded overwrite failed: {error}"));
        assert!(
            provider.take_follow_up_changes().is_empty(),
            "{name}: no work is owed after a write"
        );

        // A record that another backend stored is not kept: the engine
        // forgets what the provider does not take.
        let records = vec!["a record of another backend".to_string()];
        assert_eq!(
            provider.resume_follow_ups(records.clone()),
            records,
            "{name}: a record it cannot do is handed straight back"
        );
    }
}

#[test]
fn contract_object_ids_are_for_every_object_or_for_none_and_stay_with_a_moved_object() {
    for under_test in providers_under_test() {
        let provider = under_test.provider.as_ref();
        let name = under_test.name;
        let scratch = tempfile::TempDir::new().expect("scratch");
        let path = |text: &str| RemotePath::new(text).expect("path");

        let written = upload(
            provider,
            scratch.path(),
            "ids/first.txt",
            b"first",
            RemotePrecondition::Absent,
        )
        .unwrap_or_else(|error| panic!("{name}: upload failed: {error}"));
        upload(
            provider,
            scratch.path(),
            "ids/second.txt",
            b"second",
            RemotePrecondition::Absent,
        )
        .unwrap_or_else(|error| panic!("{name}: upload failed: {error}"));

        // A folder and two files: a backend with ids has one for each
        // of them, and a backend without ids has none anywhere. The
        // engine pairs a renamed entry by its id, so an id on some
        // entries only would pair some renames and not others.
        let mut entries = provider
            .enumerate(&RemotePath::root())
            .unwrap_or_else(|error| panic!("{name}: enumerate failed: {error}"));
        entries.extend(
            provider
                .enumerate(&path("ids"))
                .unwrap_or_else(|error| panic!("{name}: enumerate failed: {error}")),
        );
        assert_eq!(entries.len(), 3, "{name}: {entries:?}");
        let ids: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry.id.as_deref())
            .collect();
        // No backend of this suite has ids today, so the branch for a
        // backend with ids runs for none of them: what is locked here
        // is that a backend without ids reports none anywhere. The
        // Google Drive tests on the scripted transport lock the ids.
        let has_ids = !ids.is_empty();
        assert!(
            ids.is_empty() || ids.len() == entries.len(),
            "{name}: an id for every entry or for none: {entries:?}"
        );
        assert_eq!(
            written.remote_id.is_some(),
            has_ids,
            "{name}: a transfer reports the id of its object when the backend has ids"
        );
        let stat_id = |text: &str| {
            provider
                .stat(&path(text))
                .unwrap_or_else(|error| panic!("{name}: stat failed: {error}"))
                .and_then(|entry| entry.id)
        };
        if has_ids {
            let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
            assert_eq!(unique.len(), ids.len(), "{name}: one id, one object");
            assert_eq!(
                stat_id("ids/first.txt"),
                written.remote_id,
                "{name}: a lookup and a transfer name the object by the same id"
            );
        } else {
            assert_eq!(stat_id("ids/first.txt"), None, "{name}");
        }

        // The id is the object's, not the path's: it stays under a new
        // name.
        if provider.capabilities().supports_server_side_move {
            let before = stat_id("ids/first.txt");
            provider
                .move_object(&path("ids/first.txt"), &path("ids/moved.txt"), "op-move")
                .unwrap_or_else(|error| panic!("{name}: move failed: {error}"));
            assert_eq!(stat_id("ids/moved.txt"), before, "{name}");
        }

        // The ids that the engine stored come back at a start. They
        // are what the engine believed: an id for a path makes no
        // object, and a backend without ids keeps nothing.
        provider.restore_remote_ids(vec![RemoteIdentity {
            path: path("ids/never-there.txt"),
            kind: RemoteEntryKind::File,
            id: "an-id-of-no-object".to_string(),
        }]);
        assert!(
            provider
                .stat(&path("ids/never-there.txt"))
                .is_ok_and(|entry| entry.is_none()),
            "{name}: a path that came back is no object"
        );
        assert_eq!(
            provider
                .enumerate(&path("ids"))
                .unwrap_or_else(|error| panic!("{name}: enumerate failed: {error}"))
                .len(),
            2,
            "{name}: the listing is what the backend holds"
        );
    }
}

#[test]
fn contract_scope_rejects_traversal_shapes_at_the_type_boundary() {
    // RemotePath is the shared scope-safety gate: no provider can even
    // be asked to touch a traversal path.
    assert!(RemotePath::new("../escape").is_err());
    assert!(RemotePath::new("a/../../b").is_err());
    assert!(RemotePath::new("/absolute").is_err());
}

/// Cheap adapter-overhead guard: a hundred small uploads through the
/// full session machinery must complete well inside a budget sized
/// for a regression, not for a slow disk. Each upload fsyncs, and a
/// Windows CI runner spends about 50 ms on that alone, so the budget
/// is an order of magnitude above a slow host's normal: it catches
/// "someone made every session step allocate/copy quadratically",
/// not SLO-grade measurement (that is Tier 2).
#[test]
fn timing_guardrail_session_overhead_stays_under_budget() {
    let sandbox = tempfile::TempDir::new().expect("sandbox");
    let cloud = sandbox.path().join("cloud");
    std::fs::create_dir_all(&cloud).expect("cloud root");
    let provider = FilesystemProvider::with_root(&cloud).expect("provider");
    let scratch = tempfile::TempDir::new().expect("scratch");

    let started = std::time::Instant::now();
    for index in 0..100 {
        upload(
            &provider,
            scratch.path(),
            &format!("perf/file-{index}.txt"),
            format!("payload {index}").as_bytes(),
            RemotePrecondition::None,
        )
        .expect("upload");
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "100 small uploads took {elapsed:?}; the adapter overhead regressed"
    );
}

// ---------------------------------------------------------------------
// Object-store-style mock: content-addressed blobs, no rename, no tags,
// no changes feed (S3/R2-shaped capability profile).
// ---------------------------------------------------------------------

struct MockObjectStoreProvider {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MockObjectStoreProvider {
    fn new() -> Self {
        Self {
            objects: Mutex::new(BTreeMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.objects.lock().expect("mock store mutex")
    }
}

impl Provider for MockObjectStoreProvider {
    fn name(&self) -> &'static str {
        "object_store_mock"
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
        Ok(())
    }

    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
        let prefix = if directory.is_root() {
            String::new()
        } else {
            format!("{}/", directory.as_str())
        };
        let objects = self.lock();
        let mut entries = Vec::new();
        let mut seen_dirs = std::collections::BTreeSet::new();
        for (key, content) in objects.iter() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((child_dir, _)) => {
                    if seen_dirs.insert(child_dir.to_string()) {
                        entries.push(RemoteEntry {
                            path: directory.join(child_dir).expect("child dir"),
                            kind: RemoteEntryKind::Directory,
                            size_bytes: 0,
                            modified_at: SystemTime::UNIX_EPOCH,
                            content_hash: None,
                            op_id: None,
                            id: None,
                        });
                    }
                }
                None => entries.push(RemoteEntry {
                    path: directory.join(rest).expect("child file"),
                    kind: RemoteEntryKind::File,
                    size_bytes: content.len() as u64,
                    modified_at: SystemTime::UNIX_EPOCH,
                    content_hash: Some(hash_hex_of_bytes(content)),
                    op_id: None,
                    id: None,
                }),
            }
        }
        Ok(entries)
    }

    fn stat(&self, path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        Ok(self.lock().get(path.as_str()).map(|content| RemoteEntry {
            path: path.clone(),
            kind: RemoteEntryKind::File,
            size_bytes: content.len() as u64,
            modified_at: SystemTime::UNIX_EPOCH,
            content_hash: Some(hash_hex_of_bytes(content)),
            op_id: None,
            id: None,
        }))
    }

    fn content_hash(&self, path: &RemotePath) -> Result<String, ProviderError> {
        self.lock()
            .get(path.as_str())
            .map(|content| hash_hex_of_bytes(content))
            .ok_or_else(|| ProviderError::not_found(format!("no object at {path}")))
    }

    fn begin_upload(
        &self,
        request: UploadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let content = std::fs::read(&request.local_source)
            .map_err(|error| ProviderError::transient(format!("cannot read source: {error}")))?;
        let mut objects = self.lock();
        match &request.precondition {
            RemotePrecondition::None => {}
            RemotePrecondition::Absent => {
                if objects.contains_key(request.remote_path.as_str()) {
                    return Err(ProviderError::precondition_failed("object exists"));
                }
            }
            RemotePrecondition::HashEquals(expected) => {
                let current = objects
                    .get(request.remote_path.as_str())
                    .map(|content| hash_hex_of_bytes(content));
                if current.as_ref() != Some(expected) {
                    return Err(ProviderError::precondition_failed("object diverged"));
                }
            }
        }
        let hash = hash_hex_of_bytes(&content);
        let bytes = content.len() as u64;
        objects.insert(request.remote_path.as_str().to_string(), content);
        Ok(Box::new(ImmediateSession {
            outcome: Some(TransferOutcome {
                bytes_total: bytes,
                content_hash: hash,
                remote_modified_at: None,
                remote_id: None,
            }),
        }))
    }

    fn begin_download(
        &self,
        request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let content = self
            .lock()
            .get(request.remote_path.as_str())
            .cloned()
            .ok_or_else(|| {
                ProviderError::not_found(format!("no object at {}", request.remote_path))
            })?;
        std::fs::write(&request.destination, &content)
            .map_err(|error| ProviderError::transient(format!("cannot write dest: {error}")))?;
        Ok(Box::new(ImmediateSession {
            outcome: Some(TransferOutcome {
                bytes_total: content.len() as u64,
                content_hash: hash_hex_of_bytes(&content),
                remote_modified_at: None,
                remote_id: None,
            }),
        }))
    }

    fn delete(&self, path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
        if self.lock().remove(path.as_str()).is_none() {
            return Err(ProviderError::not_found(format!("no object at {path}")));
        }
        Ok(())
    }

    fn poll_changes(
        &self,
        _cursor: Option<&str>,
        _max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
        Err(ProviderError::permanent("no changes feed"))
    }
}

struct ImmediateSession {
    outcome: Option<TransferOutcome>,
}

impl TransferSession for ImmediateSession {
    fn step(&mut self, _max_bytes: u64) -> Result<TransferStep, ProviderError> {
        Ok(TransferStep::Completed(
            self.outcome
                .take()
                .expect("session stepped past completion"),
        ))
    }

    fn abort(&mut self) {}
}
