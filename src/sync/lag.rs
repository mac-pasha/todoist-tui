//! Full-sync lag detection with immediate incremental follow-up (T-508, EPIC-06).

#![allow(dead_code)]

/// Default lag threshold in seconds (5 minutes).
pub const DEFAULT_LAG_THRESHOLD_SECS: i64 = 300;

/// Parses an ISO 8601 / RFC 3339 UTC timestamp into a unix epoch in seconds.
///
/// Pure-Rust parser supporting standard forms like `2026-11-09T12:00:00Z` or `2026-11-09T12:00:00.000Z`.
pub fn parse_iso_to_unix_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }

    let year: i64 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4)? != &b'-' {
        return None;
    }
    let month: i64 = s.get(5..7)?.parse().ok()?;
    if s.as_bytes().get(7)? != &b'-' {
        return None;
    }
    let day: i64 = s.get(8..10)?.parse().ok()?;
    if s.as_bytes().get(10)? != &b'T' {
        return None;
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    if s.as_bytes().get(13)? != &b':' {
        return None;
    }
    let min: i64 = s.get(14..16)?.parse().ok()?;
    if s.as_bytes().get(16)? != &b':' {
        return None;
    }
    let sec: i64 = s.get(17..19)?.parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let mut days = (year - 1970) * 365;
    // Add leap days between 1970 and year
    for y in 1970..year {
        if is_leap_year(y) {
            days += 1;
        }
    }

    let month_idx = usize::try_from(month - 1).ok()?;
    days += DAYS_BEFORE_MONTH[month_idx];
    if month > 2 && is_leap_year(year) {
        days += 1;
    }
    days += day - 1;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

const DAYS_BEFORE_MONTH: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

const fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

/// Detects whether a response with `full_sync: true` is lagged beyond `threshold_secs`.
///
/// Returns `true` if `full_sync` is `true`, `full_sync_date_utc` is present and valid,
/// and `now_epoch_secs - parsed_timestamp > threshold_secs`.
///
/// Missing or unparseable `full_sync_date_utc` returns `false` and does not error.
#[must_use]
pub fn detect_lag(
    full_sync: bool,
    full_sync_date_utc: Option<&str>,
    now_epoch_secs: i64,
    threshold_secs: i64,
) -> bool {
    if !full_sync {
        return false;
    }

    let Some(date_str) = full_sync_date_utc else {
        return false;
    };

    match parse_iso_to_unix_secs(date_str) {
        Some(date_secs) => {
            let age = now_epoch_secs.saturating_sub(date_secs);
            age > threshold_secs
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_timestamps() {
        let ts = parse_iso_to_unix_secs("1970-01-01T00:00:00Z").unwrap();
        assert_eq!(ts, 0);

        let ts2 = parse_iso_to_unix_secs("2026-11-09T12:00:00Z").unwrap();
        assert!(ts2 > 0);
    }

    #[test]
    fn detects_lag_beyond_threshold() {
        let date = "2026-11-09T12:00:00Z";
        let date_secs = parse_iso_to_unix_secs(date).unwrap();

        // Within 5 minutes
        assert!(!detect_lag(true, Some(date), date_secs + 200, 300));

        // Exactly 5 minutes
        assert!(!detect_lag(true, Some(date), date_secs + 300, 300));

        // 5 minutes and 1 second -> lag!
        assert!(detect_lag(true, Some(date), date_secs + 301, 300));

        // Not full sync -> no lag even if timestamp is old
        assert!(!detect_lag(false, Some(date), date_secs + 1000, 300));

        // Missing timestamp -> no lag
        assert!(!detect_lag(true, None, date_secs + 1000, 300));

        // Malformed timestamp -> no lag
        assert!(!detect_lag(true, Some("invalid"), date_secs + 1000, 300));
    }
}
