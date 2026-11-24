//! Times in the text a person reads: the local time of a moment, and
//! how far away a moment is. `--json` output keeps the numbers.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `2026-11-23 14:32:10` in the time zone of the host. A host that
/// cannot say its zone gets the UTC time, marked as such.
pub fn clock(epoch_ms: u64) -> String {
    let at = UNIX_EPOCH + Duration::from_millis(epoch_ms);
    let (time, zone) = match vapor_platform::time::local_time(at) {
        Some(local) => (local, ""),
        None => (vapor_platform::time::utc_time(at), " UTC"),
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{zone}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}

/// How long until `target_ms`, seen from `now_ms`: `now`, `in 30 s`,
/// `in 4 min`, `in 2 h`.
pub fn until(target_ms: u64, now_ms: u64) -> String {
    let wait = Duration::from_millis(target_ms.saturating_sub(now_ms));
    if wait.is_zero() {
        return "now".to_string();
    }
    format!("in {}", vapor_shared::duration_text(wait))
}

/// A log line with its leading time (milliseconds since the Unix
/// epoch, as the log file keeps it) turned into the local time. A line
/// that does not start with such a time is returned as it is.
pub fn log_line(line: &str) -> std::borrow::Cow<'_, str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    // Thirteen digits since 2001; anything shorter is not a time.
    if !(12..=14).contains(&digits) || !line[digits..].starts_with(' ') {
        return std::borrow::Cow::Borrowed(line);
    }
    match line[..digits].parse::<u64>() {
        Ok(epoch_ms) => std::borrow::Cow::Owned(format!("{}{}", clock(epoch_ms), &line[digits..])),
        Err(_) => std::borrow::Cow::Borrowed(line),
    }
}

/// The current time in milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_is_said_in_the_unit_a_person_would_use() {
        assert_eq!(until(1_000, 5_000), "now");
        assert_eq!(until(31_000, 1_000), "in 30 s");
        assert_eq!(until(241_000, 1_000), "in 4 min");
        assert_eq!(until(900_000, 0), "in 15 min");
        assert_eq!(until(7_200_000, 0), "in 2 h");
    }

    #[test]
    fn a_log_line_gets_the_local_time_and_other_lines_stay() {
        let line = log_line("1709210096000 [INFO] (vapord): Updated run state");
        assert!(
            line.ends_with(" [INFO] (vapord): Updated run state"),
            "{line}"
        );
        assert!(!line.starts_with("1709210096000"), "{line}");
        assert_eq!(log_line("  a continuation line"), "  a continuation line");
        assert_eq!(log_line("42 files"), "42 files");
    }

    #[test]
    fn a_clock_time_has_a_date_and_a_time_of_day() {
        let text = clock(1_709_210_096_000);
        // The zone of the host decides the hour; the shape is fixed.
        assert_eq!(text.trim_end_matches(" UTC").len(), 19, "{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[13..14], ":");
    }
}
