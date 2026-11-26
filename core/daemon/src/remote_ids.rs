//! The ids that a provider has for the synced files and folders, kept
//! across a stop of the daemon.
//!
//! A provider with ids names a changed object by its id in the changes
//! feed, and knows which path that is only from what it saw in this
//! run. The sync index stores the id of each synced file and folder
//! (`sync_index.remote_id`, `sync_folders`). This module hands them
//! back to the provider at a start, so a rename and a deletion that
//! were made while no daemon ran are reported with the path they are
//! about.
//!
//! The engine never reads an id. It compares two of them for equality,
//! and that is all.

use std::path::Path;

use vapor_providers::{Provider, RemoteEntryKind, RemoteIdentity, RemotePath};

use crate::state_db::{DurableStateDb, StateDbError};

/// Hands the stored ids to `provider`. Called one time for a provider,
/// before its first tick. A file that syncs under another name than
/// its cloud object (a name that two cloud objects cannot both have on
/// this device) is left out: its path here is not the path there.
pub(crate) fn restore(
    state_db: &DurableStateDb,
    provider: &dyn Provider,
    local_root: Option<&Path>,
) -> Result<(), StateDbError> {
    let Some(local_root) = local_root else {
        return Ok(());
    };
    let stored = state_db.remote_identities()?;
    if stored.is_empty() {
        return Ok(());
    }
    let aliased = state_db.aliased_local_paths()?;
    let known = stored
        .into_iter()
        .filter(|(path, _, _)| !aliased.contains(path))
        .filter_map(|(path, id, is_folder)| {
            Some(RemoteIdentity {
                path: RemotePath::from_local(local_root, &path)?,
                kind: if is_folder {
                    RemoteEntryKind::Directory
                } else {
                    RemoteEntryKind::File
                },
                id,
            })
        })
        .collect();
    provider.restore_remote_ids(known);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime};

    use vapor_providers::{
        ChangesPoll, DownloadRequest, ProviderCapabilities, ProviderError, RemoteEntry,
        TransferSession, UploadRequest,
    };

    use super::*;

    /// A provider that keeps what a start handed it.
    #[derive(Default)]
    struct Remembering {
        restored: Mutex<Vec<RemoteIdentity>>,
    }

    impl Provider for Remembering {
        fn name(&self) -> &'static str {
            "remembering"
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::GDRIVE_MVP
        }
        fn ensure_cloud_root(&self, _cloud_root: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn enumerate(&self, _directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(Vec::new())
        }
        fn stat(&self, _path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
            Ok(None)
        }
        fn content_hash(&self, _path: &RemotePath) -> Result<String, ProviderError> {
            Err(ProviderError::not_found("no object"))
        }
        fn begin_upload(
            &self,
            _request: UploadRequest,
        ) -> Result<Box<dyn TransferSession>, ProviderError> {
            Err(ProviderError::permanent("no uploads"))
        }
        fn begin_download(
            &self,
            _request: DownloadRequest,
        ) -> Result<Box<dyn TransferSession>, ProviderError> {
            Err(ProviderError::permanent("no downloads"))
        }
        fn delete(&self, _path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn poll_changes(
            &self,
            _cursor: Option<&str>,
            _max_changes: usize,
        ) -> Result<ChangesPoll, ProviderError> {
            Ok(ChangesPoll::CursorExpired)
        }
        fn restore_remote_ids(&self, known: Vec<RemoteIdentity>) {
            *self.restored.lock().expect("lock") = known;
        }
    }

    #[test]
    fn a_start_hands_the_provider_the_id_of_each_synced_file_and_folder() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_750_000_000);
        let root = PathBuf::from(if cfg!(windows) {
            r"C:\Users\ana\Vapor"
        } else {
            "/Users/ana/Vapor"
        });
        let mut state_db = DurableStateDb::open_in_memory().expect("open");
        let seed = |state_db: &mut DurableStateDb, name: &str, id: Option<&str>| {
            let path = root.join(name);
            state_db
                .set_sync_index(&path, "hash", 1, None, None, "op", now)
                .expect("row");
            if let Some(id) = id {
                state_db
                    .set_sync_index_remote_id(&path, id)
                    .expect("the id of the row");
            }
            path
        };
        seed(&mut state_db, "Photos/a.jpg", Some("id-a"));
        seed(&mut state_db, "no-id.txt", None);
        let copy = seed(&mut state_db, "readme~conflict-dev-1.md", Some("id-readme"));
        state_db
            .record_name_alias("readme.md", &copy, "hash", now)
            .expect("alias");
        state_db
            .record_synced_folder(&root.join("Photos"), "id-photos", now)
            .expect("folder row");

        let provider = Remembering::default();
        restore(&state_db, &provider, Some(&root)).expect("restore");

        let mut restored: Vec<(String, RemoteEntryKind, String)> = provider
            .restored
            .lock()
            .expect("lock")
            .iter()
            .map(|known| {
                (
                    known.path.as_str().to_string(),
                    known.kind,
                    known.id.clone(),
                )
            })
            .collect();
        restored.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            restored,
            vec![
                (
                    "Photos".to_string(),
                    RemoteEntryKind::Directory,
                    "id-photos".to_string()
                ),
                (
                    "Photos/a.jpg".to_string(),
                    RemoteEntryKind::File,
                    "id-a".to_string()
                ),
            ],
            "a file with no id, and a file that syncs under another name, are left out"
        );
    }
}
