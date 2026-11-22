//! Time — the two timestamps ATLAS stores, in the format articles already use.
//!
//! `db.py` leans on `time.strftime`/`time.strptime` in UTC. The standard library
//! here has `SystemTime` but no calendar formatting, and this workspace must
//! stay offline-buildable, so the civil-date conversion is twenty lines of
//! Howard Hinnant's proleptic-Gregorian algorithm instead of a `chrono` crate.
//! The output format is fixed: `%Y-%m-%dT%H:%M:%SZ`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Whole seconds since the Unix epoch. A clock before 1970 gives `0`, not a
/// panic: an archive that cannot state its own age is better than a crash.
pub fn epoch_secs_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_secs() as i64,
        Err(_) => 0,
    }
}

/// `(year, month, day)` from days since the epoch.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Days since the epoch for a civil date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// An epoch second as `2026-11-05T10:00:00Z`, in UTC.
pub fn iso_from_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// `db.py: now_iso()` — UTC, second precision, `Z` suffix.
pub fn now_iso() -> String {
    iso_from_epoch(epoch_secs_now())
}

/// `db.py: iso_ago(hours)` — the same format, `hours` in the past.
pub fn iso_ago(hours: f64) -> String {
    let secs = epoch_secs_now() as f64 - hours * 3_600.0;
    iso_from_epoch(secs.floor() as i64)
}

/// `db.py: _epoch(ts)` — seconds since the epoch, or `0` when it does not parse.
///
/// `db.py` slices the string to 19 characters and runs
/// `time.strptime(text, "%Y-%m-%dT%H:%M:%S")`, which accepts unpadded fields
/// (`2026-1-5T3:4:5` parses there), range-checks them and rejects trailing
/// characters. This does the same three things.
pub fn parse_epoch(ts: &str) -> i64 {
    let head: Vec<char> = ts.chars().take(19).collect();
    let mut i = 0usize;

    fn number(chars: &[char], i: &mut usize, min: usize, max: usize) -> Option<i64> {
        let start = *i;
        let mut value: i64 = 0;
        while *i < chars.len() && *i - start < max {
            match chars[*i].to_digit(10) {
                Some(digit) => {
                    value = value * 10 + i64::from(digit);
                    *i += 1;
                }
                None => break,
            }
        }
        if *i - start < min {
            None
        } else {
            Some(value)
        }
    }

    fn expect(chars: &[char], i: &mut usize, want: char) -> Option<()> {
        if *i < chars.len() && chars[*i] == want {
            *i += 1;
            Some(())
        } else {
            None
        }
    }

    let parsed = (|| {
        let year = number(&head, &mut i, 1, 4)?;
        expect(&head, &mut i, '-')?;
        let month = number(&head, &mut i, 1, 2)?;
        expect(&head, &mut i, '-')?;
        let day = number(&head, &mut i, 1, 2)?;
        expect(&head, &mut i, 'T')?;
        let hour = number(&head, &mut i, 1, 2)?;
        expect(&head, &mut i, ':')?;
        let minute = number(&head, &mut i, 1, 2)?;
        expect(&head, &mut i, ':')?;
        let second = number(&head, &mut i, 1, 2)?;
        if i != head.len()
            || !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
            || !(0..=23).contains(&hour)
            || !(0..=59).contains(&minute)
            || !(0..=61).contains(&second)
        {
            return None;
        }
        Some((year, month, day, hour, minute, second))
    })();

    match parsed {
        Some((year, month, day, hour, minute, second)) => {
            days_from_civil(year, month as u32, day as u32) * 86_400
                + hour * 3_600
                + minute * 60
                + second
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

#[test]
fn now_iso_round_trips_through_parse_epoch() {
    let now = now_iso();
    assert_eq!(now.len(), 20, "YYYY-MM-DDTHH:MM:SSZ");
    assert!(now.ends_with('Z'));
    let secs = parse_epoch(&now);
    assert!(secs > 1_600_000_000, "{} -> {}", now, secs);
    assert_eq!(iso_from_epoch(secs), now);
}


#[test]
fn civil_dates_match_known_values() {
    assert_eq!(iso_from_epoch(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso_from_epoch(1_793_836_800), "2026-11-05T00:00:00Z");
    assert_eq!(iso_from_epoch(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(iso_from_epoch(1_709_164_800), "2024-02-29T00:00:00Z");
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2024, 2, 29), 19_782);
    // iso_ago is the same format, in the past.
    let ago = iso_ago(24.0);
    assert_eq!(ago.len(), 20);
    assert!(parse_epoch(&ago) < parse_epoch(&now_iso()));
}


#[test]
fn parse_epoch_mirrors_strptime() {
    // Unpadded fields parse: Python's strptime accepts them.
    assert_eq!(
        parse_epoch("2026-1-5T3:4:5"),
        parse_epoch("2026-01-05T03:04:05")
    );
    // Only the first 19 characters count.
    assert_eq!(
        parse_epoch("2026-11-05T10:00:00.123456+02:00"),
        parse_epoch("2026-11-05T10:00:00")
    );
    // Anything else is zero, never a panic.
    assert_eq!(parse_epoch(""), 0);
    assert_eq!(parse_epoch("2026-11-05 10:00:00"), 0);
    assert_eq!(parse_epoch("2026-13-05T10:00:00"), 0);
    assert_eq!(parse_epoch("not a date"), 0);
    assert_eq!(
        parse_epoch("2026-11-05T10:00:0"),
        parse_epoch("2026-11-05T10:00:00")
    );
}

}
