use std::env;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

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
    file: Mutex<File>,
}

impl StructuredLogger {
    fn new(component: &'static str, file_name: &str) -> Self {
        let min_level = env::var("VAPOR_LOG_LEVEL")
            .ok()
            .and_then(|value| LogLevel::parse(&value))
            .unwrap_or_else(build_default_level);

        let file_path = logs_directory().join(file_name);
        if let Some(parent) = file_path.parent() {
            let _ = create_dir_all(parent);
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .unwrap_or_else(|error| {
                panic!(
                    "failed to open log file at {}: {}",
                    file_path.display(),
                    error
                )
            });

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
            sanitize_text(message)
        );

        if !metadata.is_empty() {
            line.push(' ');
            for (index, (key, value)) in metadata.iter().enumerate() {
                if index > 0 {
                    line.push(' ');
                }
                line.push_str(&format!("{}={}", sanitize_text(key), sanitize_text(value)));
            }
        }

        line.push('\n');

        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

fn build_default_level() -> LogLevel {
    let configured = option_env!("VAPOR_DEFAULT_LOG_LEVEL").unwrap_or("debug");
    LogLevel::parse(configured).unwrap_or(LogLevel::Debug)
}

fn logs_directory() -> PathBuf {
    if let Some(configured) = env::var_os("VAPOR_LOG_DIR") {
        return PathBuf::from(configured);
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("Vapor");
    }

    PathBuf::from("/tmp/VaporLogs")
}

fn sanitize_text(raw: &str) -> String {
    raw.replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn global() -> &'static StructuredLogger {
    static LOGGER: OnceLock<StructuredLogger> = OnceLock::new();
    let log_file = env::var("VAPOR_DAEMON_LOG_FILE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "vapord.log".to_string());

    LOGGER.get_or_init(|| StructuredLogger::new("vapord", &log_file))
}

pub fn debug(message: &str, metadata: &[(&str, String)]) {
    global().log(LogLevel::Debug, message, metadata)
}

pub fn info(message: &str, metadata: &[(&str, String)]) {
    global().log(LogLevel::Info, message, metadata)
}

pub fn warning(message: &str, metadata: &[(&str, String)]) {
    global().log(LogLevel::Warning, message, metadata)
}

pub fn error(message: &str, metadata: &[(&str, String)]) {
    global().log(LogLevel::Error, message, metadata)
}

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
}
