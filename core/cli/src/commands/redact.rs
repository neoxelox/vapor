//! Path redaction for the support report.
//!
//! A report holds `vapor.json`, the logs, and the daemon's live state.
//! All three name the user's folders and files, and a report attached
//! to a public issue is public. With redaction on (the default), every
//! file and folder name becomes a short code. The code is stable: one
//! name gives one code in every file of a report and in every report,
//! so a maintainer can still follow one file through the logs.
//!
//! What is replaced:
//!
//! - the sync roots, the home directory, and the device id, by a tag
//!   that says what stood there (`<local-root:default>`);
//! - each path component, by `p-<code>` and its extension;
//! - the whole value of a field that holds a path, spaces included.
//!
//! What can remain: a name inside an error message that the operating
//! system or the provider wrote, when the name has a space in it. Only
//! the part up to the first space is recognised as a path there. The
//! report says so, and the user reads it before sharing.

use std::path::Path;

use serde_json::Value;
use vapor_shared::config::VaporConfig;

/// Names that describe the layout of a system or of Vapor's own
/// runtime directory, and nothing about the user.
const STRUCTURAL_NAMES: &[&str] = &[
    "Users",
    "home",
    "Volumes",
    "Library",
    "private",
    "var",
    "tmp",
    "state",
    "logs",
    "trash",
    "profiles",
    "reports",
    "vapor.json",
    "vapor.sqlite",
    "vapord.log",
    "vapor.log",
    "vapord.sock",
    "lifecycle.json",
    "meta.json",
];

/// Marker of a keep-both conflict copy; kept so a redacted name still
/// says what it is.
const CONFLICT_MARKER: &str = "~conflict-";

/// The punctuation a sentence puts around a path.
const WRAPPERS: &[char] = &['"', '\'', '`', '(', ')', '[', ']', ',', ';', ':'];

pub struct Redactor {
    /// Exact strings and what replaces them, longest first, so a root
    /// inside the home directory is tagged as the root.
    exact: Vec<(String, String)>,
}

impl Redactor {
    /// A redactor that knows the roots of `config` as the daemon
    /// resolves them, the home directory, and the device id.
    pub fn new(config: &VaporConfig, home: Option<&Path>, device_id: Option<&str>) -> Self {
        let mut exact: Vec<(String, String)> = Vec::new();
        let mut add = |needle: String, tag: String| {
            // A one-character needle (`/`, `~`) would rewrite every path.
            if needle.len() > 1 && !exact.iter().any(|(known, _)| *known == needle) {
                exact.push((needle, tag));
            }
        };
        for profile in vapor_daemon::profiles::resolve_profiles(config) {
            if let Some(local) = &profile.scope.local_root {
                add(
                    local.display().to_string(),
                    format!("<local-root:{}>", profile.id),
                );
            }
            add(
                profile.scope.cloud_root.clone(),
                format!("<cloud-root:{}>", profile.id),
            );
        }
        if let Some(home) = home {
            add(home.display().to_string(), "<home>".to_string());
        }
        if let Some(device_id) = device_id.map(str::trim) {
            add(device_id.to_string(), "<device>".to_string());
        }
        exact.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        Self { exact }
    }

    fn replace_exact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (needle, tag) in &self.exact {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), tag);
            }
        }
        out
    }

    /// One line of a log: `<time> [LEVEL] (target): Message. key=value ...`.
    /// The value of a field that holds a path is redacted whole; in
    /// every other part, each word that looks like a path is.
    pub fn log_line(&self, line: &str) -> String {
        let line = self.replace_exact(line);
        let fields = field_starts(&line);
        let Some(&(first, _)) = fields.first() else {
            return redact_words(&line);
        };
        let mut out = redact_words(&line[..first]);
        for (index, &(start, value_start)) in fields.iter().enumerate() {
            let end = fields.get(index + 1).map_or(line.len(), |next| next.0);
            let key = &line[start + 1..value_start - 1];
            let value = &line[value_start..end];
            out.push_str(&line[start..value_start]);
            if holds_a_path(key) {
                out.push_str(&redact_path(value));
            } else {
                out.push_str(&redact_words(value));
            }
        }
        out
    }

    /// A JSON document: `vapor.json`, or what the daemon answered.
    pub fn json(&self, value: &mut Value) {
        self.json_under(None, value);
    }

    fn json_under(&self, key: Option<&str>, value: &mut Value) {
        match value {
            Value::String(text) => {
                let replaced = self.replace_exact(text);
                *text = if key.is_some_and(holds_a_path) {
                    redact_path(&replaced)
                } else {
                    redact_words(&replaced)
                };
            }
            Value::Array(items) => {
                for item in items {
                    self.json_under(key, item);
                }
            }
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    self.json_under(Some(key.as_str()), item);
                }
            }
            _ => {}
        }
    }
}

/// Whether a field of this name holds a path or a file name: `path`,
/// `local_root`, `remotePath`, `watch_root`, `directory`, `fileName`.
fn holds_a_path(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let key = key.as_str();
    [
        "path",
        "root",
        "directory",
        "dir",
        "folder",
        "file",
        "filename",
        "file_name",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix))
        || matches!(
            key,
            "remote"
                | "existing"
                | "copy"
                | "target"
                | "source"
                | "destination"
                | "original"
                | "wanted"
                | "local"
                | "cloud"
                | "staging"
                | "from"
                | "to"
        )
}

/// The ` key=` positions of a log line: `(index of the space, index of
/// the value)`. A key is a lower-case identifier, the shape the logger
/// writes.
fn field_starts(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut starts = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b' ' {
            let mut end = index + 1;
            while end < bytes.len()
                && (bytes[end].is_ascii_lowercase()
                    || bytes[end] == b'_'
                    || bytes[end].is_ascii_digit())
            {
                end += 1;
            }
            let has_key = end > index + 1 && bytes[index + 1].is_ascii_lowercase();
            if has_key && end < bytes.len() && bytes[end] == b'=' {
                starts.push((index, end + 1));
                index = end;
            }
        }
        index += 1;
    }
    starts
}

/// Redacts each word of `text` that looks like a path, and leaves the
/// rest as it is.
fn redact_words(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            if !word.contains('/') && !word.contains('\\') {
                return word.to_string();
            }
            let core = word.trim_matches(WRAPPERS);
            if core.is_empty() {
                return word.to_string();
            }
            let start = word.find(core).unwrap_or(0);
            format!(
                "{}{}{}",
                &word[..start],
                redact_path(core),
                &word[start + core.len()..]
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Redacts every component of a path. The separators stay.
fn redact_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut component = String::new();
    for character in path.chars() {
        if character == '/' || character == '\\' {
            out.push_str(&redact_component(&component));
            component.clear();
            out.push(character);
        } else {
            component.push(character);
        }
    }
    out.push_str(&redact_component(&component));
    out
}

/// One file or folder name as a stable code, with its extension. A tag
/// this module wrote, a name that is part of a system's layout, and a
/// name Vapor makes itself stay as they are.
fn redact_component(name: &str) -> String {
    let keep = name.is_empty()
        || name == "."
        || name == ".."
        || name == "~"
        || (name.starts_with('<') && name.ends_with('>'))
        || name.starts_with(".vapor")
        || name == ".gitignore"
        || STRUCTURAL_NAMES.contains(&name)
        || name.starts_with("p-") && name.len() >= 10;
    if keep {
        return name.to_string();
    }
    let extension = name
        .rfind('.')
        .filter(|index| *index > 0)
        .map(|index| &name[index..])
        .filter(|extension| {
            (2..=6).contains(&extension.len())
                && extension[1..].chars().all(|c| c.is_ascii_alphanumeric())
        })
        .unwrap_or("");
    let conflict = if name.contains(CONFLICT_MARKER) {
        "~conflict"
    } else {
        ""
    };
    format!("p-{:08x}{conflict}{extension}", code(name))
}

/// A 32-bit code of a name: FNV-1a, so every build gives the same one.
fn code(name: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        let config = VaporConfig {
            local_root: Some("/Users/sam/My Documents/Vapor".to_string()),
            cloud_root: Some("/Users/sam/cloud/Vapor".to_string()),
            ..VaporConfig::default()
        };
        Redactor::new(
            &config,
            Some(Path::new("/Users/sam")),
            Some("sams-mac-k3x9"),
        )
    }

    #[test]
    fn a_log_line_keeps_its_message_and_loses_every_name() {
        let redactor = redactor();
        let line = "1794684141116 [INFO] (vapord): Removed from the cloud. \
                    path=/Users/sam/My Documents/Vapor/tax returns/2025 final.pdf \
                    remote_path=tax returns/2025 final.pdf cause=it was deleted on this device";
        let redacted = redactor.log_line(line);
        assert!(
            redacted.starts_with("1794684141116 [INFO] (vapord): Removed from the cloud. path=<local-root:default>/p-"),
            "{redacted}"
        );
        assert!(
            redacted.ends_with(" cause=it was deleted on this device"),
            "{redacted}"
        );
        for name in ["sam", "Documents", "tax", "returns", "2025", "final"] {
            assert!(!redacted.contains(name), "{name} is still in: {redacted}");
        }
        assert!(redacted.contains(".pdf"), "the extension stays: {redacted}");

        // One name gives one code, in the local path and in the cloud path.
        let local_code = redacted
            .split("path=")
            .nth(1)
            .expect("path")
            .split(' ')
            .next()
            .expect("value");
        let file_code = local_code.rsplit('/').next().expect("file");
        assert!(redacted.matches(file_code).count() >= 2, "{redacted}");
    }

    #[test]
    fn a_path_inside_a_message_or_an_error_is_redacted_where_it_stands() {
        let redactor = redactor();
        let line = "1 [WARNING] (vapord): Could not move a file. \
                    error=cannot rename /Users/sam/cloud/Vapor/notes/plan.md: Permission denied \
                    device=sams-mac-k3x9 attempts=3";
        let redacted = redactor.log_line(line);
        assert!(
            redacted.contains("error=cannot rename <cloud-root:default>/p-"),
            "{redacted}"
        );
        assert!(redacted.contains(".md: Permission denied"), "{redacted}");
        assert!(redacted.contains("device=<device>"), "{redacted}");
        assert!(redacted.contains("attempts=3"), "{redacted}");
        assert!(
            !redacted.contains("plan") && !redacted.contains("notes"),
            "{redacted}"
        );

        // A path outside every root: the home directory is tagged, the
        // layout stays, the names go.
        let outside = redactor.log_line("2 [INFO] (vapor): Wrote. file=/Users/sam/Desktop/x.txt");
        assert!(outside.contains("file=<home>/p-"), "{outside}");
        let elsewhere = redactor.log_line("3 [INFO] (vapor): Read (/Volumes/Backup/photos/a.jpg).");
        assert!(elsewhere.contains("(/Volumes/p-"), "{elsewhere}");
        assert!(
            !elsewhere.contains("Backup") && !elsewhere.contains("photos"),
            "{elsewhere}"
        );
    }

    #[test]
    fn vapors_own_names_and_a_conflict_marker_stay_readable() {
        assert_eq!(
            redact_path("/x/.vapor-root"),
            format!("/{}/.vapor-root", redact_component("x"))
        );
        assert_eq!(redact_component(".vapor-tmp-dl-op42"), ".vapor-tmp-dl-op42");
        let copy = redact_component("report~conflict-sams-mac-k3x9-1700000000000.md");
        assert!(
            copy.starts_with("p-") && copy.ends_with("~conflict.md"),
            "{copy}"
        );
        // Redacting twice changes nothing.
        let once = redact_path("/Users/sam/a b/c.txt");
        assert_eq!(redact_path(&once), once);
    }

    #[test]
    fn a_json_document_loses_paths_in_path_fields_and_in_text() {
        let redactor = redactor();
        let mut document = serde_json::json!({
            "localRoot": "/Users/sam/My Documents/Vapor",
            "deviceId": "sams-mac-k3x9",
            "syncMode": "two-way",
            "profiles": [{ "run": { "state": "running",
                "reason": "watching /Users/sam/My Documents/Vapor" } }],
            "items": [{ "path": "/Users/sam/My Documents/Vapor/a b/c d.txt", "attempts": 2 }],
            "entries": [{ "message": "Decision 3 (conflict) for notes/plan.md" }]
        });
        redactor.json(&mut document);
        assert_eq!(document["localRoot"], "<local-root:default>");
        assert_eq!(document["deviceId"], "<device>");
        assert_eq!(document["syncMode"], "two-way");
        assert_eq!(
            document["profiles"][0]["run"]["reason"],
            "watching <local-root:default>"
        );
        let item = document["items"][0]["path"].as_str().expect("path");
        assert!(
            item.starts_with("<local-root:default>/p-") && item.ends_with(".txt"),
            "{item}"
        );
        assert!(
            !item.contains(' '),
            "a path field is redacted whole: {item}"
        );
        assert_eq!(document["items"][0]["attempts"], 2);
        let message = document["entries"][0]["message"].as_str().expect("message");
        assert!(
            message.starts_with("Decision 3 (conflict) for p-"),
            "{message}"
        );
        assert!(!message.contains("plan"), "{message}");
    }
}
