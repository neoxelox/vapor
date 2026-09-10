//! Seeded workload: the operations a user (or another device) performs
//! on one side of the sync, and the content each version carries.
//!
//! Content is derived from `(seed, path, version)`, so the model never
//! stores bytes: any version can be regenerated to compare hashes, and
//! every file starts with a header line that names its origin, which
//! makes a preserved sandbox readable by hand.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::Failure;
use crate::rng::Rng;

/// Which replica an operation is applied to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Local,
    Cloud,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Local => "local",
            Side::Cloud => "cloud",
        }
    }

    pub fn other(self) -> Side {
        match self {
            Side::Local => Side::Cloud,
            Side::Cloud => Side::Local,
        }
    }
}

/// Size class of a file the workload writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SizeClass {
    /// 64 bytes to 8 KiB: config files, notes, source.
    Small,
    /// 64 KiB to 1 MiB: documents, images.
    Medium,
    /// `large_bytes` from the load shape: a chunked transfer.
    Large,
}

/// One operation on one side. Paths are slash-separated and relative to
/// the side's root.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    Create {
        path: String,
        size: SizeClass,
    },
    /// Rewrite with new content of the same class (size may change).
    Overwrite {
        path: String,
        size: SizeClass,
    },
    /// Rewrite keeping the exact byte count: invisible to a size check.
    SameSizeEdit {
        path: String,
    },
    Append {
        path: String,
        bytes: u64,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Move into another (possibly new) directory.
    Move {
        from: String,
        to: String,
    },
    Delete {
        path: String,
    },
    /// Remove a whole directory subtree.
    DeleteTree {
        dir: String,
    },
    Mkdir {
        dir: String,
    },
    /// Flip the executable bit (Unix only; a no-op elsewhere).
    Chmod {
        path: String,
        executable: bool,
    },
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Create { .. } => "create",
            Op::Overwrite { .. } => "overwrite",
            Op::SameSizeEdit { .. } => "same-size-edit",
            Op::Append { .. } => "append",
            Op::Rename { .. } => "rename",
            Op::Move { .. } => "move",
            Op::Delete { .. } => "delete",
            Op::DeleteTree { .. } => "delete-tree",
            Op::Mkdir { .. } => "mkdir",
            Op::Chmod { .. } => "chmod",
        }
    }
}

/// Load shape: how often each operation is picked and how big files
/// get. Percentages are relative weights.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LoadShape {
    pub name: String,
    pub create: u32,
    pub overwrite: u32,
    pub same_size_edit: u32,
    pub append: u32,
    pub rename: u32,
    pub mv: u32,
    pub delete: u32,
    pub delete_tree: u32,
    pub mkdir: u32,
    pub chmod: u32,
    pub small: u32,
    pub medium: u32,
    pub large: u32,
    pub large_bytes: u64,
    /// Upper bound on live files the generator keeps per side; above it
    /// creates are replaced by edits and deletes.
    pub max_files: usize,
    /// Directory depth the generator nests into.
    pub max_depth: usize,
    /// Pause between operations, milliseconds (0 = burst).
    pub op_interval_ms: u64,
}

impl LoadShape {
    pub fn named(name: &str) -> Option<Self> {
        let base = Self {
            name: name.to_string(),
            create: 30,
            overwrite: 20,
            same_size_edit: 8,
            append: 8,
            rename: 8,
            mv: 6,
            delete: 10,
            delete_tree: 2,
            mkdir: 4,
            chmod: 4,
            small: 70,
            medium: 28,
            large: 2,
            large_bytes: 24 * 1024 * 1024,
            max_files: 400,
            max_depth: 4,
            op_interval_ms: 400,
        };
        Some(match name {
            "mixed" => base,
            "trickle" => Self {
                op_interval_ms: 3_000,
                max_files: 120,
                ..base
            },
            "coding" => Self {
                create: 15,
                overwrite: 45,
                same_size_edit: 15,
                append: 10,
                rename: 5,
                mv: 2,
                delete: 6,
                delete_tree: 0,
                mkdir: 2,
                chmod: 0,
                small: 95,
                medium: 5,
                large: 0,
                max_files: 300,
                op_interval_ms: 150,
                ..base
            },
            "bulk" => Self {
                create: 80,
                overwrite: 5,
                same_size_edit: 0,
                append: 0,
                rename: 3,
                mv: 2,
                delete: 5,
                delete_tree: 1,
                mkdir: 4,
                chmod: 0,
                small: 100,
                medium: 0,
                large: 0,
                max_files: 5_000,
                max_depth: 5,
                op_interval_ms: 20,
                ..base
            },
            "large" => Self {
                create: 50,
                overwrite: 30,
                same_size_edit: 0,
                append: 5,
                rename: 5,
                mv: 0,
                delete: 10,
                delete_tree: 0,
                mkdir: 0,
                chmod: 0,
                small: 0,
                medium: 20,
                large: 80,
                large_bytes: 96 * 1024 * 1024,
                max_files: 12,
                op_interval_ms: 2_000,
                ..base
            },
            _ => return None,
        })
    }

    pub fn known_names() -> &'static [&'static str] {
        &["mixed", "trickle", "coding", "bulk", "large"]
    }

    /// The same shape restricted to creates and directories, for the
    /// seed phase that populates the tree.
    pub fn seeding(&self) -> Self {
        Self {
            name: format!("{}-seed", self.name),
            create: 90,
            overwrite: 0,
            same_size_edit: 0,
            append: 0,
            rename: 0,
            mv: 0,
            delete: 0,
            delete_tree: 0,
            mkdir: 10,
            chmod: 0,
            op_interval_ms: self.op_interval_ms.min(100),
            ..self.clone()
        }
    }

    fn pick_size(&self, rng: &mut Rng) -> SizeClass {
        let total = u64::from(self.small + self.medium + self.large);
        let roll = rng.below(total.max(1));
        if roll < u64::from(self.small) {
            SizeClass::Small
        } else if roll < u64::from(self.small + self.medium) {
            SizeClass::Medium
        } else {
            SizeClass::Large
        }
    }
}

/// Byte length for a size class, deterministic in the version.
pub fn size_for(class: SizeClass, shape: &LoadShape, rng: &mut Rng) -> u64 {
    match class {
        SizeClass::Small => 64 + rng.below(8 * 1024 - 64),
        SizeClass::Medium => 64 * 1024 + rng.below(1024 * 1024 - 64 * 1024),
        SizeClass::Large => shape.large_bytes,
    }
}

/// The bytes of `path` at `version`. The header line names the origin;
/// the rest is a keyed pseudo-random stream, so two versions never
/// share content and the file is incompressible enough to behave like
/// real data in transfer.
pub fn content_for(seed: u64, path: &str, version: u64, size: u64) -> Vec<u8> {
    let header = format!("vapor-soak seed={seed} path={path} version={version} size={size}\n");
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.extend_from_slice(header.as_bytes());
    if bytes.len() as u64 > size {
        bytes.truncate(size as usize);
        return bytes;
    }
    let key = {
        let mut hasher = Sha256::new();
        hasher.update(seed.to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(version.to_le_bytes());
        let digest = hasher.finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("8 bytes"))
    };
    let mut rng = Rng::new(key);
    while (bytes.len() as u64) < size {
        let word = rng.next_u64().to_le_bytes();
        let remaining = (size as usize) - bytes.len();
        bytes.extend_from_slice(&word[..remaining.min(8)]);
    }
    bytes
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Applies an operation to a real directory tree. Returns the number of
/// files touched (a tree delete counts every file it removed).
pub fn apply(root: &Path, op: &Op, seed: u64, version: u64, size: u64) -> Result<usize, Failure> {
    let full = |relative: &str| -> PathBuf {
        let mut path = root.to_path_buf();
        for segment in relative.split('/') {
            path.push(segment);
        }
        path
    };
    match op {
        Op::Create { path, .. } | Op::Overwrite { path, .. } => {
            let target = full(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            write_atomically(&target, &content_for(seed, path, version, size))?;
            Ok(1)
        }
        Op::SameSizeEdit { path } => {
            let target = full(path);
            let current = fs::metadata(&target)?.len();
            write_atomically(&target, &content_for(seed, path, version, current))?;
            Ok(1)
        }
        Op::Append { path, bytes } => {
            let target = full(path);
            let mut file = fs::OpenOptions::new().append(true).open(&target)?;
            use std::io::Write;
            let tail = content_for(seed, &format!("{path}#append"), version, *bytes);
            file.write_all(&tail)?;
            file.sync_all()?;
            Ok(1)
        }
        Op::Rename { from, to } | Op::Move { from, to } => {
            let destination = full(to);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(full(from), destination)?;
            Ok(1)
        }
        Op::Delete { path } => {
            fs::remove_file(full(path))?;
            Ok(1)
        }
        Op::DeleteTree { dir } => {
            let target = full(dir);
            let count = count_files(&target);
            fs::remove_dir_all(&target)?;
            Ok(count)
        }
        Op::Mkdir { dir } => {
            fs::create_dir_all(full(dir))?;
            Ok(0)
        }
        Op::Chmod { path, executable } => {
            set_executable(&full(path), *executable)?;
            Ok(1)
        }
    }
}

/// Writes through a temp file and a rename, like a careful editor, so
/// the watcher never sees a half-written payload.
fn write_atomically(target: &Path, bytes: &[u8]) -> Result<(), Failure> {
    let parent = target
        .parent()
        .ok_or_else(|| Failure::new("target has no parent"))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Failure::new("target has no name"))?;
    let temp = parent.join(format!(".soak-write-{name}.tmp"));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, target)?;
    Ok(())
}

fn count_files(root: &Path) -> usize {
    let mut count = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                count += 1;
            }
        }
    }
    count
}

pub fn set_executable(path: &Path, executable: bool) -> Result<(), Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        let mode = permissions.mode() & 0o777;
        let new_mode = if executable {
            mode | 0o111
        } else {
            mode & !0o111
        };
        permissions.set_mode(new_mode);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, executable);
    }
    Ok(())
}

/// Picks the next operation for a side given the files it currently
/// owns. `owned` are the relative paths of files the generator may
/// touch on this side; `dirs` the directories it may move into.
pub fn next_op(
    rng: &mut Rng,
    shape: &LoadShape,
    owned: &[String],
    dirs: &[String],
    live_files: usize,
) -> Op {
    let total = shape.create
        + shape.overwrite
        + shape.same_size_edit
        + shape.append
        + shape.rename
        + shape.mv
        + shape.delete
        + shape.delete_tree
        + shape.mkdir
        + shape.chmod;
    let mut roll = rng.below(u64::from(total.max(1))) as u32;
    let weights = [
        ("create", shape.create),
        ("overwrite", shape.overwrite),
        ("same-size-edit", shape.same_size_edit),
        ("append", shape.append),
        ("rename", shape.rename),
        ("move", shape.mv),
        ("delete", shape.delete),
        ("delete-tree", shape.delete_tree),
        ("mkdir", shape.mkdir),
        ("chmod", shape.chmod),
    ];
    let mut chosen = "create";
    for (name, weight) in weights {
        if roll < weight {
            chosen = name;
            break;
        }
        roll -= weight;
    }
    // Without files to edit, or above the file cap, the choice is forced.
    if owned.is_empty() && chosen != "mkdir" {
        chosen = "create";
    }
    if chosen == "create" && live_files >= shape.max_files && !owned.is_empty() {
        chosen = "overwrite";
    }
    match chosen {
        "create" => Op::Create {
            path: new_path(rng, dirs, shape.max_depth),
            size: shape.pick_size(rng),
        },
        "overwrite" => Op::Overwrite {
            path: rng.pick(owned).clone(),
            size: shape.pick_size(rng),
        },
        "same-size-edit" => Op::SameSizeEdit {
            path: rng.pick(owned).clone(),
        },
        "append" => Op::Append {
            path: rng.pick(owned).clone(),
            bytes: 16 + rng.below(4096),
        },
        "rename" => {
            let from = rng.pick(owned).clone();
            let parent = parent_of(&from);
            let to = with_extension_of(&join(&parent, &rng.name("renamed")), &from);
            Op::Rename { from, to }
        }
        "move" => {
            let from = rng.pick(owned).clone();
            let target_dir = if dirs.is_empty() || rng.chance(30) {
                rng.name("moved-dir")
            } else {
                rng.pick(dirs).clone()
            };
            let name = from.rsplit('/').next().unwrap_or(&from).to_string();
            Op::Move {
                from,
                to: join(&target_dir, &name),
            }
        }
        "delete" => Op::Delete {
            path: rng.pick(owned).clone(),
        },
        "delete-tree" => {
            // Only a directory the generator owns entirely.
            let candidates: Vec<&String> = dirs.iter().filter(|dir| !dir.is_empty()).collect();
            if candidates.is_empty() {
                Op::Delete {
                    path: rng.pick(owned).clone(),
                }
            } else {
                Op::DeleteTree {
                    dir: (*rng.pick(&candidates)).clone(),
                }
            }
        }
        "mkdir" => {
            let parent = if dirs.is_empty() || rng.chance(50) {
                String::new()
            } else {
                rng.pick(dirs).clone()
            };
            Op::Mkdir {
                dir: join(&parent, &rng.name("dir")),
            }
        }
        _ => Op::Chmod {
            path: rng.pick(owned).clone(),
            executable: rng.chance(50),
        },
    }
}

fn new_path(rng: &mut Rng, dirs: &[String], max_depth: usize) -> String {
    let extensions = [
        "txt", "md", "rs", "json", "yaml", "csv", "png", "pdf", "bin", "log2", "dat",
    ];
    let name = format!("{}.{}", rng.name("file"), rng.pick(&extensions));
    if dirs.is_empty() || rng.chance(25) {
        return name;
    }
    let dir = rng.pick(dirs);
    if dir.matches('/').count() + 1 >= max_depth {
        return name;
    }
    join(dir, &name)
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[..index].to_string(),
        None => String::new(),
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn with_extension_of(path: &str, source: &str) -> String {
    match source.rsplit_once('.') {
        Some((_, extension)) if !extension.contains('/') => format!("{path}.{extension}"),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_is_deterministic_per_seed_path_and_version() {
        let a = content_for(1, "a/b.txt", 3, 5000);
        let b = content_for(1, "a/b.txt", 3, 5000);
        let c = content_for(1, "a/b.txt", 4, 5000);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 5000);
        assert!(a.starts_with(b"vapor-soak seed=1 path=a/b.txt version=3 size=5000\n"));
    }

    #[test]
    fn tiny_sizes_truncate_the_header_instead_of_overflowing() {
        assert_eq!(content_for(9, "x", 1, 10).len(), 10);
    }

    #[test]
    fn generator_only_edits_files_it_owns() {
        let shape = LoadShape::named("mixed").expect("shape");
        let mut rng = Rng::new(5);
        let owned = vec!["a.txt".to_string(), "d/b.txt".to_string()];
        let dirs = vec!["d".to_string()];
        for _ in 0..500 {
            let op = next_op(&mut rng, &shape, &owned, &dirs, 2);
            match &op {
                Op::Overwrite { path, .. }
                | Op::SameSizeEdit { path }
                | Op::Append { path, .. }
                | Op::Delete { path }
                | Op::Chmod { path, .. } => assert!(owned.contains(path), "{op:?}"),
                Op::Rename { from, .. } | Op::Move { from, .. } => assert!(owned.contains(from)),
                Op::DeleteTree { dir } => assert_eq!(dir, "d"),
                Op::Create { .. } | Op::Mkdir { .. } => {}
            }
        }
    }

    #[test]
    fn apply_round_trips_through_the_filesystem() {
        let dir = tempfile::TempDir::new().expect("dir");
        let create = Op::Create {
            path: "sub/one.txt".to_string(),
            size: SizeClass::Small,
        };
        apply(dir.path(), &create, 7, 1, 300).expect("create");
        assert_eq!(
            fs::read(dir.path().join("sub/one.txt")).expect("read"),
            content_for(7, "sub/one.txt", 1, 300)
        );
        let rename = Op::Rename {
            from: "sub/one.txt".to_string(),
            to: "sub/two.txt".to_string(),
        };
        apply(dir.path(), &rename, 7, 2, 0).expect("rename");
        assert!(dir.path().join("sub/two.txt").is_file());
        let removed = apply(
            dir.path(),
            &Op::DeleteTree {
                dir: "sub".to_string(),
            },
            7,
            3,
            0,
        )
        .expect("tree");
        assert_eq!(removed, 1);
        assert!(!dir.path().join("sub").exists());
    }
}
