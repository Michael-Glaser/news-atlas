//! Bare numbers as instants: unix epoch seconds, milliseconds, microseconds and
//! nanoseconds, and the one compact `YYYYMMDDHHMM[SS]` shape that a feed also
//! writes as bare digits.
//!
//! Decides: that a number under 10^9 is *not* accepted as an epoch, because it is
//! just as likely a year or a compact date; which magnitude means which unit; and
//! that a 12- or 14-digit number that reads as a real date-time is refused with
//! both readings named rather than silently read as an epoch in milliseconds
//! (`20261231120000` is 2026-12-31 12:00:00 or the year 2612 — pick one, never
//! guess).
//!
//! Does not decide: what "now" is (`crate::args`), whether the result is in the
//! supported span (`crate::parse::finish_epoch`), or anything about zone handling:
//! an epoch is an instant, so no zone is involved and `assumed_utc` is false.
//!
//! A caller that knows the unit says so with a `hint` (`epoch`, `epoch_ms`,
//! `epoch_us`, `epoch_ns`) and is believed exactly.

use crate::calendar::{civil_from_days, days_in_month};
use crate::parse::{finish_epoch, Attempt};

/// Which unit a bare number is in. `Auto` asks this module to decide from the
/// magnitude, and to refuse when two readings are both plausible.
#[derive(Clone, Copy, PartialEq)]
pub enum NumUnit {
    Auto,
    Sec,
    Ms,
    Us,
    Ns,
}

/// A bare number that is also a compact `YYYYMMDDHHMM[SS]` date-time, as the
/// civil reading `2026-12-31 12:00:00`. `None` when the digits cannot be a date.
///
/// A 12- or 14-digit string is read as an epoch in milliseconds by
/// [`try_epoch`], which turns `20261231120000` into the year 2612 and
/// `202612311200` into 1976 — both silently, both wrong. Compact date-times do
/// turn up in feeds, so the ambiguity is refused and named instead of guessed
/// (a structural check on the digits: no table, no language).
fn compact_datetime(value: i64, digits: usize) -> Option<String> {
    let (scale, with_seconds) = match digits {
        12 => (100_000_000i64, false),
        14 => (10_000_000_000i64, true),
        _ => return None,
    };
    let y = value / scale;
    if !(1900..=2999).contains(&y) {
        return None;
    }
    let rest = value - y * scale;
    let (mo, d, hh, mi, ss) = if with_seconds {
        // YYYYMMDDHHMMSS: ten digits after the year.
        (
            rest / 100_000_000,
            (rest / 1_000_000) % 100,
            (rest / 10_000) % 100,
            (rest / 100) % 100,
            rest % 100,
        )
    } else {
        // YYYYMMDDHHMM: eight digits after the year.
        (
            rest / 1_000_000,
            (rest / 10_000) % 100,
            (rest / 100) % 100,
            rest % 100,
            0,
        )
    };
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo as u32) as i64 {
        return None;
    }
    if hh > 23 || mi > 59 || ss > 59 {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y, mo, d, hh, mi, ss
    ))
}

/// A bare number: unix epoch in seconds / ms / µs / ns.
pub(crate) fn try_epoch(t: &str, unit: NumUnit) -> Attempt {
    let b = t.as_bytes();
    let mut i = 0usize;
    let mut neg = false;
    match b.first() {
        Some(b'-') => {
            neg = true;
            i += 1;
        }
        Some(b'+') => i += 1,
        _ => {}
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == int_start {
        return Attempt::Miss;
    }
    if i - int_start > 19 {
        return Attempt::Bad("epoch number has more than 19 digits".to_string());
    }
    let mut value: i64 = 0;
    for k in int_start..i {
        value = match value
            .checked_mul(10)
            .and_then(|v| v.checked_add((b[k] - b'0') as i64))
        {
            Some(v) => v,
            None => return Attempt::Bad("epoch number out of range".to_string()),
        };
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let fstart = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == fstart {
            // Not "digits.fraction" after all: another reader may own it.
            return Attempt::Miss;
        }
    }
    if i != b.len() {
        return Attempt::Miss;
    }
    if neg {
        value = -value;
    }
    let unit = match unit {
        NumUnit::Auto => {
            let mag = value.unsigned_abs();
            if let Some(civil) = compact_datetime(value, i - int_start) {
                // Name both readings and say how to be unambiguous. A hint does
                // not help here: as plain epoch seconds this value is a year past
                // 9999, so "pass hint=epoch" would only move the refusal.
                let secs = value.div_euclid(1_000);
                let (ey, em, ed) = civil_from_days(secs.div_euclid(86_400));
                return Attempt::Bad(format!(
                    "ambiguous bare number ({}): it reads as the date-time {} and as an epoch in milliseconds ({:04}-{:02}-{:02}) - write the date as 2026-12-31T12:00:00Z to be unambiguous",
                    value, civil, ey, em, ed
                ));
            }
            if mag < 1_000_000_000 {
                // A bare 2026 is a year, a bare 20261231 is a compact date and
                // a bare 0 is 1970: refuse to pick one silently.
                return Attempt::Bad(format!(
                    "ambiguous bare number ({}): epoch seconds before 2001, a year, or a compact date - pass hint=\"epoch\" to force seconds",
                    value
                ));
            } else if mag < 100_000_000_000 {
                NumUnit::Sec
            } else if mag < 100_000_000_000_000 {
                NumUnit::Ms
            } else if mag < 100_000_000_000_000_000 {
                NumUnit::Us
            } else {
                NumUnit::Ns
            }
        }
        other => other,
    };
    let (epoch, kind) = match unit {
        NumUnit::Ms => (value.div_euclid(1_000), "epoch_ms"),
        NumUnit::Us => (value.div_euclid(1_000_000), "epoch_us"),
        NumUnit::Ns => (value.div_euclid(1_000_000_000), "epoch_ns"),
        _ => (value, "epoch"),
    };
    finish_epoch(epoch, kind, 0, false, 0)
}
