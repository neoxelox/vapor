//! Log hygiene. A healthy run emits no `[ERROR]` line and no warning
//! outside the set a scenario declares it expects.

use std::fs;
use std::path::Path;

/// Warnings a healthy daemon may emit on routine transitions. Empty:
/// a healthy run has a warning budget of zero. Never grows without a
/// comment naming the transition and the task that demotes it.
pub const ROUTINE_WARNINGS: &[&str] = &[];

#[derive(Clone, Debug, Default)]
pub struct LogReport {
    pub errors: Vec<String>,
    pub unexpected_warnings: Vec<String>,
    pub warning_count: usize,
    pub line_count: usize,
}

impl LogReport {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty() && self.unexpected_warnings.is_empty()
    }
}

/// Scans one daemon log. `allowed` are substrings of warning lines the
/// caller expects on top of [`ROUTINE_WARNINGS`]; `allowed_errors` the
/// same for errors (only a scenario that provokes an error uses it).
pub fn scan(path: &Path, allowed: &[String], allowed_errors: &[String]) -> LogReport {
    let mut report = LogReport::default();
    let Ok(contents) = fs::read_to_string(path) else {
        return report;
    };
    for line in contents.lines() {
        report.line_count += 1;
        if line.contains("[ERROR]") {
            if !allowed_errors
                .iter()
                .any(|pattern| line.contains(pattern.as_str()))
            {
                report.errors.push(line.to_string());
            }
        } else if line.contains("[WARNING]") {
            report.warning_count += 1;
            let routine = ROUTINE_WARNINGS
                .iter()
                .any(|pattern| line.contains(pattern));
            let expected = allowed
                .iter()
                .any(|pattern| line.contains(pattern.as_str()));
            if !routine && !expected {
                report.unexpected_warnings.push(line.to_string());
            }
        }
    }
    report
}

/// Last `n` lines of a file, for diagnostics.
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..]
        .iter()
        .map(|line| (*line).to_string())
        .collect()
}

/// `true` when any line of the file contains `needle`.
pub fn contains(path: &Path, needle: &str) -> bool {
    fs::read_to_string(path)
        .map(|contents| contents.lines().any(|line| line.contains(needle)))
        .unwrap_or(false)
}

/// The last line containing `needle`, if any.
pub fn last_line_containing(path: &Path, needle: &str) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    contents
        .lines()
        .rfind(|line| line.contains(needle))
        .map(str::to_string)
}
