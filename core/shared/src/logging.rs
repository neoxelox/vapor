use std::env;
use std::fs::{File, OpenOptions};
use std::io::{Write, stderr};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{constants, runtime_paths};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    fn as_label(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }

    fn parse(input: &str) -> Option<Self> {
        match input.to_ascii_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warning" | "warn" => Some(Self::Warning),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// The open log file plus enough state to size-rotate it without a
/// per-line `stat`.
struct LogSink {
    file: File,
    path: PathBuf,
    written: u64,
}

pub struct StructuredLogger {
    component: &'static str,
    min_level: LogLevel,
    sink: Mutex<Option<LogSink>>,
}

impl StructuredLogger {
    pub fn new(component: &'static str, file_name: &str) -> Self {
        let min_level = env::var(constants::env::VAPOR_LOG_LEVEL)
            .ok()
            .and_then(|value| LogLevel::parse(&value))
            .unwrap_or_else(build_default_level);

        let file_path = logs_directory().join(file_name);
        let sink = open_log_file(&file_path)
            .map_err(|error| {
                let _ = writeln!(
                    stderr(),
                    "vapor logging fallback: failed to open {}: {}",
                    file_path.display(),
                    error
                );
                error
            })
            .ok()
            .map(|file| {
                let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                LogSink {
                    file,
                    path: file_path,
                    written,
                }
            });

        Self {
            component,
            min_level,
            sink: Mutex::new(sink),
        }
    }

    pub fn log(&self, level: LogLevel, message: &str, metadata: &[(&str, String)]) {
        if level < self.min_level {
            return;
        }

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();

        let mut line = format!(
            "{} [{}] ({}): {}.",
            now_ms,
            level.as_label(),
            sanitize_text(self.component),
            sanitize_message(message)
        );

        if !metadata.is_empty() {
            line.push(' ');
            for (index, (key, value)) in metadata.iter().enumerate() {
                if index > 0 {
                    line.push(' ');
                }
                line.push_str(&format!(
                    "{}={}",
                    sanitize_text(key),
                    sanitize_metadata_value(key, value)
                ));
            }
        }

        line.push('\n');

        if let Ok(mut guard) = self.sink.lock() {
            if let Some(sink) = guard.as_mut() {
                if sink.file.write_all(line.as_bytes()).is_ok() {
                    let _ = sink.file.flush();
                    sink.written += line.len() as u64;
                    if sink.written >= constants::runtime::LOG_FILE_MAX_BYTES {
                        rotate_sink(sink);
                    }
                }
            } else {
                let _ = stderr().write_all(line.as_bytes());
            }
        }
    }
}

/// Size-rotates a full log file: shifts `<name>.{N-1}` → `<name>.N`
/// (dropping the oldest), moves the live file to `<name>.1`, and reopens
/// a fresh live file. A failure to reopen drops the sink to stderr rather
/// than losing the logger entirely.
fn rotate_sink(sink: &mut LogSink) {
    let generations = constants::runtime::LOG_FILE_GENERATIONS;
    if generations == 0 {
        // No generations kept: just truncate in place.
        if let Ok(file) = reopen_truncated(&sink.path) {
            sink.file = file;
            sink.written = 0;
        }
        return;
    }
    // Drop the oldest kept generation, then cascade the rest down.
    let _ = std::fs::remove_file(rotated_path(&sink.path, generations));
    for generation in (1..generations).rev() {
        let _ = std::fs::rename(
            rotated_path(&sink.path, generation),
            rotated_path(&sink.path, generation + 1),
        );
    }
    if std::fs::rename(&sink.path, rotated_path(&sink.path, 1)).is_err() {
        return;
    }
    match open_log_file(&sink.path) {
        Ok(file) => {
            sink.file = file;
            sink.written = 0;
        }
        Err(error) => {
            let _ = writeln!(
                stderr(),
                "vapor logging: failed to reopen {} after rotation: {error}",
                sink.path.display()
            );
        }
    }
}

/// Truncates a service-manager stdout/stderr redirect file at daemon
/// startup if it has grown past the size cap. These are held open by
/// launchd/systemd, so — unlike the structured log — we truncate in
/// place: the service's fd stays valid and its `O_APPEND` writes resume
/// from the new (zero) EOF, whereas renaming the inode would leave the
/// service writing into the rotated-away file forever.
pub fn trim_redirect_log_if_oversized(path: &std::path::Path) {
    let over_cap = std::fs::metadata(path)
        .map(|metadata| metadata.len() >= constants::runtime::LOG_FILE_MAX_BYTES)
        .unwrap_or(false);
    if over_cap && let Err(error) = OpenOptions::new().write(true).truncate(true).open(path) {
        let _ = writeln!(
            stderr(),
            "vapor logging: could not trim redirect log {}: {error}",
            path.display()
        );
    }
}

/// `<path>.<generation>` — the rotated-generation file name.
fn rotated_path(path: &std::path::Path, generation: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

fn reopen_truncated(path: &std::path::Path) -> std::io::Result<File> {
    runtime_paths::ensure_private_file(path)?;
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

pub struct GlobalComponentLogger {
    component: &'static str,
    file_name: &'static str,
    logger: OnceLock<StructuredLogger>,
}

impl GlobalComponentLogger {
    pub const fn new(component: &'static str, file_name: &'static str) -> Self {
        Self {
            component,
            file_name,
            logger: OnceLock::new(),
        }
    }

    pub fn log(&self, level: LogLevel, message: &str, metadata: &[(&str, String)]) {
        self.global().log(level, message, metadata)
    }

    pub fn debug(&self, message: &str, metadata: &[(&str, String)]) {
        self.log(LogLevel::Debug, message, metadata)
    }

    pub fn info(&self, message: &str, metadata: &[(&str, String)]) {
        self.log(LogLevel::Info, message, metadata)
    }

    pub fn warning(&self, message: &str, metadata: &[(&str, String)]) {
        self.log(LogLevel::Warning, message, metadata)
    }

    pub fn error(&self, message: &str, metadata: &[(&str, String)]) {
        self.log(LogLevel::Error, message, metadata)
    }

    fn global(&self) -> &StructuredLogger {
        self.logger
            .get_or_init(|| StructuredLogger::new(self.component, self.file_name))
    }
}

fn build_default_level() -> LogLevel {
    match env::var(constants::env::VAPOR_ENV)
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("dev") => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

fn logs_directory() -> PathBuf {
    runtime_paths::logs_directory()
}

fn open_log_file(path: &std::path::Path) -> std::io::Result<File> {
    runtime_paths::ensure_private_file(path)?;
    OpenOptions::new().create(true).append(true).open(path)
}

fn sanitize_text(raw: &str) -> String {
    raw.replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

pub fn sanitize_diagnostic_text(raw: &str) -> String {
    redact_inline_secrets(sanitize_text(raw))
}

fn sanitize_message(raw: &str) -> String {
    sanitize_diagnostic_text(raw)
}

fn sanitize_metadata_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) {
        return "[REDACTED]".to_string();
    }

    sanitize_diagnostic_text(value)
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SENSITIVE_KEY_MARKERS
        .iter()
        .any(|marker| key.contains(marker))
}

fn redact_inline_secrets(raw: String) -> String {
    let lower = raw.to_ascii_lowercase();
    if INLINE_SECRET_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        "[REDACTED]".to_string()
    } else {
        raw
    }
}

const SENSITIVE_KEY_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "auth_header",
    "authorization",
    "client_secret",
    "cookie",
    "credential",
    "keychain",
    "oauth",
    "password",
    "refresh",
    "secret",
    "session",
    "token",
];

const INLINE_SECRET_MARKERS: &[&str] = &[
    "access_token=",
    "api_key=",
    "api-key:",
    "apikey=",
    "authorization:",
    "bearer ",
    "client_secret=",
    "id_token=",
    "password=",
    "refresh_token=",
    "secret=",
    "session=",
    "set-cookie:",
    "token=",
    "x-api-key:",
    // JSON colon-quote forms (`"access_token": "ya29..."`): the `=`/`:`
    // markers above miss a token embedded in a JSON body, so one careless
    // log of an OAuth response body would leak verbatim.
    "\"access_token\"",
    "\"refresh_token\"",
    "\"id_token\"",
    "\"client_secret\"",
    "\"password\"",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_expected_log_levels() {
        assert_eq!(LogLevel::parse("debug"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse("warn"), Some(LogLevel::Warning));
        assert_eq!(LogLevel::parse("error"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("verbose"), None);
    }

    #[test]
    fn sanitizes_control_characters() {
        let sanitized = sanitize_text("a\tb\nc\r");
        assert_eq!(sanitized, "a\\tb\\nc\\r");
    }

    #[test]
    fn redacts_sensitive_metadata_values() {
        assert_eq!(
            sanitize_metadata_value("auth_token", "secret-value"),
            "[REDACTED]"
        );
        assert_eq!(
            sanitize_metadata_value("note", "Bearer abc123"),
            "[REDACTED]"
        );
    }

    #[test]
    fn redacts_inline_secret_shapes_for_common_auth_patterns() {
        let redacted_markers = [
            "request body: access_token=abc123",
            "replied with Set-Cookie: session=xyz",
            "api_key=secret-123",
            "X-Api-Key: keep-private",
            "client_secret=shhh",
            "response: refresh_token=rotate-me",
            "request header Authorization: Bearer xyz",
            "header authorization: basic abc",
            "cached id_token=value",
        ];
        for input in redacted_markers {
            assert_eq!(
                sanitize_diagnostic_text(input),
                "[REDACTED]",
                "input should be redacted: {input}"
            );
        }

        assert_eq!(
            sanitize_diagnostic_text("routine event without secrets"),
            "routine event without secrets"
        );
    }

    #[test]
    fn redacts_json_shaped_token_bodies() {
        for input in [
            r#"token response: {"access_token": "ya29.abc", "expires_in": 3600}"#,
            r#"{"refresh_token":"1//rotate"}"#,
            r#"{"client_secret": "shhh"}"#,
        ] {
            assert_eq!(
                sanitize_diagnostic_text(input),
                "[REDACTED]",
                "JSON token body should be redacted: {input}"
            );
        }
    }

    #[test]
    fn metadata_keys_with_auth_related_markers_are_sensitive() {
        for key in [
            "api_key",
            "apiKey",
            "client_secret",
            "refresh_token",
            "session_id",
            "OAuth-State",
        ] {
            assert_eq!(
                sanitize_metadata_value(key, "value-should-not-leak"),
                "[REDACTED]",
                "key {key} should force metadata redaction"
            );
        }
    }

    #[test]
    fn opening_log_file_gracefully_fails_for_directory_path() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let directory_path = temp_dir.path().join("logs-as-directory");
        std::fs::create_dir_all(&directory_path).expect("create directory path");

        let result = open_log_file(&directory_path);

        assert!(result.is_err());
    }

    fn open_sink(path: &std::path::Path) -> LogSink {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open");
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        LogSink {
            file,
            path: path.to_path_buf(),
            written,
        }
    }

    #[test]
    fn rotation_moves_the_full_file_aside_and_reopens_empty() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("vapord.logs");
        std::fs::write(&path, b"first generation\n").expect("seed");

        let mut sink = open_sink(&path);
        rotate_sink(&mut sink);

        assert_eq!(sink.written, 0, "reopened live file starts empty");
        assert_eq!(std::fs::read(&path).expect("live"), b"");
        assert_eq!(
            std::fs::read(rotated_path(&path, 1)).expect(".1"),
            b"first generation\n"
        );
    }

    #[test]
    fn rotation_cascades_generations_and_drops_the_oldest() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("vapord.logs");
        let generations = constants::runtime::LOG_FILE_GENERATIONS;

        // Rotate one more time than we keep, tagging each live file so we
        // can tell which generation survived.
        for round in 0..=generations {
            std::fs::write(&path, format!("round {round}\n")).expect("seed");
            let mut sink = open_sink(&path);
            rotate_sink(&mut sink);
        }

        // Exactly `generations` rotated files exist; the very first round
        // has aged out.
        assert!(
            !rotated_path(&path, generations + 1).exists(),
            "no generation beyond the cap is kept"
        );
        assert_eq!(
            std::fs::read(rotated_path(&path, 1)).expect(".1"),
            format!("round {generations}\n").into_bytes(),
            "newest rotation is at .1"
        );
        assert_eq!(
            std::fs::read(rotated_path(&path, generations)).expect("oldest kept"),
            b"round 1\n",
            "oldest kept generation is round 1 (round 0 aged out)"
        );
    }
}
