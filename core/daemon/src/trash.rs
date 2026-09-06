//! The managed trash: where a file goes when Vapor itself removes it
//! from this device, so a wrong deletion can be undone here without
//! the cloud's own trash (which the filesystem provider does not have).
//!
//! Layout: `vapor_dir/trash/<profile>/<entry id>/` holds the payload
//! under its original file name and a `meta.json` next to it with the
//! original path, when and why it was discarded. Entries older than
//! the retention window are purged by the daemon. With
//! `trash.useSystemTrash` the user's own trash is tried first (the
//! Finder's Trash on macOS) and the managed trash is the fallback, so a
//! discard never silently degrades to an unlink.
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
    pub use_system_trash: bool,
}

impl Default for TrashSettings {
    fn default() -> Self {
        Self {
            enabled: constants::trash::DEFAULT_ENABLED,
            retention: Duration::from_secs(
                u64::from(constants::trash::DEFAULT_RETENTION_DAYS) * 86_400,
            ),
            use_system_trash: constants::trash::DEFAULT_USE_SYSTEM_TRASH,
        }
    }
}

impl TrashSettings {
    pub fn resolve(config: &vapor_shared::config::VaporConfig) -> Self {
        let configured = &config.trash;
        Self {
            enabled: configured.enabled,
            retention: Duration::from_secs(u64::from(configured.retention_days) * 86_400),
            use_system_trash: configured.use_system_trash,
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
    root: PathBuf,
    settings: TrashSettings,
    system: Arc<dyn TrashBin>,
    sequence: AtomicU64,
}

impl std::fmt::Debug for LocalTrash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalTrash")
            .field("profile_id", &self.profile_id)
            .field("root", &self.root)
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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings(&self) -> TrashSettings {
        self.settings
    }

    pub fn configure(&mut self, settings: TrashSettings) {
        self.settings = settings;
    }

    /// Discards `path` (a file or a directory) for `reason`: into the
    /// user's trash when configured and possible, else into the
    /// managed trash, else (trash disabled) by unlinking. The path is
    /// gone from its original place when this returns `Ok`.
    pub fn discard(&self, path: &Path, reason: &str, now: SystemTime) -> io::Result<Disposition> {
        if !self.settings.enabled {
            remove_any(path)?;
            return Ok(Disposition::Removed);
        }
        if self.settings.use_system_trash {
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
        let id = format!(
            "{}-{:04}",
            millis(now),
            self.sequence.fetch_add(1, Ordering::Relaxed) % 10_000
        );
        let entry_dir = self.root.join(&id);
        vapor_shared::runtime_paths::ensure_private_directory(&entry_dir)?;
        let destination = entry_dir.join(&file_name);
        match fs::rename(path, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                // Another volume: copy, then remove. A copy that fails
                // leaves the original untouched.
                copy_any(path, &destination)?;
                remove_any(path)?;
            }
            Err(error) => {
                let _ = fs::remove_dir(&entry_dir);
                return Err(error);
            }
        }
        let entry = TrashEntry {
            id: id.clone(),
            profile_id: self.profile_id.clone(),
            original_path: path.to_path_buf(),
            file_name,
            kind: if metadata.is_dir() {
                "directory".to_string()
            } else {
                "file".to_string()
            },
            reason: reason.to_string(),
            discarded_at_ms: millis(now),
            size_bytes: if metadata.is_dir() {
                tree_size(&destination)
            } else {
                metadata.len()
            },
        };
        let meta = serde_json::to_vec_pretty(&entry).map_err(io::Error::other)?;
        fs::write(
            entry_dir.join(constants::runtime::TRASH_ENTRY_META_FILE_NAME),
            meta,
        )?;
        Ok(Disposition::Managed(id))
    }

    /// Every entry, newest first. An entry directory without a readable
    /// `meta.json` is skipped, never deleted.
    pub fn list(&self) -> Vec<TrashEntry> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut found: Vec<TrashEntry> = entries
            .flatten()
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
        if id.contains(std::path::MAIN_SEPARATOR) || id.contains('/') {
            return None;
        }
        self.read_entry(&self.root.join(id))
    }

    fn read_entry(&self, entry_dir: &Path) -> Option<TrashEntry> {
        let meta = fs::read(entry_dir.join(constants::runtime::TRASH_ENTRY_META_FILE_NAME)).ok()?;
        serde_json::from_slice(&meta).ok()
    }

    /// Puts an entry back at its original path, or next to it under
    /// `<stem>~restored-<id><ext>` when that path is taken. Returns
    /// where it landed.
    pub fn restore(&self, id: &str) -> io::Result<PathBuf> {
        let entry = self.entry(id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no trash entry {id}"))
        })?;
        let entry_dir = self.root.join(&entry.id);
        let payload = entry_dir.join(&entry.file_name);
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
        let cutoff = now.checked_sub(self.settings.retention);
        let mut purged = 0;
        for entry in self.list() {
            let expired = cutoff.is_some_and(|cutoff| entry.discarded_at() < cutoff);
            if expired && fs::remove_dir_all(self.root.join(&entry.id)).is_ok() {
                purged += 1;
            }
        }
        purged
    }

    /// Removes every entry. Returns how many were removed.
    pub fn empty(&self) -> usize {
        let mut removed = 0;
        for entry in self.list() {
            if fs::remove_dir_all(self.root.join(&entry.id)).is_ok() {
                removed += 1;
            }
        }
        removed
    }
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
                use_system_trash: false,
            },
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        )
    }

    #[test]
    fn a_discarded_file_is_listed_and_restored_to_its_original_path() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        let file = temp.path().join("root/notes.txt");
        fs::create_dir_all(file.parent().unwrap()).expect("root");
        fs::write(&file, b"keep me").expect("seed");

        let disposition = trash
            .discard(&file, constants::trash::REASON_CLOUD_DELETION, at(100))
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
        assert_eq!(listed[0].reason, constants::trash::REASON_CLOUD_DELETION);

        let landed = trash.restore(&id).expect("restore");
        assert_eq!(landed, file);
        assert_eq!(fs::read(&file).expect("restored"), b"keep me");
        assert!(trash.list().is_empty(), "a restored entry leaves the trash");
    }

    #[test]
    fn restoring_onto_an_occupied_path_lands_beside_it() {
        let temp = TempDir::new().expect("temp");
        let trash = managed(&temp);
        let file = temp.path().join("root/report.pdf");
        fs::create_dir_all(file.parent().unwrap()).expect("root");
        fs::write(&file, b"old").expect("seed");
        let Disposition::Managed(id) = trash
            .discard(&file, constants::trash::REASON_CLOUD_DELETION, at(100))
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
            .discard(&dir, constants::trash::REASON_MIRROR_REMOVAL, at(100))
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
            use_system_trash: true,
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
