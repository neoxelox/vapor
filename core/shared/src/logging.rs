use std::env;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const VAPOR_DIR_ENV_KEY: &str = "VAPOR_DIR";
const VAPOR_ENV_ENV_KEY: &str = "VAPOR_ENV";
const VAPOR_LOG_LEVEL_ENV_KEY: &str = "VAPOR_LOG_LEVEL";
const LOGS_DIRECTORY_NAME: &str = "logs";
const VAPOR_DIRECTORY_NAME: &str = ".vapor";

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
    pub fn new(component: &'static str, file_name: &str) -> Self {
        let min_level = env::var(VAPOR_LOG_LEVEL_ENV_KEY)
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
    match env::var(VAPOR_ENV_ENV_KEY)
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("dev") => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

fn logs_directory() -> PathBuf {
    vapor_directory().join(LOGS_DIRECTORY_NAME)
}

fn vapor_directory() -> PathBuf {
    if let Some(configured) = env::var_os(VAPOR_DIR_ENV_KEY) {
        return PathBuf::from(configured);
    }

    if (env::var_os("CI").is_some()
        || env::var(VAPOR_ENV_ENV_KEY)
            .ok()
            .map(|value| value.eq_ignore_ascii_case("dev"))
            .unwrap_or(false))
        && let Ok(current_directory) = env::current_dir()
    {
        return current_directory.join(VAPOR_DIRECTORY_NAME);
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(VAPOR_DIRECTORY_NAME);
    }

    PathBuf::from("/tmp").join(VAPOR_DIRECTORY_NAME)
}

fn sanitize_text(raw: &str) -> String {
    raw.replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
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
