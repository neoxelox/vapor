//! Keep-both conflicts: the copy names, and the question a conflict
//! puts to the user.
//!
//! Copy template per `docs/architecture/data-flow.md §Conflict
//! handling`: `{stem}~conflict-{device_id}-{timestamp_ms}{ext}`, where
//! the split happens at the last `.` of the basename. When the derived
//! path already exists, `-{seq}` (starting at 2) appends before the
//! extension until a free path is found. Every input is derivable from
//! durable state (device id from `vapor.json`, timestamp from the
//! intent's event time), so the same conflict replayed on the same
//! device lands on the same conflict path across restarts.
//!
//! The cloud settles who keeps the name: the first edit to reach it
//! stays under the file's name, and each device whose edit lost the
//! race moves it to a copy named after that device. Three devices
//! editing one file leave the file plus two copies. A [`Conflict`] is
//! all the versions of one file; it is put to the user as one
//! `conflict` decision (`vapor decisions`), answered with [`keep`], and
//! closes when no copy is left, however the copies went.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use vapor_shared::constants;

use crate::state_db::DecisionOption;

/// The marker every keep-both conflict copy carries in its file name.
/// The daemon recognizes copies by it through [`parse_conflict_copy_name`].
pub const CONFLICT_MARKER: &str = constants::conflict::COPY_MARKER;

/// Derives the keep-both conflict-copy path for `original`.
/// `path_exists` abstracts the existence probe so derivation stays
/// pure and testable.
pub fn conflict_copy_path(
    original: &Path,
    device_id: &str,
    timestamp_ms: u64,
    path_exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let file_name = original
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, extension) = match file_name.rfind('.') {
        // A leading dot (hidden file) is part of the stem, not an
        // extension separator.
        Some(0) | None => (file_name.as_str(), ""),
        Some(index) => file_name.split_at(index),
    };

    let base = format!("{stem}{CONFLICT_MARKER}{device_id}-{timestamp_ms}");
    let candidate = original.with_file_name(format!("{base}{extension}"));
    if !path_exists(&candidate) {
        return candidate;
    }
    let mut sequence: u64 = 2;
    loop {
        let candidate = original.with_file_name(format!("{base}-{sequence}{extension}"));
        if !path_exists(&candidate) {
            return candidate;
        }
        sequence += 1;
    }
}

/// A conflict-copy file name decomposed back into its parts — the
/// inverse of [`conflict_copy_path`] for names that machine derived.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictCopyName {
    /// The canonical file name the copy diverged from
    /// (`report~conflict-dev-17….md` → `report.md`).
    pub canonical_file_name: String,
    /// The device whose divergent version the copy preserves.
    pub device_id: String,
    /// When the divergence was observed (the conflicting intent's
    /// event time, epoch milliseconds).
    pub timestamp_ms: u64,
    /// Collision sequence (`-2`, `-3`, …); `None` for the first copy.
    pub sequence: Option<u64>,
}

/// Epoch-millisecond timestamps are 12+ digits for any date past 2001.
/// Requiring that length is what disambiguates the timestamp segment
/// from device ids that end in digits (`mbp2`) and from the small
/// collision sequence.
const TIMESTAMP_MIN_DIGITS: usize = 12;

/// Parses a file name produced by [`conflict_copy_path`]. Returns
/// `None` for anything that does not match the machine-generated
/// grammar (`{stem}~conflict-{device_id}-{timestamp_ms}[-{seq}]{ext}`),
/// so user files that merely contain the marker text do not
/// false-positive.
pub fn parse_conflict_copy_name(file_name: &str) -> Option<ConflictCopyName> {
    let marker_index = file_name.find(CONFLICT_MARKER)?;
    let stem = &file_name[..marker_index];
    if stem.is_empty() {
        return None;
    }
    let rest = &file_name[marker_index + CONFLICT_MARKER.len()..];
    // The generator splits the original name at its last dot, so the
    // copy's extension is whatever follows the last dot after the
    // marker (device ids are hostname-derived slugs; they never
    // contain dots).
    let (meta, extension) = match rest.rfind('.') {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    };

    let segments: Vec<&str> = meta.split('-').collect();
    let timestamp_index = segments
        .iter()
        .rposition(|s| s.len() >= TIMESTAMP_MIN_DIGITS && s.chars().all(|c| c.is_ascii_digit()))?;
    if timestamp_index == 0 {
        return None; // no device id segment before the timestamp
    }
    let timestamp_ms = segments[timestamp_index].parse::<u64>().ok()?;
    let device_id = segments[..timestamp_index].join("-");
    let sequence = match &segments[timestamp_index + 1..] {
        [] => None,
        [seq] => Some(seq.parse::<u64>().ok()?),
        _ => return None,
    };

    Some(ConflictCopyName {
        canonical_file_name: format!("{stem}{extension}"),
        device_id,
        timestamp_ms,
        sequence,
    })
}

/// The original a conflict copy belongs to.
pub fn original_path(copy_path: &Path) -> Option<PathBuf> {
    let name = copy_path.file_name()?.to_str()?;
    let parsed = parse_conflict_copy_name(name)?;
    Some(copy_path.with_file_name(parsed.canonical_file_name))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Every version of one conflicted file, as found on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    /// The path every version competes for.
    pub original: PathBuf,
    /// The local file holding a spelling of `original` that this
    /// filesystem folds onto it (`readme.md` for a cloud `Readme.md`).
    /// The copies then stand in for other cloud files rather than for
    /// edits of this one, so only this file can be kept.
    pub clash: Option<PathBuf>,
    /// The file under the name first, when there is one, then the
    /// copies, oldest first.
    pub versions: Vec<PathBuf>,
}

impl Conflict {
    /// The conflict over `original`, or `None` while no copy of it
    /// exists. A folder is never in conflict: a copy next to one is the
    /// `keep-both` answer to a file/folder clash, not a question. A
    /// folder that cannot be listed (an unplugged drive, a permission
    /// error) is an error, never "no copy left".
    pub fn find(original: &Path) -> io::Result<Option<Self>> {
        if original.is_dir() {
            return Ok(None);
        }
        let (Some(parent), Some(wanted)) = (
            original.parent(),
            original.file_name().and_then(|name| name.to_str()),
        ) else {
            return Ok(None);
        };
        let entries = match fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut copies: Vec<(u64, u64, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let parsed = parse_conflict_copy_name(name.to_str()?)?;
                let path = entry.path();
                (parsed.canonical_file_name == wanted && path.is_file())
                    .then(|| (parsed.timestamp_ms, parsed.sequence.unwrap_or(1), path))
            })
            .collect();
        if copies.is_empty() {
            return Ok(None);
        }
        copies.sort();
        let clash = crate::name_collision::colliding_local_path(original);
        let under_the_name = match &clash {
            Some(sibling) => Some(sibling.clone()),
            None => original.is_file().then(|| original.to_path_buf()),
        };
        let versions = under_the_name
            .into_iter()
            .chain(copies.into_iter().map(|(_, _, path)| path))
            .collect();
        Ok(Some(Self {
            original: original.to_path_buf(),
            clash,
            versions,
        }))
    }

    /// The versions the user may keep.
    pub fn keepable(&self) -> &[PathBuf] {
        match &self.clash {
            Some(_) => &self.versions[..1],
            None => &self.versions,
        }
    }

    /// Where the kept version ends up.
    fn destination(&self) -> &Path {
        self.clash.as_deref().unwrap_or(&self.original)
    }

    fn under_the_name(&self) -> Option<&Path> {
        self.versions
            .first()
            .filter(|first| parse_conflict_copy_name(&file_name(first)).is_none())
            .map(PathBuf::as_path)
    }
}

/// Why an answer to a conflict could not be applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeepError {
    /// The chosen file is not a version that can be kept: it is gone,
    /// or it stands in for another cloud file.
    NotAVersion(String),
    /// A file operation failed; trying again may work.
    Io(String),
}

impl std::fmt::Display for KeepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAVersion(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

/// Keeps the version whose file name is `choice` under the file's name
/// and hands every other version to `discard` (the trash). Every path
/// touched comes from [`Conflict::find`], the file under the name and
/// the machine-named copies of it, so an answer can never reach any
/// other file. The daemon syncs the result like any user edit.
///
/// The order keeps every failure recoverable: the file under the name
/// goes to the trash, the kept copy moves onto the name with a rename
/// that never replaces anything (a file that appears there meanwhile
/// stays, and the answer stops), and only then do the other copies go.
pub fn keep(
    conflict: &Conflict,
    choice: &str,
    mut discard: impl FnMut(&Path) -> io::Result<()>,
) -> Result<(), KeepError> {
    let kept = conflict
        .keepable()
        .iter()
        .find(|version| file_name(version) == choice)
        .ok_or_else(|| {
            KeepError::NotAVersion(format!(
                "{choice} is not a version of {} that can be kept; choose one of {}",
                conflict.original.display(),
                conflict
                    .keepable()
                    .iter()
                    .map(|version| file_name(version))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    let destination = conflict.destination();
    let mut discard_one = |version: &Path| match discard(version) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KeepError::Io(format!(
            "cannot move {} to the trash: {error}",
            version.display()
        ))),
    };
    if kept != destination {
        if conflict
            .versions
            .iter()
            .any(|version| version == destination)
        {
            discard_one(destination)?;
        }
        vapor_platform::fs_ops::atomic_noclobber_rename(kept, destination).map_err(|error| {
            KeepError::Io(format!(
                "cannot move {} to {}: {error}",
                kept.display(),
                destination.display()
            ))
        })?;
    }
    for version in conflict
        .versions
        .iter()
        .filter(|version| *version != kept && *version != destination)
    {
        discard_one(version)?;
    }
    Ok(())
}

/// What a `conflict` decision asks.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictQuestion {
    pub question: String,
    pub options: Vec<DecisionOption>,
    pub evidence: serde_json::Value,
}

/// The question for `conflict`. One option per version that can be
/// kept, keyed by its file name. `this_device` is the local
/// `deviceId`, so a label can say "this device's edit";
/// `trash_enabled` says where the other versions go.
pub fn question(conflict: &Conflict, this_device: &str, trash_enabled: bool) -> ConflictQuestion {
    let device_of = |path: &Path| parse_conflict_copy_name(&file_name(path)).map(|p| p.device_id);
    let evidence = serde_json::json!({
        "versions": conflict
            .versions
            .iter()
            .map(|version| {
                let metadata = fs::metadata(version).ok();
                serde_json::json!({
                    "file": file_name(version),
                    "device": device_of(version),
                    "sizeBytes": metadata.as_ref().map(fs::Metadata::len),
                    "modifiedAtMs": metadata
                        .and_then(|metadata| metadata.modified().ok())
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|age| age.as_millis() as u64),
                })
            })
            .collect::<Vec<_>>(),
    });
    let options = conflict
        .keepable()
        .iter()
        .map(|version| DecisionOption {
            key: file_name(version),
            label: match (&conflict.clash, device_of(version)) {
                (Some(_), _) => format!("Keep only {}", file_name(version)),
                (None, None) => "Keep the version already under the name".to_string(),
                (None, Some(device)) if device == this_device => {
                    "Keep this device's edit".to_string()
                }
                (None, Some(device)) => format!("Keep the edit made on {device}"),
            },
        })
        .collect();
    let original = conflict.original.display();
    let total = conflict.versions.len();
    let question = if let Some(sibling) = &conflict.clash {
        let removed = if trash_enabled {
            "The copies go to the trash and their cloud files are deleted."
        } else {
            "The copies and their cloud files are deleted."
        };
        format!(
            "The cloud has {total} files named like {original} that differ only in case or \
             accents. This device can hold one of them, {}, so the others are here as \
             conflict copies. Keep only {}? {removed}",
            sibling.display(),
            file_name(sibling),
        )
    } else {
        let others = if trash_enabled {
            "The others go to the trash."
        } else {
            "The others are deleted (the trash is turned off)."
        };
        match (conflict.under_the_name(), conflict.versions.as_slice()) {
            (Some(_), _) => format!(
                "{original} was changed in more than one place before the changes synced, so \
                 Vapor kept all {total} versions. Which one stays? {others}"
            ),
            (None, [only]) => format!(
                "{original} is gone, but its conflict copy {} is still here. Keep it under the \
                 name?",
                file_name(only)
            ),
            (None, _) => format!(
                "{original} is gone, and Vapor kept {total} conflicting versions of it. Which \
                 one takes the name? {others}"
            ),
        }
    };
    ConflictQuestion {
        question,
        options,
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const MINE: &str = "report~conflict-this-mac-1750000000123.md";
    const THEIRS: &str = "report~conflict-other-mac-1750000000456.md";

    /// `report.md` plus one copy per name in `copies`, each holding its
    /// own name as content.
    fn seed(root: &Path, copies: &[&str]) -> PathBuf {
        let original = root.join("docs").join("report.md");
        fs::create_dir_all(original.parent().expect("parent")).expect("dirs");
        fs::write(&original, b"report.md").expect("seed original");
        for copy in copies {
            fs::write(original.with_file_name(copy), copy.as_bytes()).expect("seed copy");
        }
        original
    }

    fn trash_into(bin: &Path) -> impl FnMut(&Path) -> io::Result<()> + '_ {
        move |path: &Path| {
            fs::create_dir_all(bin)?;
            fs::rename(path, bin.join(path.file_name().expect("name")))
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|path| file_name(path)).collect()
    }

    #[test]
    fn every_copy_of_a_file_belongs_to_one_conflict() {
        let temp = TempDir::new().expect("temp");
        let original = seed(
            temp.path(),
            &[THEIRS, MINE, "notes~conflict-x-1750000000999.md"],
        );

        let conflict = Conflict::find(&original)
            .expect("listable")
            .expect("a conflict");
        assert_eq!(names(&conflict.versions), vec!["report.md", MINE, THEIRS]);
        assert_eq!(conflict.keepable().len(), 3);
        assert!(
            Conflict::find(&original.with_file_name("plain.md"))
                .expect("listable")
                .is_none()
        );

        fs::remove_file(original.with_file_name(MINE)).expect("resolve by hand");
        fs::remove_file(original.with_file_name(THEIRS)).expect("resolve by hand");
        assert!(
            Conflict::find(&original).expect("listable").is_none(),
            "no copy, no conflict"
        );
    }

    #[test]
    fn the_question_offers_every_version_and_says_whose_edit_each_holds() {
        let temp = TempDir::new().expect("temp");
        let original = seed(temp.path(), &[MINE, THEIRS]);
        let conflict = Conflict::find(&original)
            .expect("listable")
            .expect("a conflict");

        let asked = question(&conflict, "this-mac", true);
        assert!(
            asked.question.contains("all 3 versions"),
            "{}",
            asked.question
        );
        assert!(
            asked.question.contains("go to the trash"),
            "{}",
            asked.question
        );
        let options: Vec<_> = asked
            .options
            .iter()
            .map(|option| (option.key.as_str(), option.label.as_str()))
            .collect();
        assert_eq!(
            options,
            vec![
                ("report.md", "Keep the version already under the name"),
                (MINE, "Keep this device's edit"),
                (THEIRS, "Keep the edit made on other-mac"),
            ]
        );
        let versions = asked.evidence["versions"].as_array().expect("versions");
        assert_eq!(versions[0]["device"], serde_json::Value::Null);
        assert_eq!(versions[1]["device"], "this-mac");
        assert_eq!(versions[2]["sizeBytes"], THEIRS.len() as u64);
    }

    #[test]
    fn keeping_a_copy_moves_it_under_the_name_and_trashes_the_rest() {
        let temp = TempDir::new().expect("temp");
        let original = seed(temp.path(), &[MINE, THEIRS]);
        let bin = temp.path().join("bin");
        let conflict = Conflict::find(&original)
            .expect("listable")
            .expect("a conflict");

        keep(&conflict, THEIRS, trash_into(&bin)).expect("keep");

        assert_eq!(fs::read(&original).expect("read"), THEIRS.as_bytes());
        assert!(Conflict::find(&original).expect("listable").is_none());
        let mut trashed: Vec<_> = fs::read_dir(&bin)
            .expect("bin")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        trashed.sort();
        assert_eq!(trashed, vec!["report.md", MINE]);
    }

    #[test]
    fn keeping_the_file_under_the_name_trashes_every_copy() {
        let temp = TempDir::new().expect("temp");
        let original = seed(temp.path(), &[MINE, THEIRS]);
        let bin = temp.path().join("bin");
        let conflict = Conflict::find(&original)
            .expect("listable")
            .expect("a conflict");

        keep(&conflict, "report.md", trash_into(&bin)).expect("keep");

        assert_eq!(fs::read(&original).expect("read"), b"report.md");
        assert_eq!(fs::read_dir(&bin).expect("bin").count(), 2);
    }

    #[test]
    fn an_answer_never_reaches_a_file_outside_the_conflict() {
        let temp = TempDir::new().expect("temp");
        let original = seed(temp.path(), &[MINE]);
        let bystander = original.with_file_name("precious.md");
        fs::write(&bystander, b"do not touch").expect("seed");
        let conflict = Conflict::find(&original)
            .expect("listable")
            .expect("a conflict");

        let refused = keep(&conflict, "precious.md", |_| panic!("nothing is discarded"));
        assert!(
            matches!(refused, Err(KeepError::NotAVersion(_))),
            "{refused:?}"
        );
        assert!(bystander.exists());
        assert!(original.with_file_name(MINE).exists());
    }

    #[test]
    fn a_lone_copy_of_a_deleted_file_is_offered_the_name() {
        let temp = TempDir::new().expect("temp");
        let original = seed(temp.path(), &[THEIRS]);
        fs::remove_file(&original).expect("delete the original");
        let conflict = Conflict::find(&original)
            .expect("listable")
            .expect("a conflict");

        let asked = question(&conflict, "this-mac", true);
        assert!(asked.question.contains("is gone"), "{}", asked.question);
        assert_eq!(asked.options.len(), 1);
        keep(&conflict, THEIRS, |_| panic!("nothing else to discard")).expect("keep");
        assert_eq!(fs::read(&original).expect("read"), THEIRS.as_bytes());
    }

    #[test]
    fn a_copy_next_to_a_folder_is_not_a_conflict() {
        let temp = TempDir::new().expect("temp");
        let folder = temp.path().join("report.md");
        fs::create_dir_all(&folder).expect("folder");
        fs::write(temp.path().join(MINE), b"file").expect("copy");
        assert!(Conflict::find(&folder).expect("listable").is_none());
    }

    #[test]
    fn a_file_that_appears_under_the_name_mid_answer_is_never_replaced() {
        let temp = TempDir::new().expect("temp");
        let original = seed(temp.path(), &[THEIRS]);
        fs::remove_file(&original).expect("the name is free when asked");
        let conflict = Conflict::find(&original)
            .expect("listable")
            .expect("a conflict");
        fs::write(&original, b"a download that just landed").expect("lands");

        let refused = keep(&conflict, THEIRS, |_| panic!("nothing is discarded"));
        assert!(matches!(refused, Err(KeepError::Io(_))), "{refused:?}");
        assert_eq!(
            fs::read(&original).expect("read"),
            b"a download that just landed"
        );
        assert!(original.with_file_name(THEIRS).exists());
    }

    #[test]
    fn a_name_clash_offers_only_the_local_file_and_drops_the_stand_ins() {
        let temp = TempDir::new().expect("temp");
        fs::write(temp.path().join("readme.md"), b"local readme").expect("seed");
        if !temp.path().join("README.MD").exists() {
            return; // a case-sensitive volume has no clash to make
        }
        let stand_in = temp.path().join("Readme~conflict-dev-1750000000123.md");
        fs::write(&stand_in, b"the cloud's other Readme").expect("seed");
        let bin = temp.path().join("bin");

        let conflict = Conflict::find(&temp.path().join("Readme.md"))
            .expect("listable")
            .expect("a clash");
        let asked = question(&conflict, "dev", true);
        let keys: Vec<_> = asked.options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, vec!["readme.md"]);
        assert!(
            asked.question.contains("Keep only readme.md"),
            "{}",
            asked.question
        );

        keep(&conflict, "readme.md", trash_into(&bin)).expect("keep");
        assert_eq!(
            fs::read(temp.path().join("readme.md")).expect("read"),
            b"local readme"
        );
        assert!(!stand_in.exists());
        assert!(bin.join(stand_in.file_name().expect("name")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_an_error_not_an_empty_conflict() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().expect("temp");
        let original = seed(temp.path(), &[MINE]);
        let folder = original.parent().expect("parent").to_path_buf();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o000)).expect("lock");
        let listable = fs::read_dir(&folder).is_ok(); // true for root
        let found = Conflict::find(&original);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("unlock");
        if !listable {
            assert!(found.is_err(), "{found:?}");
        }
    }

    fn never_exists(_: &Path) -> bool {
        false
    }

    #[test]
    fn suffix_lands_between_stem_and_extension() {
        let derived = conflict_copy_path(
            Path::new("/root/docs/report.md"),
            "alexs-mbp",
            1_750_000_000_123,
            never_exists,
        );
        assert_eq!(
            derived,
            PathBuf::from("/root/docs/report~conflict-alexs-mbp-1750000000123.md")
        );
    }

    #[test]
    fn extensionless_and_hidden_files_append_the_suffix() {
        assert_eq!(
            conflict_copy_path(Path::new("/r/Makefile"), "dev", 5, never_exists),
            PathBuf::from("/r/Makefile~conflict-dev-5")
        );
        assert_eq!(
            conflict_copy_path(Path::new("/r/.env"), "dev", 5, never_exists),
            PathBuf::from("/r/.env~conflict-dev-5")
        );
    }

    #[test]
    fn multi_dot_names_split_at_the_last_dot() {
        assert_eq!(
            conflict_copy_path(Path::new("/r/archive.tar.gz"), "dev", 7, never_exists),
            PathBuf::from("/r/archive.tar~conflict-dev-7.gz")
        );
    }

    #[test]
    fn collisions_walk_the_sequence_from_two() {
        let taken = [
            PathBuf::from("/r/a~conflict-dev-9.txt"),
            PathBuf::from("/r/a~conflict-dev-9-2.txt"),
        ];
        let derived = conflict_copy_path(Path::new("/r/a.txt"), "dev", 9, |candidate| {
            taken.iter().any(|t| t == candidate)
        });
        assert_eq!(derived, PathBuf::from("/r/a~conflict-dev-9-3.txt"));
    }

    #[test]
    fn parse_round_trips_generated_names() {
        // Realistic epoch-ms timestamps: the parser requires 12+ digits
        // to tell the timestamp apart from digit-suffixed device ids.
        let cases = [
            ("report.md", "alexs-mbp", 1_750_000_000_123_u64),
            ("Makefile", "dev", 1_750_000_000_123),
            (".env", "mbp2", 1_750_000_000_123),
            ("archive.tar.gz", "work-mac-2", 1_750_000_000_123),
        ];
        for (original, device_id, timestamp_ms) in cases {
            let derived = conflict_copy_path(
                &PathBuf::from("/r").join(original),
                device_id,
                timestamp_ms,
                never_exists,
            );
            let name = derived.file_name().unwrap().to_str().unwrap();
            let parsed = parse_conflict_copy_name(name)
                .unwrap_or_else(|| panic!("generated name must parse: {name}"));
            // Multi-dot originals lose their inner stem dots to the
            // canonical reconstruction only when the generator split
            // them — round-trip must restore the full original name.
            assert_eq!(parsed.canonical_file_name, original, "for {name}");
            assert_eq!(parsed.device_id, device_id, "for {name}");
            assert_eq!(parsed.timestamp_ms, timestamp_ms, "for {name}");
            assert_eq!(parsed.sequence, None, "for {name}");
        }
    }

    #[test]
    fn parse_recovers_the_collision_sequence() {
        let parsed = parse_conflict_copy_name("a~conflict-dev-1750000000123-3.txt").expect("parse");
        assert_eq!(parsed.canonical_file_name, "a.txt");
        assert_eq!(parsed.device_id, "dev");
        assert_eq!(parsed.timestamp_ms, 1_750_000_000_123);
        assert_eq!(parsed.sequence, Some(3));
    }

    #[test]
    fn parse_rejects_names_that_only_look_conflicted() {
        // User files containing the marker text but not the generated
        // grammar must not be treated as conflict copies.
        for name in [
            "notes~conflict-resolution.md",         // no timestamp segment
            "~conflict-dev-1750000000123.md",       // empty stem
            "a~conflict-1750000000123.txt",         // no device id segment
            "a~conflict-dev-1750000000123-2-9.txt", // trailing garbage
            "plain.txt",
        ] {
            assert_eq!(parse_conflict_copy_name(name), None, "{name}");
        }
    }

    #[test]
    fn derivation_is_deterministic_for_identical_inputs() {
        // The determinism contract: same device + same event timestamp
        // → same conflict path across daemon restarts.
        let a = conflict_copy_path(Path::new("/r/f.rs"), "dev-1", 42, never_exists);
        let b = conflict_copy_path(Path::new("/r/f.rs"), "dev-1", 42, never_exists);
        assert_eq!(a, b);
        let c = conflict_copy_path(Path::new("/r/f.rs"), "dev-2", 42, never_exists);
        assert_ne!(a, c, "different devices must not collide");
    }
}
