//! Stable per-device identifier.
//!
//! Used by the keep-both conflict suffix
//! (`{stem}~conflict-{device_id}-{timestamp_ms}{ext}`), by provider
//! op-ids, and by the sync root markers. It is the host name up to its
//! first dot, normalized to `[a-z0-9-]`, plus four random characters:
//! two devices with one host name (two Macs named `MacBook-Pro`) still
//! get two ids. With no usable host name it is 12 random hex
//! characters. The value is written to `vapor.json` as `deviceId` at
//! the first run and never derived again: a renamed machine keeps its
//! id.

use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use crate::constants;

const MAX_DEVICE_ID_LENGTH: usize = 32;
/// The random part that tells two devices with one host name apart.
const SUFFIX_LENGTH: usize = 4;

/// Normalizes a raw hostname into device-id shape: lowercase, only
/// `[a-z0-9-]`, capped at 32 chars. Empty when nothing survives.
pub fn normalize_hostname(raw: &str) -> String {
    raw.chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(MAX_DEVICE_ID_LENGTH)
        .collect()
}

/// The part of a host name a device id starts with: what stands before
/// the first dot (`Sams-MacBook-Pro.local` gives `sams-macbook-pro`),
/// normalized, and short enough to leave room for the random part.
fn host_slug(raw: &str) -> String {
    let host = raw.split('.').next().unwrap_or_default();
    let slug: String = normalize_hostname(host)
        .chars()
        .take(MAX_DEVICE_ID_LENGTH - SUFFIX_LENGTH - 1)
        .collect();
    slug.trim_matches('-').to_string()
}

/// Derives a new device id: the host slug and a random part, or 12
/// random hex characters when the host has no usable name. Two calls
/// give two ids; the caller keeps the one it uses.
pub fn derive_device_id() -> String {
    let slug = hostname().map(|h| host_slug(&h)).unwrap_or_default();
    if slug.is_empty() {
        random_fallback_id()
    } else {
        format!("{slug}-{}", random_suffix())
    }
}

static PROCESS_DEVICE_ID: OnceLock<String> = OnceLock::new();

/// The device id of this process. The daemon sets it to the id that
/// `vapor.json` holds ([`use_for_process`]); a process that never set
/// one (a test, a tool) derives one, one time.
pub fn for_process() -> String {
    PROCESS_DEVICE_ID.get_or_init(derive_device_id).clone()
}

/// Makes `device_id` the id of this process. The first call wins.
pub fn use_for_process(device_id: &str) {
    let _ = PROCESS_DEVICE_ID.set(device_id.to_string());
}

/// The device id `vapor.json` holds, without deriving one: `None`
/// until the daemon's first run has written it.
pub fn persisted(config_path: &Path) -> Option<String> {
    read_config_object(config_path)
        .ok()?
        .get(constants::config::KEY_DEVICE_ID)?
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Resolves the persisted device id, deriving and persisting one at
/// first run. An existing non-empty value always wins (never silently
/// regenerated). The write preserves every other key in `vapor.json`,
/// and it holds the configuration lock like every other writer, so a
/// `vapor config set` at the daemon's first start loses nothing.
pub fn resolve_or_persist(config_path: &Path) -> io::Result<String> {
    if let Some(existing) = persisted(config_path) {
        return Ok(existing);
    }
    // Derived before the lock is taken: reading the host name starts a
    // process, and no other writer should wait for it.
    let candidate = derive_device_id();
    crate::runtime_paths::with_config_lock(config_path, || {
        let mut document = read_config_object(config_path)?;
        if let Some(existing) = document
            .get(constants::config::KEY_DEVICE_ID)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            return Ok(existing.to_string());
        }
        let object = document.as_object_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a JSON object", config_path.display()),
            )
        })?;
        object.insert(
            constants::config::KEY_DEVICE_ID.to_string(),
            serde_json::Value::String(candidate.clone()),
        );
        crate::runtime_paths::write_config_document(config_path, &document)?;
        Ok(candidate)
    })
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

/// 64 random bits from the standard library's randomly-seeded hasher,
/// without a new dependency.
fn random_bits() -> u64 {
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// 12 random hex characters (48 bits).
fn random_fallback_id() -> String {
    format!("{:08x}{:04x}", random_bits() as u32, random_bits() as u16)
}

/// Four random characters of `[a-z0-9]`: about 1.7 million values,
/// enough to tell apart the devices of one person that share a name.
fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bits = random_bits();
    (0..SUFFIX_LENGTH)
        .map(|_| {
            let index = (bits % ALPHABET.len() as u64) as usize;
            bits /= ALPHABET.len() as u64;
            ALPHABET[index] as char
        })
        .collect()
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
    fn the_host_slug_stops_at_the_first_dot_and_leaves_room_for_the_random_part() {
        assert_eq!(host_slug("Sams-MacBook-Pro.local"), "sams-macbook-pro");
        assert_eq!(host_slug("build-7.ci.example.com"), "build-7");
        assert_eq!(host_slug(".local"), "");
        let long = host_slug(&"x".repeat(64));
        assert_eq!(long.len() + 1 + SUFFIX_LENGTH, MAX_DEVICE_ID_LENGTH);
    }

    #[test]
    fn two_devices_with_one_host_name_get_two_ids() {
        // Same host, so the same slug; the random part differs. Ten
        // draws that all collide would be one chance in 10^55.
        let ids: std::collections::BTreeSet<String> = (0..10).map(|_| derive_device_id()).collect();
        assert!(ids.len() > 1, "{ids:?}");
        for id in &ids {
            assert!(id.len() <= MAX_DEVICE_ID_LENGTH, "{id}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{id}"
            );
        }
    }

    #[test]
    fn the_process_id_is_derived_one_time() {
        assert_eq!(for_process(), for_process());
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
