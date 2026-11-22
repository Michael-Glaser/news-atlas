//! Calendar arithmetic: days, civil dates and weekdays, and nothing else.
//!
//! This module is the one a reader should be able to check by eye. It holds no
//! parsing, no formatting, no clock and no JSON — four functions over integers,
//! and the two tests below that walk every single day from 1970 to 2100.
//!
//! Decides:
//!
//! * what a leap year is (`is_leap`) and how long a month is (`days_in_month`);
//! * the day number of a civil date, counted from 1970-01-01, and back again
//!   (`days_from_civil`, `civil_from_days`) — Howard Hinnant's algorithm, pure
//!   integer arithmetic, so there is no float and therefore no drift at any
//!   year, including 2100;
//! * the weekday of a day number, with 1970-01-01 as a Thursday (`weekday_idx`).
//!
//! Does not decide: whether a date is *acceptable*. Range checks live with the
//! readers (`crate::parse`), because their error strings are part of the answer.
//!
//! The two algorithms are inverses: for every `d` in the supported span,
//! `days_from_civil(civil_from_days(d)) == d`, which is what
//! `civil_roundtrip_every_day_1970_to_2100` asserts.

pub fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days since 1970-01-01, proleptic Gregorian (Howard Hinnant's algorithm).
/// Pure integer arithmetic: no float, no rounding drift for any year.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11], March = 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `(year, month, day)` of a day count since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// 0 = Monday .. 6 = Sunday. 1970-01-01 was a Thursday.
pub fn weekday_idx(days: i64) -> u32 {
    (((days + 3) % 7) + 7) as u32 % 7
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, days_from_civil, days_in_month, is_leap};

    #[test]
    fn civil_roundtrip_every_day_1970_to_2100() {
        let first = days_from_civil(1970, 1, 1);
        let last = days_from_civil(2100, 12, 31);
        assert_eq!(first, 0);
        let mut prev = civil_from_days(first);
        assert_eq!(prev, (1970, 1, 1));
        let mut d = first;
        while d <= last {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d, "round trip failed at {}", d);
            assert!((1..=12).contains(&m));
            assert!(dd >= 1 && dd <= days_in_month(y, m));
            if d > first {
                // The date must advance by exactly one day: never skip, repeat
                // or land on a non-existent day.
                let (py, pm, pd) = prev;
                let expect = if pd < days_in_month(py, pm) {
                    (py, pm, pd + 1)
                } else if pm < 12 {
                    (py, pm + 1, 1)
                } else {
                    (py + 1, 1, 1)
                };
                assert_eq!((y, m, dd), expect, "date sequence broke at {}", d);
            }
            prev = (y, m, dd);
            d += 1;
        }
    }

    #[test]
    fn month_lengths_and_leap_years() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(days_in_month(2026, 4), 30);
        assert!(is_leap(2024) && !is_leap(1900) && is_leap(2000) && !is_leap(2100));
    }
}
