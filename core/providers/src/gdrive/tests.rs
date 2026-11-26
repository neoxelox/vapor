//! Offline Google Drive provider tests over the scripted transport.

use std::sync::Arc;

use super::*;
use crate::http::ScriptedHttpTransport;
use vapor_platform::{InMemorySecretStore, SecretStore};
use vapor_shared::ProviderErrorKind;

fn provider_with(transport: Arc<ScriptedHttpTransport>) -> GoogleDriveProvider {
    let secrets = InMemorySecretStore::new();
    secrets
        .set(
            "auth.default.gdrive.token",
            &format!(
                r#"{{"accessToken":"ya29.test","refreshToken":"1//rt","expiresAtMs":{}}}"#,
                // Far future so no refresh fires unless a test wants it.
                u64::MAX / 2
            ),
        )
        .expect("seed tokens");
    GoogleDriveProvider::new(
        GdriveConfig {
            client_id: "client".to_string(),
            client_secret: None,
            profile_id: "default".to_string(),
        },
        Arc::new(secrets),
        transport,
    )
}

fn ensured_provider(transport: Arc<ScriptedHttpTransport>) -> GoogleDriveProvider {
    // ensure: find "Vapor" under root (exists).
    transport.push_response(
        200,
        r#"{"files":[{"id":"root-folder","name":"Vapor","mimeType":"application/vnd.google-apps.folder"}]}"#,
    );
    let provider = provider_with(transport);
    provider.ensure_cloud_root("/Vapor").expect("ensure");
    provider
}

#[test]
fn ensure_creates_missing_folder_chain() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    // "Vapor" missing under root → create; "Docs" missing → create.
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, r#"{"id":"created-vapor"}"#);
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, r#"{"id":"created-docs"}"#);

    let provider = provider_with(transport.clone());
    provider
        .ensure_cloud_root("/Vapor/Docs")
        .expect("ensure creates the chain");

    let requests = transport.recorded_requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].url.contains("/files?fields=id"));
    let create_body: serde_json::Value =
        serde_json::from_slice(&requests[1].body).expect("json body");
    assert_eq!(create_body["name"], "Vapor");
    assert_eq!(
        create_body["mimeType"],
        "application/vnd.google-apps.folder"
    );
    // Auth header present, token never in the URL.
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value.starts_with("Bearer ")),
    );
}

#[test]
fn enumerate_maps_files_with_op_ids_and_hashes() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[
            {"id":"f1","name":"report.md","mimeType":"text/markdown","size":"42",
             "md5Checksum":"abc123","appProperties":{"vaporOpId":"dev-op7"},
             "modifiedTime":"2026-07-06T12:30:00.000Z"},
            {"id":"d1","name":"sub","mimeType":"application/vnd.google-apps.folder"}
        ]}"#,
    );

    let entries = provider.enumerate(&RemotePath::root()).expect("enumerate");
    assert_eq!(entries.len(), 2);
    let file = entries
        .iter()
        .find(|e| e.path.as_str() == "report.md")
        .expect("file");
    assert_eq!(file.kind, RemoteEntryKind::File);
    assert_eq!(file.size_bytes, 42);
    assert_eq!(file.content_hash.as_deref(), Some("abc123"));
    assert_eq!(file.op_id.as_deref(), Some("dev-op7"));
    assert!(file.modified_at > SystemTime::UNIX_EPOCH);
    let folder = entries
        .iter()
        .find(|e| e.path.as_str() == "sub")
        .expect("folder");
    assert_eq!(folder.kind, RemoteEntryKind::Directory);
    // Each entry carries the id that stays with its object.
    assert_eq!(file.id.as_deref(), Some("f1"));
    assert_eq!(folder.id.as_deref(), Some("d1"));
}

#[test]
fn small_upload_goes_multipart_and_reports_the_drive_md5() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // resolve target (absent) → parent is root → multipart create.
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, r#"{"id":"new-file","md5Checksum":"served-md5"}"#);

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("small.txt");
    std::fs::write(&source, b"small payload").expect("seed");

    let mut session = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("small.txt").expect("path"),
            op_id: "dev-op1".to_string(),
            precondition: RemotePrecondition::Absent,
        })
        .expect("session");
    let outcome = match session.step(u64::MAX).expect("step") {
        TransferStep::Completed(outcome) => outcome,
        other => panic!("small upload completes in one step, got {other:?}"),
    };
    assert_eq!(outcome.content_hash, "served-md5");
    assert_eq!(
        outcome.remote_id.as_deref(),
        Some("new-file"),
        "the engine stores the id of the object it wrote"
    );

    let requests = transport.recorded_requests();
    let upload = requests.last().expect("upload request");
    assert!(upload.url.contains("uploadType=multipart"));
    let body = String::from_utf8_lossy(&upload.body);
    assert!(
        body.contains("vaporOpId"),
        "op-id must ride in appProperties"
    );
    assert!(body.contains("small payload"));
}

#[test]
fn multipart_upload_reverifies_hash_precondition_and_aborts_on_drift() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // resolve the target: it exists with the hash we planned against.
    transport.push_response(
        200,
        r#"{"files":[{"id":"victim","name":"doc.txt","mimeType":"text/plain","md5Checksum":"orig-md5","size":"5"}]}"#,
    );
    // The pre-commit re-stat sees a different hash: the remote moved under us
    // between planning and the (possibly long-deferred) commit.
    transport.push_response(
        200,
        r#"{"id":"victim","name":"doc.txt","mimeType":"text/plain","md5Checksum":"raced-md5","size":"7"}"#,
    );

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("doc.txt");
    std::fs::write(&source, b"local").expect("seed");

    let mut session = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("doc.txt").expect("path"),
            op_id: "dev-op-race".to_string(),
            precondition: RemotePrecondition::HashEquals("orig-md5".to_string()),
        })
        .expect("session opens: planning-time hash still matched");

    let error = session.step(u64::MAX).expect_err("commit must be refused");
    assert_eq!(error.kind, ProviderErrorKind::PreconditionFailed);

    // The committing multipart request must never have gone out: the last
    // wire call is the read-only re-stat, not a PATCH.
    let requests = transport.recorded_requests();
    let last = requests.last().expect("at least the re-stat ran");
    assert!(
        last.url.contains("files/victim") && !last.url.contains("uploadType=multipart"),
        "expected the re-stat GET to be the final call, got {}",
        last.url
    );
    assert!(
        !requests
            .iter()
            .any(|r| r.url.contains("uploadType=multipart")),
        "no multipart commit may be issued once the precondition drifts"
    );
}

#[test]
fn resumable_upload_reverifies_hash_precondition_before_the_final_chunk() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // resolve the target: exists with the planned hash.
    transport.push_response(
        200,
        r#"{"files":[{"id":"big-victim","name":"big.bin","mimeType":"application/octet-stream","md5Checksum":"orig-md5","size":"5"}]}"#,
    );
    // initiate resumable session.
    transport.push_response_with_headers(
        200,
        "",
        vec![(
            "Location".to_string(),
            "https://upload.example/session-race".to_string(),
        )],
    );
    transport.push_response(308, ""); // first chunk accepted
    // Re-stat right before the final chunk observes a drifted hash.
    transport.push_response(
        200,
        r#"{"id":"big-victim","name":"big.bin","mimeType":"application/octet-stream","md5Checksum":"raced-md5","size":"9"}"#,
    );

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("big.bin");
    std::fs::write(
        &source,
        vec![7_u8; (SIMPLE_UPLOAD_MAX_BYTES + CHUNK_GRANULARITY) as usize],
    )
    .expect("seed");

    let mut session = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("big.bin").expect("path"),
            op_id: "dev-op-race2".to_string(),
            precondition: RemotePrecondition::HashEquals("orig-md5".to_string()),
        })
        .expect("session opens");

    // Initiation + first chunk proceed; only the final chunk trips the guard.
    assert!(matches!(
        session.step(SIMPLE_UPLOAD_MAX_BYTES).expect("initiate"),
        TransferStep::Progressed { .. }
    ));
    assert!(matches!(
        session.step(SIMPLE_UPLOAD_MAX_BYTES).expect("chunk 1"),
        TransferStep::Progressed { .. }
    ));
    let error = session
        .step(u64::MAX)
        .expect_err("final chunk must be refused");
    assert_eq!(error.kind, ProviderErrorKind::PreconditionFailed);

    // The terminating chunk (whose Content-Range ends at total-1) must never
    // have gone out: the last wire call is the read-only re-stat GET.
    let requests = transport.recorded_requests();
    let terminating_range = format!("-{}/", SIMPLE_UPLOAD_MAX_BYTES + CHUNK_GRANULARITY - 1);
    assert!(
        !requests.iter().any(|r| r
            .headers
            .iter()
            .any(|(name, value)| name == "Content-Range" && value.contains(&terminating_range))),
        "the final committing chunk must not be sent after the precondition drifts"
    );
    let last = requests.last().expect("at least the re-stat ran");
    assert!(
        last.url.contains("files/big-victim"),
        "expected the re-stat GET to be the final call, got {}",
        last.url
    );
}

#[test]
fn a_resumable_upload_whose_final_answer_has_no_checksum_reports_the_hash_of_its_bytes() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(200, r#"{"files":[]}"#); // resolve absent
    transport.push_response_with_headers(
        200,
        "",
        vec![(
            "Location".to_string(),
            "https://upload.example/session-2".to_string(),
        )],
    );
    // Drive acknowledges only half of the first chunk, so the session
    // sends the rest of it again: those bytes are hashed one time.
    let half = SIMPLE_UPLOAD_MAX_BYTES / 2;
    transport.push_response_with_headers(
        308,
        "",
        vec![("Range".to_string(), format!("bytes=0-{}", half - 1))],
    );
    transport.push_response(308, ""); // the rest of the first chunk
    // What Drive answers when the session was opened without `fields`:
    // the default set, with no checksum and no modified time.
    transport.push_response(
        200,
        r#"{"kind":"drive#file","id":"big","name":"big.bin","mimeType":"application/octet-stream"}"#,
    );

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("big.bin");
    let content: Vec<u8> = (0..(SIMPLE_UPLOAD_MAX_BYTES + CHUNK_GRANULARITY))
        .map(|index| (index % 251) as u8)
        .collect();
    std::fs::write(&source, &content).expect("seed");

    let mut session = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("big.bin").expect("path"),
            op_id: "dev-op3".to_string(),
            precondition: RemotePrecondition::None,
        })
        .expect("session");
    let outcome = loop {
        match session.step(SIMPLE_UPLOAD_MAX_BYTES).expect("step") {
            TransferStep::Completed(outcome) => break outcome,
            TransferStep::Progressed { .. } => {}
        }
    };
    assert_eq!(
        outcome.content_hash,
        md5_hex_of_bytes(&content),
        "an empty hash in the sync index makes the next edit of the file impossible to upload"
    );
}

#[test]
fn a_lookup_binds_a_path_only_to_a_file_the_listing_shows_under_that_name() {
    // The query can answer for another spelling of the name, and for a
    // Google Doc, which the listing skips. Neither one is the file at
    // this path, whatever its id.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[
            {"id":"a-other-case","name":"README.md","mimeType":"text/plain","size":"1","md5Checksum":"upper"},
            {"id":"b-the-file","name":"readme.md","mimeType":"text/plain","size":"2","md5Checksum":"lower"}
        ]}"#,
    );
    let stat = provider
        .stat(&RemotePath::new("readme.md").expect("path"))
        .expect("stat")
        .expect("present");
    assert_eq!(stat.content_hash.as_deref(), Some("lower"));

    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[
            {"id":"a-sheet","name":"Budget","mimeType":"application/vnd.google-apps.spreadsheet"},
            {"id":"b-file","name":"Budget","mimeType":"text/plain","size":"3","md5Checksum":"file"}
        ]}"#,
    );
    let stat = provider
        .stat(&RemotePath::new("Budget").expect("path"))
        .expect("stat")
        .expect("present");
    assert_eq!(stat.content_hash.as_deref(), Some("file"));

    // Only a Google Doc under the name: nothing is at the path.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[
            {"id":"a-sheet","name":"Budget","mimeType":"application/vnd.google-apps.spreadsheet"}
        ]}"#,
    );
    assert!(
        provider
            .stat(&RemotePath::new("Budget").expect("path"))
            .expect("stat")
            .is_none()
    );
}

#[test]
fn two_files_with_one_name_give_one_entry_and_the_choice_never_depends_on_the_order() {
    let listing = |first: &str, second: &str, third: &str| {
        format!(
            r#"{{"files":[
                {{"id":"{first}","name":"report.txt","mimeType":"text/plain","size":"1","md5Checksum":"m-{first}"}},
                {{"id":"{second}","name":"report.txt","mimeType":"text/plain","size":"2","md5Checksum":"m-{second}"}},
                {{"id":"{third}","name":"report.txt","mimeType":"text/plain","size":"3","md5Checksum":"m-{third}"}},
                {{"id":"other","name":"notes.txt","mimeType":"text/plain","size":"4","md5Checksum":"m-other"}}
            ]}}"#
        )
    };
    for order in [["id-b", "id-c", "id-a"], ["id-c", "id-a", "id-b"]] {
        let transport = Arc::new(ScriptedHttpTransport::new());
        let provider = ensured_provider(transport.clone());
        transport.push_response(200, listing(order[0], order[1], order[2]));
        let entries = provider.enumerate(&RemotePath::root()).expect("enumerate");
        let names: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(
            names,
            vec!["notes.txt", "report.txt"],
            "one entry for each name"
        );
        let report = entries
            .iter()
            .find(|entry| entry.path.as_str() == "report.txt")
            .expect("report");
        assert_eq!(
            report.content_hash.as_deref(),
            Some("m-id-a"),
            "the file with the lowest id, whatever order Drive answers in"
        );
        // The files that lost the name are no entries, and the listing
        // says so: one name to ask about, however many files share it.
        transport.push_response(200, listing(order[0], order[1], order[2]));
        let listed = provider
            .enumerate_with_unsyncable(&RemotePath::root())
            .expect("listing");
        assert_eq!(listed.entries.len(), 2);
        assert_eq!(listed.unsyncable.len(), 1);
        assert_eq!(listed.unsyncable[0].name, "report.txt");
        assert!(listed.unsyncable[0].reason.contains("a second file"));

        // A lookup of the path, on a provider that has listed nothing,
        // finds the same file.
        let transport = Arc::new(ScriptedHttpTransport::new());
        let provider = ensured_provider(transport.clone());
        transport.push_response(200, listing(order[0], order[1], order[2]));
        let stat = provider
            .stat(&RemotePath::new("report.txt").expect("path"))
            .expect("stat")
            .expect("present");
        assert_eq!(stat.content_hash.as_deref(), Some("m-id-a"));
        let lookup = transport.recorded_requests().pop().expect("the lookup");
        assert!(
            lookup.url.contains("pageSize=100"),
            "the lookup reads every file of that name: {}",
            lookup.url
        );
    }
}

#[test]
fn a_folder_that_a_request_confirmed_is_not_looked_up_again_until_the_feed_reports_it() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let folder = |id: &str, name: &str| {
        format!(
            r#"{{"files":[{{"id":"{id}","name":"{name}","mimeType":"application/vnd.google-apps.folder"}}]}}"#
        )
    };
    transport.push_response(200, folder("id-a", "a"));
    transport.push_response(200, folder("id-b", "b"));
    let target = RemotePath::new("a/b/one.txt").expect("path");
    let before = transport.recorded_requests().len();
    assert_eq!(provider.ensure_parent_id(&target).expect("first"), "id-b");
    assert_eq!(
        transport.recorded_requests().len() - before,
        2,
        "one lookup for each folder"
    );

    // The next upload into the same folder asks nothing: the scripted
    // transport has no answer left, so any request would fail.
    let sibling = RemotePath::new("a/b/two.txt").expect("path");
    assert_eq!(provider.ensure_parent_id(&sibling).expect("second"), "id-b");
    assert_eq!(
        transport.recorded_requests().len() - before,
        2,
        "no request"
    );

    // The feed reports that folder `a` changed: it and everything under
    // it are looked up again.
    provider.evict_path("a");
    transport.push_response(200, folder("id-a2", "a"));
    transport.push_response(200, folder("id-b2", "b"));
    assert_eq!(provider.ensure_parent_id(&sibling).expect("third"), "id-b2");
}

#[test]
fn an_upload_that_drive_stored_with_another_checksum_fails_and_is_sent_again() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(200, r#"{"files":[]}"#); // resolve absent
    transport.push_response_with_headers(
        200,
        "",
        vec![(
            "Location".to_string(),
            "https://upload.example/session-9".to_string(),
        )],
    );
    transport.push_response(308, "");
    // Drive answers with the checksum of other bytes.
    transport.push_response(200, r#"{"id":"big","md5Checksum":"0123456789abcdef"}"#);

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("big.bin");
    std::fs::write(
        &source,
        vec![7_u8; (SIMPLE_UPLOAD_MAX_BYTES + CHUNK_GRANULARITY) as usize],
    )
    .expect("seed");
    let mut session = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("big.bin").expect("path"),
            op_id: "dev-op9".to_string(),
            precondition: RemotePrecondition::None,
        })
        .expect("session");
    session.step(SIMPLE_UPLOAD_MAX_BYTES).expect("initiate");
    session.step(SIMPLE_UPLOAD_MAX_BYTES).expect("chunk 1");
    let error = session
        .step(u64::MAX)
        .expect_err("a checksum that differs is not a completed upload");
    assert_eq!(error.kind, ProviderErrorKind::Transient, "{error:?}");
}

#[test]
fn large_upload_is_resumable_with_content_range_chunks() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(200, r#"{"files":[]}"#); // resolve absent
    transport.push_response_with_headers(
        200,
        "",
        vec![(
            "Location".to_string(),
            "https://upload.example/session-1".to_string(),
        )],
    );
    transport.push_response(308, ""); // first chunk accepted
    // Two 256KiB-aligned chunks beyond the simple-upload threshold.
    let payload = vec![7_u8; (SIMPLE_UPLOAD_MAX_BYTES + CHUNK_GRANULARITY) as usize];
    let sent_md5 = md5_hex_of_bytes(&payload);
    transport.push_response(200, format!(r#"{{"id":"big","md5Checksum":"{sent_md5}"}}"#)); // final

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("big.bin");
    std::fs::write(&source, &payload).expect("seed");

    let mut session = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("big.bin").expect("path"),
            op_id: "dev-op2".to_string(),
            precondition: RemotePrecondition::None,
        })
        .expect("session");

    // Step 1: initiation.
    assert!(matches!(
        session.step(SIMPLE_UPLOAD_MAX_BYTES).expect("initiate"),
        TransferStep::Progressed { .. }
    ));
    // Step 2: first chunk (bounded by max_bytes → 5MB aligned down).
    assert!(matches!(
        session.step(SIMPLE_UPLOAD_MAX_BYTES).expect("chunk 1"),
        TransferStep::Progressed { .. }
    ));
    // Step 3: remainder completes.
    let outcome = match session.step(u64::MAX).expect("chunk 2") {
        TransferStep::Completed(outcome) => outcome,
        other => panic!("expected completion, got {other:?}"),
    };
    assert_eq!(outcome.content_hash, sent_md5);

    let requests = transport.recorded_requests();
    let initiate = &requests[2];
    assert!(initiate.url.contains("uploadType=resumable"));
    assert!(
        initiate.url.contains("fields=") && initiate.url.contains("md5Checksum"),
        "the request that opens the session asks for the checksum: {}",
        initiate.url
    );
    let chunk1 = &requests[3];
    assert_eq!(chunk1.url, "https://upload.example/session-1");
    let range = chunk1
        .headers
        .iter()
        .find(|(name, _)| name == "Content-Range")
        .map(|(_, value)| value.clone())
        .expect("content range");
    assert!(range.starts_with("bytes 0-"), "range was {range}");
    let chunk2 = &requests[4];
    let range2 = chunk2
        .headers
        .iter()
        .find(|(name, _)| name == "Content-Range")
        .map(|(_, value)| value.clone())
        .expect("content range");
    assert!(
        range2.ends_with(&format!("/{}", SIMPLE_UPLOAD_MAX_BYTES + CHUNK_GRANULARITY)),
        "total must ride in the range: {range2}"
    );
}

#[test]
fn download_fails_a_range_that_returns_no_bytes() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[{"id":"dl","name":"data.bin","mimeType":"application/octet-stream","size":"8"}]}"#,
    );
    transport.push_response(206, ""); // a proxy answered the range with nothing

    let scratch = tempfile::TempDir::new().expect("scratch");
    let mut session = provider
        .begin_download(DownloadRequest {
            remote_path: RemotePath::new("data.bin").expect("path"),
            destination: scratch.path().join("data.bin"),
        })
        .expect("session");
    let error = session
        .step(4)
        .expect_err("an empty range body must fail the step");
    assert_eq!(error.kind, vapor_shared::ProviderErrorKind::Transient);
    assert!(error.message.contains("empty body"), "{}", error.message);
}

#[test]
fn download_streams_ranges_and_hashes_with_md5() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[{"id":"dl","name":"data.bin","mimeType":"application/octet-stream","size":"8"}]}"#,
    );
    transport.push_response(206, "1234"); // first range
    transport.push_response(206, "5678"); // second range

    let scratch = tempfile::TempDir::new().expect("scratch");
    let destination = scratch.path().join("data.bin");
    let mut session = provider
        .begin_download(DownloadRequest {
            remote_path: RemotePath::new("data.bin").expect("path"),
            destination: destination.clone(),
        })
        .expect("session");

    assert!(matches!(
        session.step(4).expect("range 1"),
        TransferStep::Progressed {
            bytes_transferred: 4
        }
    ));
    let outcome = match session.step(4).expect("range 2") {
        TransferStep::Completed(outcome) => outcome,
        other => panic!("expected completion, got {other:?}"),
    };
    assert_eq!(outcome.bytes_total, 8);
    assert_eq!(outcome.content_hash, md5_hex_of_bytes(b"12345678"));
    assert_eq!(std::fs::read(&destination).expect("payload"), b"12345678");

    let requests = transport.recorded_requests();
    let first_range = requests[2]
        .headers
        .iter()
        .find(|(name, _)| name == "Range")
        .map(|(_, value)| value.clone())
        .expect("range header");
    assert_eq!(first_range, "bytes=0-3");
}

#[test]
fn a_download_that_does_not_match_drives_checksum_is_thrown_away_and_read_again() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // Drive describes the file as it was when the download began.
    let described = md5_hex_of_bytes(b"12345678");
    transport.push_response(
        200,
        format!(
            r#"{{"files":[{{"id":"dl","name":"data.bin","mimeType":"application/octet-stream","size":"8","md5Checksum":"{described}"}}]}}"#
        ),
    );
    // The file is replaced between the two ranges: the head of one
    // version and the tail of another.
    transport.push_response(206, "1234");
    transport.push_response(206, "WXYZ");

    let scratch = tempfile::TempDir::new().expect("scratch");
    let destination = scratch.path().join("data.bin");
    let mut session = provider
        .begin_download(DownloadRequest {
            remote_path: RemotePath::new("data.bin").expect("path"),
            destination: destination.clone(),
        })
        .expect("session");
    session.step(4).expect("range 1");
    let error = session
        .step(4)
        .expect_err("mixed content must not complete");
    assert_eq!(error.kind, ProviderErrorKind::Transient);
    assert!(
        !destination.exists(),
        "the mixed payload is not left where an apply would find it"
    );
}

#[test]
fn delete_moves_the_file_to_trash() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[{"id":"victim","name":"gone.txt","mimeType":"text/plain"}]}"#,
    );
    transport.push_response(200, r#"{"id":"victim","trashed":true}"#);

    provider
        .delete(&RemotePath::new("gone.txt").expect("path"), "op-del")
        .expect("delete");

    let requests = transport.recorded_requests();
    let trash = requests.last().expect("trash request");
    assert_eq!(trash.method, "PATCH");
    let body: serde_json::Value = serde_json::from_slice(&trash.body).expect("json");
    assert_eq!(body["trashed"], true, "delete must trash, not purge");
}

#[test]
fn changes_baseline_and_increments_map_paths_and_op_ids() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());

    transport.push_response(200, r#"{"startPageToken":"token-1"}"#);
    let baseline = provider.poll_changes(None, 100).expect("baseline");
    let cursor = match baseline {
        ChangesPoll::Page(page) => {
            assert!(page.changes.is_empty());
            page.next_cursor
        }
        other => panic!("baseline must be a page, got {other:?}"),
    };
    assert_eq!(cursor, "token-1");

    // One modified file whose parent is the sync root.
    transport.push_response(
        200,
        r#"{"newStartPageToken":"token-2","changes":[
            {"fileId":"f9","file":{"id":"f9","name":"changed.txt","mimeType":"text/plain",
             "size":"3","md5Checksum":"h9","parents":["root-folder"],
             "appProperties":{"vaporOpId":"their-op"}}}
        ]}"#,
    );
    let page = match provider.poll_changes(Some(&cursor), 100).expect("poll") {
        ChangesPoll::Page(page) => page,
        other => panic!("expected page, got {other:?}"),
    };
    assert_eq!(page.next_cursor, "token-2");
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].path.as_str(), "changed.txt");
    assert_eq!(page.changes[0].kind, RemoteChangeKind::CreatedOrModified);
    assert_eq!(page.changes[0].op_id.as_deref(), Some("their-op"));
    assert_eq!(page.changes[0].content_hash.as_deref(), Some("h9"));

    // A trashed file we have mapped surfaces as Removed.
    transport.push_response(
        200,
        r#"{"newStartPageToken":"token-3","changes":[
            {"fileId":"f9","removed":true}
        ]}"#,
    );
    let page = match provider.poll_changes(Some("token-2"), 100).expect("poll") {
        ChangesPoll::Page(page) => page,
        other => panic!("expected page, got {other:?}"),
    };
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].kind, RemoteChangeKind::Removed);
    assert_eq!(page.changes[0].path.as_str(), "changed.txt");
}

#[test]
fn a_second_file_under_one_name_never_reads_as_the_file_the_path_names() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let poll = |cursor: &str| match provider.poll_changes(Some(cursor), 100).expect("poll") {
        ChangesPoll::Page(page) => page.changes,
        other => panic!("expected page, got {other:?}"),
    };
    let change_of = |id: &str, name: &str, hash: &str| {
        format!(
            r#"{{"newStartPageToken":"next","changes":[
                {{"fileId":"{id}","file":{{"id":"{id}","name":"{name}","mimeType":"text/plain",
                 "size":"3","md5Checksum":"{hash}","parents":["root-folder"]}}}}
            ]}}"#
        )
    };
    let both = r#"{"files":[
        {"id":"b-second","name":"n.txt","mimeType":"text/plain","size":"3","md5Checksum":"second"},
        {"id":"a-kept","name":"n.txt","mimeType":"text/plain","size":"3","md5Checksum":"kept"}
    ]}"#;

    // Nothing is known about `n.txt` yet, and the file that changed is
    // the second one of that name. One lookup finds the file the path
    // names, and the change is not a change of the path.
    transport.push_response(200, change_of("b-second", "n.txt", "second"));
    transport.push_response(200, both);
    assert!(poll("t1").is_empty(), "the second file stays in the cloud");

    // The file the path names changes: that is a change of the path,
    // and it needs no lookup.
    transport.push_response(200, change_of("a-kept", "n.txt", "kept-2"));
    let changes = poll("t2");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path.as_str(), "n.txt");
    assert_eq!(changes[0].content_hash.as_deref(), Some("kept-2"));

    // The second file is trashed, then a third one is renamed away from
    // the name: neither is the removal of `n.txt`.
    transport.push_response(
        200,
        r#"{"newStartPageToken":"next","changes":[{"fileId":"b-second","removed":true}]}"#,
    );
    assert!(poll("t3").is_empty(), "the path still names the kept file");
    transport.push_response(200, change_of("c-third", "n.txt", "third"));
    assert!(poll("t4").is_empty());
    transport.push_response(200, change_of("c-third", "other.txt", "third"));
    transport.push_response(
        200,
        r#"{"files":[
            {"id":"c-third","name":"other.txt","mimeType":"text/plain","size":"3","md5Checksum":"third"}
        ]}"#,
    );
    let changes = poll("t5");
    assert_eq!(
        changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.clone()))
            .collect::<Vec<_>>(),
        vec![("other.txt", RemoteChangeKind::CreatedOrModified)],
        "the rename gives the new name, and no removal of the name the kept file has"
    );

    // The kept file is trashed: that is the removal of the path.
    transport.push_response(
        200,
        r#"{"newStartPageToken":"next","changes":[{"fileId":"a-kept","removed":true}]}"#,
    );
    let changes = poll("t6");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, RemoteChangeKind::Removed);
    assert_eq!(changes[0].path.as_str(), "n.txt");
}

#[test]
fn a_busy_out_of_scope_file_is_walked_once_then_served_from_the_negative_cache() {
    // The changes feed is Drive-wide, so a file the user edits constantly
    // outside the sync root must not re-walk its parent chain every poll.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());

    // Poll 1: a change to a file whose parent is NOT the sync root.
    transport.push_response(
        200,
        r#"{"newStartPageToken":"token-2","changes":[
            {"fileId":"f-out","file":{"id":"f-out","name":"colleague.doc","mimeType":"text/plain",
             "size":"3","md5Checksum":"h","parents":["outside-parent"]}}
        ]}"#,
    );
    // The one parent lookup: a top-level folder that is not our root.
    transport.push_response(
        200,
        r#"{"id":"outside-parent","name":"TheirDrive","mimeType":"application/vnd.google-apps.folder"}"#,
    );
    // Poll 2: the same file changes again.
    transport.push_response(
        200,
        r#"{"newStartPageToken":"token-3","changes":[
            {"fileId":"f-out","file":{"id":"f-out","name":"colleague.doc","mimeType":"text/plain",
             "size":"4","md5Checksum":"h2","parents":["outside-parent"]}}
        ]}"#,
    );

    for cursor in ["token-1", "token-2"] {
        match provider.poll_changes(Some(cursor), 100).expect("poll") {
            ChangesPoll::Page(page) => assert!(
                page.changes.is_empty(),
                "an out-of-scope change must surface nothing"
            ),
            other => panic!("expected page, got {other:?}"),
        }
    }

    let parent_walks = transport
        .recorded_requests()
        .iter()
        .filter(|r| r.url.contains("files/outside-parent"))
        .count();
    assert_eq!(
        parent_walks, 1,
        "the second poll must be served from the negative cache with zero walk requests"
    );
}

/// One poll of the feed: its answer, then the answers to what the
/// provider asks about an entry. Gives the changes as path and kind.
fn poll_the_feed(
    provider: &GoogleDriveProvider,
    transport: &ScriptedHttpTransport,
    changes: &str,
    lookups: &[(u16, &str)],
) -> Vec<(String, RemoteChangeKind)> {
    transport.push_response(
        200,
        format!(r#"{{"newStartPageToken":"next","changes":[{changes}]}}"#),
    );
    for (status, lookup) in lookups {
        transport.push_response(*status, lookup.to_string());
    }
    match provider.poll_changes(Some("token"), 100).expect("poll") {
        ChangesPoll::Page(page) => page
            .changes
            .into_iter()
            .map(|change| (change.path.as_str().to_string(), change.kind))
            .collect(),
        other => panic!("expected a page, got {other:?}"),
    }
}

fn folder_entry(id: &str, name: &str, parent: &str) -> String {
    format!(
        r#"{{"fileId":"{id}","file":{{"id":"{id}","name":"{name}","mimeType":"application/vnd.google-apps.folder","parents":["{parent}"]}}}}"#
    )
}

/// A folder entry of the feed with the time the folder was made.
fn folder_entry_made(id: &str, name: &str, parent: &str, made: SystemTime) -> String {
    let seconds = made
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since the epoch to a civil date (the algorithm of Howard
    // Hinnant), so the test needs no date crate.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let stamp = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    );
    format!(
        r#"{{"fileId":"{id}","file":{{"id":"{id}","name":"{name}","mimeType":"application/vnd.google-apps.folder","parents":["{parent}"],"createdTime":"{stamp}"}}}}"#
    )
}

fn folder_listed(id: &str, name: &str, parent: &str) -> String {
    format!(
        r#"{{"files":[{{"id":"{id}","name":"{name}","mimeType":"application/vnd.google-apps.folder","parents":["{parent}"]}}]}}"#
    )
}

/// A provider that listed the root and found the folders `Reports`
/// (`d1`) and `Archive` (`d2`) in it.
fn provider_that_knows_two_folders(transport: &Arc<ScriptedHttpTransport>) -> GoogleDriveProvider {
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[
            {"id":"d1","name":"Reports","mimeType":"application/vnd.google-apps.folder","parents":["root-folder"]},
            {"id":"d2","name":"Archive","mimeType":"application/vnd.google-apps.folder","parents":["root-folder"]}
        ]}"#,
    );
    provider.enumerate(&RemotePath::root()).expect("enumerate");
    provider
}

/// The kind of a change that says a folder is the one that was at
/// `from`.
fn folder_moved_from(from: &str) -> RemoteChangeKind {
    RemoteChangeKind::Moved {
        from: RemotePath::new(from).expect("path"),
        entry_kind: RemoteEntryKind::Directory,
    }
}

#[test]
fn a_folder_that_gets_another_name_or_place_is_reported_as_one_move_or_for_a_walk() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_knows_two_folders(&transport);
    let moved = || RemoteChangeKind::FolderMoved;

    // Drive has one entry for a folder that got another name, and none
    // for the files in it. The provider knows where the folder was, so
    // the one change names both places.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "reports", "root-folder"),
            &[(200, &folder_listed("d1", "reports", "root-folder"))]
        ),
        vec![("reports".to_string(), folder_moved_from("Reports"))]
    );

    // An entry for the folder where it is says nothing new, and costs
    // no request: the echo of each folder this device makes is one.
    let before = transport.recorded_requests().len();
    assert!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "reports", "root-folder"),
            &[]
        )
        .is_empty()
    );
    assert_eq!(
        transport.recorded_requests().len() - before,
        1,
        "the feed alone"
    );

    // A folder that was never seen, made a moment ago, is a new one:
    // its files come with entries of their own, and no request is made
    // for it.
    let before = transport.recorded_requests().len();
    assert!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry_made("d9", "New", "root-folder", SystemTime::now()),
            &[]
        )
        .is_empty()
    );
    assert_eq!(
        transport.recorded_requests().len() - before,
        1,
        "the feed alone"
    );

    // A folder that was never seen and that is older moved in from
    // outside the synced folder, or came back from the trash: Drive has
    // this one entry for it and none for its files, so it is reported
    // for a walk.
    let last_year = SystemTime::now() - Duration::from_secs(365 * 24 * 3600);
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry_made("d8", "Moved in", "root-folder", last_year),
            &[]
        ),
        vec![("Moved in".to_string(), moved())]
    );
    // And so is one with no date: a walk costs a request, a folder
    // that is missed costs a day.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d7", "Undated", "root-folder"),
            &[]
        ),
        vec![("Undated".to_string(), moved())]
    );

    // The folder moves into another folder of the sync root: one move
    // again.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "reports", "d2"),
            &[(200, &folder_listed("d1", "reports", "d2"))]
        ),
        vec![("Archive/reports".to_string(), folder_moved_from("reports"))]
    );

    // It moves out of the synced folder: no place in it names the
    // folder now, and the place it had is all there is to walk.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "reports", "elsewhere"),
            &[(
                200,
                r#"{"id":"elsewhere","name":"Other","mimeType":"application/vnd.google-apps.folder"}"#
            )]
        ),
        vec![("Archive/reports".to_string(), moved())]
    );

    // It comes back a moment later. The provider remembers that it was
    // outside, for the folder it was in then: in another folder it is
    // judged again, because the feed says this one time only.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "reports", "root-folder"),
            &[]
        ),
        vec![("reports".to_string(), moved())]
    );
}

fn identity(path: &str, kind: RemoteEntryKind, id: &str) -> RemoteIdentity {
    RemoteIdentity {
        path: RemotePath::new(path).expect("path"),
        kind,
        id: id.to_string(),
    }
}

/// A provider of a new run of the daemon: it listed nothing, and the
/// engine handed it the ids of what was synced. `Reports` (`d1`) holds
/// `q1.pdf` (`f1`), `Archive` (`d2`) is empty, and `notes.txt` (`f9`)
/// is at the top.
fn provider_that_got_its_ids_back(transport: &Arc<ScriptedHttpTransport>) -> GoogleDriveProvider {
    let provider = ensured_provider(transport.clone());
    provider.restore_remote_ids(vec![
        identity("Reports", RemoteEntryKind::Directory, "d1"),
        identity("Reports/q1.pdf", RemoteEntryKind::File, "f1"),
        identity("Archive", RemoteEntryKind::Directory, "d2"),
        identity("notes.txt", RemoteEntryKind::File, "f9"),
    ]);
    provider
}

fn file_entry(id: &str, name: &str, parent: &str, md5: &str) -> String {
    format!(
        r#"{{"fileId":"{id}","file":{{"id":"{id}","name":"{name}","mimeType":"text/plain","size":"3","md5Checksum":"{md5}","parents":["{parent}"],"modifiedTime":"2026-07-06T12:30:00.000Z"}}}}"#
    )
}

fn file_listed(id: &str, name: &str, parent: &str) -> String {
    format!(
        r#"{{"files":[{{"id":"{id}","name":"{name}","mimeType":"text/plain","size":"3","parents":["{parent}"]}}]}}"#
    )
}

#[test]
fn after_a_restart_a_renamed_folder_is_reported_with_its_old_and_its_new_path() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_got_its_ids_back(&transport);

    // The folder got another name while no daemon ran. The provider
    // of this run never listed it: the id that came back says where
    // it was.
    let before = transport.recorded_requests().len();
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "Quarterly", "root-folder"),
            &[(200, &folder_listed("d1", "Quarterly", "root-folder"))]
        ),
        vec![("Quarterly".to_string(), folder_moved_from("Reports"))]
    );
    let asked: Vec<String> = transport.recorded_requests()[before..]
        .iter()
        .map(|request| request.url.clone())
        .collect();
    assert_eq!(
        asked.len(),
        2,
        "the feed, and which folder the new name is: {asked:?}"
    );
    assert!(
        asked[1].contains("name"),
        "no folder is listed for it: {asked:?}"
    );

    // What the folder holds went along, with the ids that came back:
    // a change of the file in it is at the new path, with no request.
    let before = transport.recorded_requests().len();
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &file_entry("f1", "q1.pdf", "d1", "h1"),
            &[]
        ),
        vec![(
            "Quarterly/q1.pdf".to_string(),
            RemoteChangeKind::CreatedOrModified
        )]
    );
    assert_eq!(
        transport.recorded_requests().len() - before,
        1,
        "the feed alone"
    );

    // A move into another folder that came back the same way.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "Quarterly", "d2"),
            &[(200, &folder_listed("d1", "Quarterly", "d2"))]
        ),
        vec![(
            "Archive/Quarterly".to_string(),
            folder_moved_from("Quarterly")
        )]
    );
}

#[test]
fn after_a_restart_a_deletion_in_the_feed_is_applied_by_the_path_that_came_back() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_got_its_ids_back(&transport);

    // The feed names a removed object by its id alone.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            r#"{"fileId":"f9","removed":true},{"fileId":"d1","removed":true}"#,
            &[]
        ),
        vec![
            ("notes.txt".to_string(), RemoteChangeKind::Removed),
            ("Reports".to_string(), RemoteChangeKind::Removed)
        ]
    );
    // It is said one time.
    assert!(
        poll_the_feed(
            &provider,
            &transport,
            r#"{"fileId":"f9","removed":true}"#,
            &[]
        )
        .is_empty()
    );
}

#[test]
fn a_renamed_file_is_reported_as_one_move_with_the_state_of_the_file() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_got_its_ids_back(&transport);
    let file_moved_from = |from: &str| RemoteChangeKind::Moved {
        from: RemotePath::new(from).expect("path"),
        entry_kind: RemoteEntryKind::File,
    };

    transport.push_response(
        200,
        format!(
            r#"{{"newStartPageToken":"next","changes":[{}]}}"#,
            file_entry("f9", "minutes.txt", "root-folder", "h9")
        ),
    );
    transport.push_response(200, file_listed("f9", "minutes.txt", "root-folder"));
    let changes = match provider.poll_changes(Some("token"), 100).expect("poll") {
        ChangesPoll::Page(page) => page.changes,
        other => panic!("expected a page, got {other:?}"),
    };
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].path.as_str(), "minutes.txt");
    assert_eq!(changes[0].kind, file_moved_from("notes.txt"));
    // A file can get new content with its new name, so the change
    // says what the file holds, and which object it is.
    assert_eq!(changes[0].content_hash.as_deref(), Some("h9"));
    assert_eq!(changes[0].size_bytes, Some(3));
    assert!(changes[0].modified_at.is_some());
    assert_eq!(changes[0].id.as_deref(), Some("f9"));

    // The same file moves into a folder: one move again.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &file_entry("f9", "minutes.txt", "d2", "h9"),
            &[(200, &file_listed("f9", "minutes.txt", "d2"))]
        ),
        vec![(
            "Archive/minutes.txt".to_string(),
            file_moved_from("minutes.txt")
        )]
    );
}

#[test]
fn an_entry_for_an_id_that_the_provider_does_not_know_is_no_move() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_got_its_ids_back(&transport);

    // A file that is new to this provider: where it was is not known,
    // so it is a new file, and the engine compares it as one.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &file_entry("f5", "new.txt", "root-folder", "h5"),
            &[(200, &file_listed("f5", "new.txt", "root-folder"))]
        ),
        vec![("new.txt".to_string(), RemoteChangeKind::CreatedOrModified)]
    );
    // A removal of an object that never had a path here says nothing.
    assert!(
        poll_the_feed(
            &provider,
            &transport,
            r#"{"fileId":"f6","removed":true}"#,
            &[]
        )
        .is_empty()
    );
    // A folder with no known old place is reported for a walk, as
    // before: it may hold files that the feed never names.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d5", "Moved in", "root-folder"),
            &[]
        ),
        vec![("Moved in".to_string(), RemoteChangeKind::FolderMoved)]
    );
}

#[test]
fn a_path_that_came_back_is_confirmed_before_it_is_used_and_a_listing_of_this_run_stands() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // This run listed the root and found `notes.txt` as `f-now`.
    transport.push_response(200, file_listed("f-now", "notes.txt", "root-folder"));
    provider.enumerate(&RemotePath::root()).expect("enumerate");

    // The ids of the last run come after it: the file had another id
    // then, and a file that is gone had a path.
    provider.restore_remote_ids(vec![
        identity("notes.txt", RemoteEntryKind::File, "f-then"),
        identity("gone.txt", RemoteEntryKind::File, "f-gone"),
    ]);

    // What this run found stands.
    transport.push_response(
        200,
        r#"{"id":"f-now","name":"notes.txt","mimeType":"text/plain","size":"3","parents":["root-folder"]}"#,
    );
    let entry = provider
        .stat(&RemotePath::new("notes.txt").expect("path"))
        .expect("stat")
        .expect("the file");
    assert_eq!(entry.id.as_deref(), Some("f-now"));

    // A path that came back is asked about before it is trusted: the
    // object is gone, and a lookup by name finds nothing.
    let before = transport.recorded_requests().len();
    transport.push_response(404, r#"{"error":{"code":404,"message":"File not found"}}"#);
    transport.push_response(200, r#"{"files":[]}"#);
    assert!(
        provider
            .stat(&RemotePath::new("gone.txt").expect("path"))
            .expect("stat")
            .is_none()
    );
    let asked = &transport.recorded_requests()[before..];
    assert_eq!(asked.len(), 2);
    assert!(asked[0].url.contains("files/f-gone"));
}

#[test]
fn a_file_judged_outside_is_judged_again_when_a_folder_comes_into_the_root() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_knows_two_folders(&transport);
    let file = r#"{"fileId":"f1","file":{"id":"f1","name":"a.txt","mimeType":"text/plain","size":"3","md5Checksum":"h","parents":["d5"]}}"#;
    let elsewhere =
        r#"{"id":"d5","name":"Elsewhere","mimeType":"application/vnd.google-apps.folder"}"#;

    // A file in a folder outside the sync root changes: not ours, and
    // the next entry for it costs no request.
    assert!(poll_the_feed(&provider, &transport, file, &[(200, elsewhere)]).is_empty());
    let before = transport.recorded_requests().len();
    assert!(poll_the_feed(&provider, &transport, file, &[]).is_empty());
    assert_eq!(
        transport.recorded_requests().len() - before,
        1,
        "the feed alone"
    );

    // Its folder comes into the sync root. The entry of the file names
    // the same folder as before, and the file is ours now.
    let last_year = SystemTime::now() - Duration::from_secs(365 * 24 * 3600);
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry_made("d5", "Elsewhere", "root-folder", last_year),
            &[]
        ),
        vec![("Elsewhere".to_string(), RemoteChangeKind::FolderMoved)]
    );
    assert_eq!(
        poll_the_feed(&provider, &transport, file, &[]),
        vec![(
            "Elsewhere/a.txt".to_string(),
            RemoteChangeKind::CreatedOrModified
        )]
    );
}

#[test]
fn a_young_folder_that_was_seen_outside_is_a_folder_that_moved_in() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_knows_two_folders(&transport);
    let moved = || RemoteChangeKind::FolderMoved;
    let now = SystemTime::now();
    let outside =
        r#"{"id":"top","name":"Elsewhere","mimeType":"application/vnd.google-apps.folder"}"#;

    // A folder is made outside the sync root and a file is put in it.
    // The folder is new and not ours; the file is judged outside, in
    // that folder.
    let young = folder_entry_made("d7", "Fresh", "top", now);
    let file = r#"{"fileId":"f7","file":{"id":"f7","name":"a.txt","mimeType":"text/plain","size":"3","md5Checksum":"h","parents":["d7"]}}"#;
    let in_top = r#"{"id":"d7","name":"Fresh","mimeType":"application/vnd.google-apps.folder","parents":["top"]}"#;
    assert!(
        poll_the_feed(
            &provider,
            &transport,
            &format!("{young},{file}"),
            &[(200, in_top), (200, outside)]
        )
        .is_empty()
    );

    // A minute later the folder is moved into the sync root. It is
    // still young, and the feed has this one entry for it and none for
    // the file. The provider saw a file of it outside, so it is no new
    // folder: it gets its walk.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry_made("d7", "Fresh", "root-folder", now),
            &[]
        ),
        vec![("Fresh".to_string(), moved())]
    );

    // A young folder of the sync root leaves it and comes back: the
    // provider judged the folder itself outside in between.
    let known = folder_entry_made("d1", "Reports", "top", now);
    assert_eq!(
        poll_the_feed(&provider, &transport, &known, &[(200, outside)]),
        vec![("Reports".to_string(), moved())]
    );
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry_made("d1", "Reports", "root-folder", now),
            &[]
        ),
        vec![("Reports".to_string(), moved())]
    );
}

#[test]
fn a_file_judged_outside_is_ours_when_a_listing_finds_its_folder_inside() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let file = r#"{"fileId":"f1","file":{"id":"f1","name":"a.txt","mimeType":"text/plain","size":"3","md5Checksum":"h","parents":["d5"]}}"#;
    let elsewhere =
        r#"{"id":"d5","name":"Elsewhere","mimeType":"application/vnd.google-apps.folder"}"#;
    assert!(poll_the_feed(&provider, &transport, file, &[(200, elsewhere)]).is_empty());

    // The folder comes into the sync root, and no feed entry that the
    // provider acts on says so: a walk of the root lists it.
    transport.push_response(
        200,
        r#"{"files":[
            {"id":"d5","name":"Elsewhere","mimeType":"application/vnd.google-apps.folder","parents":["root-folder"]}
        ]}"#,
    );
    provider.enumerate(&RemotePath::root()).expect("enumerate");

    // The entry of the file names the folder it was judged in. That
    // folder is known inside now, so the verdict is out of date.
    assert_eq!(
        poll_the_feed(&provider, &transport, file, &[]),
        vec![(
            "Elsewhere/a.txt".to_string(),
            RemoteChangeKind::CreatedOrModified
        )]
    );
}

#[test]
fn a_file_that_is_moved_out_of_the_sync_root_left_its_path() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let entry = |parent: &str| {
        format!(
            r#"{{"fileId":"f1","file":{{"id":"f1","name":"a.txt","mimeType":"text/plain","size":"3","md5Checksum":"h","parents":["{parent}"]}}}}"#
        )
    };
    assert_eq!(
        poll_the_feed(&provider, &transport, &entry("root-folder"), &[]),
        vec![("a.txt".to_string(), RemoteChangeKind::CreatedOrModified)]
    );

    // The file goes to a folder outside the sync root. The feed names
    // the file and where it is now; the path it had holds nothing.
    let elsewhere =
        r#"{"id":"top","name":"Elsewhere","mimeType":"application/vnd.google-apps.folder"}"#;
    assert_eq!(
        poll_the_feed(&provider, &transport, &entry("top"), &[(200, elsewhere)]),
        vec![("a.txt".to_string(), RemoteChangeKind::Removed)]
    );
    // The next entry for the file outside says nothing more.
    assert!(poll_the_feed(&provider, &transport, &entry("top"), &[]).is_empty());
}

#[test]
fn a_folder_whose_new_place_cannot_be_read_asks_for_a_walk_of_everything() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_knows_two_folders(&transport);
    // The folder moved under a folder this provider does not know, and
    // the cloud does not answer the question about that folder. The
    // feed will not say it again.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            &folder_entry("d1", "Reports", "made-a-moment-ago"),
            &[(503, "")]
        ),
        vec![
            ("Reports".to_string(), RemoteChangeKind::FolderMoved),
            (String::new(), RemoteChangeKind::FolderMoved),
        ],
        "the folder that held it, and the root for the place that is not known"
    );

    // The same for a file whose folder cannot be read.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            r#"{"fileId":"f1","file":{"id":"f1","name":"a.txt","mimeType":"text/plain","md5Checksum":"h","parents":["made-a-moment-ago"]}}"#,
            &[(503, "")]
        ),
        vec![(String::new(), RemoteChangeKind::FolderMoved)]
    );
}

#[test]
fn a_file_under_a_folder_that_moved_is_still_known_when_it_is_removed() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_that_knows_two_folders(&transport);
    let file = |parent: &str| {
        format!(
            r#"{{"id":"fx","name":"x.txt","mimeType":"text/plain","md5Checksum":"h","size":"1","parents":["{parent}"]}}"#
        )
    };
    let folder = |name: &str| {
        format!(
            r#"{{"id":"d1","name":"{name}","mimeType":"application/vnd.google-apps.folder","parents":["root-folder"]}}"#
        )
    };
    // The file is known under the old name of its folder.
    transport.push_response(200, folder("Reports")); // the folder is still there
    transport.push_response(200, format!(r#"{{"files":[{}]}}"#, file("d1")));
    provider
        .enumerate(&RemotePath::new("Reports").expect("path"))
        .expect("list the folder");

    // The folder gets another name. The walk that follows lists it
    // under the new one.
    poll_the_feed(
        &provider,
        &transport,
        &folder_entry("d1", "Plans", "root-folder"),
        &[(200, &folder_listed("d1", "Plans", "root-folder"))],
    );
    transport.push_response(200, folder("Plans"));
    transport.push_response(200, format!(r#"{{"files":[{}]}}"#, file("d1")));
    provider
        .enumerate(&RemotePath::new("Plans").expect("path"))
        .expect("list the folder under its new name");

    // The engine asks about the path the file had: nothing is there.
    transport.push_response(200, r#"{"files":[]}"#);
    assert!(
        provider
            .stat(&RemotePath::new("Reports/x.txt").expect("path"))
            .expect("stat")
            .is_none()
    );

    // The file is removed in the cloud: the feed names it by its id,
    // and the provider must still know where it is.
    assert_eq!(
        poll_the_feed(
            &provider,
            &transport,
            r#"{"fileId":"fx","removed":true}"#,
            &[]
        ),
        vec![("Plans/x.txt".to_string(), RemoteChangeKind::Removed)]
    );
}

#[test]
fn a_changes_response_that_advances_no_cursor_is_a_transient_error_not_progress() {
    // A parsable 200 with neither nextPageToken nor newStartPageToken must
    // not persist the same cursor (which would re-poll the identical page
    // forever); surface it transiently so the poll retries.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(200, r#"{"changes":[]}"#);

    let error = provider
        .poll_changes(Some("cursor-1"), 100)
        .expect_err("no-cursor response must not be accepted as progress");
    assert_eq!(error.kind, ProviderErrorKind::Transient);
}

#[test]
fn expired_page_token_maps_to_cursor_expired() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(410, "");
    assert_eq!(
        provider.poll_changes(Some("stale"), 100).expect("poll"),
        ChangesPoll::CursorExpired
    );
}

#[test]
fn rate_limits_classify_with_retry_after() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response_with_headers(
        429,
        r#"{"error":{"errors":[{"reason":"rateLimitExceeded"}]}}"#,
        vec![("Retry-After".to_string(), "7".to_string())],
    );
    let error = provider
        .enumerate(&RemotePath::root())
        .expect_err("rate limit must fail");
    match error.kind {
        ProviderErrorKind::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(7)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[test]
fn a_401_forces_one_refresh_and_retries() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(401, "");
    // Refresh grant succeeds…
    transport.push_response(200, r#"{"access_token":"ya29.fresh","expires_in":3600}"#);
    // …and the retried call answers.
    transport.push_response(200, r#"{"files":[]}"#);

    let entries = provider.enumerate(&RemotePath::root()).expect("enumerate");
    assert!(entries.is_empty());
    let requests = transport.recorded_requests();
    // ensure + failed call + token refresh + retried call.
    assert_eq!(requests.len(), 4);
    assert!(requests[2].url.contains("oauth2.googleapis.com"));
}

#[test]
fn a_401_on_a_download_chunk_refreshes_and_resends_instead_of_failing_the_intent() {
    // The session hot path (download ranges, upload chunks) shares the
    // provider's TokenManager and its one-shot 401 refresh-retry. A token
    // that expires mid-transfer must resend the chunk, not fail the whole
    // multi-gigabyte intent terminally.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // resolve the file.
    transport.push_response(
        200,
        r#"{"files":[{"id":"dl","name":"data.bin","mimeType":"application/octet-stream","size":"4"}]}"#,
    );
    transport.push_response(401, ""); // first range: token went stale
    transport.push_response(200, r#"{"access_token":"ya29.fresh","expires_in":3600}"#);
    transport.push_response(206, "data"); // resent range succeeds

    let scratch = tempfile::TempDir::new().expect("scratch");
    let destination = scratch.path().join("data.bin");
    let mut session = provider
        .begin_download(DownloadRequest {
            remote_path: RemotePath::new("data.bin").expect("path"),
            destination: destination.clone(),
        })
        .expect("session");

    let outcome = match session.step(4).expect("range after refresh") {
        TransferStep::Completed(outcome) => outcome,
        other => panic!("expected completion, got {other:?}"),
    };
    assert_eq!(outcome.bytes_total, 4);
    assert_eq!(std::fs::read(&destination).expect("payload"), b"data");

    let requests = transport.recorded_requests();
    assert!(
        requests
            .iter()
            .any(|r| r.url.contains("oauth2.googleapis.com")),
        "a token refresh must have been issued on the session path"
    );
}

#[test]
fn missing_credentials_surface_an_actionable_auth_error() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = GoogleDriveProvider::new(
        GdriveConfig {
            client_id: "client".to_string(),
            client_secret: None,
            profile_id: "workp".to_string(),
        },
        Arc::new(InMemorySecretStore::new()),
        transport,
    );
    let error = provider
        .ensure_cloud_root("/Vapor")
        .expect_err("no credentials must fail");
    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert!(error.message.contains("vapor auth login"));
    assert!(error.message.contains("workp"), "names the profile");
}

#[test]
fn a_refused_sign_in_stops_token_requests_until_a_new_sign_in_lands() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let secrets = Arc::new(InMemorySecretStore::new());
    secrets
        .set(
            "auth.default.gdrive.token",
            r#"{"accessToken":"ya29.old","refreshToken":"1//dead","expiresAtMs":0}"#,
        )
        .expect("seed expired tokens");
    let provider = GoogleDriveProvider::new(
        GdriveConfig {
            client_id: "client".to_string(),
            client_secret: None,
            profile_id: "default".to_string(),
        },
        secrets.clone(),
        transport.clone(),
    );

    // The refresh token expired (a Testing-mode client drops it after a
    // week): Google answers invalid_grant.
    transport.push_response(400, r#"{"error":"invalid_grant"}"#);
    let error = provider
        .ensure_cloud_root("/Vapor")
        .expect_err("a refused refresh must fail");
    assert_eq!(error.kind, ProviderErrorKind::Authentication);

    // While the store still holds the refused token, a retry fails
    // without asking Google again.
    let error = provider
        .ensure_cloud_root("/Vapor")
        .expect_err("still signed out");
    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert!(error.message.contains("vapor auth login --profile default"));
    assert_eq!(transport.recorded_requests().len(), 1);

    // `vapor auth login` stores a new token set; the same provider
    // instance uses it on its next call, without a restart.
    secrets
        .set(
            "auth.default.gdrive.token",
            &format!(
                r#"{{"accessToken":"ya29.new","refreshToken":"1//new","expiresAtMs":{}}}"#,
                u64::MAX / 2
            ),
        )
        .expect("store the new sign-in");
    transport.push_response(
        200,
        r#"{"files":[{"id":"root-folder","name":"Vapor","mimeType":"application/vnd.google-apps.folder"}]}"#,
    );
    provider
        .ensure_cloud_root("/Vapor")
        .expect("the new sign-in works");
    let requests = transport.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Bearer ya29.new")
    );
}

/// A provider over a store the test keeps a hand on, with tokens that
/// expire at `expires_at_ms`.
fn provider_signed_in_until(
    transport: Arc<ScriptedHttpTransport>,
    secrets: Arc<dyn SecretStore>,
    expires_at_ms: u64,
) -> GoogleDriveProvider {
    secrets
        .set(
            "auth.default.gdrive.token",
            &format!(
                r#"{{"accessToken":"ya29.test","refreshToken":"1//rt","expiresAtMs":{expires_at_ms}}}"#
            ),
        )
        .expect("seed tokens");
    GoogleDriveProvider::new(
        GdriveConfig {
            client_id: "client".to_string(),
            client_secret: None,
            profile_id: "default".to_string(),
        },
        secrets,
        transport,
    )
}

const ROOT_LISTING: &str = r#"{"files":[{"id":"root-folder","name":"Vapor","mimeType":"application/vnd.google-apps.folder"}]}"#;

/// The next call looks at the secret store, and a store with no tokens
/// counts as a sign-out after `confirm`.
fn look_at_the_store_now(provider: &GoogleDriveProvider, confirm: Duration) {
    let mut look = provider.tokens.look.lock().expect("look");
    look.check = Duration::ZERO;
    look.confirm = confirm;
    look.due = None;
}

#[test]
fn a_sign_out_stops_a_provider_that_holds_the_tokens_in_memory() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let secrets = Arc::new(InMemorySecretStore::new());
    let provider = provider_signed_in_until(transport.clone(), secrets.clone(), u64::MAX / 2);
    transport.push_response(200, ROOT_LISTING);
    provider.ensure_cloud_root("/Vapor").expect("signed in");

    // `vapor auth logout` removes the tokens from the store. The
    // provider of a daemon that runs still has them in memory.
    secrets
        .delete("auth.default.gdrive.token")
        .expect("sign out");
    // Inside the time the tokens in memory stand, a call still goes.
    transport.push_response(200, ROOT_LISTING);
    provider
        .ensure_cloud_root("/Vapor")
        .expect("the store is not read at each call");
    // The first look that finds no tokens is not yet a sign-out: the
    // keychain replaces an item by a delete and an add.
    look_at_the_store_now(&provider, Duration::ZERO);
    transport.push_response(200, ROOT_LISTING);
    provider
        .ensure_cloud_root("/Vapor")
        .expect("one look that finds nothing is not a sign-out");
    // The next look finds none again: the profile is signed out, and
    // the call fails with no request.
    let before = transport.recorded_requests().len();
    let error = provider
        .ensure_cloud_root("/Vapor")
        .expect_err("signed out");
    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert!(error.message.contains("vapor auth login --profile default"));
    assert_eq!(transport.recorded_requests().len(), before);

    // Another sign-in takes the place of the tokens in memory.
    secrets
        .set(
            "auth.default.gdrive.token",
            &format!(
                r#"{{"accessToken":"ya29.other","refreshToken":"1//other","expiresAtMs":{}}}"#,
                u64::MAX / 2
            ),
        )
        .expect("sign in again");
    transport.push_response(200, ROOT_LISTING);
    provider
        .ensure_cloud_root("/Vapor")
        .expect("signed in again");
    assert!(
        transport
            .recorded_requests()
            .last()
            .expect("a request")
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Bearer ya29.other")
    );
}

#[test]
fn a_store_that_is_empty_for_one_look_does_not_sign_the_profile_out() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let secrets = Arc::new(InMemorySecretStore::new());
    let provider = provider_signed_in_until(transport.clone(), secrets.clone(), u64::MAX / 2);
    transport.push_response(200, ROOT_LISTING);
    provider.ensure_cloud_root("/Vapor").expect("signed in");

    // Another process writes the tokens again: the item is gone for a
    // moment between its delete and its add.
    let stored = secrets.get("auth.default.gdrive.token").expect("tokens");
    secrets.delete("auth.default.gdrive.token").expect("delete");
    look_at_the_store_now(&provider, Duration::ZERO);
    transport.push_response(200, ROOT_LISTING);
    provider
        .ensure_cloud_root("/Vapor")
        .expect("the look in between");
    secrets
        .set("auth.default.gdrive.token", &stored)
        .expect("add");
    transport.push_response(200, ROOT_LISTING);
    provider
        .ensure_cloud_root("/Vapor")
        .expect("the tokens are back: no sign-out");
}

/// What a store does at a chosen read, as another process that acts at
/// that moment makes it.
enum AtThatRead {
    /// `vapor auth logout`.
    SignOut,
    /// `vapor auth login` with another account.
    SignIn(String),
    /// A keychain that is locked from then on.
    StopAnswering,
}

struct StoreThatChanges {
    inner: InMemorySecretStore,
    reads_before: std::sync::Mutex<u32>,
    at_that_read: AtThatRead,
    stopped: std::sync::atomic::AtomicBool,
}

impl StoreThatChanges {
    fn new(reads_before: u32, at_that_read: AtThatRead) -> Self {
        Self {
            inner: InMemorySecretStore::new(),
            reads_before: std::sync::Mutex::new(reads_before),
            at_that_read,
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl SecretStore for StoreThatChanges {
    fn get(&self, name: &str) -> Result<String, vapor_platform::SecretStoreError> {
        let mut left = self.reads_before.lock().expect("reads");
        if *left == 0 {
            match &self.at_that_read {
                AtThatRead::SignOut => self.inner.delete(name)?,
                AtThatRead::SignIn(tokens) => self.inner.set(name, tokens)?,
                AtThatRead::StopAnswering => self
                    .stopped
                    .store(true, std::sync::atomic::Ordering::SeqCst),
            }
        }
        *left = left.saturating_sub(1);
        if self.stopped.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(vapor_platform::SecretStoreError::Unavailable(
                "the keychain is locked".to_string(),
            ));
        }
        self.inner.get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), vapor_platform::SecretStoreError> {
        self.inner.set(name, value)
    }
    fn delete(&self, name: &str) -> Result<(), vapor_platform::SecretStoreError> {
        self.inner.delete(name)
    }
    fn list(&self) -> Result<Vec<String>, vapor_platform::SecretStoreError> {
        self.inner.list()
    }
}

#[test]
fn a_refresh_that_ends_after_a_sign_out_does_not_write_the_tokens_back() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    // The first read gives the tokens, which are at their end. The
    // user signs out while the refresh runs.
    let secrets = Arc::new(StoreThatChanges::new(1, AtThatRead::SignOut));
    let provider = provider_signed_in_until(transport.clone(), secrets.clone(), 0);
    transport.push_response(200, r#"{"access_token":"ya29.fresh","expires_in":3600}"#);
    // The call that asked for the refresh still goes: one empty look
    // is not yet a sign-out.
    transport.push_response(200, ROOT_LISTING);
    provider
        .ensure_cloud_root("/Vapor")
        .expect("the call of the refresh");
    assert_eq!(
        secrets.inner.list().expect("list"),
        Vec::<String>::new(),
        "the new tokens are not written into a store the user emptied"
    );

    // The next look confirms the sign-out.
    look_at_the_store_now(&provider, Duration::ZERO);
    let before = transport.recorded_requests().len();
    let error = provider
        .ensure_cloud_root("/Vapor")
        .expect_err("signed out");
    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert_eq!(transport.recorded_requests().len(), before);
    assert_eq!(secrets.inner.list().expect("list"), Vec::<String>::new());
}

#[test]
fn a_refresh_does_not_write_an_old_sign_in_over_a_new_one() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    // The user signs in with another account while the refresh of the
    // first one runs.
    let other = format!(
        r#"{{"accessToken":"ya29.other","refreshToken":"1//other","expiresAtMs":{}}}"#,
        u64::MAX / 2
    );
    let secrets = Arc::new(StoreThatChanges::new(1, AtThatRead::SignIn(other.clone())));
    let provider = provider_signed_in_until(transport.clone(), secrets.clone(), 0);
    transport.push_response(200, r#"{"access_token":"ya29.fresh","expires_in":3600}"#);

    let error = provider
        .ensure_cloud_root("/Vapor")
        .expect_err("the tokens of the refresh belong to the sign-in that was replaced");
    assert_eq!(error.kind, ProviderErrorKind::Transient);
    assert_eq!(
        secrets
            .inner
            .get("auth.default.gdrive.token")
            .expect("tokens"),
        other,
        "the store keeps the new sign-in"
    );

    // The next call uses the new sign-in.
    transport.push_response(200, ROOT_LISTING);
    provider.ensure_cloud_root("/Vapor").expect("signed in");
    assert!(
        transport
            .recorded_requests()
            .last()
            .expect("a request")
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Bearer ya29.other")
    );
}

#[test]
fn a_store_that_does_not_answer_does_not_sign_the_profile_out() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    // The keychain locks after the first read.
    let secrets = Arc::new(StoreThatChanges::new(1, AtThatRead::StopAnswering));
    let provider = provider_signed_in_until(transport.clone(), secrets.clone(), u64::MAX / 2);
    transport.push_response(200, ROOT_LISTING);
    provider.ensure_cloud_root("/Vapor").expect("signed in");

    for _ in 0..3 {
        look_at_the_store_now(&provider, Duration::ZERO);
        transport.push_response(200, ROOT_LISTING);
        provider
            .ensure_cloud_root("/Vapor")
            .expect("the tokens in memory are still the sign-in");
    }
}

#[test]
fn a_store_that_cannot_be_asked_at_the_start_is_tried_again_and_asks_for_no_sign_in() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    // The keychain is locked when the daemon starts, before the user
    // unlocks it.
    let secrets = Arc::new(StoreThatChanges::new(u32::MAX, AtThatRead::StopAnswering));
    let provider = provider_signed_in_until(transport.clone(), secrets.clone(), u64::MAX / 2);
    secrets
        .stopped
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = provider
        .ensure_cloud_root("/Vapor")
        .expect_err("no tokens can be read");
    assert_eq!(
        error.kind,
        ProviderErrorKind::Transient,
        "a store that cannot be asked is no reason to sign in again: {error}"
    );
    assert!(error.message.contains("the keychain is locked"), "{error}");
    assert!(
        transport.recorded_requests().is_empty(),
        "no request without a token"
    );

    // The keychain opens: the next call reads the sign-in.
    secrets
        .stopped
        .store(false, std::sync::atomic::Ordering::SeqCst);
    transport.push_response(200, ROOT_LISTING);
    provider.ensure_cloud_root("/Vapor").expect("signed in");
}

#[test]
fn a_401_while_the_store_cannot_be_asked_refreshes_with_the_tokens_in_memory() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    // The keychain locks after the first read.
    let secrets = Arc::new(StoreThatChanges::new(1, AtThatRead::StopAnswering));
    let provider = provider_signed_in_until(transport.clone(), secrets.clone(), u64::MAX / 2);
    transport.push_response(401, "");
    transport.push_response(200, r#"{"access_token":"ya29.fresh","expires_in":3600}"#);
    transport.push_response(200, ROOT_LISTING);
    provider
        .ensure_cloud_root("/Vapor")
        .expect("the refresh token in memory is still the sign-in");
    let requests = transport.recorded_requests();
    assert!(
        requests
            .last()
            .expect("a request")
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Bearer ya29.fresh"),
        "the call goes again with the new token"
    );
}

#[test]
fn upload_precondition_absent_fails_when_the_target_exists() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[{"id":"exists","name":"taken.txt","mimeType":"text/plain","md5Checksum":"h"}]}"#,
    );
    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("s.txt");
    std::fs::write(&source, b"x").expect("seed");
    let error = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("taken.txt").expect("path"),
            op_id: "op".to_string(),
            precondition: RemotePrecondition::Absent,
        })
        .err()
        .expect("must fail");
    assert_eq!(error.kind, ProviderErrorKind::PreconditionFailed);
}

#[test]
fn rfc3339_parser_round_trips_drive_timestamps() {
    let parsed = parse_rfc3339_millis("2026-07-06T12:30:00.500Z").expect("parse");
    let since_epoch = parsed
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("post-epoch");
    // 2026-07-06 12:30:00.5 UTC (verified against Python's datetime).
    assert_eq!(since_epoch.as_millis(), 1_783_341_000_500);
    assert!(parse_rfc3339_millis("not a timestamp").is_none());
    assert!(parse_rfc3339_millis("2026-07-06T12:30:00Z").is_some());
}

#[test]
fn classifies_daily_and_storage_quota_403s_distinctly() {
    let daily = HttpResponse {
        status: 403,
        headers: Vec::new(),
        body: br#"{"error":{"errors":[{"reason":"dailyLimitExceeded"}]}}"#.to_vec(),
    };
    assert!(matches!(
        classify_api_failure(&daily).kind,
        ProviderErrorKind::RateLimited { .. }
    ));

    let storage = HttpResponse {
        status: 403,
        headers: Vec::new(),
        body: br#"{"error":{"errors":[{"reason":"storageQuotaExceeded"}]}}"#.to_vec(),
    };
    // A full Drive is not a rate limit and not a fault of the file: the
    // upload waits and is tried again when there is room.
    assert!(matches!(
        classify_api_failure(&storage).kind,
        ProviderErrorKind::StorageFull
    ));
}

#[test]
fn resumable_range_header_parses_committed_offset() {
    assert_eq!(parse_resumable_range_end("bytes=0-262143"), Some(262_143));
    assert_eq!(parse_resumable_range_end("bytes=0-0"), Some(0));
    assert_eq!(parse_resumable_range_end("garbage"), None);
}

#[test]
fn multipart_boundary_never_collides_with_payload() {
    // Even a payload containing a plausible boundary line gets a boundary
    // that is not a subslice of it.
    let payload = b"--vapor-deadbeef\r\nContent".to_vec();
    let boundary = multipart_boundary_absent_in(&payload);
    assert!(!contains_subslice(
        &payload,
        format!("--{boundary}").as_bytes()
    ));
}

#[test]
fn a_drive_name_that_cannot_be_a_path_is_reported_with_the_listing() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let listing_of_the_root = r#"{"files":[
        {"id":"f1","name":"notes.txt","mimeType":"text/plain","md5Checksum":"h1","size":"3"},
        {"id":"f2","name":"2024/budget.txt","mimeType":"text/plain","md5Checksum":"h2","size":"3"}
    ]}"#;
    // It is reported at every listing, for as long as it is there.
    for _ in 0..2 {
        transport.push_response(200, listing_of_the_root);
        let listing = provider
            .enumerate_with_unsyncable(&RemotePath::root())
            .expect("listing");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["notes.txt"]);
        assert_eq!(listing.unsyncable.len(), 1);
        assert_eq!(listing.unsyncable[0].name, "2024/budget.txt");
    }
}

#[test]
fn google_native_types_are_excluded_but_folders_and_files_are_not() {
    assert!(is_google_native_non_folder(
        "application/vnd.google-apps.document"
    ));
    assert!(is_google_native_non_folder(
        "application/vnd.google-apps.shortcut"
    ));
    assert!(!is_google_native_non_folder(FOLDER_MIME));
    assert!(!is_google_native_non_folder("application/pdf"));
    assert!(!is_google_native_non_folder("text/plain"));
}

fn revision(id: &str, md5: &str, modified: &str) -> GdRevision {
    GdRevision {
        id: id.to_string(),
        md5_checksum: Some(md5.to_string()),
        modified_time: Some(modified.to_string()),
        size: Some("10".to_string()),
    }
}

/// What the check keeps for an upload of `ours` over `base` that took
/// one second to commit, when the file holds `head` at the time of the
/// check and another device's upload put its tag on the file.
fn replaced(revisions: &[GdRevision], head: &str) -> Vec<GdRevision> {
    versions_of_the_same_moment(revisions, "base", "ours", Duration::from_secs(1), &[])
        .into_kept(Some(head), false)
}

#[test]
fn a_version_written_at_the_moment_of_an_upload_was_replaced_unseen() {
    let base = revision("r0", "base", "2026-11-23T10:00:00.000Z");
    let theirs = revision("r1", "theirs", "2026-11-23T10:00:05.100Z");
    let ours = revision("r2", "ours", "2026-11-23T10:00:05.300Z");

    // Nobody came in between: the list says so with no look at the head.
    assert!(
        versions_of_the_same_moment(
            &[base.clone(), ours.clone()],
            "base",
            "ours",
            Duration::from_secs(1),
            &[]
        )
        .others
        .is_empty()
    );
    // Another device's upload landed between the check and the commit.
    assert_eq!(
        replaced(&[base.clone(), theirs.clone(), ours.clone()], "ours"),
        vec![theirs.clone()]
    );
    // An older version that the base itself replaced is history.
    let older = revision("r-1", "older", "2026-11-22T09:00:00.000Z");
    assert!(replaced(&[older, base.clone(), ours.clone()], "ours").is_empty());
    // With no version of the base in the list, nothing can be said.
    assert!(replaced(&[theirs.clone(), ours.clone()], "ours").is_empty());
    // Two devices came in between, one of them twice with one content.
    let third = revision("r1b", "third", "2026-11-23T10:00:05.200Z");
    let theirs_again = revision("r1c", "theirs", "2026-11-23T10:00:05.250Z");
    assert_eq!(
        replaced(
            &[
                base.clone(),
                theirs,
                third.clone(),
                theirs_again.clone(),
                ours.clone()
            ],
            "ours"
        ),
        vec![third, theirs_again]
    );
    // An edit that another device made later, on top of this upload, is
    // the next change of the feed.
    let later = revision("r3", "later", "2026-11-23T10:02:00.000Z");
    assert!(replaced(&[base, ours, later], "later").is_empty());
}

#[test]
fn the_head_of_the_file_says_which_of_two_writes_was_replaced() {
    // Drive gives a version its time before it commits the write. Of
    // two writes of one moment, the one that holds the name can carry
    // the older time and be listed first.
    let base = revision("r0", "base", "2026-11-23T10:00:00.000Z");
    let ours_first = revision("r1", "ours", "2026-11-23T10:00:05.100Z");
    let theirs_last = revision("r2", "theirs", "2026-11-23T10:00:05.150Z");
    let listed = [base.clone(), ours_first, theirs_last.clone()];
    // This upload holds the name: the other write is the one to keep.
    assert_eq!(replaced(&listed, "ours"), vec![theirs_last]);
    // The other write holds the name: nothing of it is lost.
    assert!(replaced(&listed, "theirs").is_empty());

    let theirs_first = revision("r1", "theirs", "2026-11-23T10:00:05.100Z");
    let ours_last = revision("r2", "ours", "2026-11-23T10:00:05.150Z");
    let listed = [base.clone(), theirs_first.clone(), ours_last.clone()];
    // Listed last, and the name holds the other write all the same:
    // that write is not lost, and a copy of it would be a second copy
    // of the file. The device that wrote the head keeps this upload.
    assert!(replaced(&listed, "theirs").is_empty());
    assert_eq!(replaced(&listed, "ours"), vec![theirs_first.clone()]);

    // The check came late, and a later edit holds the name by then.
    // What was written at the moment of this upload is still kept,
    // on whichever side of it Drive lists it.
    let later = revision("r3", "later", "2026-11-23T10:04:00.000Z");
    assert_eq!(
        replaced(
            &[base.clone(), theirs_first.clone(), ours_last, later.clone()],
            "later"
        ),
        vec![theirs_first]
    );
    let ours_first = revision("r1", "ours", "2026-11-23T10:00:05.100Z");
    let theirs_last = revision("r2", "theirs", "2026-11-23T10:00:05.150Z");
    assert_eq!(
        replaced(&[base, ours_first, theirs_last.clone(), later], "later"),
        vec![theirs_last]
    );
}

#[test]
fn an_upload_that_a_program_other_than_vapor_replaced_keeps_its_own_version() {
    // The other write of that moment holds the name. A device that
    // runs Vapor would keep this upload's version from its own check.
    // The file still has the tag of this upload, so the writer was
    // another program, and nobody else keeps the version.
    let base = revision("r0", "base", "2026-11-23T10:00:00.000Z");
    let ours = revision("r1", "ours", "2026-11-23T10:00:05.100Z");
    let theirs = revision("r2", "theirs", "2026-11-23T10:00:05.400Z");
    let listed = [base.clone(), ours.clone(), theirs.clone()];
    let keep = |head: &str, own_tag: bool| {
        versions_of_the_same_moment(&listed, "base", "ours", Duration::from_secs(1), &[])
            .into_kept(Some(head), own_tag)
    };
    assert_eq!(keep("theirs", true), vec![ours.clone()]);
    // The tag of another device: its check keeps this version.
    assert!(keep("theirs", false).is_empty());
    // This upload holds the name: its own version needs no copy.
    assert_eq!(keep("ours", true), vec![theirs.clone()]);

    // An edit that such a program made a minute later, on top of this
    // upload, replaced nothing unseen, whatever the tag says.
    let later = revision("r3", "later", "2026-11-23T10:01:05.100Z");
    assert!(
        versions_of_the_same_moment(
            &[base, ours, later],
            "base",
            "ours",
            Duration::from_secs(1),
            &[]
        )
        .into_kept(Some("later"), true)
        .is_empty()
    );
}

#[test]
fn the_next_saves_of_this_device_are_not_writes_of_another_one() {
    // An editor saves a file each two seconds, and the check of the
    // first save runs after the third landed. The saves in between are
    // this device's own, and each replaced the one before it knowingly.
    let base = revision("r0", "base", "2026-11-23T10:00:00.000Z");
    let first = revision("r1", "ours", "2026-11-23T10:00:05.000Z");
    let second = revision("r2", "second save", "2026-11-23T10:00:05.900Z");
    let third = revision("r3", "third save", "2026-11-23T10:00:07.800Z");
    let listed = [base, first, second.clone(), third];
    let own_writes = ["second save".to_string(), "third save".to_string()];
    assert!(
        versions_of_the_same_moment(&listed, "base", "ours", Duration::from_secs(1), &own_writes)
            .into_kept(Some("third save"), false)
            .is_empty(),
        "a save of this device is not kept as a replaced version"
    );
    // The same list from a provider that did not write those versions:
    // the one inside the moment of the upload is another device's.
    assert_eq!(
        versions_of_the_same_moment(&listed, "base", "ours", Duration::from_secs(1), &[])
            .into_kept(Some("third save"), false),
        vec![second]
    );
}

#[test]
fn a_version_of_another_moment_is_history_wherever_drive_lists_it() {
    // The version the upload replaced carries a time its writer set
    // (Drive for desktop puts the time the file has on its disk), so
    // every version Drive stored before it is listed after it.
    let base = revision("r9", "base", "2019-03-01T08:00:00.000Z");
    let last_week = revision("r1", "last week", "2026-11-16T09:00:00.000Z");
    let yesterday = revision("r2", "yesterday", "2026-11-22T18:30:00.000Z");
    let ours = revision("r10", "ours", "2026-11-23T10:00:05.300Z");
    let listed = [base.clone(), last_week, yesterday, ours.clone()];
    assert!(
        replaced(&listed, "ours").is_empty(),
        "the history of a file is not a set of replaced versions"
    );

    // A write that landed while this upload committed is still found
    // among them.
    let theirs = revision("r3", "theirs", "2026-11-23T10:00:04.900Z");
    let mut with_a_race = listed.to_vec();
    with_a_race.insert(3, theirs.clone());
    assert_eq!(replaced(&with_a_race, "ours"), vec![theirs]);

    // A commit that took long leaves that much room for another write.
    let minutes_before = revision("r4", "slow", "2026-11-23T09:57:00.000Z");
    let slow = [base.clone(), minutes_before.clone(), ours.clone()];
    assert!(replaced(&slow, "ours").is_empty());
    assert_eq!(
        versions_of_the_same_moment(&slow, "base", "ours", Duration::from_secs(300), &[])
            .into_kept(Some("ours"), false),
        vec![minutes_before]
    );

    // A version with a time after the commit of this upload was not
    // replaced by it: a save that came three seconds later, or a
    // version its writer dated in the future.
    let soon_after = revision("r5", "next", "2026-11-23T10:00:08.300Z");
    let future = revision("r6", "future", "2031-01-01T00:00:00.000Z");
    assert!(replaced(&[base, ours, soon_after, future], "ours").is_empty());
}

/// A guarded overwrite of `doc.txt` (file `doc-id` in the root) that
/// lands, with the base `base-md5`.
fn landed_overwrite(provider: &GoogleDriveProvider, transport: &ScriptedHttpTransport) -> String {
    transport.push_response(
        200,
        r#"{"files":[{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}]}"#,
    );
    // The re-verification before the commit still sees the base.
    transport.push_response(
        200,
        r#"{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}"#,
    );
    let ours = md5_hex_of_bytes(b"our edit");
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("doc.txt");
    std::fs::write(&source, b"our edit").expect("seed");
    let mut session = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("doc.txt").expect("path"),
            op_id: "dev-op-doc".to_string(),
            precondition: RemotePrecondition::HashEquals("base-md5".to_string()),
        })
        .expect("session");
    assert!(matches!(
        session.step(u64::MAX).expect("the upload lands"),
        TransferStep::Completed(_)
    ));
    ours
}

#[test]
fn a_version_that_an_upload_replaced_unseen_is_kept_as_a_copy_at_the_next_poll() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let uploads_before = transport.recorded_requests().len();

    // The versions of the file: another device wrote between the
    // re-verification and the commit.
    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"10"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    // The file holds this upload: the other version is the replaced one.
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#); // no copy of that name yet
    transport.push_response(200, "their edit"); // the content of r1
    transport.push_response(200, r#"{"id":"copy-id"}"#); // the copy is created
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);

    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[uploads_before..];
    assert_eq!(
        work.len(),
        6,
        "{:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    assert!(work[0].url.contains("/files/doc-id/revisions?"));
    assert!(work[1].url.contains("/files/doc-id?fields="));
    assert!(work[3].url.contains("/files/doc-id/revisions/r1?alt=media"));
    assert_eq!(work[4].method, "POST");
    let created = String::from_utf8_lossy(&work[4].body);
    // 2026-11-23T10:00:05.100Z in epoch milliseconds.
    assert!(
        created.contains(r#""name":"doc~conflict-replaced-1795428005100.txt""#),
        "{created}"
    );
    assert!(
        created.contains(r#""parents":["root-folder"]"#),
        "{created}"
    );
    assert!(created.contains("their edit"));
    assert!(work[5].url.contains("/changes?"));

    // The work is done: the next poll reads the feed and nothing else.
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("second poll");
    assert_eq!(transport.recorded_requests().len(), requests.len() + 1);
}

#[test]
fn an_upload_that_replaced_only_its_base_makes_no_copy() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let uploads_before = transport.recorded_requests().len();

    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    assert_eq!(
        requests.len() - uploads_before,
        2,
        "one check, one read of the feed"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.url.contains("alt=media"))
    );
}

#[test]
fn an_upload_that_holds_the_name_keeps_a_version_that_drive_lists_after_it() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let uploads_before = transport.recorded_requests().len();

    // Both writes landed, and this one committed second: it holds the
    // name. Drive gave the other version the later time all the same,
    // so nothing is listed between the base and this upload.
    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"8"}},
                {{"id":"r2","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.122Z","size":"10"}}
            ]}}"#
        ),
    );
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[uploads_before..];
    assert!(
        work.iter()
            .any(|request| request.url.contains("/files/doc-id/revisions/r2?alt=media")),
        "the other version is read: {:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    let created = work
        .iter()
        .find(|request| request.method == "POST")
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .expect("the other version is kept as a copy");
    // 2026-11-23T10:00:05.122Z in epoch milliseconds.
    assert!(
        created.contains(r#""name":"doc~conflict-replaced-1795428005122.txt""#),
        "{created}"
    );
    assert!(created.contains("their edit"));
}

/// One more guarded overwrite of `doc.txt` by the same provider:
/// `content` over the version with the checksum `base`. `commit` is
/// the answer of the request that writes.
fn overwrite_again(
    provider: &GoogleDriveProvider,
    transport: &ScriptedHttpTransport,
    base: &str,
    content: &[u8],
    op_id: &str,
    commit: (u16, String),
) -> Result<TransferStep, ProviderError> {
    let current = format!(
        r#"{{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"{base}","size":"4","parents":["root-folder"]}}"#
    );
    transport.push_response(200, current.clone()); // the file the path names
    transport.push_response(200, current); // the re-verification
    transport.push_response(commit.0, commit.1);
    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("doc.txt");
    std::fs::write(&source, content).expect("seed");
    provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("doc.txt").expect("path"),
            op_id: op_id.to_string(),
            precondition: RemotePrecondition::HashEquals(base.to_string()),
        })
        .expect("session")
        .step(u64::MAX)
}

#[test]
fn the_next_saves_of_this_device_make_no_copies() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // An editor saves three times inside one poll of the feed.
    let first = landed_overwrite(&provider, &transport);
    let second = md5_hex_of_bytes(b"second save");
    let third = md5_hex_of_bytes(b"third save");
    for (base, content, md5) in [
        (&first, &b"second save"[..], &second),
        (&second, &b"third save"[..], &third),
    ] {
        let step = overwrite_again(
            &provider,
            &transport,
            base,
            content,
            "dev-op-next",
            (200, format!(r#"{{"id":"doc-id","md5Checksum":"{md5}"}}"#)),
        );
        assert!(matches!(step, Ok(TransferStep::Completed(_))));
    }
    let uploads_before = transport.recorded_requests().len();

    // Each of the three checks reads the same list. The versions after
    // an upload are this device's own.
    let versions = format!(
        r#"{{"revisions":[
            {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
            {{"id":"r1","md5Checksum":"{first}","modifiedTime":"2026-11-23T10:00:05.000Z","size":"8"}},
            {{"id":"r2","md5Checksum":"{second}","modifiedTime":"2026-11-23T10:00:05.900Z","size":"11"}},
            {{"id":"r3","md5Checksum":"{third}","modifiedTime":"2026-11-23T10:00:06.700Z","size":"10"}}
        ]}}"#
    );
    for _ in 0..3 {
        transport.push_response(200, versions.clone());
    }
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[uploads_before..];
    assert_eq!(
        work.len(),
        4,
        "three lists and the feed: {:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    assert!(
        !work
            .iter()
            .any(|request| request.method == "POST" || request.url.contains("alt=media")),
        "a save of this device is not kept as a replaced version"
    );
}

#[test]
fn an_upload_that_another_program_replaced_keeps_its_own_version() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let uploads_before = transport.recorded_requests().len();

    // A program that is not Vapor wrote at the same moment and its
    // write committed second. It does not look at what it replaces,
    // and it leaves the tag of this upload on the file.
    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"8"}},
                {{"id":"r2","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.400Z","size":"10"}}
            ]}}"#
        ),
    );
    transport.push_response(
        200,
        format!(
            r#"{{"id":"doc-id","md5Checksum":"{theirs}","appProperties":{{"vaporOpId":"dev-op-doc"}}}}"#
        ),
    );
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "our edit"); // the content of r1
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[uploads_before..];
    assert!(
        work.iter()
            .any(|request| request.url.contains("/files/doc-id/revisions/r1?alt=media")),
        "the version of this upload is read: {:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    let created = work
        .iter()
        .find(|request| request.method == "POST")
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .expect("the version of this upload is kept as a copy");
    // 2026-11-23T10:00:05.100Z in epoch milliseconds.
    assert!(
        created.contains(r#""name":"doc~conflict-replaced-1795428005100.txt""#),
        "{created}"
    );
    assert!(created.contains("our edit"));
}

#[test]
fn a_device_that_runs_vapor_and_holds_the_name_is_left_to_keep_the_other_version() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let uploads_before = transport.recorded_requests().len();

    // The same versions, and the file carries the tag of another
    // upload: a device that runs Vapor wrote the head, and its check
    // keeps the version of this one.
    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"8"}},
                {{"id":"r2","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.400Z","size":"10"}}
            ]}}"#
        ),
    );
    transport.push_response(
        200,
        format!(
            r#"{{"id":"doc-id","md5Checksum":"{theirs}","appProperties":{{"vaporOpId":"other-device-op"}}}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    let requests = transport.recorded_requests();
    assert_eq!(
        requests.len() - uploads_before,
        3,
        "the versions, the head, the feed"
    );
}

#[test]
fn a_commit_whose_answer_is_lost_still_gets_its_check() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // The write lands, and its answer does not come.
    let ours = md5_hex_of_bytes(b"our edit");
    transport.push_response(
        200,
        r#"{"files":[{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}]}"#,
    );
    transport.push_response(
        200,
        r#"{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}"#,
    );
    transport.push_response(503, "");
    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("doc.txt");
    std::fs::write(&source, b"our edit").expect("seed");
    let error = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("doc.txt").expect("path"),
            op_id: "dev-op-doc".to_string(),
            precondition: RemotePrecondition::HashEquals("base-md5".to_string()),
        })
        .expect("session")
        .step(u64::MAX)
        .expect_err("no answer");
    assert_eq!(error.kind, ProviderErrorKind::Transient);

    // The retry of the engine finds its own bytes in the cloud and
    // sends nothing, so this check is the only one. Another device
    // wrote between the look and the write.
    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"10"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert!(
        transport.recorded_requests().iter().any(|request| {
            request.method == "POST"
                && String::from_utf8_lossy(&request.body).contains("their edit")
        }),
        "the version that the lost commit replaced is kept"
    );

    // A commit that did not land leaves a check that finds no version
    // of it and ends: one request.
    let unsent = overwrite_again(
        &provider,
        &transport,
        &ours,
        b"never stored",
        "dev-op-later",
        (503, String::new()),
    );
    assert!(unsent.is_err());
    let before = transport.recorded_requests().len();
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert_eq!(transport.recorded_requests().len() - before, 2);
}

#[test]
fn a_head_that_cannot_be_read_is_asked_again_and_a_file_that_is_gone_ends_the_check() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let theirs = md5_hex_of_bytes(b"their edit");
    let versions = format!(
        r#"{{"revisions":[
            {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
            {{"id":"r1","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"10"}},
            {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
        ]}}"#
    );

    // The cloud does not answer the question about the head: the check
    // stays, and the feed is read all the same.
    transport.push_response(200, versions.clone());
    transport.push_response(503, "");
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    // At the next poll the file is gone: nothing is left to keep, and
    // the check ends.
    transport.push_response(200, versions);
    transport.push_response(404, "");
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    let before = transport.recorded_requests().len();
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t4"}"#);
    provider.poll_changes(Some("t3"), 100).expect("poll");
    assert_eq!(
        transport.recorded_requests().len() - before,
        1,
        "the feed alone"
    );
    assert!(
        !transport
            .recorded_requests()
            .iter()
            .any(|request| request.method == "POST" && request.url.contains("uploadType=multipart")),
        "no copy was made"
    );
}

#[test]
fn an_upload_that_another_write_replaced_makes_no_copy_of_the_file() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let uploads_before = transport.recorded_requests().len();

    // Both writes landed. Drive lists this upload last, and the name
    // holds the other write: its commit came second.
    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"10"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.150Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(
        200,
        format!(r#"{{"id":"doc-id","md5Checksum":"{theirs}"}}"#),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    assert_eq!(
        requests.len() - uploads_before,
        3,
        "the versions, the head, the feed"
    );
    assert!(
        !requests[uploads_before..]
            .iter()
            .any(|request| request.method == "POST" || request.url.contains("alt=media")),
        "the head of the file is not a replaced version"
    );
}

#[test]
fn a_check_that_fails_is_tried_again_and_a_copy_that_is_there_is_not_made_twice() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);

    // The first poll cannot read the versions.
    transport.push_response(503, "");
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider
        .poll_changes(Some("t1"), 100)
        .expect("the feed is read all the same");

    // The second can, and another device kept the version already.
    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"10"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(
        200,
        format!(
            r#"{{"files":[{{"id":"copy-id","name":"doc~conflict-replaced-1795428005100.txt","mimeType":"text/plain","md5Checksum":"{theirs}"}}]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    let before = transport.recorded_requests().len();
    provider.poll_changes(Some("t2"), 100).expect("poll");

    let requests = transport.recorded_requests();
    assert_eq!(
        requests.len() - before,
        4,
        "the versions, the head, the name, the feed"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.url.contains("alt=media"))
    );
    assert!(
        !requests[before..]
            .iter()
            .any(|request| request.method == "POST")
    );
}

#[test]
fn a_check_waits_through_an_outage_of_any_length() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);

    // The cloud cannot be reached for far more polls than the check
    // has tries: an outage spends none of them.
    for _ in 0..(REPLACED_WORK_ATTEMPTS * 3) {
        transport.push_response(503, "");
        transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
        provider
            .poll_changes(Some("t1"), 100)
            .expect("the feed is read");
    }

    let theirs = md5_hex_of_bytes(b"their edit");
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"10"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");

    assert!(
        transport.recorded_requests().iter().any(|request| {
            request.method == "POST"
                && request.url.contains("uploadType=multipart")
                && String::from_utf8_lossy(&request.body).contains("their edit")
        }),
        "the replaced version is kept when the cloud is back"
    );
}

#[test]
fn a_large_replaced_version_is_copied_one_chunk_for_each_poll() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let size = CHUNK_START + 1000;

    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r1","md5Checksum":"big-md5","modifiedTime":"2026-11-23T10:00:05.100Z","size":"{size}"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response_with_headers(
        200,
        "",
        vec![(
            "Location".to_string(),
            "https://upload.example/copy-session".to_string(),
        )],
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("first poll");

    // One step of the copy for each poll: the bytes are outside the
    // budgets of the engine, and the feed must not wait for them.
    transport.push_response(206, vec![1_u8; CHUNK_START as usize]); // the first range
    transport.push_response(308, ""); // accepted, not the end
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t2"), 100).expect("second poll");

    transport.push_response(206, vec![1_u8; 1000]); // the rest
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("third poll");

    let requests = transport.recorded_requests();
    let ranges: Vec<&str> = requests
        .iter()
        .flat_map(|request| request.headers.iter())
        .filter(|(name, _)| name == "Content-Range")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(
        ranges,
        vec![
            format!("bytes 0-{}/{size}", CHUNK_START - 1).as_str(),
            format!("bytes {CHUNK_START}-{}/{size}", size - 1).as_str(),
        ]
    );
    // Nothing is left to do.
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t4"}"#);
    let before = transport.recorded_requests().len();
    provider.poll_changes(Some("t3"), 100).expect("third poll");
    assert_eq!(transport.recorded_requests().len(), before + 1);
}

/// The records that `provider` reported as started since the last
/// call. A test that expects something else in the list fails here.
fn started_records(provider: &GoogleDriveProvider) -> Vec<String> {
    provider
        .take_follow_up_changes()
        .into_iter()
        .map(|change| match change {
            FollowUpChange::Started(record) => record,
            FollowUpChange::Finished(record) => panic!("a record ended early: {record}"),
        })
        .collect()
}

/// A provider as the daemon builds it after a stop: nothing in memory,
/// and the records that the stopped daemon had stored.
fn provider_after_a_stop(
    transport: &Arc<ScriptedHttpTransport>,
    records: &[String],
) -> GoogleDriveProvider {
    let provider = provider_with(transport.clone());
    assert_eq!(
        provider.resume_follow_ups(records.to_vec()),
        Vec::<String>::new(),
        "every record is taken back"
    );
    transport.push_response(200, ROOT_LISTING);
    provider.ensure_cloud_root("/Vapor").expect("ensure");
    provider
}

/// The versions of `doc.txt` when another device wrote `their edit`
/// between the look and the write of `landed_overwrite`.
fn versions_with_their_edit(ours: &str, their_size: u64) -> String {
    let theirs = md5_hex_of_bytes(b"their edit");
    format!(
        r#"{{"revisions":[
            {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
            {{"id":"r1","md5Checksum":"{theirs}","modifiedTime":"2026-11-23T10:00:05.100Z","size":"{their_size}"}},
            {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
        ]}}"#
    )
}

#[test]
fn a_check_that_a_stopped_daemon_left_runs_after_the_start_and_makes_the_copy() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    // The record is there when the step of the upload has returned,
    // so the daemon stores it with the outcome of the transfer.
    let records = started_records(&provider);
    assert_eq!(records.len(), 1, "one check for one overwrite");
    assert!(
        !records[0].contains("ya29") && !records[0].contains("1//rt"),
        "no token in what the daemon stores: {}",
        records[0]
    );
    // The daemon stops before a poll made the check.
    drop(provider);

    let provider = provider_after_a_stop(&transport, &records);
    let before = transport.recorded_requests().len();
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#); // no copy of that name yet
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[before..];
    assert_eq!(
        work.len(),
        6,
        "{:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    assert!(work[0].url.contains("/files/doc-id/revisions?"));
    assert!(work[3].url.contains("/files/doc-id/revisions/r1?alt=media"));
    assert_eq!(work[4].method, "POST");
    let created = String::from_utf8_lossy(&work[4].body);
    assert!(
        created.contains(r#""name":"doc~conflict-replaced-1795428005100.txt""#),
        "the copy has the name it has with no stop: {created}"
    );
    assert!(
        created.contains(r#""parents":["root-folder"]"#),
        "{created}"
    );
    assert!(created.contains("their edit"));
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())],
        "the daemon forgets the record when the copy is made"
    );

    // Nothing is left: the next poll reads the feed alone.
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("second poll");
    assert_eq!(transport.recorded_requests().len(), requests.len() + 1);
    assert!(provider.take_follow_up_changes().is_empty());
}

#[test]
fn a_commit_whose_answer_is_lost_leaves_a_check_that_survives_a_stop() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = md5_hex_of_bytes(b"our edit");
    transport.push_response(
        200,
        r#"{"files":[{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}]}"#,
    );
    transport.push_response(
        200,
        r#"{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}"#,
    );
    transport.push_response(503, "");
    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("doc.txt");
    std::fs::write(&source, b"our edit").expect("seed");
    let failed = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("doc.txt").expect("path"),
            op_id: "dev-op-doc".to_string(),
            precondition: RemotePrecondition::HashEquals("base-md5".to_string()),
        })
        .expect("session")
        .step(u64::MAX);
    assert_eq!(
        failed.expect_err("no answer").kind,
        ProviderErrorKind::Transient
    );
    // The upload failed, and the write can have landed: the record is
    // there for the daemon all the same.
    let records = started_records(&provider);
    assert_eq!(records.len(), 1);
    drop(provider);

    let provider = provider_after_a_stop(&transport, &records);
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert!(
        transport.recorded_requests().iter().any(|request| {
            request.method == "POST"
                && request.url.contains("uploadType=multipart")
                && String::from_utf8_lossy(&request.body).contains("their edit")
        }),
        "the version that the lost commit replaced is kept after the stop"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );
}

#[test]
fn a_copy_that_a_stop_cut_is_finished_after_the_start_and_made_one_time() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let records = started_records(&provider);
    let size = CHUNK_START + 1000;
    let theirs = md5_hex_of_bytes(b"their edit");
    let open_a_session = |transport: &ScriptedHttpTransport| {
        transport.push_response(200, versions_with_their_edit(&ours, size));
        transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
        transport.push_response(200, r#"{"files":[]}"#); // no copy under the name
        transport.push_response_with_headers(
            200,
            "",
            vec![(
                "Location".to_string(),
                "https://upload.example/copy-session".to_string(),
            )],
        );
        transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    };

    // The first daemon reads the versions, opens the session of the
    // copy, and stops before a byte of it is in.
    open_a_session(&transport);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert!(
        provider.take_follow_up_changes().is_empty(),
        "the work of the record has not ended, and the address of the session is not for the daemon to store"
    );
    assert!(!records[0].contains("upload.example"));
    drop(provider);

    // The second daemon starts the copy again, sends its first chunk,
    // and stops too.
    let provider = provider_after_a_stop(&transport, &records);
    open_a_session(&transport);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    transport.push_response(206, vec![1_u8; CHUNK_START as usize]);
    transport.push_response(308, "");
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert!(provider.take_follow_up_changes().is_empty());
    drop(provider);

    // The third daemon makes the whole copy.
    let provider = provider_after_a_stop(&transport, &records);
    open_a_session(&transport);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    transport.push_response(206, vec![1_u8; CHUNK_START as usize]);
    transport.push_response(308, "");
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    transport.push_response(206, vec![1_u8; 1000]);
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );
    drop(provider);

    // A daemon that stopped after the copy was in and before it stored
    // the end of the record gets the record once more. The copy is
    // under its name, and no second one is made.
    let provider = provider_after_a_stop(&transport, &records);
    let before = transport.recorded_requests().len();
    transport.push_response(200, versions_with_their_edit(&ours, size));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(
        200,
        format!(
            r#"{{"files":[{{"id":"copy-id","name":"doc~conflict-replaced-1795428005100.txt","mimeType":"text/plain","md5Checksum":"{theirs}"}}]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t4"}"#);
    provider.poll_changes(Some("t3"), 100).expect("poll");
    assert_eq!(
        transport.recorded_requests().len() - before,
        4,
        "the versions, the head, the name, the feed"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );

    // Over the four daemons, one session got its last chunk: one copy.
    let last_chunk = format!("bytes {CHUNK_START}-{}/{size}", size - 1);
    let copies_made = transport
        .recorded_requests()
        .iter()
        .filter(|request| {
            request
                .headers
                .iter()
                .any(|(name, value)| name == "Content-Range" && value == &last_chunk)
        })
        .count();
    assert_eq!(copies_made, 1);
}

#[test]
fn a_check_that_finds_nothing_to_keep_after_a_stop_ends_and_is_forgotten() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    let ours = landed_overwrite(&provider, &transport);
    let records = started_records(&provider);
    drop(provider);

    // The file is gone, and its versions with it.
    let provider = provider_after_a_stop(&transport, &records);
    let before = transport.recorded_requests().len();
    transport.push_response(404, "");
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert_eq!(transport.recorded_requests().len() - before, 2);
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );
    drop(provider);

    // The versions hold the base and this upload: nothing was replaced
    // unseen.
    let provider = provider_after_a_stop(&transport, &records);
    let before = transport.recorded_requests().len();
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert_eq!(transport.recorded_requests().len() - before, 2);
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );

    // Each of the two is forgotten: the next poll reads the feed alone.
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    let before = transport.recorded_requests().len();
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert_eq!(transport.recorded_requests().len() - before, 1);
    assert!(
        !transport
            .recorded_requests()
            .iter()
            .any(|request| request.method == "POST" && request.url.contains("uploadType=")),
        "no copy was made"
    );
}

#[test]
fn a_check_that_can_never_be_made_does_not_stay() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    landed_overwrite(&provider, &transport);
    let record = started_records(&provider).remove(0);
    drop(provider);

    // A record that this build cannot read, and one that is older than
    // Drive keeps a version, are not taken back: the daemon forgets
    // them at the start.
    let FollowUpRecord::ReplacedCheck(check) = serde_json::from_str(&record).expect("a record")
    else {
        panic!("a landed overwrite leaves a check that knows its file by the id: {record}");
    };
    let old = serde_json::to_string(&FollowUpRecord::ReplacedCheck(ReplacedCheck {
        landed_at_ms: check.landed_at_ms - (REPLACED_CHECK_MAX_AGE.as_millis() as u64 + 60_000),
        ..check.clone()
    }))
    .expect("a record");
    // So is one whose time is no time: a damaged entry must not stop
    // the daemon at its start.
    let no_time = serde_json::to_string(&FollowUpRecord::ReplacedCheck(ReplacedCheck {
        landed_at_ms: u64::MAX,
        ..check.clone()
    }))
    .expect("a record");
    let provider = provider_with(transport.clone());
    let unusable = vec![
        "not a record".to_string(),
        r#"{"anotherKind":{}}"#.to_string(),
        old,
        no_time,
    ];
    assert_eq!(provider.resume_follow_ups(unusable.clone()), unusable);

    // A check for a file in a folder that is no longer the cloud root
    // of the profile ends with no request and no copy there.
    let elsewhere = serde_json::to_string(&FollowUpRecord::ReplacedCheck(ReplacedCheck {
        root_id: "another-root".to_string(),
        ..check
    }))
    .expect("a record");
    // The account can no longer read the file of the last one: the
    // check has its tries, and then it ends.
    assert!(
        provider
            .resume_follow_ups(vec![elsewhere.clone(), record.clone()])
            .is_empty()
    );
    transport.push_response(200, ROOT_LISTING);
    provider.ensure_cloud_root("/Vapor").expect("ensure");
    let mut ended = Vec::new();
    for _ in 0..REPLACED_WORK_ATTEMPTS {
        transport.push_response(403, r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#);
        transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
        provider.poll_changes(Some("t1"), 100).expect("poll");
        ended.extend(provider.take_follow_up_changes());
    }
    assert_eq!(
        ended,
        vec![
            FollowUpChange::Finished(elsewhere),
            FollowUpChange::Finished(record),
        ]
    );
    let before = transport.recorded_requests().len();
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert_eq!(
        transport.recorded_requests().len() - before,
        1,
        "the feed alone"
    );
    assert!(
        !transport
            .recorded_requests()
            .iter()
            .any(|request| request.url.contains("another-root")),
        "nothing was asked of the other folder"
    );
}

#[test]
fn the_next_saves_of_this_device_make_no_copies_after_a_stop() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    // An editor saves two times, and the daemon stops before a poll.
    let first = landed_overwrite(&provider, &transport);
    let second = md5_hex_of_bytes(b"second save");
    let step = overwrite_again(
        &provider,
        &transport,
        &first,
        b"second save",
        "dev-op-next",
        (
            200,
            format!(r#"{{"id":"doc-id","md5Checksum":"{second}"}}"#),
        ),
    );
    assert!(matches!(step, Ok(TransferStep::Completed(_))));
    let records = started_records(&provider);
    assert_eq!(records.len(), 2);
    drop(provider);

    let provider = provider_after_a_stop(&transport, &records);
    let before = transport.recorded_requests().len();
    // The second save is in the list, less than a second after the
    // first: it is this device's own, also after the stop.
    let versions = format!(
        r#"{{"revisions":[
            {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
            {{"id":"r1","md5Checksum":"{first}","modifiedTime":"2026-11-23T10:00:05.000Z","size":"8"}},
            {{"id":"r2","md5Checksum":"{second}","modifiedTime":"2026-11-23T10:00:05.900Z","size":"11"}}
        ]}}"#
    );
    transport.push_response(200, versions.clone());
    transport.push_response(200, versions);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[before..];
    assert_eq!(
        work.len(),
        3,
        "two lists and the feed: {:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        records
            .into_iter()
            .map(FollowUpChange::Finished)
            .collect::<Vec<_>>()
    );
}

/// What the daemon does when an upload that runs again finds its own
/// content in the cloud under the tag `dev-op-lost`, which it gave out
/// and never recorded: one lookup of `doc.txt`, on a worker, and then
/// the call that says so, with the base `base-md5`. Returns the
/// checksum of the content.
fn the_daemon_finds_its_content_in_the_cloud(
    provider: &GoogleDriveProvider,
    transport: &ScriptedHttpTransport,
) -> String {
    let ours = md5_hex_of_bytes(b"our edit");
    transport.push_response(
        200,
        format!(
            r#"{{"files":[{{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"{ours}","size":"8","parents":["root-folder"]}}]}}"#
        ),
    );
    let path = RemotePath::new("doc.txt").expect("path");
    let seen = provider.stat(&path).expect("stat").expect("the file");
    assert_eq!(seen.content_hash.as_deref(), Some(ours.as_str()));
    let before = transport.recorded_requests().len();
    provider.note_unrecorded_write(&path, "base-md5", &ours, "dev-op-lost");
    assert_eq!(
        transport.recorded_requests().len(),
        before,
        "the call runs on the tick thread: it makes no request"
    );
    ours
}

/// How many requests created a file with the bytes `their edit`.
fn copies_of_their_edit(transport: &ScriptedHttpTransport) -> usize {
    transport
        .recorded_requests()
        .iter()
        .filter(|request| {
            request.method == "POST"
                && request.url.contains("uploadType=multipart")
                && String::from_utf8_lossy(&request.body).contains("their edit")
        })
        .count()
}

#[test]
fn a_write_whose_outcome_no_daemon_recorded_gets_its_check_and_the_other_version_is_kept() {
    // Two devices overwrote `doc.txt` in the same moment. The write of
    // this one landed last and holds the name, and the daemon was
    // killed before it stored the outcome: no record of a check is
    // left. The provider of the next start has nothing in memory.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_after_a_stop(&transport, &[]);
    let ours = the_daemon_finds_its_content_in_the_cloud(&provider, &transport);

    // The check is owed from the call on, so the daemon stores it in
    // the tick that records the file as in sync.
    let records = started_records(&provider);
    assert_eq!(records.len(), 1, "one check for one write");
    assert!(
        !records[0].contains("ya29") && !records[0].contains("1//rt"),
        "no token in what the daemon stores: {}",
        records[0]
    );

    let before = transport.recorded_requests().len();
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#); // no copy of that name yet
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[before..];
    assert_eq!(
        work.len(),
        6,
        "{:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    assert!(work[0].url.contains("/files/doc-id/revisions?"));
    assert!(work[1].url.contains("/files/doc-id?fields="));
    assert!(work[3].url.contains("/files/doc-id/revisions/r1?alt=media"));
    assert_eq!(work[4].method, "POST");
    let created = String::from_utf8_lossy(&work[4].body);
    assert!(
        created.contains(r#""name":"doc~conflict-replaced-1795428005100.txt""#),
        "the copy has the name it has for a recorded write: {created}"
    );
    assert!(
        created.contains(r#""parents":["root-folder"]"#),
        "{created}"
    );
    assert!(created.contains("their edit"));
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())],
        "the daemon forgets the record when the copy is made"
    );

    // Nothing is left: the next poll reads the feed alone.
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("second poll");
    assert_eq!(transport.recorded_requests().len(), requests.len() + 1);
    assert_eq!(copies_of_their_edit(&transport), 1);
}

#[test]
fn a_write_with_no_recorded_outcome_that_replaced_its_base_costs_one_request_and_no_copy() {
    // The common case: the upload landed over the content it meant to
    // replace, and the daemon stopped before it stored the outcome.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_after_a_stop(&transport, &[]);
    let ours = the_daemon_finds_its_content_in_the_cloud(&provider, &transport);
    let records = started_records(&provider);
    assert_eq!(records.len(), 1);

    // Nothing is between the base and the content. The versions that
    // the base itself replaced are the history of the file.
    let before = transport.recorded_requests().len();
    transport.push_response(
        200,
        format!(
            r#"{{"revisions":[
                {{"id":"r-1","md5Checksum":"older-md5","modifiedTime":"2026-11-23T09:58:00.000Z","size":"6"}},
                {{"id":"r0","md5Checksum":"base-md5","modifiedTime":"2026-11-23T10:00:00.000Z","size":"4"}},
                {{"id":"r2","md5Checksum":"{ours}","modifiedTime":"2026-11-23T10:00:05.300Z","size":"8"}}
            ]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[before..];
    assert_eq!(
        work.len(),
        2,
        "the versions and the feed: {:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    // A copy is the one thing this work warns about, and none is made:
    // no version is read, and no file is created.
    assert!(
        !work
            .iter()
            .any(|request| request.method != "GET" || request.url.contains("alt=media")),
        "nothing is read or written for a copy"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())],
        "the check ends, and the daemon forgets it"
    );
}

#[test]
fn a_second_call_for_one_write_makes_no_second_check_and_no_second_copy() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_after_a_stop(&transport, &[]);
    let ours = the_daemon_finds_its_content_in_the_cloud(&provider, &transport);
    let path = RemotePath::new("doc.txt").expect("path");

    // The call comes again while the check waits for its poll: a tick
    // that the database did not save, and the upload that ran again.
    provider.note_unrecorded_write(&path, "base-md5", &ours, "dev-op-lost");
    let records = started_records(&provider);
    assert_eq!(records.len(), 1, "one check is owed for one write");

    let before = transport.recorded_requests().len();
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert_eq!(
        transport.recorded_requests().len() - before,
        6,
        "one check, one copy, the feed"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );

    // The call comes once more after the check has ended. The check
    // is made again, finds the copy under its name, and makes no
    // second one.
    provider.note_unrecorded_write(&path, "base-md5", &ours, "dev-op-lost");
    let again = started_records(&provider);
    assert_eq!(again.len(), 1);
    let theirs = md5_hex_of_bytes(b"their edit");
    let before = transport.recorded_requests().len();
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(
        200,
        format!(
            r#"{{"files":[{{"id":"copy-id","name":"doc~conflict-replaced-1795428005100.txt","mimeType":"text/plain","md5Checksum":"{theirs}"}}]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert_eq!(
        transport.recorded_requests().len() - before,
        4,
        "the versions, the head, the name, the feed"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(again[0].clone())]
    );
    assert_eq!(copies_of_their_edit(&transport), 1);
}

#[test]
fn a_write_that_a_stored_check_names_gets_no_second_check_from_the_call() {
    // The answer of the commit did not come, and the upload left its
    // check with the daemon, which stored it and stopped.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = ensured_provider(transport.clone());
    transport.push_response(
        200,
        r#"{"files":[{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}]}"#,
    );
    transport.push_response(
        200,
        r#"{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"base-md5","size":"4","parents":["root-folder"]}"#,
    );
    transport.push_response(503, "");
    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("doc.txt");
    std::fs::write(&source, b"our edit").expect("seed");
    let failed = provider
        .begin_upload(UploadRequest {
            local_source: source,
            remote_path: RemotePath::new("doc.txt").expect("path"),
            op_id: "dev-op-doc".to_string(),
            precondition: RemotePrecondition::HashEquals("base-md5".to_string()),
        })
        .expect("session")
        .step(u64::MAX);
    assert_eq!(
        failed.expect_err("no answer").kind,
        ProviderErrorKind::Transient
    );
    let stored = started_records(&provider);
    assert_eq!(stored.len(), 1);
    drop(provider);

    // The next daemon takes the check back. Its upload runs again,
    // finds its content in the cloud, and says so: the stored check is
    // the check for that write.
    let provider = provider_after_a_stop(&transport, &stored);
    let ours = the_daemon_finds_its_content_in_the_cloud(&provider, &transport);
    assert!(
        provider.take_follow_up_changes().is_empty(),
        "no second check is owed for the write"
    );

    let before = transport.recorded_requests().len();
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert_eq!(
        transport.recorded_requests().len() - before,
        6,
        "one check, one copy, the feed"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(stored[0].clone())]
    );
    assert_eq!(copies_of_their_edit(&transport), 1);
}

#[test]
fn the_check_for_a_write_with_no_recorded_outcome_survives_the_next_stops() {
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_after_a_stop(&transport, &[]);
    let ours = the_daemon_finds_its_content_in_the_cloud(&provider, &transport);
    let records = started_records(&provider);
    assert_eq!(records.len(), 1);
    // The daemon stored the record, and stops before a poll.
    drop(provider);

    // The second daemon makes the copy, and stops before it stored the
    // end of the record.
    let provider = provider_after_a_stop(&transport, &records);
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(200, r#"{"files":[]}"#);
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");
    assert_eq!(copies_of_their_edit(&transport), 1);
    drop(provider);

    // The third daemon gets the record once more. The copy is under
    // its name, and no second one is made.
    let provider = provider_after_a_stop(&transport, &records);
    let theirs = md5_hex_of_bytes(b"their edit");
    let before = transport.recorded_requests().len();
    transport.push_response(200, versions_with_their_edit(&ours, 10));
    transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
    transport.push_response(
        200,
        format!(
            r#"{{"files":[{{"id":"copy-id","name":"doc~conflict-replaced-1795428005100.txt","mimeType":"text/plain","md5Checksum":"{theirs}"}}]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert_eq!(
        transport.recorded_requests().len() - before,
        4,
        "the versions, the head, the name, the feed"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );
    assert_eq!(copies_of_their_edit(&transport), 1);
}

#[test]
fn a_file_known_by_its_path_only_gets_its_check_when_the_poll_has_read_its_ids() {
    // The feed took the file out of what the provider knows between
    // the lookup of the daemon and its call: the call has no id to
    // name the file by, and it makes no request to read one.
    let transport = Arc::new(ScriptedHttpTransport::new());
    let provider = provider_after_a_stop(&transport, &[]);
    let ours = md5_hex_of_bytes(b"our edit");
    let path = RemotePath::new("notes/doc.txt").expect("path");
    let before = transport.recorded_requests().len();
    provider.note_unrecorded_write(&path, "base-md5", &ours, "dev-op-lost");
    provider.note_unrecorded_write(&path, "base-md5", &ours, "dev-op-lost");
    assert_eq!(transport.recorded_requests().len(), before, "no request");
    let records = started_records(&provider);
    assert_eq!(records.len(), 1, "one check for one write");
    assert!(records[0].contains("notes/doc.txt"), "{}", records[0]);

    // The poll reads the folder and the file by the path, and makes
    // the check from there.
    let folder = r#"{"files":[{"id":"notes-id","name":"notes","mimeType":"application/vnd.google-apps.folder","parents":["root-folder"]}]}"#;
    let file = format!(
        r#"{{"files":[{{"id":"doc-id","name":"doc.txt","mimeType":"text/plain","md5Checksum":"{ours}","size":"8","parents":["notes-id"]}}]}}"#
    );
    let script_the_check = |transport: &ScriptedHttpTransport, name_lookup: String| {
        transport.push_response(200, folder);
        transport.push_response(200, file.clone());
        transport.push_response(200, versions_with_their_edit(&ours, 10));
        transport.push_response(200, format!(r#"{{"id":"doc-id","md5Checksum":"{ours}"}}"#));
        transport.push_response(200, name_lookup);
    };
    script_the_check(&transport, r#"{"files":[]}"#.to_string());
    transport.push_response(200, "their edit");
    transport.push_response(200, r#"{"id":"copy-id"}"#);
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);
    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[before..];
    assert_eq!(
        work.len(),
        8,
        "{:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    assert!(work[2].url.contains("/files/doc-id/revisions?"));
    assert!(work[5].url.contains("/files/doc-id/revisions/r1?alt=media"));
    let created = String::from_utf8_lossy(&work[6].body);
    assert!(
        created.contains(r#""name":"doc~conflict-replaced-1795428005100.txt""#),
        "{created}"
    );
    assert!(
        created.contains(r#""parents":["notes-id"]"#),
        "the copy is next to the file: {created}"
    );
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );
    drop(provider);

    // A daemon that stopped before it stored the end of the record
    // reads the ids again, and finds the copy.
    let provider = provider_after_a_stop(&transport, &records);
    let theirs = md5_hex_of_bytes(b"their edit");
    script_the_check(
        &transport,
        format!(
            r#"{{"files":[{{"id":"copy-id","name":"doc~conflict-replaced-1795428005100.txt","mimeType":"text/plain","md5Checksum":"{theirs}"}}]}}"#
        ),
    );
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t3"}"#);
    provider.poll_changes(Some("t2"), 100).expect("poll");
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );
    assert_eq!(copies_of_their_edit(&transport), 1);

    // No file is at the path any more: the check ends with nothing to
    // read.
    let provider = provider_after_a_stop(&transport, &records);
    let before = transport.recorded_requests().len();
    transport.push_response(200, r#"{"files":[]}"#); // no folder of that name
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t4"}"#);
    provider.poll_changes(Some("t3"), 100).expect("poll");
    assert_eq!(transport.recorded_requests().len() - before, 2);
    assert_eq!(
        provider.take_follow_up_changes(),
        vec![FollowUpChange::Finished(records[0].clone())]
    );
}
