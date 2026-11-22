//! The ISO-8601 shape, written and read: `2026-12-31T23:59:59Z`.
//!
//! Decides, in both directions:
//!
//! * `iso_utc` — the exact string the archive stores and the UI reads:
//!   `%Y-%m-%dT%H:%M:%SZ`, always UTC, always zero-padded, twenty bytes, with
//!   the seconds taken from the epoch by `rem_euclid` so a negative epoch still
//!   prints a real time of day.
//! * `try_iso` — the ISO/RFC-3339 reader: `YYYY-MM-DD`, a `T` or a space before
//!   the clock, `hh:mm[:ss[.fff]]`, an optional 12-hour marker (`2026-01-01 10:00
//!   PM`, which publishers do write), an optional zone (`Z`, `+02:00`, `+0200`,
//!   `UTC`), and its own refusal messages ("ISO date without a month", "trailing
//!   characters after the timestamp", "unrecognised timezone "XYZ"", …).
//!
//! Does not decide: whether the date is in range (that is `crate::parse`), or
//! which reader runs first. Both directions share one shape, so a string this
//! module writes is a string this module reads back — `round_trip_is_idempotent`
//! asserts exactly that.

use crate::calendar::{civil_from_days, days_from_civil};
use crate::parse::{check_fields, check_time, finish_epoch, Attempt};
use crate::phrases::{apply_meridiem, meridiem_at};
use crate::scanners::{parse_clock, parse_zone, skip_spaces, tail, take_digits};

fn push2(out: &mut String, v: i64) {
    out.push((b'0' + (v / 10) as u8) as char);
    out.push((b'0' + (v % 10) as u8) as char);
}

fn push4(out: &mut String, v: i64) {
    out.push((b'0' + ((v / 1000) % 10) as u8) as char);
    out.push((b'0' + ((v / 100) % 10) as u8) as char);
    out.push((b'0' + ((v / 10) % 10) as u8) as char);
    out.push((b'0' + (v % 10) as u8) as char);
}

/// `%Y-%m-%dT%H:%M:%SZ` — the format the archive stores and the UI reads.
pub fn iso_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let mut secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let h = secs / 3600;
    secs %= 3600;
    let mi = secs / 60;
    let s = secs % 60;
    let mut out = String::with_capacity(20);
    push4(&mut out, y);
    out.push('-');
    push2(&mut out, m as i64);
    out.push('-');
    push2(&mut out, d as i64);
    out.push('T');
    push2(&mut out, h);
    out.push(':');
    push2(&mut out, mi);
    out.push(':');
    push2(&mut out, s);
    out.push('Z');
    out
}

/// `YYYY-MM-DD[(T| )HH:MM[:SS[.fff]]][zone]`
pub(crate) fn try_iso(t: &str) -> Attempt {
    let b = t.as_bytes();
    let mut i = 0usize;
    let y = match take_digits(b, &mut i, 4, 4) {
        Some(v) => v,
        None => return Attempt::Miss,
    };
    if b.get(i) != Some(&b'-') {
        return Attempt::Miss;
    }
    i += 1;
    let m = match take_digits(b, &mut i, 1, 2) {
        Some(v) => v,
        None => return Attempt::Bad("ISO date without a month".to_string()),
    };
    if b.get(i) != Some(&b'-') {
        return Attempt::Miss;
    }
    i += 1;
    let d = match take_digits(b, &mut i, 1, 2) {
        Some(v) => v,
        None => return Attempt::Bad("ISO date without a day".to_string()),
    };
    if let Some(why) = check_fields(y, m, d) {
        return Attempt::Bad(why);
    }
    let mut hh = 0i64;
    let mut mi = 0i64;
    let mut ss = 0i64;
    let mut frac_us = 0u32;
    let mut tz = 0i32;
    let mut has_tz = false;
    let mut meridiem: Option<bool> = None;
    if i < b.len() {
        let sep = b[i];
        let was_t = sep == b'T' || sep == b't';
        match sep {
            b'T' | b't' => i += 1,
            b' ' | b'\t' => skip_spaces(b, &mut i),
            other => {
                return Attempt::Bad(format!(
                    "unexpected {:?} after the date (expected 'T' or a space)",
                    other as char
                ))
            }
        }
        if i < b.len() && b[i].is_ascii_digit() {
            match parse_clock(b, &mut i) {
                Some((h, mm, s, f)) => {
                    hh = h;
                    mi = mm;
                    ss = s;
                    frac_us = f;
                }
                None => return Attempt::Bad("unreadable time of day".to_string()),
            }
        } else if was_t {
            return Attempt::Bad("ISO timestamp ends after 'T' without a time".to_string());
        }
        skip_spaces(b, &mut i);
        // "2026-01-01 10:00 AM": an ISO date with a 12-hour clock is written by
        // publishers whose CMS thinks it is writing English prose. Reading the
        // marker is the difference between the right hour and a refusal that
        // complains about a missing timezone.
        if let Some((pm, len)) = meridiem_at(&b[i..]) {
            meridiem = Some(pm);
            i += len;
            skip_spaces(b, &mut i);
        }
        if i < b.len() {
            match parse_zone(b, &mut i) {
                Some(off) => {
                    tz = off;
                    has_tz = true;
                }
                None => {
                    return Attempt::Bad(format!(
                        "unrecognised timezone {:?} (write Z, +HH:MM, +HHMM, or UTC/GMT)",
                        tail(&b[i..])
                    ))
                }
            }
        }
    }
    if i != b.len() {
        return Attempt::Bad("trailing characters after the timestamp".to_string());
    }
    let hh = match apply_meridiem(hh, meridiem) {
        Some(h) => h,
        None => return Attempt::Bad(format!("hour {} is not on a 12-hour clock", hh)),
    };
    if let Some(why) = check_time(hh, mi, ss) {
        return Attempt::Bad(why);
    }
    let days = days_from_civil(y, m as u32, d as u32);
    let epoch = days * 86_400 + hh * 3600 + mi * 60 + ss - tz as i64;
    finish_epoch(epoch, "iso", tz, has_tz, frac_us)
}
