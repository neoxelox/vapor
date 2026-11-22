//! Incremental reconcile comparison walk.
//!
//! Reconcile compares the local tree with the provider's remote tree
//! and enqueues the durable intents that make the two converge under
//! the scope's [`SyncMode`]. The walk is deliberately incremental: a
//! bounded number of directories per checkpoint, so the reconcile
//! controller's slice/throttle discipline (`AGENTS.md §3`) applies to
//! real comparison work: the runtime holds the walker across slice
//! pauses and resumes where it left off.
//!
//! Divergence handling per mode (`docs/architecture/sync-modes.md`):
//! - `two-way`: local-only → upload; remote-only → download; content
//!   divergence routes through the conflict machinery via an
//!   upload whose planner consults the conflict policy.
//! - `pull-only` (strict mirror, cloud authoritative): remote-only and
//!   divergent entries → download; local-only entries → local removal.
//! - `push-only` (strict mirror, local authoritative): local-only and
//!   divergent entries → upload; remote-only entries → remote delete.
//!
//! Equality is a quick check, never a hash: files whose sizes differ
//! diverge; files of equal size diverge when the sync index shows the
//! local copy was touched since the last transfer (a different mtime at
//! the millisecond the index stores, the rsync quick check). That catches
//! an edit made while the daemon was not running, which the watcher
//! cannot see and which a size-only comparison missed. A pair the index
//! has no row for (first sync of two pre-populated roots, a lost state
//! DB) is unverified rather than converged: it is routed the same way,
//! and the transfer planner hashes it once, converges silently when the
//! content matches, and records the row so the quick check works from
//! then on. A whole-tree hash pass stays out of the walk's budget on
//! purpose.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use vapor_providers::{Provider, ProviderError, RemoteEntry, RemoteEntryKind, RemotePath};
use vapor_shared::{ProviderErrorKind, SyncMode};

use crate::event_intents::PendingIntentKind;
use crate::fs_events::SharedEventPathFilter;
use crate::local_hash::{SetAsideFile, SetAsideVerdict};
use crate::provider_jobs::{ProviderCall, ProviderCallMode};
use crate::state_db::{DurableStateDb, StateDbError};

#[derive(Debug)]
pub enum WalkError {
    Provider(ProviderError),
    StateDb(StateDbError),
}

impl From<StateDbError> for WalkError {
    fn from(error: StateDbError) -> Self {
        Self::StateDb(error)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalkStats {
    pub directories_compared: usize,
    pub intents_enqueued: usize,
    /// Strict-mirror removals scheduled by this walk.
    pub mirror_deletes: usize,
    /// Strict-mirror overwrites scheduled by this walk.
    pub mirror_reverts: usize,
    /// Entries on this device that an ignore rule kept out of the walk:
    /// a file, or a folder with everything under it, counted one time.
    pub ignored_local_entries: u64,
}

/// One local/remote entry pair keyed by name inside a directory.
#[derive(Debug, Default)]
struct EntryPair {
    local: Option<LocalEntry>,
    remote: Option<RemoteEntry>,
}

#[derive(Debug)]
struct LocalEntry {
    path: PathBuf,
    is_dir: bool,
    size_bytes: u64,
    modified_at: Option<SystemTime>,
}

/// A remote listing requested on an earlier slice whose round trip is
/// still in progress.
struct PendingEnumeration {
    directory: PathBuf,
    call: ProviderCall<Result<Vec<RemoteEntry>, ProviderError>>,
    /// When the listing was asked for. A sync index row written after
    /// this is newer than what the listing shows.
    requested_at: SystemTime,
}

pub struct ReconcileWalker {
    /// The scope's local root (remote paths derive relative to it).
    scope_root: PathBuf,
    /// The subtree being reconciled (== scope_root for whole-scope).
    subtree_root: PathBuf,
    pending_enumeration: Option<PendingEnumeration>,
    /// Files the directory just compared holds under a set-aside name.
    set_aside_found: Vec<SetAsideFile>,
    /// The comparison of those files with their originals, on its own
    /// thread: the walk reads no file content on the tick thread.
    pending_set_aside: Option<ProviderCall<Vec<SetAsideVerdict>>>,
    /// Two-way file/directory type mismatches found by this walk; the
    /// runtime surfaces them on the timeline so the user can act.
    type_mismatches: Vec<PathBuf>,
    /// Local names this walk could not carry to the other side; the
    /// runtime asks about each once.
    unsyncable_names: Vec<crate::unsyncable::UnsyncableName>,
    /// Remote names that would alias an existing, differently-cased
    /// local file (`(wanted local path, existing local path)`); left
    /// untouched on both sides and surfaced on the timeline.
    name_collisions: Vec<(PathBuf, PathBuf)>,
    /// Local entries this walk renamed to the cloud's spelling of
    /// their name, as (old path, new path).
    respelled: Vec<(PathBuf, PathBuf)>,
    /// Folders this walk skipped because it could not read them. A
    /// folder that was not compared is not converged; the runtime
    /// walks each again later.
    unreadable_dirs: Vec<(PathBuf, std::io::ErrorKind)>,
    /// Folders that hold an ignore file whose rules are not in force
    /// yet. They were not compared: with the old rules, a file that the
    /// new ones hide would upload. A reload was asked for, and the
    /// runtime walks them again when it lands.
    dirs_waiting_for_rules: Vec<PathBuf>,
    /// Ignore rules, applied symmetrically: local entries and remote
    /// entries (via their local-equivalent path) that match never
    /// produce intents and are never descended into. Without this the
    /// walk would pull ignored names (`.DS_Store`, `node_modules/`)
    /// down from the cloud side, and manufacture keep-both conflicts
    /// against local counterparts the watcher rightly never uploaded.
    path_filter: Option<Arc<SharedEventPathFilter>>,
    pending_dirs: VecDeque<PathBuf>,
    stats: WalkStats,
    /// A reattached or re-created root is merged: a file present on
    /// one side only is transferred, never treated as a deletion to
    /// propagate, whatever the index says.
    merge_without_deletions: bool,
    /// Names the conflict copies this walk materializes colliding
    /// remote names as.
    device_id: String,
}

impl ReconcileWalker {
    pub fn new(
        scope_root: &Path,
        subtree_root: &Path,
        path_filter: Option<Arc<SharedEventPathFilter>>,
    ) -> Self {
        Self {
            scope_root: scope_root.to_path_buf(),
            subtree_root: subtree_root.to_path_buf(),
            path_filter,
            pending_enumeration: None,
            set_aside_found: Vec::new(),
            pending_set_aside: None,
            type_mismatches: Vec::new(),
            unsyncable_names: Vec::new(),
            name_collisions: Vec::new(),
            respelled: Vec::new(),
            unreadable_dirs: Vec::new(),
            dirs_waiting_for_rules: Vec::new(),
            pending_dirs: VecDeque::from([subtree_root.to_path_buf()]),
            stats: WalkStats::default(),
            merge_without_deletions: false,
            device_id: String::new(),
        }
    }

    pub fn with_merge_without_deletions(mut self, merge: bool) -> Self {
        self.merge_without_deletions = merge;
        self
    }

    pub fn with_device_id(mut self, device_id: &str) -> Self {
        self.device_id = device_id.to_string();
        self
    }

    /// A remote name this filesystem cannot hold next to an existing
    /// local name is materialized under a conflict-copy name, and the
    /// alias between the two is recorded so the copy keeps syncing with
    /// its own cloud object from then on.
    fn materialize_colliding_remote(
        &mut self,
        state_db: &mut DurableStateDb,
        wanted: &Path,
        existing: &Path,
        remote: &RemoteEntry,
        now: SystemTime,
    ) -> Result<(), WalkError> {
        if remote.kind != RemoteEntryKind::File {
            // A colliding directory has no copy to make; reported as
            // before.
            self.name_collisions
                .push((wanted.to_path_buf(), existing.to_path_buf()));
            return Ok(());
        }
        let millis = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let copy = crate::conflict::conflict_copy_path(wanted, &self.device_id, millis, |path| {
            path.exists() || crate::name_collision::colliding_local_path(path).is_some()
        });
        state_db.record_name_alias(
            remote.path.as_str(),
            &copy,
            remote.content_hash.as_deref().unwrap_or(""),
            now,
        )?;
        state_db.enqueue_download_from(&copy, remote.path.as_str(), now)?;
        crate::logging::info(
            "Remote name collides with a local file; materializing it as a conflict copy",
            &[
                ("remote", remote.path.as_str().to_string()),
                ("existing", existing.display().to_string()),
                ("copy", copy.display().to_string()),
            ],
        );
        self.name_collisions.push((wanted.to_path_buf(), copy));
        Ok(())
    }

    /// Whether the ignore rules take the entry at `local_path` out of
    /// scope. The walk knows what it just listed, so it says whether
    /// the entry is a folder: a folder rule hides folders only.
    fn ignores(&self, local_path: &Path, is_folder: bool) -> bool {
        self.path_filter
            .as_ref()
            .map(|filter| filter.should_ignore_kind(local_path, is_folder))
            .unwrap_or(false)
    }

    pub fn subtree_root(&self) -> &Path {
        &self.subtree_root
    }

    pub fn stats(&self) -> WalkStats {
        self.stats
    }

    /// Drains the per-walk stat deltas accumulated since the last call
    /// (the runtime folds them into its cumulative mirror counters).
    /// Drains the two-way type mismatches found since the last call.
    pub fn take_type_mismatches(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.type_mismatches)
    }

    pub fn take_unsyncable_names(&mut self) -> Vec<crate::unsyncable::UnsyncableName> {
        std::mem::take(&mut self.unsyncable_names)
    }

    /// Whether this walk covers the whole scope, so what it did not
    /// report is known to be gone.
    pub fn is_whole_scope(&self) -> bool {
        self.subtree_root == self.scope_root
    }

    pub fn take_name_collisions(&mut self) -> Vec<(PathBuf, PathBuf)> {
        std::mem::take(&mut self.name_collisions)
    }

    pub fn take_respelled(&mut self) -> Vec<(PathBuf, PathBuf)> {
        std::mem::take(&mut self.respelled)
    }

    pub fn take_unreadable_dirs(&mut self) -> Vec<(PathBuf, std::io::ErrorKind)> {
        std::mem::take(&mut self.unreadable_dirs)
    }

    /// The folders this walk left for the reload of the ignore rules.
    pub fn take_dirs_waiting_for_rules(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.dirs_waiting_for_rules)
    }

    /// A local file is set aside under a temp name for the moment a
    /// download takes its place, on the thread that also runs this
    /// walk. One that the walk finds was left by a crash or a failed
    /// apply, and it may hold an edit that exists nowhere else. It
    /// comes back as a conflict copy and uploads like one. Bytes that
    /// equal the file now at the original name are dropped instead;
    /// `verdict` is that comparison, made off the tick thread.
    fn recover_set_aside_file(
        &mut self,
        state_db: &mut DurableStateDb,
        verdict: &SetAsideVerdict,
        now: SystemTime,
    ) -> Result<(), WalkError> {
        let aside = verdict.file.aside.as_path();
        let Ok(metadata) = fs::symlink_metadata(aside) else {
            return Ok(());
        };
        if !metadata.is_file() {
            return Ok(());
        }
        let original = crate::local_hash::set_aside_original_path(&verdict.file);
        if verdict.still_same_as(&original) {
            let _ = fs::remove_file(aside);
            return Ok(());
        }
        let millis = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let copy =
            crate::conflict::conflict_copy_path(&original, &self.device_id, millis, |path| {
                path.exists() || crate::name_collision::colliding_local_path(path).is_some()
            });
        if let Err(error) = fs::rename(aside, &copy) {
            crate::logging::warning(
                "Could not recover a local file that was set aside before a crash",
                &[
                    ("path", aside.display().to_string()),
                    ("error", error.to_string()),
                ],
            );
            return Ok(());
        }
        state_db.enqueue_intents_coalesced(
            &[(copy.clone(), PendingIntentKind::Upload, now)],
            crate::safeguards::IntentSource::Fresh,
        )?;
        crate::logging::info(
            "Recovered a local file that was set aside before a crash as a conflict copy",
            &[("conflict_copy", copy.display().to_string())],
        );
        Ok(())
    }

    /// One name, two spellings. On a filesystem that folds names, a
    /// local-only entry and a cloud-only entry that fold together are
    /// one file (or folder) whose name changed spelling on one side:
    /// a rename, carried to the other side as a move. It is never two
    /// files that collide, and never a deletion.
    ///
    /// The sync index says which side made the change: its rows carry
    /// the spelling that was last synced. Rows under the cloud's
    /// spelling and none under the local one mean this device renamed
    /// the entry, and the cloud follows through the upload route's
    /// move detection. In every other case the cloud's spelling stays
    /// and the local entry is renamed to it here: the cloud renamed
    /// it, both sides did, or the pair was never synced.
    ///
    /// Two cloud entries that fold together are left to the collision
    /// path, and so is a pair of different kinds.
    fn settle_spellings(
        &mut self,
        sync_mode: SyncMode,
        state_db: &mut DurableStateDb,
        directory: &Path,
        pairs: &mut BTreeMap<String, EntryPair>,
        batch: &mut Vec<(PathBuf, PendingIntentKind, SystemTime)>,
        now: SystemTime,
    ) -> Result<(), WalkError> {
        let mut by_fold: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
        for (name, pair) in pairs.iter() {
            let group = by_fold
                .entry(crate::name_collision::fold(name))
                .or_default();
            match (pair.local.is_some(), pair.remote.is_some()) {
                (true, false) => group.0.push(name.clone()),
                (false, true) => group.1.push(name.clone()),
                // A pair under one spelling makes any other spelling
                // of the name a second object.
                _ => group.1.push(String::new()),
            }
        }
        for (local_names, remote_names) in by_fold.into_values() {
            let ([local_name], [remote_name]) = (local_names.as_slice(), remote_names.as_slice())
            else {
                continue;
            };
            if remote_name.is_empty() {
                continue;
            }
            let local_path = directory.join(local_name);
            let cloud_spelled = directory.join(remote_name);
            // A filesystem that keeps the spellings apart holds two
            // entries; they are what they look like.
            if crate::name_collision::colliding_local_path(&cloud_spelled).as_deref()
                != Some(local_path.as_path())
            {
                continue;
            }
            let (Some(local), Some(remote)) = (
                pairs.get(local_name).and_then(|pair| pair.local.as_ref()),
                pairs.get(remote_name).and_then(|pair| pair.remote.as_ref()),
            ) else {
                continue;
            };
            let is_dir = local.is_dir;
            if is_dir != (remote.kind == RemoteEntryKind::Directory) {
                continue;
            }
            let (synced_as_cloud_spells, synced_as_local_spells) = if is_dir {
                (
                    state_db.sync_index_has_rows_under(&cloud_spelled)?,
                    state_db.sync_index_has_rows_under(&local_path)?,
                )
            } else {
                (
                    state_db.sync_index(&cloud_spelled)?.is_some(),
                    state_db.sync_index(&local_path)?.is_some(),
                )
            };
            let this_device_renamed = match sync_mode {
                SyncMode::PushOnly => true,
                SyncMode::PullOnly => false,
                SyncMode::TwoWay => {
                    synced_as_cloud_spells
                        && !synced_as_local_spells
                        && !self.merge_without_deletions
                }
            };
            if this_device_renamed {
                // What the watcher delivers for a live rename: the old
                // name leaves, the new one arrives. The upload of the
                // new name finds the old row and moves the cloud
                // object; a folder goes child by child.
                crate::logging::info(
                    "A name changed spelling on this device; carrying the rename to the cloud",
                    &[
                        ("from", cloud_spelled.display().to_string()),
                        ("to", local_path.display().to_string()),
                    ],
                );
                batch.push((cloud_spelled, PendingIntentKind::Delete, now));
                pairs.remove(remote_name);
                if !is_dir {
                    batch.push((local_path, PendingIntentKind::Rename, now));
                    pairs.remove(local_name);
                }
                continue;
            }
            if let Err(error) = fs::rename(&local_path, &cloud_spelled) {
                crate::logging::warning(
                    "Could not rename a local entry to the cloud's spelling of its name",
                    &[
                        ("from", local_path.display().to_string()),
                        ("to", cloud_spelled.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                continue;
            }
            if is_dir {
                state_db.rekey_sync_index_under(&local_path, &cloud_spelled)?;
            } else {
                state_db.rekey_sync_index(&local_path, &cloud_spelled)?;
            }
            crate::logging::info(
                "Renamed a local entry to the cloud's spelling of its name",
                &[
                    ("from", local_path.display().to_string()),
                    ("to", cloud_spelled.display().to_string()),
                ],
            );
            if let Some(mut local) = pairs.remove(local_name).and_then(|pair| pair.local) {
                local.path = cloud_spelled.clone();
                pairs.entry(remote_name.clone()).or_default().local = Some(local);
            }
            self.respelled.push((local_path, cloud_spelled));
        }
        Ok(())
    }

    /// Removes the sync index rows of names that neither side holds any
    /// more: a file that left both sides while no daemon watched, or
    /// the old spelling of a name that both sides respelled. Such a row
    /// would later vouch for a deletion that never happened: the file
    /// put back on this device would read as "deleted in the cloud" and
    /// go to the trash. With no row, the file is new and is kept.
    ///
    /// A row written after the cloud listing was asked for stays: the
    /// listing is older than the transfer that wrote it.
    fn forget_names_that_left_both_sides(
        &mut self,
        state_db: &mut DurableStateDb,
        directory: &Path,
        listed: &std::collections::BTreeSet<String>,
        pairs: &BTreeMap<String, EntryPair>,
        listed_at: SystemTime,
    ) -> Result<(), WalkError> {
        let mut removed = 0usize;
        for name in state_db.sync_index_names_in(directory)? {
            if listed.contains(&name) || pairs.contains_key(&name) {
                continue;
            }
            removed += state_db
                .remove_sync_index_tree_written_before(&directory.join(&name), listed_at)?;
        }
        if removed > 0 {
            crate::logging::info(
                "Forgot the sync index rows of names that left both sides",
                &[
                    ("directory", directory.display().to_string()),
                    ("rows", removed.to_string()),
                ],
            );
        }
        Ok(())
    }

    /// The entries on this device that an ignore rule hid from this
    /// walk so far; the count starts again from zero.
    pub fn take_ignored_local_entries(&mut self) -> u64 {
        std::mem::take(&mut self.stats.ignored_local_entries)
    }

    pub fn take_mirror_deltas(&mut self) -> (usize, usize) {
        let deltas = (self.stats.mirror_reverts, self.stats.mirror_deletes);
        self.stats.mirror_reverts = 0;
        self.stats.mirror_deletes = 0;
        deltas
    }

    /// Compares up to `max_directories` directories and durably
    /// enqueues the resulting convergence intents. Returns `Ok(true)`
    /// when the walk has no work left.
    ///
    /// Each directory's remote listing is a provider call; in threaded
    /// mode it runs on its own thread and the walk resumes on the slice
    /// that finds the listing ready, so a slow `enumerate` never holds
    /// the tick thread. `should_continue` is consulted after each
    /// compared directory so the caller can bound the chunk by a
    /// wall-clock slice as well as by the directory budget.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        &mut self,
        provider: Arc<dyn Provider>,
        mode: &ProviderCallMode,
        sync_mode: SyncMode,
        state_db: &mut DurableStateDb,
        max_directories: usize,
        now: SystemTime,
        should_continue: &dyn Fn() -> bool,
    ) -> Result<bool, WalkError> {
        for _ in 0..max_directories {
            if let Some(call) = self.pending_set_aside.as_mut() {
                let Some(result) = call.take() else {
                    // Still comparing on its thread.
                    return Ok(false);
                };
                self.pending_set_aside = None;
                // A comparison that panicked leaves the files where
                // they are; the next walk meets them again.
                for verdict in result.unwrap_or_default() {
                    self.recover_set_aside_file(state_db, &verdict, now)?;
                }
            }
            if self.pending_enumeration.is_none() {
                let Some(directory) = self.pending_dirs.pop_front() else {
                    return Ok(true);
                };
                let remote_dir = if directory == self.scope_root {
                    RemotePath::root()
                } else {
                    match RemotePath::from_local(&self.scope_root, &directory) {
                        Some(path) => path,
                        None => {
                            crate::logging::warning(
                                "Reconcile walk skipped a directory outside the scope root",
                                &[("directory", directory.display().to_string())],
                            );
                            continue;
                        }
                    }
                };
                let provider = provider.clone();
                let call =
                    ProviderCall::start(mode, "enumerate", move || provider.enumerate(&remote_dir));
                self.pending_enumeration = Some(PendingEnumeration {
                    directory,
                    call,
                    requested_at: now,
                });
            }
            let pending = self
                .pending_enumeration
                .as_mut()
                .expect("a listing was just requested");
            let Some(result) = pending.call.take() else {
                // Still listing on the provider thread; nothing else can
                // progress until it lands.
                return Ok(false);
            };
            let PendingEnumeration {
                directory,
                requested_at,
                ..
            } = self.pending_enumeration.take().expect("pending listing");
            let remote_entries = match result {
                Ok(Ok(entries)) => entries,
                Ok(Err(error)) if error.kind == ProviderErrorKind::NotFound => Vec::new(),
                Ok(Err(error)) => return Err(WalkError::Provider(error)),
                Err(panic) => {
                    return Err(WalkError::Provider(ProviderError::permanent(format!(
                        "enumerate panicked: {panic}"
                    ))));
                }
            };
            self.compare_directory(
                sync_mode,
                state_db,
                &directory,
                remote_entries,
                requested_at,
                now,
            )?;
            self.stats.directories_compared += 1;
            if !self.set_aside_found.is_empty() {
                let found = std::mem::take(&mut self.set_aside_found);
                self.pending_set_aside = Some(ProviderCall::start(mode, "set-aside", move || {
                    crate::local_hash::compare_set_aside_files(found)
                }));
            }
            if !should_continue() {
                break;
            }
        }
        Ok(self.pending_dirs.is_empty()
            && self.pending_enumeration.is_none()
            && self.pending_set_aside.is_none())
    }

    fn compare_directory(
        &mut self,
        sync_mode: SyncMode,
        state_db: &mut DurableStateDb,
        directory: &Path,
        remote_entries: Vec<RemoteEntry>,
        listed_at: SystemTime,
        now: SystemTime,
    ) -> Result<(), WalkError> {
        let mut pairs: BTreeMap<String, EntryPair> = BTreeMap::new();

        match fs::read_dir(directory) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                        // Non-UTF-8 names are unrepresentable in the sync
                        // path model; the runtime asks about the file
                        // once instead of logging on every walk.
                        self.unsyncable_names
                            .push(crate::unsyncable::UnsyncableName::not_utf8(&entry.path()));
                        continue;
                    };
                    if name.contains('\\') {
                        // A name the remote path model refuses. Asked
                        // about one time, like a name that is not UTF-8;
                        // a folder is not entered, so nothing under it
                        // logs a warning on every pass.
                        self.unsyncable_names
                            .push(crate::unsyncable::UnsyncableName::backslash(&entry.path()));
                        continue;
                    }
                    if vapor_providers::filesystem::is_internal_file_name(&name) {
                        if let Some(original) = crate::conflict::set_aside_original_name(&name) {
                            self.set_aside_found.push(SetAsideFile {
                                aside: entry.path(),
                                original_name: original.map(str::to_owned),
                            });
                            continue;
                        }
                        // Reap orphaned download-staging temps from unclean
                        // crashes as we pass over them; in-flight (young)
                        // temps are left alone.
                        vapor_providers::filesystem::reap_if_stale_temp_file(
                            &entry.path(),
                            &name,
                            now,
                        );
                        continue;
                    }
                    // An ignore file whose rules are not in force (it
                    // came with a folder that moved in, or changed with
                    // no watcher event): this folder is compared when
                    // they are.
                    if let Some(filter) = &self.path_filter
                        && filter.is_ignore_file(&entry.path())
                        && let Ok(metadata) = entry.metadata()
                        && metadata.is_file()
                        && !filter.covers_or_requests(
                            &entry.path(),
                            (metadata.modified().ok(), metadata.len()),
                        )
                    {
                        crate::logging::info(
                            "A folder holds an ignore file whose rules are not loaded; comparing it after the reload",
                            &[("directory", directory.display().to_string())],
                        );
                        self.dirs_waiting_for_rules.push(directory.to_path_buf());
                        return Ok(());
                    }
                    // A link is not followed: it counts as what it is,
                    // never as the folder it points at.
                    let is_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
                    if self.ignores(&entry.path(), is_folder) {
                        if self.path_filter.as_ref().is_some_and(|filter| {
                            filter.is_ignored_by_rule(&entry.path(), is_folder)
                        }) {
                            self.stats.ignored_local_entries += 1;
                        }
                        continue;
                    }
                    let metadata = match entry.path().symlink_metadata() {
                        Ok(metadata) => metadata,
                        // The entry was just listed, so it exists: a stat
                        // failure is transient (permission, EIO). Never let
                        // it read as "locally absent": that would drive a
                        // strict-mirror remote delete of content that is
                        // still present. Abandon this directory; it is
                        // walked again later.
                        Err(error) => {
                            crate::logging::warning(
                                "Reconcile walk cannot stat a local entry; deferring the directory",
                                &[
                                    ("path", entry.path().display().to_string()),
                                    ("error", error.to_string()),
                                ],
                            );
                            self.unreadable_dirs
                                .push((directory.to_path_buf(), error.kind()));
                            return Ok(());
                        }
                    };
                    // Regular files and directories only: symlinks,
                    // FIFOs, sockets, and device nodes are outside the
                    // sync contract (the executor refuses them too:
                    // hashing a FIFO would block forever).
                    if !metadata.is_file() && !metadata.is_dir() {
                        continue;
                    }
                    pairs.entry(name).or_default().local = Some(LocalEntry {
                        path: entry.path(),
                        is_dir: metadata.is_dir(),
                        size_bytes: metadata.len(),
                        modified_at: metadata.modified().ok(),
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            // A non-NotFound read failure (EPERM/EACCES/EMFILE/EIO) is NOT a
            // positively-observed empty directory. Falling through with an
            // empty local view would classify every remote entry as
            // local-only and strict-mirror-delete the cloud tree (or churn
            // spurious downloads in two-way). Skip the directory entirely
            // and let a later reconcile pass retry once the error clears.
            Err(error) => {
                crate::logging::warning(
                    "Reconcile walk cannot read a local directory; deferring it",
                    &[
                        ("directory", directory.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                self.unreadable_dirs
                    .push((directory.to_path_buf(), error.kind()));
                return Ok(());
            }
        }

        // A remote name materialized under an alias pairs with its
        // local copy, not with the name it cannot have here.
        let aliases: BTreeMap<String, String> =
            state_db.name_aliases_in(directory)?.into_iter().collect();
        for entry in remote_entries {
            let Some(name) = entry.path.file_name().map(ToOwned::to_owned) else {
                continue;
            };
            let name = aliases.get(&name).cloned().unwrap_or(name);
            // Remote entries are judged by the local path they would
            // converge onto, so one rule set governs both directions.
            let is_folder = entry.kind == vapor_providers::RemoteEntryKind::Directory;
            if self.ignores(&directory.join(&name), is_folder) {
                continue;
            }
            pairs.entry(name).or_default().remote = Some(entry);
        }

        let mut batch: Vec<(PathBuf, PendingIntentKind, SystemTime)> = Vec::new();
        let listed: std::collections::BTreeSet<String> = pairs.keys().cloned().collect();
        self.settle_spellings(sync_mode, state_db, directory, &mut pairs, &mut batch, now)?;
        self.forget_names_that_left_both_sides(state_db, directory, &listed, &pairs, listed_at)?;

        // Remote-only names that differ only by case cannot all
        // materialize on a case-insensitive local filesystem. The
        // lexically first one proceeds; the rest are collisions, the
        // same verdict a later walk would reach once the first exists.
        let mut skipped_aliases: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        if crate::name_collision::folds_case_at(directory) {
            let mut by_fold: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (name, pair) in &pairs {
                if pair.local.is_none() && pair.remote.is_some() {
                    by_fold
                        .entry(crate::name_collision::fold(name))
                        .or_default()
                        .push(name.clone());
                }
            }
            for names in by_fold.into_values() {
                if names.len() < 2 {
                    continue;
                }
                let winner = directory.join(&names[0]);
                for name in &names[1..] {
                    let wanted = directory.join(name);
                    if let Some(remote) = pairs.get(name).and_then(|pair| pair.remote.clone()) {
                        self.materialize_colliding_remote(
                            state_db, &wanted, &winner, &remote, now,
                        )?;
                    }
                    skipped_aliases.insert(name.clone());
                }
            }
        }

        for (name, pair) in pairs {
            if skipped_aliases.contains(&name) {
                continue;
            }
            let local_path = directory.join(&name);
            match (pair.local, pair.remote) {
                (Some(local), Some(remote)) => match (local.is_dir, remote.kind) {
                    (true, RemoteEntryKind::Directory) => {
                        self.pending_dirs.push_back(local.path);
                    }
                    (false, RemoteEntryKind::File) => {
                        let diverged = local.size_bytes != remote.size_bytes
                            || touched_since_last_sync(state_db, &local_path, &local)?
                            || remote_touched_since_last_sync(state_db, &local_path, &remote)?;
                        if diverged {
                            match sync_mode {
                                SyncMode::TwoWay
                                    if only_the_cloud_changed(state_db, &local_path, &local)? =>
                                {
                                    // The index vouches for the local
                                    // file, so the change is the cloud's:
                                    // it comes down. The upload planner
                                    // would reach the same answer after a
                                    // full hash of the unchanged file.
                                    batch.push((local_path, PendingIntentKind::Download, now));
                                }
                                SyncMode::TwoWay => {
                                    // Divergence in two-way routes through
                                    // the upload planner, where the
                                    // conflict policy decides.
                                    batch.push((local_path, PendingIntentKind::Upload, now));
                                }
                                SyncMode::PullOnly => {
                                    batch.push((local_path, PendingIntentKind::Download, now));
                                    self.stats.mirror_reverts += 1;
                                }
                                SyncMode::PushOnly => {
                                    batch.push((local_path, PendingIntentKind::Upload, now));
                                    self.stats.mirror_reverts += 1;
                                }
                            }
                        }
                    }
                    // Type mismatch (file vs directory): resolve in favor
                    // of the mode's source of truth. For the file/file and
                    // dir/dir replacements the clearing intent and the
                    // re-materializing intent share a path, so the
                    // executor's per-path serialization orders them. For a
                    // local-file-vs-remote-directory clear the children are
                    // enqueued at *different* paths (local_path/child), so
                    // ordering is NOT guaranteed: a child transfer whose
                    // parent has not yet been cleared fails transiently and
                    // converges via retry backoff (never a lost update). Do
                    // not add code here that relies on the parent clearing
                    // before its children.
                    (local_is_dir, remote_kind) => match sync_mode {
                        SyncMode::PullOnly => {
                            batch.push((
                                local_path.clone(),
                                PendingIntentKind::ApplyRemoteDelete,
                                now,
                            ));
                            self.stats.mirror_deletes += 1;
                            match remote_kind {
                                RemoteEntryKind::Directory => {
                                    self.pending_dirs.push_back(local_path);
                                }
                                RemoteEntryKind::File => {
                                    batch.push((
                                        directory.join(&name),
                                        PendingIntentKind::Download,
                                        now,
                                    ));
                                }
                            }
                        }
                        SyncMode::PushOnly => {
                            batch.push((local_path.clone(), PendingIntentKind::Delete, now));
                            self.stats.mirror_deletes += 1;
                            if local_is_dir {
                                self.pending_dirs.push_back(local_path);
                            } else {
                                batch.push((directory.join(&name), PendingIntentKind::Upload, now));
                            }
                        }
                        SyncMode::TwoWay => {
                            crate::logging::warning(
                                "Reconcile found a file/directory type mismatch in two-way mode; leaving both sides untouched",
                                &[("path", local_path.display().to_string())],
                            );
                            self.type_mismatches.push(local_path.clone());
                        }
                    },
                },
                (Some(local), None) => match sync_mode {
                    SyncMode::TwoWay | SyncMode::PushOnly => {
                        if local.is_dir {
                            self.pending_dirs.push_back(local.path);
                        } else if sync_mode == SyncMode::TwoWay
                            && !self.merge_without_deletions
                            && deleted_in_cloud_while_away(state_db, &local_path, &local)?
                        {
                            // A deletion to finish, not a file to
                            // resurrect: a synced file the cloud no
                            // longer has while the local copy is exactly
                            // what was last synced (the cloud side
                            // deleted it while no daemon was running).
                            // The executor's own guard and the
                            // mass-delete guard still get their say.
                            batch.push((local_path, PendingIntentKind::ApplyRemoteDelete, now));
                        } else {
                            batch.push((local_path, PendingIntentKind::Upload, now));
                        }
                    }
                    SyncMode::PullOnly => {
                        // Strict mirror: local-only content does not
                        // exist at the source of truth: remove it,
                        // recursively for directories.
                        batch.push((local_path, PendingIntentKind::ApplyRemoteDelete, now));
                        self.stats.mirror_deletes += 1;
                    }
                },
                (None, Some(remote)) => {
                    // A file this device synced under exactly this
                    // spelling and no longer has was renamed or deleted
                    // here; the entry that now answers for the name
                    // under another spelling is not a collision.
                    if sync_mode == SyncMode::TwoWay
                        && !self.merge_without_deletions
                        && remote.kind == RemoteEntryKind::File
                        && deleted_here_while_away(state_db, &local_path, &remote)?
                    {
                        batch.push((local_path, PendingIntentKind::Delete, now));
                        continue;
                    }
                    if let Some(existing) = crate::name_collision::colliding_local_path(&local_path)
                    {
                        self.materialize_colliding_remote(
                            state_db,
                            &local_path,
                            &existing,
                            &remote,
                            now,
                        )?;
                        continue;
                    }
                    match sync_mode {
                        SyncMode::TwoWay | SyncMode::PullOnly => match remote.kind {
                            RemoteEntryKind::Directory => {
                                // Children materialize local parents on
                                // apply; walk deeper to find them.
                                self.pending_dirs.push_back(local_path);
                            }
                            RemoteEntryKind::File => {
                                if sync_mode == SyncMode::TwoWay
                                    && !self.merge_without_deletions
                                    && deleted_here_while_away(state_db, &local_path, &remote)?
                                {
                                    // The mirror image: a synced file
                                    // this device no longer has while the
                                    // cloud copy is exactly what was last
                                    // synced (deleted here while no daemon
                                    // was running).
                                    batch.push((local_path, PendingIntentKind::Delete, now));
                                } else {
                                    batch.push((local_path, PendingIntentKind::Download, now));
                                }
                            }
                        },
                        SyncMode::PushOnly => {
                            // Strict mirror: cloud-only content does not
                            // exist at the local source of truth: remove it.
                            // A directory expands into per-entry deletes in
                            // the executor; no provider deletes a tree.
                            batch.push((local_path, PendingIntentKind::Delete, now));
                            self.stats.mirror_deletes += 1;
                        }
                    }
                }
                (None, None) => unreachable!("pair map only holds observed entries"),
            }
        }

        if !batch.is_empty() {
            // A merge after a root answer transfers every one-sided
            // file on the user's say-so; its uploads must not be read
            // as cloud deletions by the upload gate.
            self.stats.intents_enqueued += state_db.enqueue_intents_coalesced_with(
                &batch,
                crate::safeguards::IntentSource::ReconcileBacklog,
                if self.merge_without_deletions {
                    crate::state_db::Approval::Guards
                } else {
                    crate::state_db::Approval::None
                },
            )?;
        }
        Ok(())
    }
}

/// Equal sizes are not enough: a local edit that kept the byte count
/// (the common case for config files and source edits) is only visible
/// through the mtime the sync index recorded at the last transfer. No
/// index row means the pair was never verified by this daemon, so it
/// counts as diverged too: the planner hashes it once and records the
/// row, which is the only way the index gets rebuilt after a loss.
fn touched_since_last_sync(
    state_db: &DurableStateDb,
    local_path: &Path,
    local: &LocalEntry,
) -> Result<bool, WalkError> {
    Ok(match state_db.sync_index(local_path)? {
        None => true,
        Some(index) => {
            index.local_modified_at.is_some()
                && !index.matches_local(local.size_bytes, local.modified_at)
        }
    })
}

/// The index has a row that records the local file's modification
/// time, and the file is still exactly that: positive evidence that
/// this device did not change it, so a divergence is the cloud's.
fn only_the_cloud_changed(
    state_db: &DurableStateDb,
    local_path: &Path,
    local: &LocalEntry,
) -> Result<bool, WalkError> {
    Ok(state_db.sync_index(local_path)?.is_some_and(|index| {
        index.local_modified_at.is_some()
            && index.matches_local(local.size_bytes, local.modified_at)
    }))
}

/// The cloud-side twin of [`touched_since_last_sync`]: a remote edit
/// that kept the byte count is only visible through the remote mtime
/// the index recorded at the last transfer. A row without one (written
/// before the provider reported it) cannot claim divergence here; the
/// local check and the size comparison still apply.
fn remote_touched_since_last_sync(
    state_db: &DurableStateDb,
    local_path: &Path,
    remote: &RemoteEntry,
) -> Result<bool, WalkError> {
    Ok(match state_db.sync_index(local_path)? {
        None => true,
        Some(index) => {
            index.remote_modified_at.is_some()
                && !index.matches_remote(remote.size_bytes, remote.modified_at)
        }
    })
}

/// A synced file the cloud no longer has, while the local copy is
/// exactly what was last synced: the cloud side deleted it while no
/// daemon was watching. A local copy that changed since, or a pair the
/// index cannot vouch for (no row, no recorded mtime), keeps the file.
/// The index row is the only evidence of a deletion. A modification
/// time is set by the user's tools (a copy that keeps times, a restore
/// from a backup), so it never says which event came first.
fn deleted_in_cloud_while_away(
    state_db: &DurableStateDb,
    local_path: &Path,
    local: &LocalEntry,
) -> Result<bool, WalkError> {
    Ok(match state_db.sync_index(local_path)? {
        Some(index) => index.matches_local(local.size_bytes, local.modified_at),
        None => false,
    })
}

/// The mirror image: a synced file this device no longer has, while the
/// cloud copy is exactly what was last synced (the remote mtime the
/// index recorded, or its hash when the provider carries one).
fn deleted_here_while_away(
    state_db: &DurableStateDb,
    local_path: &Path,
    remote: &RemoteEntry,
) -> Result<bool, WalkError> {
    Ok(match state_db.sync_index(local_path)? {
        Some(index) => {
            index.matches_remote(remote.size_bytes, remote.modified_at)
                || remote
                    .content_hash
                    .as_deref()
                    .is_some_and(|hash| hash == index.content_hash)
        }
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vapor_platform::fs_caps::{CaseSensitivity, InMemoryFilesystemCapabilities};
    use vapor_providers::FilesystemProvider;

    struct Fixture {
        _temp: tempfile::TempDir,
        local_root: PathBuf,
        cloud_root: PathBuf,
        provider: Arc<dyn Provider>,
        state_db: DurableStateDb,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::TempDir::new().expect("temp dir");
            let local_root = temp.path().join("local");
            let cloud_root = temp.path().join("cloud");
            std::fs::create_dir_all(&local_root).expect("local root");
            std::fs::create_dir_all(&cloud_root).expect("cloud root");
            let local_root =
                vapor_shared::paths::canonicalize(&local_root).expect("canonical local");
            let caps = Arc::new(InMemoryFilesystemCapabilities::new(
                true,
                CaseSensitivity::Sensitive,
            ));
            let (provider, _feed) =
                FilesystemProvider::with_manual_feed(&cloud_root, caps).expect("manual provider");
            let state_db = DurableStateDb::open(temp.path().join("state/vapor.sqlite"))
                .expect("open state db");
            Self {
                local_root,
                cloud_root: temp.path().join("cloud"),
                _temp: temp,
                provider: Arc::new(provider),
                state_db,
            }
        }

        fn run_walk(&mut self, mode: SyncMode) -> WalkStats {
            let mut walker = ReconcileWalker::new(&self.local_root, &self.local_root, None);
            for _ in 0..64 {
                let done = walker
                    .process(
                        self.provider.clone(),
                        &ProviderCallMode::Inline,
                        mode,
                        &mut self.state_db,
                        8,
                        ts(0),
                        &|| true,
                    )
                    .expect("walk step");
                if done {
                    break;
                }
            }
            walker.stats()
        }

        fn queued_kinds(&mut self) -> Vec<(PathBuf, PendingIntentKind)> {
            let mut drained = Vec::new();
            while let Some(intent) = self.state_db.lease_next_ready(ts(10)).expect("lease") {
                drained.push((intent.path.clone(), intent.kind));
            }
            drained.sort_by(|a, b| a.0.cmp(&b.0));
            drained
        }
    }

    fn ts(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_750_000_000_000 + ms)
    }

    #[test]
    fn the_walk_forgets_the_rows_of_names_that_left_both_sides() {
        let mut fixture = Fixture::new();
        let root = fixture.local_root.clone();
        let earlier = ts(0) - std::time::Duration::from_secs(60);
        let seed = |fixture: &mut Fixture, path: &Path, at: SystemTime| {
            fixture
                .state_db
                .set_sync_index(path, "hash", 1, None, None, "op", at)
                .expect("index");
        };
        // Present on both sides, and present on one side only.
        for name in ["both.txt", "local.txt", "cloud.txt", "photos/kept.jpg"] {
            seed(&mut fixture, &root.join(name), earlier);
        }
        std::fs::create_dir_all(root.join("photos")).expect("dir");
        std::fs::create_dir_all(fixture.cloud_root.join("photos")).expect("dir");
        for (side, name) in [
            (&root, "both.txt"),
            (&fixture.cloud_root.clone(), "both.txt"),
            (&root, "local.txt"),
            (&fixture.cloud_root.clone(), "cloud.txt"),
            (&root, "photos/kept.jpg"),
            (&fixture.cloud_root.clone(), "photos/kept.jpg"),
        ] {
            std::fs::write(side.join(name), b"x").expect("seed");
        }
        // On neither side: a file, a folder's files, a name that starts
        // like a folder that stays, and a file inside that folder.
        let gone = [
            "gone.txt",
            "old/a.txt",
            "old/deeper/b.txt",
            "photos.txt",
            "photos/gone.jpg",
        ];
        for name in gone {
            seed(&mut fixture, &root.join(name), earlier);
        }
        // A transfer that finished after the listing was asked for.
        seed(&mut fixture, &root.join("just-synced.txt"), ts(5));

        fixture.run_walk(SyncMode::TwoWay);

        for name in gone {
            assert_eq!(
                fixture.state_db.sync_index(&root.join(name)).expect("read"),
                None,
                "{name}"
            );
        }
        for name in [
            "both.txt",
            "local.txt",
            "cloud.txt",
            "photos/kept.jpg",
            "just-synced.txt",
        ] {
            assert!(
                fixture
                    .state_db
                    .sync_index(&root.join(name))
                    .expect("read")
                    .is_some(),
                "{name}"
            );
        }
    }

    #[test]
    fn two_way_walk_uploads_local_only_and_downloads_remote_only() {
        let mut fixture = Fixture::new();
        std::fs::create_dir_all(fixture.local_root.join("sub")).expect("dirs");
        std::fs::write(fixture.local_root.join("sub/local-only.txt"), b"L").expect("seed");
        std::fs::create_dir_all(fixture.cloud_root.join("remote-dir")).expect("dirs");
        std::fs::write(fixture.cloud_root.join("remote-dir/remote-only.txt"), b"R").expect("seed");

        let stats = fixture.run_walk(SyncMode::TwoWay);
        assert!(stats.directories_compared >= 3);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![
                (
                    fixture.local_root.join("remote-dir/remote-only.txt"),
                    PendingIntentKind::Download
                ),
                (
                    fixture.local_root.join("sub/local-only.txt"),
                    PendingIntentKind::Upload
                ),
            ]
        );
    }

    #[test]
    fn pull_only_walk_mirrors_cloud_removing_local_only_content() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.local_root.join("local-only.txt"), b"L").expect("seed");
        std::fs::create_dir_all(fixture.local_root.join("local-only-dir")).expect("dirs");
        std::fs::write(fixture.local_root.join("local-only-dir/nested.txt"), b"N").expect("seed");
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"remote-version").expect("seed");
        std::fs::write(fixture.local_root.join("shared.txt"), b"different-local!").expect("seed");

        let stats = fixture.run_walk(SyncMode::PullOnly);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![
                (
                    fixture.local_root.join("local-only-dir"),
                    PendingIntentKind::ApplyRemoteDelete
                ),
                (
                    fixture.local_root.join("local-only.txt"),
                    PendingIntentKind::ApplyRemoteDelete
                ),
                (
                    fixture.local_root.join("shared.txt"),
                    PendingIntentKind::Download
                ),
            ]
        );
        assert_eq!(stats.mirror_deletes, 2);
        assert_eq!(stats.mirror_reverts, 1);
    }

    #[test]
    fn push_only_walk_mirrors_local_removing_cloud_only_content() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.cloud_root.join("cloud-only.txt"), b"C").expect("seed");
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"remote-version").expect("seed");
        std::fs::write(fixture.local_root.join("shared.txt"), b"different-local!").expect("seed");
        std::fs::write(fixture.local_root.join("local-only.txt"), b"L").expect("seed");

        let stats = fixture.run_walk(SyncMode::PushOnly);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![
                (
                    fixture.local_root.join("cloud-only.txt"),
                    PendingIntentKind::Delete
                ),
                (
                    fixture.local_root.join("local-only.txt"),
                    PendingIntentKind::Upload
                ),
                (
                    fixture.local_root.join("shared.txt"),
                    PendingIntentKind::Upload
                ),
            ]
        );
        assert_eq!(stats.mirror_deletes, 1);
        assert_eq!(stats.mirror_reverts, 1);
    }

    #[test]
    fn a_file_set_aside_before_a_crash_comes_back_as_a_conflict_copy() {
        let mut fixture = Fixture::new();
        let original = fixture.local_root.join("plan.md");
        // The crash came after the first rename: the edit sits under
        // the temp name, last changed long ago, and nothing is at the
        // original name.
        let aside = crate::conflict::aside_path(&original, "dev-1-1");
        std::fs::write(&aside, b"an edit that exists nowhere else").expect("seed");
        let long_ago = SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3_600);
        std::fs::File::options()
            .write(true)
            .open(&aside)
            .and_then(|file| file.set_modified(long_ago))
            .expect("old mtime");
        // A second one whose bytes equal the file that took its place.
        let replaced = fixture.local_root.join("same.md");
        std::fs::write(&replaced, b"identical").expect("seed");
        let identical_aside = crate::conflict::aside_path(&replaced, "dev-1-2");
        std::fs::write(&identical_aside, b"identical").expect("seed");

        fixture.run_walk(SyncMode::TwoWay);

        assert!(!aside.exists(), "the temp name is gone");
        assert!(!identical_aside.exists(), "identical bytes are dropped");
        let copy = std::fs::read_dir(&fixture.local_root)
            .expect("list")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("plan~conflict-") && name.ends_with(".md"))
            })
            .expect("the edit came back under a conflict name");
        assert_eq!(
            std::fs::read(&copy).expect("copy"),
            b"an edit that exists nowhere else"
        );
        let queued = fixture.queued_kinds();
        assert!(
            queued.contains(&(copy.clone(), PendingIntentKind::Upload)),
            "the copy uploads: {queued:?}"
        );
    }

    #[test]
    fn the_walk_compares_a_set_aside_file_off_the_calling_thread() {
        let mut fixture = Fixture::new();
        let replaced = fixture.local_root.join("same.md");
        std::fs::write(&replaced, b"identical").expect("seed");
        let identical_aside = crate::conflict::aside_path(&replaced, "dev-1-2");
        std::fs::write(&identical_aside, b"identical").expect("seed");
        let edited = fixture.local_root.join("plan.md");
        std::fs::write(&edited, b"what the download put here").expect("seed");
        let edit_aside = crate::conflict::aside_path(&edited, "dev-1-3");
        std::fs::write(&edit_aside, b"an edit that exists nowhere").expect("seed");

        let reads_before = crate::local_hash::content_reads_on_this_thread();
        let mode = ProviderCallMode::Threaded { waker: None };
        let mut walker = ReconcileWalker::new(&fixture.local_root, &fixture.local_root, None);
        // Bounded by iteration count: the loop ends as soon as the
        // threads deliver.
        let mut done = false;
        for _ in 0..1_000_000 {
            done = walker
                .process(
                    fixture.provider.clone(),
                    &mode,
                    SyncMode::TwoWay,
                    &mut fixture.state_db,
                    8,
                    ts(0),
                    &|| true,
                )
                .expect("walk step");
            if done {
                break;
            }
            std::thread::yield_now();
        }

        assert!(done);
        assert!(!identical_aside.exists(), "identical bytes are dropped");
        assert!(!edit_aside.exists(), "the edit left the temp name");
        assert!(
            std::fs::read_dir(&fixture.local_root)
                .expect("list")
                .flatten()
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("plan~conflict-")),
            "the edit came back under a conflict name"
        );
        assert_eq!(
            crate::local_hash::content_reads_on_this_thread(),
            reads_before,
            "the walk's thread read no file"
        );
    }

    #[test]
    fn a_change_made_only_in_the_cloud_is_queued_as_a_download() {
        let mut fixture = Fixture::new();
        let local = fixture.local_root.join("report.txt");
        std::fs::write(&local, b"version one").expect("seed local");
        let metadata = std::fs::symlink_metadata(&local).expect("stat");
        fixture
            .state_db
            .set_sync_index(
                &local,
                "hash-of-version-one",
                metadata.len(),
                metadata.modified().ok(),
                Some(ts(0)),
                "op",
                ts(0),
            )
            .expect("index");
        std::fs::write(
            fixture.cloud_root.join("report.txt"),
            b"version two, longer",
        )
        .expect("cloud edit");

        fixture.run_walk(SyncMode::TwoWay);
        assert_eq!(
            fixture.queued_kinds(),
            vec![(local.clone(), PendingIntentKind::Download)],
            "the unchanged local file is not sent through the upload planner"
        );

        // A local file the index cannot vouch for still goes to the
        // planner, where the conflict policy decides.
        std::fs::write(&local, b"edited here too!!").expect("local edit");
        fixture.run_walk(SyncMode::TwoWay);
        assert_eq!(
            fixture.queued_kinds(),
            vec![(local, PendingIntentKind::Upload)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_the_walk_cannot_read_is_reported_for_a_later_walk() {
        use std::os::unix::fs::PermissionsExt;
        let mut fixture = Fixture::new();
        let locked = fixture.local_root.join("locked");
        std::fs::create_dir(&locked).expect("dir");
        std::fs::write(locked.join("inside.txt"), b"x").expect("seed");
        std::fs::create_dir_all(fixture.cloud_root.join("locked")).expect("cloud dir");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");
        if std::fs::read_dir(&locked).is_ok() {
            // Running as a user that permissions do not stop.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).ok();
            return;
        }

        let mut walker = ReconcileWalker::new(&fixture.local_root, &fixture.local_root, None);
        for _ in 0..64 {
            if walker
                .process(
                    fixture.provider.clone(),
                    &ProviderCallMode::Inline,
                    SyncMode::TwoWay,
                    &mut fixture.state_db,
                    8,
                    ts(0),
                    &|| true,
                )
                .expect("walk step")
            {
                break;
            }
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        assert_eq!(
            walker.take_unreadable_dirs(),
            vec![(locked, std::io::ErrorKind::PermissionDenied)]
        );
        assert!(
            fixture.queued_kinds().is_empty(),
            "nothing under a folder that was not read is touched"
        );
    }

    #[test]
    fn a_cloud_file_with_no_index_row_downloads_whatever_its_age() {
        // Deleted here once, then put back in the cloud by a tool that
        // keeps modification times: the file is old, and it is still a
        // file this device never compared. It comes down.
        let mut fixture = Fixture::new();
        let remote_file = fixture.cloud_root.join("put-back.txt");
        std::fs::write(&remote_file, b"an older copy").expect("seed remote");
        let long_ago = SystemTime::now() - std::time::Duration::from_secs(90 * 24 * 3_600);
        std::fs::File::options()
            .write(true)
            .open(&remote_file)
            .and_then(|file| file.set_modified(long_ago))
            .expect("old mtime");

        fixture.run_walk(SyncMode::TwoWay);
        assert_eq!(
            fixture.queued_kinds(),
            vec![(
                fixture.local_root.join("put-back.txt"),
                PendingIntentKind::Download
            )]
        );
    }

    #[test]
    fn a_local_file_with_no_index_row_uploads_whatever_its_age() {
        // The mirror image: deleted in the cloud once, then restored
        // here from a backup with its old modification time.
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("restored.txt");
        std::fs::write(&local_file, b"restored from a backup").expect("seed local");
        let long_ago = SystemTime::now() - std::time::Duration::from_secs(90 * 24 * 3_600);
        std::fs::File::options()
            .write(true)
            .open(&local_file)
            .and_then(|file| file.set_modified(long_ago))
            .expect("old mtime");

        fixture.run_walk(SyncMode::TwoWay);
        assert_eq!(
            fixture.queued_kinds(),
            vec![(local_file, PendingIntentKind::Upload)]
        );
    }

    #[test]
    fn equal_size_pair_without_an_index_row_is_verified_once() {
        // Two pre-populated roots (or a lost state DB): the walk cannot
        // tell identical from divergent content by size alone, so the
        // pair is routed to the upload planner, which hashes it and
        // records the index row. With the row present and matching, the
        // next walk treats the pair as converged.
        let mut fixture = Fixture::new();
        let local = fixture.local_root.join("same.txt");
        std::fs::write(&local, b"12345").expect("seed");
        std::fs::write(fixture.cloud_root.join("same.txt"), b"12345").expect("seed");

        fixture.run_walk(SyncMode::TwoWay);
        assert_eq!(
            fixture.queued_kinds(),
            vec![(local.clone(), PendingIntentKind::Upload)]
        );

        let mtime = std::fs::symlink_metadata(&local)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(&local, "hash-of-12345", 5, mtime, None, "op-1", ts(1))
            .expect("index row");
        fixture.run_walk(SyncMode::TwoWay);
        assert!(fixture.queued_kinds().is_empty());
    }

    /// A provider that only answers `enumerate`, with whatever names
    /// the test hands it, so a listing a real filesystem cloud root
    /// cannot hold (two names differing only by case) can be staged.
    struct ListingProvider {
        entries: Vec<RemoteEntry>,
    }

    impl Provider for ListingProvider {
        fn name(&self) -> &'static str {
            "listing"
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            vapor_providers::inert_stub_provider().capabilities()
        }
        fn ensure_cloud_root(&self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(if directory.is_root() {
                self.entries.clone()
            } else {
                Vec::new()
            })
        }
        fn stat(&self, _: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
            Ok(None)
        }
        fn content_hash(&self, _: &RemotePath) -> Result<String, ProviderError> {
            Err(ProviderError::not_found("listing provider has no content"))
        }
        fn begin_upload(
            &self,
            _: vapor_providers::UploadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, ProviderError> {
            Err(ProviderError::not_found("listing provider cannot upload"))
        }
        fn begin_download(
            &self,
            _: vapor_providers::DownloadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, ProviderError> {
            Err(ProviderError::not_found("listing provider cannot download"))
        }
        fn delete(&self, _: &RemotePath, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn poll_changes(
            &self,
            _: Option<&str>,
            _: usize,
        ) -> Result<vapor_providers::ChangesPoll, ProviderError> {
            Err(ProviderError::not_found("listing provider has no feed"))
        }
    }

    fn remote_file(name: &str, size: u64) -> RemoteEntry {
        RemoteEntry {
            path: RemotePath::root().join(name).expect("name"),
            kind: RemoteEntryKind::File,
            size_bytes: size,
            modified_at: ts(0),
            content_hash: None,
            op_id: None,
        }
    }

    #[test]
    fn a_remote_name_aliasing_a_local_file_materializes_as_an_aliased_conflict_copy() {
        // The cloud holds Readme.md and readme.md; a case-insensitive
        // local root can hold one. The second name must not download
        // onto the first one's file (that rewrote the cloud object it
        // came from); it comes down under a conflict-copy name that is
        // aliased to its own cloud object from then on.
        let mut fixture = Fixture::new();
        std::fs::write(fixture.local_root.join("Readme.md"), b"upper").expect("seed");
        let aliases_locally =
            std::fs::symlink_metadata(fixture.local_root.join("readme.md")).is_ok();
        let provider: Arc<dyn Provider> = Arc::new(ListingProvider {
            entries: vec![remote_file("Readme.md", 5), remote_file("readme.md", 5)],
        });

        let mut walker = ReconcileWalker::new(&fixture.local_root, &fixture.local_root, None)
            .with_device_id("dev");
        let mut done = false;
        for _ in 0..64 {
            done = walker
                .process(
                    provider.clone(),
                    &ProviderCallMode::Inline,
                    SyncMode::TwoWay,
                    &mut fixture.state_db,
                    8,
                    ts(0),
                    &|| true,
                )
                .expect("walk step");
            if done {
                break;
            }
        }
        assert!(done);
        let queued = fixture.queued_kinds();
        // Readme.md itself has no index row, so it is verified once.
        let verify = (
            fixture.local_root.join("Readme.md"),
            PendingIntentKind::Upload,
        );
        if aliases_locally {
            assert_eq!(queued.len(), 2, "{queued:?}");
            assert_eq!(queued[0], verify);
            let (copy, kind) = &queued[1];
            assert_eq!(*kind, PendingIntentKind::Download);
            let copy_name = copy.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                copy_name.starts_with("readme~conflict-dev-") && copy_name.ends_with(".md"),
                "the colliding name comes down as a conflict copy: {copy_name}"
            );
            let (alias_local, _) = fixture
                .state_db
                .name_alias("readme.md")
                .expect("query")
                .expect("the alias is recorded");
            assert_eq!(&alias_local, copy);
            assert_eq!(
                fixture
                    .state_db
                    .alias_remote_for_local(copy)
                    .expect("query")
                    .as_deref(),
                Some("readme.md")
            );
            let intent = fixture
                .state_db
                .list_queue_intents(4)
                .expect("queue")
                .into_iter()
                .find(|intent| intent.kind == PendingIntentKind::Download)
                .expect("download");
            assert_eq!(
                intent.remote_path.as_deref(),
                Some("readme.md"),
                "the download names the cloud object it comes from"
            );
            assert_eq!(
                walker.take_name_collisions(),
                vec![(fixture.local_root.join("readme.md"), copy.clone())]
            );
        } else {
            // Case-sensitive local root: both names can coexist and the
            // second one downloads like any remote-only file.
            assert_eq!(
                queued,
                vec![
                    verify,
                    (
                        fixture.local_root.join("readme.md"),
                        PendingIntentKind::Download
                    )
                ]
            );
            assert!(walker.take_name_collisions().is_empty());
        }
    }

    #[test]
    fn an_aliased_copy_pairs_with_its_own_cloud_object_on_later_walks() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.local_root.join("Readme.md"), b"upper").expect("seed");
        let copy = fixture.local_root.join("readme~conflict-dev-1.md");
        std::fs::write(&copy, b"lower").expect("copy");
        // Both files are synced: Readme.md with its mirror, the copy
        // with the aliased readme.md.
        for (path, op) in [
            (fixture.local_root.join("Readme.md"), "op-a"),
            (copy.clone(), "op-b"),
        ] {
            let mtime = std::fs::symlink_metadata(&path)
                .and_then(|m| m.modified())
                .ok();
            fixture
                .state_db
                .set_sync_index(&path, "hash", 5, mtime, Some(ts(0)), op, ts(1))
                .expect("index");
        }
        fixture
            .state_db
            .record_name_alias("readme.md", &copy, "hash", ts(1))
            .expect("alias");
        let provider: Arc<dyn Provider> = Arc::new(ListingProvider {
            entries: vec![remote_file("Readme.md", 5), remote_file("readme.md", 5)],
        });
        let mut walker = ReconcileWalker::new(&fixture.local_root, &fixture.local_root, None)
            .with_device_id("dev");
        let mut done = false;
        for _ in 0..64 {
            done = walker
                .process(
                    provider.clone(),
                    &ProviderCallMode::Inline,
                    SyncMode::TwoWay,
                    &mut fixture.state_db,
                    8,
                    ts(0),
                    &|| true,
                )
                .expect("walk step");
            if done {
                break;
            }
        }
        assert!(done);
        assert!(
            fixture.queued_kinds().is_empty(),
            "both pairs are converged: {:?}",
            fixture.queued_kinds()
        );
        assert!(walker.take_name_collisions().is_empty());
    }

    #[test]
    fn walk_applies_ignore_rules_to_both_sides() {
        let mut fixture = Fixture::new();
        let filter = Arc::new(crate::fs_events::SharedEventPathFilter::new(
            &fixture.local_root,
            crate::path_filter::EventPathFilterOptions::default(),
        ));

        // Divergent Finder metadata on both sides: without symmetric
        // filtering this pairs as content divergence and manufactures a
        // keep-both conflict for a file the watcher rightly never syncs.
        std::fs::write(fixture.local_root.join(".DS_Store"), b"local finder state").expect("seed");
        std::fs::write(
            fixture.cloud_root.join(".DS_Store"),
            b"different cloud bytes",
        )
        .expect("seed");
        // Cloud-only ignored subtree: must not be descended into or
        // downloaded.
        std::fs::create_dir_all(fixture.cloud_root.join("node_modules/pkg")).expect("dirs");
        std::fs::write(fixture.cloud_root.join("node_modules/pkg/index.js"), b"x").expect("seed");
        // Local-only ignored file: must not be uploaded by the walk.
        std::fs::write(fixture.local_root.join("scratch.tmp"), b"t").expect("seed");
        // Control: real divergence still converges.
        std::fs::write(fixture.cloud_root.join("real.txt"), b"content").expect("seed");

        let mut walker =
            ReconcileWalker::new(&fixture.local_root, &fixture.local_root, Some(filter));
        let mut done = false;
        for _ in 0..64 {
            done = walker
                .process(
                    fixture.provider.clone(),
                    &ProviderCallMode::Inline,
                    SyncMode::TwoWay,
                    &mut fixture.state_db,
                    8,
                    ts(0),
                    &|| true,
                )
                .expect("walk step");
            if done {
                break;
            }
        }
        assert!(done, "walk must finish");

        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(
                fixture.local_root.join("real.txt"),
                PendingIntentKind::Download
            )],
            "only the non-ignored file may produce an intent"
        );
    }

    #[test]
    fn a_folder_with_an_ignore_file_that_is_not_loaded_waits_for_the_reload() {
        let mut fixture = Fixture::new();
        let filter = Arc::new(crate::fs_events::SharedEventPathFilter::new(
            &fixture.local_root,
            crate::path_filter::EventPathFilterOptions::default(),
        ));
        // After the rules were built, a project arrives with rules of
        // its own, and no watcher event tells of it.
        let project = fixture.local_root.join("project");
        std::fs::create_dir_all(project.join("src")).expect("dirs");
        std::fs::write(project.join(".gitignore"), "secret.env\n").expect("rules");
        std::fs::write(project.join("secret.env"), b"TOKEN").expect("secret");
        std::fs::write(project.join("src/main.rs"), b"fn main() {}").expect("source");
        std::fs::write(fixture.local_root.join("plain.txt"), b"plain").expect("plain");

        let walk = |fixture: &mut Fixture| {
            let mut walker = ReconcileWalker::new(
                &fixture.local_root,
                &fixture.local_root,
                Some(filter.clone()),
            );
            for _ in 0..64 {
                if walker
                    .process(
                        fixture.provider.clone(),
                        &ProviderCallMode::Inline,
                        SyncMode::TwoWay,
                        &mut fixture.state_db,
                        8,
                        ts(0),
                        &|| true,
                    )
                    .expect("walk step")
                {
                    break;
                }
            }
            walker.take_dirs_waiting_for_rules()
        };

        // The folder is left alone, with everything under it, and a
        // reload is asked for. The rest of the tree is compared.
        assert_eq!(walk(&mut fixture), vec![project.clone()]);
        assert_eq!(
            fixture.queued_kinds(),
            vec![(
                fixture.local_root.join("plain.txt"),
                PendingIntentKind::Upload
            )]
        );

        // With the rules in, the folder is compared by them.
        assert!(filter.rebuild_if_requested(), "the walk asked for a reload");
        assert!(walk(&mut fixture).is_empty());
        let queued = fixture.queued_kinds();
        assert!(
            queued.contains(&(project.join("src/main.rs"), PendingIntentKind::Upload))
                && queued.contains(&(project.join(".gitignore"), PendingIntentKind::Upload)),
            "{queued:?}"
        );
        assert!(
            !queued.iter().any(|(path, _)| path.ends_with("secret.env")),
            "{queued:?}"
        );
    }

    #[test]
    fn walk_is_incremental_across_process_calls() {
        let mut fixture = Fixture::new();
        for index in 0..5 {
            let dir = fixture.local_root.join(format!("dir-{index}"));
            std::fs::create_dir_all(&dir).expect("dirs");
            std::fs::write(dir.join("f.txt"), b"x").expect("seed");
        }

        let mut walker = ReconcileWalker::new(&fixture.local_root, &fixture.local_root, None);
        // Budget of 2 directories per call: the root plus one child.
        let first_done = walker
            .process(
                fixture.provider.clone(),
                &ProviderCallMode::Inline,
                SyncMode::TwoWay,
                &mut fixture.state_db,
                2,
                ts(0),
                &|| true,
            )
            .expect("walk step");
        assert!(!first_done, "five child dirs cannot finish in one call");

        let mut done = false;
        for _ in 0..8 {
            done = walker
                .process(
                    fixture.provider.clone(),
                    &ProviderCallMode::Inline,
                    SyncMode::TwoWay,
                    &mut fixture.state_db,
                    2,
                    ts(0),
                    &|| true,
                )
                .expect("walk step");
            if done {
                break;
            }
        }
        assert!(done, "walk must finish within the budget");
        assert_eq!(fixture.queued_kinds().len(), 5, "one upload per file");
    }
}
