//! Incremental reconcile comparison walk.
//!
//! Reconcile compares the local tree with the provider's remote tree
//! and enqueues the durable intents that make the two converge under
//! the scope's [`SyncMode`]. The walk is deliberately incremental — a
//! bounded number of directories per checkpoint — so the reconcile
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
}

pub struct ReconcileWalker {
    /// The scope's local root (remote paths derive relative to it).
    scope_root: PathBuf,
    /// The subtree being reconciled (== scope_root for whole-scope).
    subtree_root: PathBuf,
    pending_enumeration: Option<PendingEnumeration>,
    /// Two-way file/directory type mismatches found by this walk; the
    /// runtime surfaces them on the timeline so the user can act.
    type_mismatches: Vec<PathBuf>,
    /// Remote names that would alias an existing, differently-cased
    /// local file (`(wanted local path, existing local path)`); left
    /// untouched on both sides and surfaced on the timeline.
    name_collisions: Vec<(PathBuf, PathBuf)>,
    /// Ignore rules, applied symmetrically: local entries and remote
    /// entries (via their local-equivalent path) that match never
    /// produce intents and are never descended into. Without this the
    /// walk would pull ignored names (`.DS_Store`, `node_modules/`)
    /// down from the cloud side — and manufacture keep-both conflicts
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
            type_mismatches: Vec::new(),
            name_collisions: Vec::new(),
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

    fn ignores(&self, local_path: &Path) -> bool {
        self.path_filter
            .as_ref()
            .map(|filter| filter.should_ignore(local_path))
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

    pub fn take_name_collisions(&mut self) -> Vec<(PathBuf, PathBuf)> {
        std::mem::take(&mut self.name_collisions)
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
                self.pending_enumeration = Some(PendingEnumeration { directory, call });
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
            let directory = self
                .pending_enumeration
                .take()
                .expect("pending listing")
                .directory;
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
            self.compare_directory(sync_mode, state_db, &directory, remote_entries, now)?;
            self.stats.directories_compared += 1;
            if !should_continue() {
                return Ok(self.pending_dirs.is_empty());
            }
        }
        Ok(self.pending_dirs.is_empty() && self.pending_enumeration.is_none())
    }

    fn compare_directory(
        &mut self,
        sync_mode: SyncMode,
        state_db: &mut DurableStateDb,
        directory: &Path,
        remote_entries: Vec<RemoteEntry>,
        now: SystemTime,
    ) -> Result<(), WalkError> {
        let mut pairs: BTreeMap<String, EntryPair> = BTreeMap::new();

        match fs::read_dir(directory) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                        // Non-UTF-8 names are unrepresentable in the sync
                        // path model; log so a file that never syncs is
                        // diagnosable (macOS enforces UTF-8, so this is rare).
                        crate::logging::warning(
                            "Reconcile walk skipped a non-UTF-8 local file name (cannot be synced)",
                            &[("path", entry.path().to_string_lossy().into_owned())],
                        );
                        continue;
                    };
                    if vapor_providers::filesystem::is_internal_file_name(&name) {
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
                    if self.ignores(&entry.path()) {
                        continue;
                    }
                    let metadata = match entry.path().symlink_metadata() {
                        Ok(metadata) => metadata,
                        // The entry was just listed, so it exists — a stat
                        // failure is transient (permission, EIO). Never let
                        // it read as "locally absent": that would drive a
                        // strict-mirror remote delete of content that is
                        // still present. Abandon this directory; a later
                        // reconcile retries it.
                        Err(error) => {
                            crate::logging::warning(
                                "Reconcile walk cannot stat a local entry; deferring the directory",
                                &[
                                    ("path", entry.path().display().to_string()),
                                    ("error", error.to_string()),
                                ],
                            );
                            return Ok(());
                        }
                    };
                    // Regular files and directories only: symlinks,
                    // FIFOs, sockets, and device nodes are outside the
                    // sync contract (the executor refuses them too —
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
            if self.ignores(&directory.join(&name)) {
                continue;
            }
            pairs.entry(name).or_default().remote = Some(entry);
        }

        // Remote-only names that differ only by case cannot all
        // materialize on a case-insensitive local filesystem. The
        // lexically first one proceeds; the rest are collisions, the
        // same verdict a later walk would reach once the first exists.
        let mut skipped_aliases: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        if crate::name_collision::local_filesystem_folds_case() {
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

        let mut batch: Vec<(PathBuf, PendingIntentKind, SystemTime)> = Vec::new();
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
                            && (remote_deletion_wins(state_db, &local_path, local.modified_at)
                                || deleted_in_cloud_while_away(state_db, &local_path, &local)?)
                        {
                            // A deletion to finish, not a file to
                            // resurrect: either a remote-origin tombstone
                            // the local copy predates, or a synced file the
                            // cloud no longer has while the local copy is
                            // exactly what was last synced (the cloud side
                            // deleted it while no daemon was running).
                            // The executor's own guard and the
                            // mass-deletion guard still get their say.
                            batch.push((local_path, PendingIntentKind::ApplyRemoteDelete, now));
                        } else {
                            batch.push((local_path, PendingIntentKind::Upload, now));
                        }
                    }
                    SyncMode::PullOnly => {
                        // Strict mirror: local-only content does not
                        // exist at the source of truth — remove it,
                        // recursively for directories.
                        batch.push((local_path, PendingIntentKind::ApplyRemoteDelete, now));
                        self.stats.mirror_deletes += 1;
                    }
                },
                (None, Some(remote)) => {
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
                                    && (local_deletion_wins(
                                        state_db,
                                        &local_path,
                                        remote.modified_at,
                                    ) || deleted_here_while_away(
                                        state_db,
                                        &local_path,
                                        &remote,
                                    )?)
                                {
                                    // The mirror image: a local-origin
                                    // tombstone the remote copy predates,
                                    // or a synced file this device no
                                    // longer has while the cloud copy is
                                    // exactly what was last synced (deleted
                                    // here while no daemon was running).
                                    batch.push((local_path, PendingIntentKind::Delete, now));
                                } else {
                                    batch.push((local_path, PendingIntentKind::Download, now));
                                }
                            }
                        },
                        SyncMode::PushOnly => {
                            // Strict mirror: cloud-only content does not
                            // exist at the local source of truth — remove it
                            // (provider delete handles directories).
                            batch.push((local_path, PendingIntentKind::Delete, now));
                            self.stats.mirror_deletes += 1;
                        }
                    }
                }
                (None, None) => unreachable!("pair map only holds observed entries"),
            }
        }

        if !batch.is_empty() {
            self.stats.intents_enqueued += state_db.enqueue_intents_coalesced(
                &batch,
                crate::safeguards::IntentSource::ReconcileBacklog,
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

/// Whether a remote-origin tombstone should win over a surviving local
/// file: the deletion wins only when the local copy was not modified
/// after the deletion ("data preservation wins over deletion" — a newer
/// local edit uploads instead).
fn remote_deletion_wins(
    state_db: &DurableStateDb,
    local_path: &Path,
    local_modified_at: Option<SystemTime>,
) -> bool {
    match state_db.tombstone(local_path) {
        Ok(Some(tombstone)) if tombstone.origin == crate::state_db::TombstoneOrigin::Remote => {
            // An unreadable mtime cannot prove the file predates the
            // deletion; keeping data wins over honouring the tombstone.
            match local_modified_at {
                Some(modified_at) => modified_at <= tombstone.deleted_at,
                None => false,
            }
        }
        _ => false,
    }
}

/// Whether a local-origin tombstone should win over a surviving remote
/// file: symmetric to [`remote_deletion_wins`] — a remote copy modified
/// after our deletion re-downloads instead.
fn local_deletion_wins(
    state_db: &DurableStateDb,
    local_path: &Path,
    remote_modified_at: SystemTime,
) -> bool {
    match state_db.tombstone(local_path) {
        Ok(Some(tombstone)) if tombstone.origin == crate::state_db::TombstoneOrigin::Local => {
            remote_modified_at <= tombstone.deleted_at
        }
        _ => false,
    }
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
    fn local_tombstone_replays_the_deletion_when_remote_is_unchanged() {
        // Restart-safe replay: we deleted locally, the propagation
        // was lost, and the remote copy has not changed since — the
        // reconcile finishes the deletion instead of resurrecting it.
        let mut fixture = Fixture::new();
        let remote_file = fixture.cloud_root.join("deleted-here.txt");
        std::fs::write(&remote_file, b"old remote copy").expect("seed remote");

        // Tombstone is NEWER than the remote copy's mtime.
        fixture
            .state_db
            .record_tombstone(
                &fixture.local_root.join("deleted-here.txt"),
                crate::state_db::TombstoneOrigin::Local,
                SystemTime::now() + std::time::Duration::from_secs(60),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(
                fixture.local_root.join("deleted-here.txt"),
                PendingIntentKind::Delete
            )],
            "the walk must finish propagating the deletion"
        );
    }

    #[test]
    fn remote_recreation_after_local_tombstone_downloads_again() {
        // The remote copy is newer than our deletion: it was recreated
        // or edited after we deleted — data preservation wins.
        let mut fixture = Fixture::new();
        std::fs::write(fixture.cloud_root.join("recreated.txt"), b"newer").expect("seed remote");

        fixture
            .state_db
            .record_tombstone(
                &fixture.local_root.join("recreated.txt"),
                crate::state_db::TombstoneOrigin::Local,
                SystemTime::now() - std::time::Duration::from_secs(3_600),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(
                fixture.local_root.join("recreated.txt"),
                PendingIntentKind::Download
            )]
        );
    }

    #[test]
    fn remote_tombstone_replays_the_local_removal_when_local_is_unchanged() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("deleted-there.txt");
        std::fs::write(&local_file, b"stale local copy").expect("seed local");

        fixture
            .state_db
            .record_tombstone(
                &local_file,
                crate::state_db::TombstoneOrigin::Remote,
                SystemTime::now() + std::time::Duration::from_secs(60),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(local_file, PendingIntentKind::ApplyRemoteDelete)],
            "the walk must finish applying the remote deletion"
        );
    }

    #[test]
    fn local_edit_after_remote_tombstone_uploads_instead_of_deleting() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("revived.txt");
        std::fs::write(&local_file, b"edited after the deletion").expect("seed local");

        // Tombstone predates the local file's mtime.
        fixture
            .state_db
            .record_tombstone(
                &local_file,
                crate::state_db::TombstoneOrigin::Remote,
                SystemTime::now() - std::time::Duration::from_secs(3_600),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(local_file, PendingIntentKind::Upload)],
            "data preservation must win over the stale deletion"
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
        fn ensure_cloud_sync_directory(&self, _: &str) -> Result<(), ProviderError> {
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
