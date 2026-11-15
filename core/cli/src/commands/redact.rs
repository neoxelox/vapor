//! Path redaction for the support report.
//!
//! A report holds `vapor.json`, the logs, and the daemon's live state.
//! All three name the user's folders and files, and a report attached
//! to a public issue is public. With redaction on (the default), every
//! file and folder name becomes a short code. The code is stable: one
//! name gives one code in every file of a report and in every report,
//! so a maintainer can still follow one file through the logs.
//!
//! The report is read twice. The first pass learns the names: every
//! component of every path it finds. The second pass replaces:
//!
//! - the sync roots, the home directory, and the device id, by a tag
//!   that says what stood there (`<local-root:default>`);
//! - the whole value of a field that holds a path or a name, spaces
//!   included, component by component, by `p-<code>` and the extension;
//! - each word with a path separator in it, the same way;
//! - each learned name where it stands alone in a sentence (a question
//!   that names a file, an error that names it without its folder);
//! - each ignore rule that is not one of the rules Vapor ships with.
//!
//! What can remain, and the report says so: a name that stands alone in
//! a sentence when it is one plain lowercase word (`notes`), which
//! cannot be told from the sentence's own words, or when no path in the
//! report holds it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;
use vapor_shared::config::VaporConfig;
use vapor_shared::constants;

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
    constants::runtime::STATE_DIRECTORY_NAME,
    constants::runtime::LOGS_DIRECTORY_NAME,
    constants::runtime::TRASH_DIRECTORY_NAME,
    constants::runtime::PROFILES_DIRECTORY_NAME,
    constants::runtime::REPORTS_DIRECTORY_NAME,
    constants::runtime::CONFIGURATION_FILE_NAME,
    constants::runtime::SQLITE_DATABASE_FILE_NAME,
    constants::runtime::DAEMON_LOG_FILE_NAME,
    constants::runtime::APP_LOG_FILE_NAME,
    constants::ipc::SOCKET_FILE_NAME,
    constants::runtime::LIFECYCLE_STATE_FILE_NAME,
    constants::runtime::TRASH_ENTRY_META_FILE_NAME,
];

/// Marker of a keep-both conflict copy; kept so a redacted name still
/// says what it is.
const CONFLICT_MARKER: &str = "~conflict-";

/// The punctuation a sentence puts around a path.
const WRAPPERS: &[char] = &['"', '\'', '`', '(', ')', '[', ']', ',', ';', ':'];

/// The punctuation a sentence puts after a name.
const SENTENCE_ENDS: &[char] = &['.', '?', '!'];

/// The programs whose `name/version` strings say which build ran.
const BUILD_NAMES: &[&str] = &[
    constants::runtime::DAEMON_BINARY_NAME,
    constants::runtime::CLI_BINARY_NAME,
    "vapor-cli",
];

pub struct Redactor {
    /// Exact strings and what replaces them, longest first, so a root
    /// inside the home directory is tagged as the root.
    exact: Vec<(String, String)>,
    /// The names the first pass found in paths. One of them is
    /// replaced where it stands alone in a sentence too.
    names: HashSet<String>,
    /// The learned names with a space in them, by their first word,
    /// longest first.
    spaced: HashMap<String, Vec<String>>,
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
        Self {
            exact,
            names: HashSet::new(),
            spaced: HashMap::new(),
        }
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

    /// First pass over one line of a log: remembers the names in it.
    pub fn learn_log_line(&mut self, line: &str) {
        let line = self.replace_exact(line);
        let fields = field_starts(&line);
        let message_end = fields.first().map_or(line.len(), |first| first.0);
        self.learn_sentence(&line[..message_end]);
        for (index, &(start, value_start)) in fields.iter().enumerate() {
            let end = fields.get(index + 1).map_or(line.len(), |next| next.0);
            let value = &line[value_start..end];
            if holds_a_path(&line[start + 1..value_start - 1]) {
                self.learn_path(value);
            } else {
                self.learn_sentence(value);
            }
        }
    }

    /// First pass over a JSON document: remembers the names in it.
    pub fn learn_json(&mut self, value: &Value) {
        self.learn_json_under(None, value);
    }

    fn learn_json_under(&mut self, key: Option<&str>, value: &Value) {
        match value {
            Value::String(text) => {
                let text = self.replace_exact(text);
                match key {
                    Some(key) if holds_ignore_rules(key) => {}
                    Some(key) if holds_a_path(key) => self.learn_path(&text),
                    _ => self.learn_sentence(&text),
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.learn_json_under(key, item);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    self.learn_json_under(Some(key.as_str()), item);
                }
            }
            _ => {}
        }
    }

    /// Remembers the names of the paths that a sentence holds.
    fn learn_sentence(&mut self, text: &str) {
        for word in text.split(' ') {
            let core = word
                .trim_start_matches(WRAPPERS)
                .trim_end_matches(|c| WRAPPERS.contains(&c) || SENTENCE_ENDS.contains(&c));
            if is_path_word(core) && !core.contains("://") {
                self.learn_path(core);
            }
        }
    }

    fn learn_path(&mut self, path: &str) {
        for name in path.split(['/', '\\']) {
            // A name of one or two characters says next to nothing, and
            // would be found inside every other word.
            if stays_as_it_is(name) || name.chars().count() < 3 {
                continue;
            }
            if !self.names.insert(name.to_string()) {
                continue;
            }
            let Some((first, _)) = name.split_once(' ') else {
                continue;
            };
            if !first.is_empty() {
                let names = self.spaced.entry(first.to_string()).or_default();
                names.push(name.to_string());
                names.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
            }
        }
    }

    /// One line of a log: `<time> [LEVEL] (target): Message. key=value ...`.
    /// The value of a field that holds a path is redacted whole; every
    /// other part is redacted as a sentence.
    pub fn log_line(&self, line: &str) -> String {
        let line = self.replace_exact(line);
        let fields = field_starts(&line);
        let Some(&(first, _)) = fields.first() else {
            return self.sentence(&line);
        };
        let mut out = self.sentence(&line[..first]);
        for (index, &(start, value_start)) in fields.iter().enumerate() {
            let end = fields.get(index + 1).map_or(line.len(), |next| next.0);
            let key = &line[start + 1..value_start - 1];
            let value = &line[value_start..end];
            out.push_str(&line[start..value_start]);
            if holds_a_path(key) {
                out.push_str(&redact_path(value));
            } else {
                out.push_str(&self.sentence(value));
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
                *text = match key {
                    Some(key) if holds_ignore_rules(key) => {
                        if constants::filtering::DEFAULT_PRE_IGNORE_RULES.contains(&text.as_str()) {
                            replaced
                        } else {
                            redact_path(&replaced)
                        }
                    }
                    Some(key) if holds_a_path(key) => redact_path(&replaced),
                    _ => self.sentence(&replaced),
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

    /// Redacts a sentence: each word that is a path, and each learned
    /// name that stands alone. The rest stays as it is.
    fn sentence(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix(' ') {
                out.push(' ');
                rest = after;
                continue;
            }
            let word = &rest[..rest.find(' ').unwrap_or(rest.len())];
            let lead = word.len() - word.trim_start_matches(WRAPPERS).len();
            if let Some(name) = self.spaced_name_at(&rest[lead..]) {
                out.push_str(&rest[..lead]);
                out.push_str(&redact_component(name));
                rest = &rest[lead + name.len()..];
                continue;
            }
            out.push_str(&self.word(word));
            rest = &rest[word.len()..];
        }
        out
    }

    /// The learned name with spaces in it that `text` starts with.
    fn spaced_name_at(&self, text: &str) -> Option<&str> {
        let first = text.split(' ').next()?;
        self.spaced
            .get(first)?
            .iter()
            .map(String::as_str)
            .find(|name| {
                text.strip_prefix(*name).is_some_and(|after| {
                    after.chars().next().is_none_or(|next| {
                        next == ' ' || WRAPPERS.contains(&next) || SENTENCE_ENDS.contains(&next)
                    })
                })
            })
    }

    fn word(&self, word: &str) -> String {
        let start = word.len() - word.trim_start_matches(WRAPPERS).len();
        let body = &word[start..];
        let rebuilt = |from: &str, to: String| {
            format!("{}{to}{}", &word[..start], &word[start + from.len()..])
        };
        if body.contains('/') || body.contains('\\') {
            // The sentence's own punctuation after the path: `a.jpg)`,
            // and `a.jpg).` with the full stop after the bracket.
            let closed = body.trim_end_matches(WRAPPERS);
            let core = if closed.len() < body.len() {
                closed
            } else {
                let before_stop = body.trim_end_matches(SENTENCE_ENDS);
                let closed = before_stop.trim_end_matches(WRAPPERS);
                if closed.len() < before_stop.len() {
                    closed
                } else {
                    body
                }
            };
            return if !is_path_word(core) {
                word.to_string()
            } else if core.contains("://") {
                rebuilt(core, redact_url(core))
            } else {
                rebuilt(core, redact_path(core))
            };
        }
        // A name alone in a sentence, with or without the sentence's
        // punctuation after it.
        let mut name = body;
        loop {
            if self.names.contains(name) && !is_a_plain_word(name) {
                return rebuilt(name, redact_component(name));
            }
            match name.char_indices().next_back() {
                Some((index, last))
                    if index > 0 && (WRAPPERS.contains(&last) || SENTENCE_ENDS.contains(&last)) =>
                {
                    name = &name[..index];
                }
                _ => return word.to_string(),
            }
        }
    }
}

/// Whether a field of this name holds a path or a file name: `path`,
/// `local_root`, `remotePath`, `watch_root`, `directory`, `fileName`,
/// `conflict_copy`, `name`.
fn holds_a_path(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    // `paths`, `roots`, `names`: a list of them.
    let key = key
        .strip_suffix('s')
        .filter(|one| {
            ["path", "root", "name"]
                .iter()
                .any(|kind| one.ends_with(kind))
        })
        .unwrap_or(key.as_str());
    [
        "path",
        "root",
        "directory",
        "dir",
        "folder",
        "file",
        "name",
        "copy",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix))
        || matches!(
            key,
            "remote"
                | "existing"
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
                | "canonical"
                | "landed"
                | "subtree"
                | "pattern"
                | "home"
                | "volume_trash"
        )
}

/// Whether a `vapor.json` key holds ignore rules. A rule the user wrote
/// names their files; a rule Vapor ships with says nothing about them.
fn holds_ignore_rules(key: &str) -> bool {
    key == constants::config::KEY_PRE_IGNORE_RULES
        || key == constants::config::KEY_POST_IGNORE_RULES
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

/// Whether a word with a separator in it is a path. A count (`2/2`), a
/// byte range (`0-262143/1000000`), a rate (`kbit/s`), and the name of
/// a Vapor build (`vapord/0.2.0`) are not, and a maintainer needs them.
fn is_path_word(word: &str) -> bool {
    if !word.contains('/') && !word.contains('\\') {
        return false;
    }
    let parts: Vec<&str> = word.split(['/', '\\']).collect();
    let numbers_only = parts.iter().all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | '*' | '.'))
    });
    if numbers_only {
        return false;
    }
    if let [first, second] = parts.as_slice() {
        let rate = matches!(*second, "s" | "sec" | "min" | "h")
            && (1..=6).contains(&first.len())
            && first.chars().all(|c| c.is_ascii_alphabetic());
        let build = BUILD_NAMES.contains(first)
            && second.starts_with(|c: char| c.is_ascii_digit())
            && second.contains('.');
        if rate || build {
            return false;
        }
    }
    true
}

/// A URL with its host and the fixed words of its path kept, and every
/// other part of the path as a code: an object id, a file name. The
/// query goes: it can hold a session id.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return redact_path(url);
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, _)) => (rest, "?<query>"),
        None => (rest, ""),
    };
    let mut parts = rest.split('/');
    let mut out = format!("{scheme}://{}", parts.next().unwrap_or_default());
    for part in parts {
        out.push('/');
        let fixed = (1..=16).contains(&part.len())
            && part.starts_with(|c: char| c.is_ascii_lowercase())
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if fixed {
            out.push_str(part);
        } else {
            out.push_str(&redact_component(part));
        }
    }
    out.push_str(query);
    out
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

/// Whether `name` is a code this module wrote: `p-` and eight hex
/// digits, then nothing, the conflict mark, or an extension.
fn is_a_code(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("p-") else {
        return false;
    };
    let Some(digits) = rest.get(..8) else {
        return false;
    };
    let after = &rest[8..];
    digits
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        && (after.is_empty() || after.starts_with('.') || after.starts_with("~conflict"))
}

/// A name with nothing in it but lowercase letters: in a sentence it
/// cannot be told from the sentence's own words.
fn is_a_plain_word(name: &str) -> bool {
    name.chars().all(|c| c.is_ascii_lowercase())
}

/// A tag or a code this module wrote, a name that is part of a system's
/// layout, and a name Vapor makes itself: none of them is the user's.
fn stays_as_it_is(name: &str) -> bool {
    name.is_empty()
        || name == "."
        || name == ".."
        || name == "~"
        || (name.starts_with('<') && name.ends_with('>'))
        || name.starts_with(".vapor")
        || name == ".gitignore"
        || STRUCTURAL_NAMES.contains(&name)
        || is_a_code(name)
}

/// One file or folder name as a stable code, with its extension.
fn redact_component(name: &str) -> String {
    if stays_as_it_is(name) {
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

    // The fixtures are Unix paths. The redaction of drive-letter and
    // backslash paths is open work for the Windows surface.
    #[cfg(unix)]
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

    // The fixtures are Unix paths. The redaction of drive-letter and
    // backslash paths is open work for the Windows surface.
    #[cfg(unix)]
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

    #[test]
    fn a_name_alone_in_a_sentence_is_replaced_once_a_path_gave_it() {
        let mut redactor = redactor();
        let path_line = "1 [INFO] (vapord): Kept both versions. \
                         path=/Users/sam/My Documents/Vapor/Tax 2025/final return.pdf \
                         conflict_copy=/Users/sam/My Documents/Vapor/Tax 2025/final return~conflict-sams-mac-k3x9-175.pdf \
                         canonical=/Users/sam/My Documents/Vapor/Tax 2025/Budget.xlsx";
        let question = serde_json::json!({ "entries": [{ "message":
            "Keep only final return.pdf? Answer with `vapor decisions resolve 3 --choose <option>` \
             (final return.pdf | Budget.xlsx)." }] });
        redactor.learn_log_line(path_line);
        redactor.learn_json(&question);

        // Fields that hold a path under a name the first list missed.
        let line = redactor.log_line(path_line);
        for name in ["Tax", "2025", "final", "return", "Budget", "sams-mac"] {
            assert!(!line.contains(name), "{name} is still in: {line}");
        }
        assert!(line.contains("~conflict.pdf"), "{line}");

        // The same names with no folder in front of them.
        let mut document = question.clone();
        redactor.json(&mut document);
        let message = document["entries"][0]["message"].as_str().expect("message");
        for name in ["final", "return", "Budget"] {
            assert!(!message.contains(name), "{name} is still in: {message}");
        }
        assert!(
            message.starts_with("Keep only p-") && message.contains(".pdf? Answer with `vapor"),
            "{message}"
        );
        let file_code = redact_component("final return.pdf");
        assert_eq!(message.matches(&file_code).count(), 2, "{message}");

        // An error that names a file with no folder.
        let bare = redactor
            .log_line("2 [WARNING] (vapord): Failed. error=remote file Budget.xlsx does not exist");
        assert!(
            bare.ends_with(&format!(
                "error=remote file {} does not exist",
                redact_component("Budget.xlsx")
            )),
            "{bare}"
        );
    }

    #[test]
    fn what_a_maintainer_needs_is_not_read_as_a_path() {
        let redactor = redactor();
        for kept in [
            "daemon=vapord/0.2.0-alpha.3",
            "client_id=vapor-cli/0.2.0-alpha.3",
            "reason=upload concurrency 2/2 in use",
            "rate=512 kbit/s",
            "range=bytes 0-262143/1000000",
        ] {
            let line = format!("1 [INFO] (vapord): Tick. {kept}");
            assert_eq!(redactor.log_line(&line), line);
        }
        let mut status = serde_json::json!({ "daemon": "vapord/0.2.0-alpha.3" });
        redactor.json(&mut status);
        assert_eq!(status["daemon"], "vapord/0.2.0-alpha.3");

        // A URL keeps its host and the fixed words of its path; an
        // object id and the query go.
        let line = redactor.log_line(
            "1 [WARNING] (vapord): Failed. error=GET https://www.googleapis.com/drive/v3/files/1AbC_dEfGh?upload_id=xyz: 503",
        );
        assert!(
            line.contains("https://www.googleapis.com/drive/v3/files/p-"),
            "{line}"
        );
        assert!(line.ends_with("?<query>: 503"), "{line}");
        assert!(
            !line.contains("1AbC_dEfGh") && !line.contains("xyz"),
            "{line}"
        );
    }

    #[test]
    fn an_ignore_rule_the_user_wrote_is_replaced_and_a_shipped_one_stays() {
        let redactor = redactor();
        let shipped = constants::filtering::DEFAULT_PRE_IGNORE_RULES[0];
        let mut config = serde_json::json!({
            "preIgnoreRules": [shipped, "Private Diary", "secret-plans/*.md"],
            "postIgnoreRules": ["!Keep This.txt"],
        });
        redactor.json(&mut config);
        assert_eq!(config["preIgnoreRules"][0], shipped);
        let text = config.to_string();
        for name in ["Private", "Diary", "secret", "plans", "Keep"] {
            assert!(!text.contains(name), "{name} is still in: {text}");
        }
    }

    #[test]
    fn only_a_code_this_module_wrote_stays_as_a_code() {
        assert_eq!(redact_component("p-0123abcd.txt"), "p-0123abcd.txt");
        assert_eq!(
            redact_component("p-0123abcd~conflict.md"),
            "p-0123abcd~conflict.md"
        );
        // A name of the user's that only starts the same way.
        for name in ["p-and-l-2025.xlsx", "p-0123abcdef", "p-0123ABCD"] {
            assert_ne!(redact_component(name), name);
        }
    }
}
