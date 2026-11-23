//! The managed trash: where a file goes when Vapor itself removes it
//! from this device, so a wrong deletion can be undone here without
//! the cloud's own trash (which the filesystem provider does not have).
//!
//! Layout: `vapor_dir/trash/<profile>/<entry id>/` holds the payload
//! under its original file name and a `meta.json` next to it with the
//! original path, when and why it was discarded. A sync root on
//! another volume (an external drive) gets a second location on that
//! volume, `<volume root>/.vapor/trash/<profile>/`, with the same
//! layout, so a discard there is a rename on that drive and never a
//! copy onto this one; both locations are listed, restored from, and
//! purged together. Entries older than the retention window are
//! purged by the daemon. With `trash.useSystem` the user's own
//! trash is tried first (the Finder's Trash on macOS) and the managed
//! trash is the fallback, so a discard never silently degrades to an
//! unlink.
//!
//! Design: `docs/architecture/data-flow.md` §Remote to local.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use vapor_platform::TrashBin;
use vapor_shared::constants;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrashSettings {
    pub enabled: bool,
    pub retention: Duration,
    pub use_system: bool,
}

impl Default for TrashSettings {
    fn default() -> Self {
        Self {
            enabled: constants::trash::DEFAULT_ENABLED,
            retention: Duration::from_secs(
                u64::from(constants::trash::DEFAULT_RETENTION_DAYS) * 86_400,
            ),
            use_system: constants::trash::DEFAULT_USE_SYSTEM,
        }
    }
}

impl TrashSettings {
    pub fn resolve(config: &vapor_shared::config::VaporConfig) -> Self {
        let configured = &config.trash;
        Self {
            enabled: configured.enabled,
            retention: Duration::from_secs(u64::from(configured.retention_days) * 86_400),
            use_system: configured.use_system,
        }
    }
}

/// Where a discarded item ended up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// In the managed trash, under this entry id.
    Managed(String),
    /// In the user's own trash, at this path.
    System(PathBuf),
    /// Unlinked: the trash is disabled.
    Removed,
}

/// The `meta.json` of a managed-trash entry.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    pub id: String,
    pub profile_id: String,
    pub original_path: PathBuf,
    pub file_name: String,
    /// `file` or `directory`.
    pub kind: String,
    pub reason: String,
    pub discarded_at_ms: u64,
    pub size_bytes: u64,
}

impl TrashEntry {
    pub fn discarded_at(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(self.discarded_at_ms)
    }
}

pub struct LocalTrash {
    profile_id: String,
    /// The home location, under the runtime directory.
    root: PathBuf,
    /// The profile's local sync root, when known: the one place
    /// discards come from, and so the one other volume that may hold
    /// a location of its own.
    sync_root: Option<PathBuf>,
    /// Tests stand in a second location without a second volume.
    #[cfg(test)]
    pinned_volume: Option<PathBuf>,
    /// Tests stand in a trash on another volume: the rename of an item
    /// into an entry answers `CrossesDevices`, as the OS does there.
    #[cfg(test)]
    pub(crate) rename_crosses_devices: bool,
    settings: TrashSettings,
    system: Arc<dyn TrashBin>,
    sequence: AtomicU64,
}

impl std::fmt::Debug for LocalTrash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalTrash")
            .field("profile_id", &self.profile_id)
            .field("root", &self.root)
            .field("sync_root", &self.sync_root)
            .field("settings", &self.settings)
            .finish()
    }
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl LocalTrash {
    pub fn new(
        profile_id: &str,
        root: PathBuf,
        settings: TrashSettings,
        system: Arc<dyn TrashBin>,
    ) -> Self {
        Self {
            profile_id: profile_id.to_string(),
            root,
            sync_root: None,
            #[cfg(test)]
            pinned_volume: None,
            #[cfg(test)]
            rename_crosses_devices: false,
            settings,
            system,
            sequence: AtomicU64::new(0),
        }
    }

    /// Opens a profile's managed trash for reading and restoring; the
    /// CLI uses this without a daemon.
    pub fn open(profile_id: &str, root: PathBuf) -> Self {
        Self::new(
            profile_id,
            root,
            TrashSettings::default(),
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        )
    }

    /// Names the profile's local sync root, which is where every
    /// discard comes from. When that root sits on another volume, the
    /// trash keeps a location there too.
    pub fn with_sync_root(mut self, sync_root: Option<PathBuf>) -> Self {
        self.sync_root = sync_root;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The location on the sync root's volume, when that volume is not
    /// the runtime directory's. `None` while the sync root is absent
    /// (an unplugged drive) or unknown.
    pub fn volume_root(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if self.pinned_volume.is_some() {
            return self.pinned_volume.clone();
        }
        let sync_root = self.sync_root.as_ref()?;
        self.volume_location_for(sync_root)
    }

    fn volume_location_for(&self, path: &Path) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(pinned) = &self.pinned_volume {
            return Some(pinned.clone());
        }
        if vapor_platform::fs_ops::same_volume(path, &self.root).ok()? {
            return None;
        }
        let top = vapor_platform::fs_ops::volume_root_of(path).ok()?;
        Some(
            top.join(constants::runtime::VAPOR_DIRECTORY_NAME)
                .join(constants::runtime::TRASH_DIRECTORY_NAME)
                .join(&self.profile_id),
        )
    }

    /// Every location that may hold entries: the home one and, when
    /// present, the sync root's volume.
    fn locations(&self) -> Vec<PathBuf> {
        let mut locations = vec![self.root.clone()];
        if let Some(volume) = self.volume_root()
            && volume.is_dir()
        {
            locations.push(volume);
        }
        locations
    }

    /// The location a discard of `path` lands in: the home one when
    /// `path` is on its volume, else the location on `path`'s volume,
    /// created on first use. A volume that refuses the directory (read
    /// only) falls back to the home location, which then costs a copy.
    fn location_for(&self, path: &Path) -> PathBuf {
        match self.volume_location_for(path) {
            Some(volume) => match vapor_shared::runtime_paths::ensure_private_directory(&volume) {
                Ok(()) => volume,
                Err(error) => {
                    crate::logging::warning(
                        "The sync root's volume refused a trash directory; the item is copied to the runtime directory's trash instead",
                        &[
                            ("volume_trash", volume.display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    self.root.clone()
                }
            },
            None => self.root.clone(),
        }
    }

    pub fn settings(&self) -> TrashSettings {
        self.settings
    }

    pub fn configure(&mut self, settings: TrashSettings) {
        self.settings = settings;
    }

    /// This trash with other settings. The daemon shares one trash with
    /// its workers, so a change of the settings makes a new one.
    pub fn with_settings(&self, settings: TrashSettings) -> Self {
        Self {
            profile_id: self.profile_id.clone(),
            root: self.root.clone(),
            sync_root: self.sync_root.clone(),
            #[cfg(test)]
            pinned_volume: self.pinned_volume.clone(),
            #[cfg(test)]
            rename_crosses_devices: self.rename_crosses_devices,
            settings,
            system: self.system.clone(),
            sequence: AtomicU64::new(self.sequence.load(std::sync::atomic::Ordering::Relaxed)),
        }
    }

    /// Discards `path` (a file or a directory) for `reason`: into the
    /// user's trash when configured and possible, else into the
    /// managed trash, else (trash disabled) by unlinking. The path is
    /// gone from its original place when this returns `Ok`.
    pub fn discard(&self, path: &Path, reason: &str, now: SystemTime) -> io::Result<Disposition> {
        self.discard_as(path, reason, now, true)
    }

    /// [`Self::discard`] for the tick thread, which reads no file
    /// content: when only a copy can move the item (the volume of the
    /// item refuses a trash folder, so the item has to go to the
    /// runtime directory on another volume), nothing is moved and the
    /// error is `CrossesDevices`. The caller then runs
    /// [`Self::discard`] on a worker.
    pub fn discard_without_copy(
        &self,
        path: &Path,
        reason: &str,
        now: SystemTime,
    ) -> io::Result<Disposition> {
        self.discard_as(path, reason, now, false)
    }

    fn discard_as(
        &self,
        path: &Path,
        reason: &str,
        now: SystemTime,
        may_copy: bool,
    ) -> io::Result<Disposition> {
        if !self.settings.enabled {
            remove_any(path)?;
            return Ok(Disposition::Removed);
        }
        if self.settings.use_system {
            match self.system.trash(path) {
                Ok(landed) => return Ok(Disposition::System(landed)),
                Err(error) => crate::logging::debug(
                    "The user trash refused the item; keeping it in the managed trash",
                    &[
                        ("path", path.display().to_string()),
                        ("error", error.to_string()),
                    ],
                ),
            }
        }
        let metadata = fs::symlink_metadata(path)?;
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item".to_string());
        let (id, entry_dir) = self.new_entry_dir(&self.location_for(path), now)?;
        // The metadata goes in first. An entry cut off after this step
        // is listed by nothing and purged like any other; the reverse
        // order would leave a payload that nothing names.
        let entry = TrashEntry {
            id: id.clone(),
            profile_id: self.profile_id.clone(),
            original_path: path.to_path_buf(),
            file_name: file_name.clone(),
            kind: if metadata.is_dir() {
                "directory".to_string()
            } else {
                "file".to_string()
            },
            reason: reason.to_string(),
            discarded_at_ms: millis(now),
            size_bytes: if metadata.is_dir() {
                tree_size(path)
            } else {
                metadata.len()
            },
        };
        let moved = serde_json::to_vec_pretty(&entry)
            .map_err(io::Error::other)
            .and_then(|meta| {
                fs::write(
                    entry_dir.join(constants::runtime::TRASH_ENTRY_META_FILE_NAME),
                    meta,
                )
            })
            .map_err(MoveError::NothingMoved)
            .and_then(|()| {
                let destination = payload_path(&entry_dir, &file_name);
                match self.rename_into_entry(path, &destination) {
                    Err(error) if error.kind() == io::ErrorKind::CrossesDevices && may_copy => {
                        copy_then_remove(path, &destination)
                    }
                    result => result.map_err(MoveError::NothingMoved),
                }
            });
        match moved {
            Ok(()) => Ok(Disposition::Managed(id)),
            Err(MoveError::NothingMoved(error)) => {
                // The item is still whole at its path: the entry
                // describes nothing.
                let _ = fs::remove_dir_all(&entry_dir);
                Err(error)
            }
            Err(MoveError::CopiedNotRemoved(error)) => {
                // The entry holds the whole item, and a part of the
                // original may be gone. The entry stays: it is the only
                // complete copy. The discard still failed, so the
                // caller tries again and what is left goes to an entry
                // of its own.
                crate::logging::warning(
                    "Copied an item to the trash but could not remove the original; the copy stays in the trash",
                    &[
                        ("path", path.display().to_string()),
                        ("trash_entry", id),
                        ("error", error.to_string()),
                    ],
                );
                Err(error)
            }
        }
    }

    fn rename_into_entry(&self, path: &Path, destination: &Path) -> io::Result<()> {
        #[cfg(test)]
        if self.rename_crosses_devices {
            return Err(io::Error::new(
                io::ErrorKind::CrossesDevices,
                "the trash is on another volume",
            ));
        }
        fs::rename(path, destination)
    }

    /// Makes the directory of a new entry in `location` and returns its
    /// id. The id is the time and a counter that starts at zero in each
    /// process, so two processes (or one that restarts within a
    /// millisecond's reach) can draw the same one: the directory is
    /// made with a call that fails when it exists, and the next id is
    /// tried.
    fn new_entry_dir(&self, location: &Path, now: SystemTime) -> io::Result<(String, PathBuf)> {
        // Made when it is missing. A location that is there keeps the
        // mode it has: one that its owner made read-only refuses the
        // entry, and the discard fails with the item still in place.
        if !location.is_dir() {
            vapor_shared::runtime_paths::ensure_private_directory(location)?;
        }
        loop {
            let id = format!(
                "{}-{:04}",
                millis(now),
                self.sequence.fetch_add(1, Ordering::Relaxed) % 10_000
            );
            let entry_dir = location.join(&id);
            match fs::create_dir(&entry_dir) {
                Ok(()) => {
                    vapor_shared::runtime_paths::ensure_private_directory(&entry_dir)?;
                    return Ok((id, entry_dir));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Every entry in every location, newest first. An entry directory
    /// without a readable `meta.json`, or without its payload, is not
    /// listed.
    pub fn list(&self) -> Vec<TrashEntry> {
        let mut found: Vec<TrashEntry> = self
            .locations()
            .iter()
            .filter_map(|location| fs::read_dir(location).ok())
            .flat_map(|entries| entries.flatten().collect::<Vec<_>>())
            .filter_map(|entry| self.read_entry(&entry.path()))
            .collect();
        found.sort_by(|a, b| {
            b.discarded_at_ms
                .cmp(&a.discarded_at_ms)
                .then(b.id.cmp(&a.id))
        });
        found
    }

    pub fn entry(&self, id: &str) -> Option<TrashEntry> {
        self.entry_dir(id)
            .and_then(|entry_dir| self.read_entry(&entry_dir))
    }

    /// The directory holding entry `id`, in whichever location has it.
    fn entry_dir(&self, id: &str) -> Option<PathBuf> {
        if id.is_empty() || id.contains(std::path::MAIN_SEPARATOR) || id.contains('/') {
            return None;
        }
        self.locations()
            .into_iter()
            .map(|location| location.join(id))
            .find(|entry_dir| {
                entry_dir
                    .join(constants::runtime::TRASH_ENTRY_META_FILE_NAME)
                    .is_file()
            })
    }

    fn read_entry(&self, entry_dir: &Path) -> Option<TrashEntry> {
        read_entry(entry_dir).filter(|entry| has_payload(entry_dir, entry))
    }

    /// Puts an entry back at its original path, or next to it under
    /// `<stem>~restored-<id><ext>` when that path is taken. Returns
    /// where it landed.
    pub fn restore(&self, id: &str) -> io::Result<PathBuf> {
        let entry_dir = self.entry_dir(id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no trash entry {id}"))
        })?;
        let entry = self.read_entry(&entry_dir).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no trash entry {id}"))
        })?;
        let payload = payload_path(&entry_dir, &entry.file_name);
        if fs::symlink_metadata(&payload).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("trash entry {id} has no payload"),
            ));
        }
        let mut target = entry.original_path.clone();
        if fs::symlink_metadata(&target).is_ok() {
            target = restored_beside(&entry.original_path, &entry.id);
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(&payload, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                copy_any(&payload, &target)?;
                remove_any(&payload)?;
            }
            Err(error) => return Err(error),
        }
        let _ = fs::remove_dir_all(&entry_dir);
        Ok(target)
    }

    /// Removes the entries discarded before `now - retention`. Returns
    /// how many were purged.
    pub fn purge_expired(&self, now: SystemTime) -> usize {
        self.purge_job(now)()
    }

    /// [`Self::purge_expired`] as a job that owns what it needs, so the
    /// daemon runs it on a worker: removing a discarded tree is disk
    /// work that grows with the tree.
    pub fn purge_job(&self, now: SystemTime) -> impl FnOnce() -> usize + Send + 'static {
        let locations = self.locations();
        let retention = self.settings.retention;
        move || {
            locations
                .iter()
                .map(|location| purge_location(location, retention, now))
                .sum()
        }
    }

    /// Removes every entry. Returns how many were removed.
    pub fn empty(&self) -> usize {
        self.list()
            .iter()
            .filter(|entry| self.remove_entry(&entry.id))
            .count()
    }

    fn remove_entry(&self, id: &str) -> bool {
        self.entry_dir(id)
            .is_some_and(|entry_dir| fs::remove_dir_all(entry_dir).is_ok())
    }
}

/// A purge of the home trash of every profile that is not in
/// `running`, as a job that owns what it needs. A profile that is
/// disabled, or that left `vapor.json`, has no runtime to purge its
/// trash; without this its discarded files would stay for ever. The
/// retention is the same one, counted from each discard.
pub fn purge_other_profiles_job(
    trash_root: PathBuf,
    running: Vec<String>,
    retention: Duration,
    now: SystemTime,
) -> impl FnOnce() -> usize + Send + 'static {
    move || {
        let Ok(profiles) = fs::read_dir(&trash_root) else {
            return 0;
        };
        profiles
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| !running.iter().any(|id| id == name))
            })
            .map(|entry| purge_location(&entry.path(), retention, now))
            .sum()
    }
}

fn read_entry(entry_dir: &Path) -> Option<TrashEntry> {
    let meta = fs::read(entry_dir.join(constants::runtime::TRASH_ENTRY_META_FILE_NAME)).ok()?;
    serde_json::from_slice(&meta).ok()
}

fn has_payload(entry_dir: &Path, entry: &TrashEntry) -> bool {
    fs::symlink_metadata(payload_path(entry_dir, &entry.file_name)).is_ok()
}

/// Where an entry keeps the item it holds: under the item's own name,
/// next to the metadata file. An item that has the metadata file's name
/// (in any spelling the volume folds onto it) would replace that file,
/// so it is kept under a name that no metadata file has.
fn payload_path(entry_dir: &Path, file_name: &str) -> PathBuf {
    let meta = constants::runtime::TRASH_ENTRY_META_FILE_NAME;
    if vapor_shared::paths::fold_name(file_name) == vapor_shared::paths::fold_name(meta) {
        entry_dir.join(format!("{file_name}~payload"))
    } else {
        entry_dir.join(file_name)
    }
}

/// How a move into the trash failed.
enum MoveError {
    /// The item is whole at its original path.
    NothingMoved(io::Error),
    /// The entry holds a complete copy, and the original could not be
    /// removed, or only a part of it was.
    CopiedNotRemoved(io::Error),
}

/// Moves `path` to `destination` on another volume: a copy, then the
/// removal of the original. A copy that fails leaves the original
/// untouched.
fn copy_then_remove(path: &Path, destination: &Path) -> Result<(), MoveError> {
    copy_any(path, destination).map_err(MoveError::NothingMoved)?;
    remove_any(path).map_err(MoveError::CopiedNotRemoved)
}

/// Removes what has expired in one trash location. An entry expires by
/// the time in its `meta.json`. A directory with no readable metadata
/// expires by its own modification time, so it does not stay for ever.
/// An entry that never got its payload goes after a short wait.
fn purge_location(location: &Path, retention: Duration, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(location) else {
        return 0;
    };
    let age = |since: SystemTime| now.duration_since(since).unwrap_or_default();
    let without_payload = Duration::from_secs(constants::trash::ENTRY_WITHOUT_PAYLOAD_SECONDS);
    let mut purged = 0;
    for entry_dir in entries.flatten().map(|entry| entry.path()) {
        let Ok(metadata) = fs::symlink_metadata(&entry_dir) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let expired = match read_entry(&entry_dir) {
            Some(entry) if has_payload(&entry_dir, &entry) => age(entry.discarded_at()) > retention,
            Some(entry) => age(entry.discarded_at()) > without_payload.min(retention),
            None => metadata
                .modified()
                .is_ok_and(|modified| age(modified) > retention),
        };
        if expired && fs::remove_dir_all(&entry_dir).is_ok() {
            purged += 1;
        }
    }
    purged
}

fn restored_beside(original: &Path, id: &str) -> PathBuf {
    let file_name = original
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_string(), format!(".{extension}")),
        _ => (file_name, String::new()),
    };
    original.with_file_name(format!("{stem}~restored-{id}{extension}"))
}

fn remove_any(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn copy_any(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_any(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    } else if metadata.is_file() {
        fs::copy(source, destination).map(|_| ())
    } else {
        // A symlink or special file is outside the sync contract and
        // carries no payload worth keeping.
        Ok(())
    }
}

fn tree_size(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => tree_size(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn managed(temp: &TempDir) -> LocalTrash {
        LocalTrash::new(
            "default",
            temp.path().join("trash/default"),
            TrashSettings {
                enabled: true,
                retention: Duration::from_secs(3_600),
                use_system: false,
            },
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        )
    }

    #[test]
    fn a_discard_that_needs_a_copy_moves_nothing_unless_a_copy_is_allowed() {
        let temp = TempDir::new().expect("temp");
        let mut trash = managed(&temp);
        trash.rename_crosses_devices = true;
        let file = temp.path().join("on-another-volume.txt");
        fs::write(&file, b"payload").expect("seed");

        // The tick thread asks: no copy, no entry, the file stays.
        let refused = trash
            .discard_without_copy(&file, "cloud-delete", at(100))
            .expect_err("only a copy can move it");
        assert_eq!(refused.kind(), io::ErrorKind::CrossesDevices);
        assert_eq!(fs::read(&file).expect("the file stays"), b"payload");
        assert!(
            trash.list().is_empty(),
            "no entry for an item that did not move"
        );

        // A worker asks: the copy is made and the original goes.
        trash
            .discard(&file, "cloud-delete", at(100))
            .expect("the copy moves it");
        assert!(!file.exists());
        assert_eq!(trash.list().len(), 1);
    }

    #[test]
    fn a_discarded_file_is_listed_and_restored_to_its_original_path() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        let file = temp.path().join("root/notes.txt");
        fs::create_dir_all(file.parent().unwrap()).expect("root");
        fs::write(&file, b"keep me").expect("seed");

        let disposition = trash
            .discard(&file, constants::trash::REASON_CLOUD_DELETE, at(100))
            .expect("discard");
        let Disposition::Managed(id) = disposition else {
            panic!("expected the managed trash, got {disposition:?}");
        };
        assert!(!file.exists());
        let listed = trash.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].original_path, file);
        assert_eq!(listed[0].kind, "file");
        assert_eq!(listed[0].size_bytes, 7);
        assert_eq!(listed[0].reason, constants::trash::REASON_CLOUD_DELETE);

        let landed = trash.restore(&id).expect("restore");
        assert_eq!(landed, file);
        assert_eq!(fs::read(&file).expect("restored"), b"keep me");
        assert!(trash.list().is_empty(), "a restored entry leaves the trash");
    }

    #[test]
    fn two_discards_with_one_id_never_share_an_entry() {
        let temp = TempDir::new().expect("temp");
        // Two processes over one trash: each counts from zero, and the
        // clock gives both the same millisecond.
        let first = managed(&temp);
        let second = managed(&temp);
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("a")).expect("root");
        fs::create_dir_all(root.join("b")).expect("root");
        fs::write(root.join("a/notes.txt"), b"from a").expect("seed");
        fs::write(root.join("b/notes.txt"), b"from b").expect("seed");

        let Disposition::Managed(one) = first
            .discard(&root.join("a/notes.txt"), "cloud-delete", at(100))
            .expect("discard")
        else {
            panic!("managed");
        };
        let Disposition::Managed(two) = second
            .discard(&root.join("b/notes.txt"), "cloud-delete", at(100))
            .expect("discard")
        else {
            panic!("managed");
        };
        assert_ne!(one, two, "the second discard took the next id");
        let listed = first.list();
        assert_eq!(listed.len(), 2, "{listed:?}");
        first.restore(&one).expect("restore");
        first.restore(&two).expect("restore");
        assert_eq!(fs::read(root.join("a/notes.txt")).expect("a"), b"from a");
        assert_eq!(fs::read(root.join("b/notes.txt")).expect("b"), b"from b");
    }

    #[test]
    fn the_trash_of_a_profile_that_does_not_run_is_purged_by_the_same_retention() {
        let temp = TempDir::new().expect("temp");
        let trash_root = temp.path().join("trash");
        let root = temp.path().join("root");
        fs::create_dir_all(&root).expect("root");
        let retention = Duration::from_secs(3_600);
        let discard = |profile: &str, name: &str, at_seconds: u64| {
            let file = root.join(name);
            fs::write(&file, b"x").expect("seed");
            LocalTrash::new(
                profile,
                trash_root.join(profile),
                TrashSettings {
                    enabled: true,
                    retention,
                    use_system: false,
                },
                Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
            )
            .discard(&file, "cloud-delete", at(at_seconds))
            .expect("discard");
        };
        discard("running", "a.txt", 100);
        discard("removed", "old.txt", 100);
        discard("removed", "recent.txt", 100 + 3_000);

        let purge = purge_other_profiles_job(
            trash_root.clone(),
            vec!["running".to_string()],
            retention,
            at(100 + 3_601),
        );
        assert_eq!(purge(), 1, "only what is past the retention goes");
        let left = |profile: &str| {
            fs::read_dir(trash_root.join(profile))
                .map(|entries| entries.count())
                .unwrap_or(0)
        };
        assert_eq!(left("removed"), 1, "the recent entry stays");
        assert_eq!(left("running"), 1, "a running profile purges its own trash");
    }

    #[test]
    fn a_file_named_like_the_metadata_file_is_listed_and_restored() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        let root = temp.path().join("root");
        fs::create_dir_all(&root).expect("root");
        for name in ["meta.json", "META.JSON"] {
            let file = root.join(name);
            fs::write(&file, b"the user's own file").expect("seed");
            let Disposition::Managed(id) = trash
                .discard(&file, "cloud-delete", at(100))
                .expect("discard")
            else {
                panic!("the managed trash holds it");
            };
            let listed = trash.list();
            assert_eq!(listed.len(), 1, "{name}: the entry reads as an entry");
            assert_eq!(listed[0].file_name, name);
            assert_eq!(trash.restore(&id).expect("restore"), file);
            assert_eq!(fs::read(&file).expect("read"), b"the user's own file");
            fs::remove_file(&file).expect("reset");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_whose_original_cannot_be_removed_keeps_the_copy() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().expect("temp");
        let folder = temp.path().join("root/album");
        fs::create_dir_all(folder.join("locked")).expect("dirs");
        fs::write(folder.join("a.jpg"), b"a").expect("seed");
        fs::write(folder.join("locked/b.jpg"), b"b").expect("seed");
        // The folder can be read, so the copy is whole. Nothing can be
        // removed from `locked`, so the removal stops part way.
        fs::set_permissions(folder.join("locked"), fs::Permissions::from_mode(0o555))
            .expect("lock");
        let destination = temp.path().join("entry/album");
        fs::create_dir_all(destination.parent().unwrap()).expect("entry");

        let moved = copy_then_remove(&folder, &destination);
        fs::set_permissions(folder.join("locked"), fs::Permissions::from_mode(0o755)).ok();
        if moved.is_ok() {
            // Running as a user that permissions do not stop.
            return;
        }
        assert!(matches!(moved, Err(MoveError::CopiedNotRemoved(_))));
        assert_eq!(fs::read(destination.join("a.jpg")).expect("copy"), b"a");
        assert_eq!(
            fs::read(destination.join("locked/b.jpg")).expect("copy"),
            b"b",
            "the copy is whole, whatever is left of the original"
        );
    }

    #[test]
    fn a_discard_that_cannot_move_the_item_leaves_no_entry() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        // Nothing is at the path any more when the move runs.
        let gone = temp.path().join("root/gone.txt");
        assert!(trash.discard(&gone, "cloud-delete", at(100)).is_err());
        assert!(trash.list().is_empty());
        let leftovers = fs::read_dir(trash.root())
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "no entry directory stays behind");
    }

    #[test]
    fn what_a_cut_off_discard_leaves_is_not_listed_and_does_not_stay_for_ever() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        let file = temp.path().join("root/kept.txt");
        fs::create_dir_all(file.parent().unwrap()).expect("root");
        fs::write(&file, b"kept").expect("seed");
        let Disposition::Managed(kept) = trash
            .discard(&file, "cloud-delete", at(10_000))
            .expect("discard")
        else {
            panic!("managed");
        };

        // The metadata of a discard that was cut off before its move.
        let cut_off = trash.root().join("10000000-9999");
        fs::create_dir_all(&cut_off).expect("entry");
        let meta = TrashEntry {
            id: "10000000-9999".to_string(),
            profile_id: "default".to_string(),
            original_path: temp.path().join("root/never-moved.txt"),
            file_name: "never-moved.txt".to_string(),
            kind: "file".to_string(),
            reason: "cloud-delete".to_string(),
            discarded_at_ms: 10_000_000,
            size_bytes: 3,
        };
        fs::write(
            cut_off.join(constants::runtime::TRASH_ENTRY_META_FILE_NAME),
            serde_json::to_vec(&meta).expect("meta"),
        )
        .expect("write meta");

        let ids: Vec<String> = trash.list().into_iter().map(|entry| entry.id).collect();
        assert_eq!(
            ids,
            vec![kept.clone()],
            "an entry with no payload is not listed"
        );
        // A discard in flight looks the same for a moment: it is left
        // alone until it is old enough.
        assert_eq!(trash.purge_expired(at(10_000 + 60)), 0);
        assert!(cut_off.exists());
        let later = at(10_000 + constants::trash::ENTRY_WITHOUT_PAYLOAD_SECONDS + 1);
        assert_eq!(trash.purge_expired(later), 2, "retention is one hour here");
        assert!(!cut_off.exists());
    }

    #[test]
    fn the_sync_roots_volume_holds_its_own_location_and_both_are_listed_purged_and_emptied() {
        let temp = TempDir::new().expect("temp");
        let mut trash = managed(&temp);
        // A second location stands in for the sync root's volume.
        let volume = temp.path().join("volume/.vapor/trash/default");
        trash.pinned_volume = Some(volume.clone());
        let on_volume = temp.path().join("volume/Vapor/a.txt");
        fs::create_dir_all(on_volume.parent().unwrap()).expect("root");
        fs::write(&on_volume, b"on the volume").expect("seed");

        let Disposition::Managed(id) = trash
            .discard(&on_volume, constants::trash::REASON_CLOUD_DELETE, at(100))
            .expect("discard")
        else {
            panic!("expected the managed trash");
        };
        assert!(
            volume.join(&id).join("a.txt").is_file(),
            "the entry lands on the sync root's volume"
        );
        assert!(
            !trash.root().join(&id).exists(),
            "nothing is written to the home location"
        );

        // An older entry in the home location, as if from before the
        // volume was in use.
        let home_entry = trash.root().join("1000-0000");
        fs::create_dir_all(&home_entry).expect("home entry");
        fs::write(home_entry.join("old.txt"), b"old").expect("payload");
        let meta = TrashEntry {
            id: "1000-0000".into(),
            profile_id: "default".into(),
            original_path: temp.path().join("volume/Vapor/old.txt"),
            file_name: "old.txt".into(),
            kind: "file".into(),
            reason: constants::trash::REASON_CLOUD_DELETE.into(),
            discarded_at_ms: 1_000,
            size_bytes: 3,
        };
        fs::write(
            home_entry.join(constants::runtime::TRASH_ENTRY_META_FILE_NAME),
            serde_json::to_vec(&meta).expect("meta"),
        )
        .expect("write meta");

        let ids: Vec<String> = trash.list().into_iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![id.clone(), "1000-0000".to_string()]);
        assert!(trash.entry("1000-0000").is_some() && trash.entry(&id).is_some());

        let landed = trash.restore(&id).expect("restore from the volume");
        assert_eq!(landed, on_volume);
        assert_eq!(fs::read(&on_volume).expect("restored"), b"on the volume");

        // The old home entry is past retention (3_600 s); the fresh
        // one on the volume is not.
        fs::write(&on_volume, b"again").expect("seed again");
        trash
            .discard(&on_volume, constants::trash::REASON_CLOUD_DELETE, at(5_000))
            .expect("discard again");
        assert_eq!(trash.purge_expired(at(5_000)), 1);
        assert_eq!(trash.list().len(), 1);
        assert_eq!(trash.empty(), 1);
        assert!(trash.list().is_empty());
    }

    #[test]
    fn restoring_onto_an_occupied_path_lands_beside_it() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        let file = temp.path().join("root/report.pdf");
        fs::create_dir_all(file.parent().unwrap()).expect("root");
        fs::write(&file, b"old").expect("seed");
        let Disposition::Managed(id) = trash
            .discard(&file, constants::trash::REASON_CLOUD_DELETE, at(100))
            .expect("discard")
        else {
            panic!("managed");
        };
        fs::write(&file, b"new").expect("a new file took the name");
        let landed = trash.restore(&id).expect("restore");
        assert_eq!(
            landed,
            file.with_file_name(format!("report~restored-{id}.pdf"))
        );
        assert_eq!(fs::read(&landed).expect("restored"), b"old");
        assert_eq!(fs::read(&file).expect("kept"), b"new");
    }

    #[test]
    fn a_directory_is_discarded_whole_and_purged_after_retention() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        let dir = temp.path().join("root/project");
        fs::create_dir_all(dir.join("src")).expect("dir");
        fs::write(dir.join("src/main.rs"), b"fn main() {}").expect("file");
        let Disposition::Managed(id) = trash
            .discard(&dir, constants::trash::REASON_MIRROR_DELETE, at(100))
            .expect("discard")
        else {
            panic!("managed");
        };
        assert!(!dir.exists());
        let entry = trash.entry(&id).expect("entry");
        assert_eq!(entry.kind, "directory");
        assert_eq!(entry.size_bytes, 12);
        assert!(trash.root().join(&id).join("project/src/main.rs").is_file());

        assert_eq!(trash.purge_expired(at(100 + 3_599)), 0, "inside retention");
        assert_eq!(trash.purge_expired(at(100 + 3_601)), 1, "past retention");
        assert!(trash.list().is_empty());
    }

    #[test]
    fn the_user_trash_is_tried_first_and_the_managed_trash_catches_its_refusal() {
        let temp = TempDir::new().expect("temp");
        let bin = Arc::new(vapor_platform::InMemoryTrashBin::new(
            temp.path().join("bin"),
        ));
        let settings = TrashSettings {
            enabled: true,
            retention: Duration::from_secs(60),
            use_system: true,
        };
        let trash = LocalTrash::new("default", temp.path().join("trash"), settings, bin.clone());
        let file = temp.path().join("a.txt");
        fs::write(&file, b"a").expect("seed");
        let disposition = trash.discard(&file, "test", at(1)).expect("discard");
        assert!(
            matches!(disposition, Disposition::System(_)),
            "{disposition:?}"
        );
        assert_eq!(bin.moved().len(), 1);
        assert!(trash.list().is_empty());

        let refusing = LocalTrash::new(
            "default",
            temp.path().join("trash"),
            settings,
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        );
        let other = temp.path().join("b.txt");
        fs::write(&other, b"b").expect("seed");
        let disposition = refusing.discard(&other, "test", at(2)).expect("discard");
        assert!(
            matches!(disposition, Disposition::Managed(_)),
            "{disposition:?}"
        );
        assert_eq!(refusing.list().len(), 1);
    }

    #[test]
    fn a_disabled_trash_unlinks() {
        let temp = TempDir::new().expect("temp");
        let mut trash = managed(&temp);
        trash.configure(TrashSettings {
            enabled: false,
            ..TrashSettings::default()
        });
        let file = temp.path().join("gone.txt");
        fs::write(&file, b"x").expect("seed");
        assert_eq!(
            trash.discard(&file, "test", at(1)).expect("discard"),
            Disposition::Removed
        );
        assert!(!file.exists());
        assert!(trash.list().is_empty());
    }

    #[test]
    fn entry_ids_never_escape_the_trash_root() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        assert!(trash.entry("../../etc").is_none());
        assert!(trash.restore("../x").is_err());
    }
}
