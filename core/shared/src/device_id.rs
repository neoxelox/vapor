//! Stable per-device identifier.
//!
//! Used by the keep-both conflict suffix
//! (`{stem}~conflict-{device_id}-{timestamp_ms}{ext}`) and by provider
//! op-ids. Derivation per `docs/architecture/data-flow.md §Conflict
//! handling`: hostname normalized to `[a-z0-9-]` (length-capped at 32);
//! when nothing survives normalization, a random 12-hex-char fallback.
//! The resolved value persists in `vapor.json` as `deviceId` at first
//! run and is never silently regenerated — renaming the machine does
//! not change an already-persisted id.

use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::Path;

use crate::constants;

const MAX_DEVICE_ID_LENGTH: usize = 32;

/// Normalizes a raw hostname into device-id shape: lowercase, only
/// `[a-z0-9-]`, capped at 32 chars. Empty when nothing survives.
pub fn normalize_hostname(raw: &str) -> String {
    raw.chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(MAX_DEVICE_ID_LENGTH)
        .collect()
}

/// Derives a fresh device id from the host name, falling back to a
/// random 12-hex-char identifier when normalization strips everything.
pub fn derive_device_id() -> String {
    let normalized = hostname()
        .map(|h| normalize_hostname(&h))
        .unwrap_or_default();
    if normalized.is_empty() {
        random_fallback_id()
    } else {
        normalized
    }
}

/// Resolves the persisted device id, deriving and persisting one at
/// first run. An existing non-empty value always wins (never silently
/// regenerated). The write preserves every other key in `vapor.json`.
pub fn resolve_or_persist(config_path: &Path) -> io::Result<String> {
    let mut document = read_config_object(config_path)?;
    if let Some(existing) = document
        .get(constants::config::KEY_DEVICE_ID)
        .and_then(|value| value.as_str())
        && !existing.trim().is_empty()
    {
        return Ok(existing.to_string());
    }

    let device_id = derive_device_id();
    let object = document
        .as_object_mut()
        .expect("read_config_object returns an object");
    object.insert(
        constants::config::KEY_DEVICE_ID.to_string(),
        serde_json::Value::String(device_id.clone()),
    );
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut serialized = serde_json::to_string_pretty(&document)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    serialized.push('\n');
    // Atomic replace: the Swift store and the CLI use the same
    // temp+rename discipline on this file.
    let temp_path = config_path.with_extension("vapor-tmp");
    fs::write(&temp_path, serialized.as_bytes())?;
    fs::rename(&temp_path, config_path)?;
    Ok(device_id)
}

fn read_config_object(path: &Path) -> io::Result<serde_json::Value> {
    match fs::read_to_string(path) {
        Ok(contents) if contents.trim().is_empty() => {
            Ok(serde_json::Value::Object(Default::default()))
        }
        Ok(contents) => serde_json::from_str(&contents).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot parse {}: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(serde_json::Value::Object(Default::default()))
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    // `hostname(1)` is POSIX-standard and avoids an FFI dependency for
    // a value we read exactly once per process start.
    let output = std::process::Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() { None } else { Some(raw) }
}

#[cfg(windows)]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok().filter(|v| !v.is_empty())
}

#[cfg(not(any(unix, windows)))]
fn hostname() -> Option<String> {
    None
}

/// 12 hex chars sourced from the standard library's randomly-seeded
/// hasher (two independent `RandomState` seeds). Not a UUID, but the
/// same 48 bits of entropy the truncated-UUID fallback would carry,
/// without a new dependency.
fn random_fallback_id() -> String {
    let a = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let b = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    format!("{:08x}{:04x}", (a as u32), (b as u16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn hostname_normalization_keeps_only_the_documented_alphabet() {
        assert_eq!(
            normalize_hostname("Alexs-MacBook-Pro.local"),
            "alexs-macbook-prolocal"
        );
        assert_eq!(normalize_hostname("ÜBER_host!42"), "berhost42");
        assert_eq!(normalize_hostname("___"), "");
        assert_eq!(
            normalize_hostname(&"x".repeat(64)).len(),
            32,
            "length is capped at 32"
        );
    }

    #[test]
    fn fallback_id_is_twelve_hex_chars() {
        let id = random_fallback_id();
        assert_eq!(id.len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_persists_once_and_never_regenerates() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        std::fs::write(&path, r#"{ "autoLaunch": false }"#).expect("seed");

        let first = resolve_or_persist(&path).expect("first resolve");
        assert!(!first.is_empty());
        let second = resolve_or_persist(&path).expect("second resolve");
        assert_eq!(first, second, "persisted id must be stable");

        // Other keys survive the write.
        let contents = std::fs::read_to_string(&path).expect("file");
        assert!(contents.contains("\"autoLaunch\": false"));
        assert!(contents.contains("\"deviceId\""));
    }

    #[test]
    fn existing_device_id_always_wins() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        std::fs::write(&path, r#"{ "deviceId": "pinned-device-42" }"#).expect("seed");
        assert_eq!(
            resolve_or_persist(&path).expect("resolve"),
            "pinned-device-42"
        );
    }

    #[test]
    fn missing_file_gets_created_with_a_device_id() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        let id = resolve_or_persist(&path).expect("resolve");
        assert!(!id.is_empty());
        assert!(path.exists());
    }
}
