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

pub struct StructuredLogger {
    component: &'static str,
    min_level: LogLevel,
    file: Mutex<Option<File>>,
}

impl StructuredLogger {
    pub fn new(component: &'static str, file_name: &str) -> Self {
        let min_level = env::var(constants::env::VAPOR_LOG_LEVEL)
            .ok()
            .and_then(|value| LogLevel::parse(&value))
            .unwrap_or_else(build_default_level);

        let file_path = logs_directory().join(file_name);
        let file = open_log_file(&file_path)
            .map_err(|error| {
                let _ = writeln!(
                    stderr(),
                    "vapor logging fallback: failed to open {}: {}",
                    file_path.display(),
                    error
                );
                error
            })
            .ok();

        Self {
            component,
            min_level,
            file: Mutex::new(file),
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

        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            } else {
                let _ = stderr().write_all(line.as_bytes());
            }
        }
    }
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
}
