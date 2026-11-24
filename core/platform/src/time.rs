//! The local wall-clock time of the host, for text a person reads.
//!
//! Durable state and the wire carry milliseconds since the Unix epoch;
//! only the last step before a terminal turns one into a local time.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A calendar time in the time zone of the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// `at` in the time zone of the host, or `None` when the host cannot
/// say (no native call on this OS yet, a time out of range). The
/// caller then prints UTC and says so.
pub fn local_time(at: SystemTime) -> Option<LocalTime> {
    let seconds = at.duration_since(UNIX_EPOCH).ok()?.as_secs();
    local_time_impl(i64::try_from(seconds).ok()?)
}

/// `at` in UTC. Never fails for a time at or after the epoch.
pub fn utc_time(at: SystemTime) -> LocalTime {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    LocalTime {
        year,
        month,
        day,
        hour: (of_day / 3_600) as u8,
        minute: (of_day % 3_600 / 60) as u8,
        second: (of_day % 60) as u8,
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian date (the algorithm
/// of Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year as i32, month as u8, day as u8)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn local_time_impl(seconds: i64) -> Option<LocalTime> {
    // `time_t` is 64 bits wide on every target Vapor builds for.
    let seconds: libc::time_t = seconds;
    // SAFETY: `tm` is plain data that `localtime_r` fills in; both
    // pointers are valid for the call, and the reentrant variant keeps
    // no state of its own.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(LocalTime {
        year: tm.tm_year + 1900,
        month: u8::try_from(tm.tm_mon + 1).ok()?,
        day: u8::try_from(tm.tm_mday).ok()?,
        hour: u8::try_from(tm.tm_hour).ok()?,
        minute: u8::try_from(tm.tm_min).ok()?,
        second: u8::try_from(tm.tm_sec).ok()?,
    })
}

#[cfg(not(unix))]
fn local_time_impl(_seconds: i64) -> Option<LocalTime> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn utc_time_reads_known_dates() {
        assert_eq!(
            utc_time(at(0)),
            LocalTime {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0
            }
        );
        // A leap day, and the last second of a year.
        assert_eq!(
            utc_time(at(1_709_210_096)),
            LocalTime {
                year: 2024,
                month: 2,
                day: 29,
                hour: 12,
                minute: 34,
                second: 56
            }
        );
        assert_eq!(
            utc_time(at(1_735_689_599)),
            LocalTime {
                year: 2024,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59,
                second: 59
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_time_is_within_a_day_of_utc() {
        let moment = at(1_709_210_096);
        let local = local_time(moment).expect("the host has a time zone");
        let utc = utc_time(moment);
        // Whatever the zone of the host, the two differ by hours.
        let day_gap = (i32::from(local.day) - i32::from(utc.day)).abs();
        assert!(day_gap <= 1 || day_gap >= 27, "{local:?} against {utc:?}");
        assert_eq!(local.second, utc.second);
    }
}
