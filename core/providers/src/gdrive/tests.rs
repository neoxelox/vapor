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
            .map(|change| (change.path.as_str(), change.kind))
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

#[test]
fn a_version_between_the_base_and_the_upload_was_replaced_unseen() {
    let base = revision("r0", "base", "2026-11-23T10:00:00.000Z");
    let theirs = revision("r1", "theirs", "2026-11-23T10:00:05.100Z");
    let ours = revision("r2", "ours", "2026-11-23T10:00:05.300Z");

    // Nobody came in between.
    assert!(revisions_replaced_unseen(&[base.clone(), ours.clone()], "base", "ours").is_empty());
    // Another device's upload landed between the check and the commit.
    assert_eq!(
        revisions_replaced_unseen(
            &[base.clone(), theirs.clone(), ours.clone()],
            "base",
            "ours"
        ),
        vec![theirs.clone()]
    );
    // The order is the one Drive lists, not the one of the times: a
    // client may give its version the time the file has on its disk.
    let old_clock = revision("r1", "theirs", "2026-11-01T08:00:00.000Z");
    assert_eq!(
        revisions_replaced_unseen(
            &[base.clone(), old_clock.clone(), ours.clone()],
            "base",
            "ours"
        ),
        vec![old_clock]
    );
    // A version after the upload is the next change, which the feed
    // reports; it was not replaced by this upload.
    let later = revision("r3", "later", "2026-11-23T10:00:09.000Z");
    assert!(
        revisions_replaced_unseen(&[base.clone(), ours.clone(), later], "base", "ours").is_empty()
    );
    // An older version that the base itself replaced is history.
    let older = revision("r-1", "older", "2026-11-22T09:00:00.000Z");
    assert!(
        revisions_replaced_unseen(&[older.clone(), base.clone(), ours.clone()], "base", "ours")
            .is_empty()
    );
    // With no version of the base in the list, nothing can be said.
    assert!(revisions_replaced_unseen(&[theirs.clone(), ours.clone()], "base", "ours").is_empty());
    // Two devices came in between, one of them twice with one content.
    let third = revision("r1b", "third", "2026-11-23T10:00:05.200Z");
    let theirs_again = revision("r1c", "theirs", "2026-11-23T10:00:05.250Z");
    assert_eq!(
        revisions_replaced_unseen(
            &[base, theirs, third.clone(), theirs_again.clone(), ours],
            "base",
            "ours"
        ),
        vec![third, theirs_again]
    );
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
    transport.push_response(200, r#"{"files":[]}"#); // no copy of that name yet
    transport.push_response(200, "their edit"); // the content of r1
    transport.push_response(200, r#"{"id":"copy-id"}"#); // the copy is created
    transport.push_response(200, r#"{"changes":[],"newStartPageToken":"t2"}"#);

    provider.poll_changes(Some("t1"), 100).expect("poll");

    let requests = transport.recorded_requests();
    let work = &requests[uploads_before..];
    assert_eq!(
        work.len(),
        5,
        "{:?}",
        work.iter().map(|r| &r.url).collect::<Vec<_>>()
    );
    assert!(work[0].url.contains("/files/doc-id/revisions?"));
    assert!(work[2].url.contains("/files/doc-id/revisions/r1?alt=media"));
    assert_eq!(work[3].method, "POST");
    let created = String::from_utf8_lossy(&work[3].body);
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
    assert!(work[4].url.contains("/changes?"));

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
        3,
        "the versions, the name, the feed"
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
