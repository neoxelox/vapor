use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"));
    let root_dir = manifest_dir.join("../..");
    let version_path = root_dir.join("VERSION");

    println!("cargo:rerun-if-changed={}", version_path.display());
    emit_git_rerun_markers(&root_dir);

    let version = fs::read_to_string(&version_path)
        .expect("failed to read VERSION")
        .trim()
        .to_string();
    validate_version(&version).expect("invalid VERSION contents");

    let git_commit_short = git_output(&root_dir, &["rev-parse", "--short=7", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());

    let generated = format!(
        "pub const VERSION: &str = {:?};\n\
pub const GIT_COMMIT_SHORT: &str = {:?};\n",
        version, git_commit_short,
    );

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("missing OUT_DIR"));
    fs::write(out_dir.join("vapor_build_info.rs"), generated)
        .expect("failed to write vapor_build_info.rs");
}

fn emit_git_rerun_markers(root_dir: &Path) {
    let Some(git_dir) = resolve_git_dir(root_dir) else {
        return;
    };

    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());

    let Ok(head_contents) = fs::read_to_string(&head_path) else {
        return;
    };

    let Some(reference) = head_contents.trim().strip_prefix("ref: ") else {
        return;
    };

    let ref_path = git_dir.join(reference);
    if ref_path.exists() {
        println!("cargo:rerun-if-changed={}", ref_path.display());
    }
}

fn resolve_git_dir(root_dir: &Path) -> Option<PathBuf> {
    let dot_git = root_dir.join(".git");

    if dot_git.is_dir() {
        return Some(dot_git);
    }

    let contents = fs::read_to_string(dot_git).ok()?;
    let git_dir = contents.trim().strip_prefix("gitdir: ")?;
    let git_dir_path = PathBuf::from(git_dir);

    if git_dir_path.is_absolute() {
        Some(git_dir_path)
    } else {
        Some(root_dir.join(git_dir_path))
    }
}

fn git_output(root_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_dir)
        .args(args)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn validate_version(version: &str) -> Result<(), String> {
    let (release_version, prerelease) = match version.split_once('-') {
        Some((release_version, prerelease)) => (release_version, Some(prerelease)),
        None => (version, None),
    };

    let release_parts: Vec<&str> = release_version.split('.').collect();
    if release_parts.len() != 3
        || release_parts
            .iter()
            .any(|part| part.parse::<u64>().is_err())
    {
        return Err(format!("invalid release version '{version}'"));
    }

    if let Some(value) = prerelease {
        let (label, number) = value
            .split_once('.')
            .ok_or_else(|| format!("invalid prerelease version '{version}'"))?;
        if !matches!(label, "alpha" | "beta" | "rc") {
            return Err(format!("unsupported prerelease label in '{version}'"));
        }
        let parsed_number = number
            .parse::<u64>()
            .map_err(|_| format!("invalid prerelease number in '{version}'"))?;
        if parsed_number == 0 || parsed_number > 255 {
            return Err(format!(
                "prerelease number must be between 1 and 255 in '{version}'"
            ));
        }
    }

    Ok(())
}
