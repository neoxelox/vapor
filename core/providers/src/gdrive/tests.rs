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
    provider
        .ensure_cloud_sync_directory("/Vapor")
        .expect("ensure");
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
        .ensure_cloud_sync_directory("/Vapor/Docs")
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
    transport.push_response(200, r#"{"id":"big","md5Checksum":"big-md5"}"#); // final

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source = scratch.path().join("big.bin");
    // Two 256KiB-aligned chunks beyond the simple-upload threshold.
    std::fs::write(
        &source,
        vec![7_u8; (SIMPLE_UPLOAD_MAX_BYTES + CHUNK_GRANULARITY) as usize],
    )
    .expect("seed");

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
    assert_eq!(outcome.content_hash, "big-md5");

    let requests = transport.recorded_requests();
    let initiate = &requests[2];
    assert!(initiate.url.contains("uploadType=resumable"));
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
        .ensure_cloud_sync_directory("/Vapor")
        .expect_err("no credentials must fail");
    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert!(error.message.contains("vapor auth login gdrive"));
    assert!(error.message.contains("workp"), "names the profile");
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
    // Storage-full is surfaced (permanent), not misclassified as a rate
    // limit that would retry forever.
    assert!(matches!(
        classify_api_failure(&storage).kind,
        ProviderErrorKind::Permanent
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
