//! Calendar days as integers: days since 1970-01-01, in UTC.
//!
//! UTC rather than local time keeps the standard library sufficient. A due
//! date therefore turns at UTC midnight, a few hours off local midnight.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The day containing a Unix timestamp.
pub fn day(unix: i64) -> i64 {
    unix.div_euclid(86_400)
}

/// Parses `YYYY-MM-DD`, rejecting dates that do not exist.
pub fn parse(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = &s[r];
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days).contains(&d).then(|| from_civil(y, m, d))
}

/// `YYYY-MM-DD` for a day number.
/// Algorithm from https://howardhinnant.github.io/date_algorithms.html#civil_from_days
pub fn format(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
/// Algorithm from https://howardhinnant.github.io/date_algorithms.html#days_from_civil
fn from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_days() {
        for (s, want) in [
            ("1970-01-01", 0),
            ("1969-12-31", -1),
            ("2000-03-01", 11017),
            ("2026-09-14", 20710),
            ("2028-02-29", 21243),
            ("2100-03-01", 47541),
        ] {
            assert_eq!(parse(s), Some(want), "{s}");
            assert_eq!(format(want), s);
        }
    }

    #[test]
    fn rejects_impossible_and_malformed_dates() {
        for s in [
            "2026-02-29",
            "2100-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "2026-1-01",
            "2026-+1-01",
            "20260101",
            "2026/01/01",
            "",
        ] {
            assert_eq!(parse(s), None, "{s}");
        }
    }

    #[test]
    fn day_of_timestamp() {
        assert_eq!(day(0), 0);
        assert_eq!(day(86_399), 0);
        assert_eq!(day(-1), -1);
        assert_eq!(day(20710 * 86_400 + 5), 20710);
    }
}
