//! The work a provider still owes after a write, kept across a stop
//! of the daemon.
//!
//! A provider can owe work for a write that landed: Google Drive reads
//! the versions of a file after a guarded overwrite of it, and copies
//! back a version that the overwrite replaced unseen. The provider
//! holds that work in memory, and it gives the engine one record of
//! text for each piece. This module stores the records in the state
//! database of the profile, hands them back to the provider at the
//! next start, and removes each one when the provider says that its
//! work ended.
//!
//! The engine never reads a record. The key of its entry is a hash of
//! the text, so the same text names the same entry in every run.

use std::time::SystemTime;

use sha2::{Digest, Sha256};
use vapor_providers::{FollowUpChange, Provider};
use vapor_shared::constants;

use crate::state_db::{DurableStateDb, StateDbError};

/// Hands the stored records to `provider`, and forgets the ones it
/// does not take. Called one time for a provider, before its first
/// tick.
pub(crate) fn resume(
    state_db: &mut DurableStateDb,
    provider: &dyn Provider,
) -> Result<(), StateDbError> {
    let stored = state_db.states_with_prefix(constants::provider::FOLLOW_UP_STATE_KEY_PREFIX)?;
    if stored.is_empty() {
        return Ok(());
    }
    let not_taken =
        provider.resume_follow_ups(stored.into_iter().map(|entry| entry.value).collect());
    for record in &not_taken {
        state_db.delete_state(&state_key(record))?;
    }
    if !not_taken.is_empty() {
        crate::logging::info(
            "Forgot the work of an earlier run that the provider does not take back",
            &[("records", not_taken.len().to_string())],
        );
    }
    Ok(())
}

/// Writes what the provider reported since the last call: a record
/// that started is stored, and one that finished is removed. The
/// runtime calls it at the end of each tick, inside the batch of the
/// tick, so the record of a write is saved with the sync index row of
/// that write.
pub(crate) fn save(
    state_db: &mut DurableStateDb,
    provider: &dyn Provider,
    now: SystemTime,
) -> Result<(), StateDbError> {
    let changes = provider.take_follow_up_changes();
    if changes.is_empty() {
        return Ok(());
    }
    let mut stored =
        state_db.count_states_with_prefix(constants::provider::FOLLOW_UP_STATE_KEY_PREFIX)?;
    let (mut too_long, mut over_the_limit) = (0_usize, 0_usize);
    for change in changes {
        match change {
            FollowUpChange::Started(record) => {
                if record.len() > constants::state::MAX_STATE_VALUE_LENGTH {
                    too_long += 1;
                } else if stored >= constants::provider::FOLLOW_UPS_STORED_MAX {
                    over_the_limit += 1;
                } else {
                    state_db.set_state(&state_key(&record), &record, now)?;
                    stored += 1;
                }
            }
            FollowUpChange::Finished(record) => {
                if state_db.delete_state(&state_key(&record))? {
                    stored = stored.saturating_sub(1);
                }
            }
        }
    }
    // One line for one tick, however many records it is about.
    if too_long + over_the_limit > 0 {
        crate::logging::warning(
            "Could not store work that a provider owes after a write; the work runs in this daemon, and a stop of the daemon loses it",
            &[
                ("too_long_for_an_entry", too_long.to_string()),
                ("over_the_stored_limit", over_the_limit.to_string()),
                ("stored_records", stored.to_string()),
            ],
        );
    }
    Ok(())
}

/// The key of the entry that holds `record`.
fn state_key(record: &str) -> String {
    let digest = Sha256::digest(record.as_bytes());
    let mut key = String::from(constants::provider::FOLLOW_UP_STATE_KEY_PREFIX);
    for byte in &digest[..16] {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use vapor_providers::{
        ChangesPoll, DownloadRequest, ProviderCapabilities, ProviderError, RemoteEntry, RemotePath,
        TransferSession, UploadRequest,
    };

    use super::*;

    fn at(millis: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
    }

    /// A provider that owes what a test says, and takes back every
    /// record but the ones named in `refuses`.
    #[derive(Default)]
    struct OwingProvider {
        changes: Mutex<Vec<FollowUpChange>>,
        resumed: Mutex<Vec<String>>,
        refuses: Vec<String>,
    }

    impl OwingProvider {
        fn report(&self, change: FollowUpChange) {
            self.changes.lock().expect("lock").push(change);
        }
    }

    impl Provider for OwingProvider {
        fn name(&self) -> &'static str {
            "owing"
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
        fn take_follow_up_changes(&self) -> Vec<FollowUpChange> {
            std::mem::take(&mut self.changes.lock().expect("lock"))
        }
        fn resume_follow_ups(&self, records: Vec<String>) -> Vec<String> {
            let (refused, taken) = records
                .into_iter()
                .partition(|record| self.refuses.contains(record));
            *self.resumed.lock().expect("lock") = taken;
            refused
        }
    }

    fn stored(state_db: &DurableStateDb) -> Vec<String> {
        state_db
            .states_with_prefix(constants::provider::FOLLOW_UP_STATE_KEY_PREFIX)
            .expect("list")
            .into_iter()
            .map(|entry| entry.value)
            .collect()
    }

    #[test]
    fn a_record_is_stored_until_the_provider_reports_its_end() {
        let mut state_db = DurableStateDb::open_in_memory().expect("open");
        let provider = OwingProvider::default();

        provider.report(FollowUpChange::Started("check a".to_string()));
        provider.report(FollowUpChange::Started("check b".to_string()));
        save(&mut state_db, &provider, at(10)).expect("save");
        assert_eq!(stored(&state_db), ["check a", "check b"]);

        // Nothing new: nothing is written, and nothing is lost.
        save(&mut state_db, &provider, at(20)).expect("save");
        assert_eq!(stored(&state_db), ["check a", "check b"]);

        provider.report(FollowUpChange::Finished("check a".to_string()));
        save(&mut state_db, &provider, at(30)).expect("save");
        assert_eq!(stored(&state_db), ["check b"]);

        // A record that starts and ends between two saves leaves
        // nothing, and the end of a record that is not stored is no
        // error.
        provider.report(FollowUpChange::Started("check c".to_string()));
        provider.report(FollowUpChange::Finished("check c".to_string()));
        provider.report(FollowUpChange::Finished("never stored".to_string()));
        save(&mut state_db, &provider, at(40)).expect("save");
        assert_eq!(stored(&state_db), ["check b"]);
    }

    #[test]
    fn the_stored_records_go_back_to_the_provider_and_the_ones_it_refuses_are_forgotten() {
        let mut state_db = DurableStateDb::open_in_memory().expect("open");
        let earlier = OwingProvider::default();
        for (record, when) in [("oldest", 10), ("unreadable", 20), ("newest", 30)] {
            earlier.report(FollowUpChange::Started(record.to_string()));
            save(&mut state_db, &earlier, at(when)).expect("save");
        }

        let provider = OwingProvider {
            refuses: vec!["unreadable".to_string()],
            ..OwingProvider::default()
        };
        resume(&mut state_db, &provider).expect("resume");
        assert_eq!(
            *provider.resumed.lock().expect("lock"),
            ["oldest", "newest"],
            "oldest first"
        );
        assert_eq!(stored(&state_db), ["oldest", "newest"]);

        // A provider that owes nothing after a write takes none: the
        // records of another provider do not stay.
        let filesystem = vapor_providers::FilesystemProvider::new();
        resume(&mut state_db, &filesystem).expect("resume");
        assert!(stored(&state_db).is_empty());
    }

    #[test]
    fn the_provider_of_a_profile_that_does_not_run_leaves_the_records_stored() {
        let mut state_db = DurableStateDb::open_in_memory().expect("open");
        let earlier = OwingProvider::default();
        earlier.report(FollowUpChange::Started("check a".to_string()));
        save(&mut state_db, &earlier, at(10)).expect("save");

        let stand_in = vapor_providers::suspended_provider();
        resume(&mut state_db, stand_in.as_ref()).expect("resume");
        assert_eq!(stored(&state_db), ["check a"]);
    }

    #[test]
    fn the_store_is_bounded_in_count_and_in_the_length_of_a_record() {
        let mut state_db = DurableStateDb::open_in_memory().expect("open");
        let provider = OwingProvider::default();
        for index in 0..constants::provider::FOLLOW_UPS_STORED_MAX {
            provider.report(FollowUpChange::Started(format!("check {index}")));
        }
        provider.report(FollowUpChange::Started("one too many".to_string()));
        provider.report(FollowUpChange::Started(
            "x".repeat(constants::state::MAX_STATE_VALUE_LENGTH + 1),
        ));
        save(&mut state_db, &provider, at(10)).expect("save");
        assert_eq!(
            stored(&state_db).len(),
            constants::provider::FOLLOW_UPS_STORED_MAX
        );
        assert!(!stored(&state_db).contains(&"one too many".to_string()));

        // The end of a stored record makes room for the next one.
        provider.report(FollowUpChange::Finished("check 0".to_string()));
        provider.report(FollowUpChange::Started("fits again".to_string()));
        save(&mut state_db, &provider, at(20)).expect("save");
        assert!(stored(&state_db).contains(&"fits again".to_string()));
        assert_eq!(
            stored(&state_db).len(),
            constants::provider::FOLLOW_UPS_STORED_MAX
        );
    }
}
