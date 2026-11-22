//! The readers, by family, and the one entry point that chooses between them.
//!
//! Decides: what a stamp is (`Stamp`) and what a reader may answer (`Hit`,
//! `Miss`, `Bad`); that every value is range-checked with a specific message
//! ("day 32 does not exist in month 1 of 2026", "second 60 out of range 0..59");
//! that the supported span is the years 1..9999 and nothing outside it is
//! answered; the order in which the families are tried when the caller gives no
//! hint (ISO, epoch, RFC-2822, month name, dotted, relative); and that the first
//! *refusal* is remembered while later readers are still allowed to succeed.
//!
//! Does not decide: how a field is scanned (`crate::scanners`), what the month
//! and weekday words are (`crate::phrases`), the ISO shape (`crate::iso`), or
//! what "now" is (`crate::args`).
//!
//! `parse_stamp` is the only way in: the readers below are not a public surface,
//! so a caller cannot accidentally get a different answer per family.

use atlas_lib::json::{self, Json};

use crate::args::{now_arg, text_arg};
use crate::calendar::{civil_from_days, days_in_month, days_from_civil, weekday_idx};
use crate::epoch::try_epoch;
use crate::iso::{iso_utc, try_iso};
use crate::phrases::{
    apply_meridiem, meridiem_at, month_from_name, try_monthname, try_relative, weekday_from_name,
};
use crate::scanners::{filler_len, parse_clock, parse_zone, skip_spaces, tail, take_digits};

/// Longest timestamp we look at. Real ones are < 40 bytes; this stops a
/// 10 kB line from costing anything.
pub(crate) const MAX_INPUT_BYTES: usize = 200;

/// The epoch unit, re-exported: `parse::NumUnit` was the public path before the
/// reader moved to `crate::epoch`, and a move may not make a caller change.
pub use crate::epoch::NumUnit;

pub(crate) const WD: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// A timestamp that was fully understood.
#[derive(Debug, Clone)]
pub struct Stamp {
    pub epoch: i64,
    pub kind: &'static str,
    /// Offset written in the input, in seconds east of UTC.
    pub tz_off: i32,
    pub has_tz: bool,
    /// The weekday of [`Stamp::epoch`], from the calendar — never from the text.
    pub weekday: &'static str,
    /// `false` only when the text also *stated* a weekday and the date
    /// arithmetic disagrees with it. A stamp that states no weekday cannot
    /// contradict the calendar, so it is `true`.
    pub weekday_ok: bool,
    pub frac_us: u32,
}

impl Stamp {
    /// The input was a wall-clock reading with no zone in it, and it was read as
    /// UTC. This is the flag that makes the assumption visible instead of
    /// silent.
    ///
    /// `false` for an epoch number and for a relative phrase: those are exact
    /// instants, and no zone was guessed — `has_tz` is `false` for them too, but
    /// that says "there was no zone to read", not "UTC was assumed".
    pub fn assumed_utc(&self) -> bool {
        !self.has_tz && matches!(self.kind, "iso" | "rfc2822" | "de_date" | "month_name")
    }
}

/// Result of running one candidate reader.
pub(crate) enum Attempt {
    Hit(Stamp),
    /// The input is not this syntax at all — try the next reader.
    Miss,
    /// The input *is* this syntax but the values are impossible, or the
    /// reader must refuse. Reported as-is: a worse message is not a fix.
    Bad(String),
}

// -- building a stamp ---------------------------------------------------

pub(crate) fn finish_epoch(epoch: i64, kind: &'static str, tz: i32, has_tz: bool, frac_us: u32) -> Attempt {
    let days = epoch.div_euclid(86_400);
    let (y, _, _) = civil_from_days(days);
    if !(1..=9999).contains(&y) {
        return Attempt::Bad(format!(
            "timestamp is outside the supported span (year {}; the app stores years 1..9999)",
            y
        ));
    }
    Attempt::Hit(Stamp {
        epoch,
        kind,
        tz_off: tz,
        has_tz,
        weekday: WD[weekday_idx(days) as usize],
        weekday_ok: true,
        frac_us,
    })
}

pub(crate) fn check_fields(y: i64, m: i64, d: i64) -> Option<String> {
    if !(1..=12).contains(&m) {
        return Some(format!("month {} out of range 1..12", m));
    }
    if d < 1 || d > days_in_month(y, m as u32) as i64 {
        return Some(format!("day {} does not exist in month {} of {}", d, m, y));
    }
    if !(1..=9999).contains(&y) {
        return Some(format!("year {} out of range 1..9999", y));
    }
    None
}

pub(crate) fn check_time(h: i64, mi: i64, s: i64) -> Option<String> {
    if !(0..=23).contains(&h) {
        return Some(format!("hour {} out of range 0..23", h));
    }
    if !(0..=59).contains(&mi) {
        return Some(format!("minute {} out of range 0..59", mi));
    }
    if !(0..=59).contains(&s) {
        // A leap second is real, but a second past 59 is almost always a
        // typo, and a stored leap second would break every arithmetic that
        // assumes a minute has 60 of them.
        return Some(format!("second {} out of range 0..59", s));
    }
    None
}



pub(crate) fn try_rfc2822(t: &str) -> Attempt {
    let b = t.as_bytes();
    let mut i = 0usize;
    let mut wd: Option<usize> = None;
    let wstart = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i > wstart {
        // A leading word is either the weekday or this is not RFC-2822
        // ("November 21, 2026" belongs to the month-name reader).
        match t.get(wstart..i).and_then(weekday_from_name) {
            Some(k) => wd = Some(k),
            None => return Attempt::Miss,
        }
        if b.get(i) == Some(&b',') {
            i += 1;
        }
        skip_spaces(b, &mut i);
    }
    let dstart = i;
    let d = match take_digits(b, &mut i, 1, 2) {
        Some(v) => v,
        None => return Attempt::Miss,
    };
    if i - dstart > 2 || !matches!(b.get(i), Some(b' ') | Some(b'\t')) {
        return Attempt::Miss;
    }
    skip_spaces(b, &mut i);
    let mstart = i;
    while i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'.') {
        i += 1;
    }
    if i == mstart {
        return Attempt::Bad("RFC-2822 date without a month name".to_string());
    }
    let m = match t.get(mstart..i).and_then(month_from_name) {
        Some(m) => m as i64,
        None => return Attempt::Miss,
    };
    if !matches!(b.get(i), Some(b' ') | Some(b'\t')) {
        return Attempt::Miss;
    }
    skip_spaces(b, &mut i);
    let ystart = i;
    let yraw = match take_digits(b, &mut i, 2, 4) {
        Some(v) => v,
        None => return Attempt::Bad("RFC-2822 date without a year".to_string()),
    };
    let y = if i - ystart <= 2 {
        // RFC-2822 §4.3: two-digit years 00..49 are 2000s, 50..99 are 1900s.
        if yraw < 50 {
            2000 + yraw
        } else {
            1900 + yraw
        }
    } else {
        yraw
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
    skip_spaces(b, &mut i);
    // German feeds write "21 Nov 2026 um 10:00 Uhr", US ones "10:00 AM EST".
    if let Some(len) = filler_len(&b[i..]) {
        i += len;
        skip_spaces(b, &mut i);
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
        skip_spaces(b, &mut i);
    }
    if let Some((pm, len)) = meridiem_at(&b[i..]) {
        meridiem = Some(pm);
        i += len;
        skip_spaces(b, &mut i);
    }
    if let Some(len) = filler_len(&b[i..]) {
        i += len;
        skip_spaces(b, &mut i);
    }
    hh = match apply_meridiem(hh, meridiem) {
        Some(h) => h,
        None => return Attempt::Bad(format!("hour {} is not on a 12-hour clock", hh)),
    };
    if i < b.len() {
        if b[i] == b'(' {
            i = b.len(); // a trailing "(CET)" style comment says nothing new
        } else {
            match parse_zone(b, &mut i) {
                Some(off) => {
                    tz = off;
                    has_tz = true;
                    skip_spaces(b, &mut i);
                    if i < b.len() && b[i] == b'(' {
                        i = b.len();
                    }
                }
                None => {
                    return Attempt::Bad(format!(
                        "unrecognised timezone {:?} in an RFC-2822 date (write +HHMM, or a name such as UTC, GMT, CET, EST, AEST)",
                        tail(&b[i..])
                    ))
                }
            }
        }
    }
    if i != b.len() {
        return Attempt::Bad("trailing characters after an RFC-2822 date".to_string());
    }
    if let Some(why) = check_time(hh, mi, ss) {
        return Attempt::Bad(why);
    }
    let days = days_from_civil(y, m as u32, d as u32);
    let epoch = days * 86_400 + hh * 3600 + mi * 60 + ss - tz as i64;
    match finish_epoch(epoch, "rfc2822", tz, has_tz, frac_us) {
        Attempt::Hit(mut stamp) => {
            if let Some(k) = wd {
                // Reported, never fatal: real feeds do get this wrong and a
                // wrong weekday must not cost us the article.
                stamp.weekday_ok = stamp.weekday == WD[k];
            }
            Attempt::Hit(stamp)
        }
        other => other,
    }
}

/// `31.12.2026 [10:00[:00]] [zone]`
pub(crate) fn try_dotted(t: &str) -> Attempt {
    let b = t.as_bytes();
    let mut i = 0usize;
    let d = match take_digits(b, &mut i, 1, 2) {
        Some(v) => v,
        None => return Attempt::Miss,
    };
    if b.get(i) != Some(&b'.') {
        return Attempt::Miss;
    }
    i += 1;
    let m = match take_digits(b, &mut i, 1, 2) {
        Some(v) => v,
        None => return Attempt::Bad("dotted date without a month".to_string()),
    };
    if b.get(i) != Some(&b'.') {
        return Attempt::Miss;
    }
    i += 1;
    let ystart = i;
    let yraw = match take_digits(b, &mut i, 2, 4) {
        Some(v) => v,
        None => return Attempt::Bad("dotted date without a year".to_string()),
    };
    let y = if i - ystart <= 2 {
        if yraw < 50 {
            2000 + yraw
        } else {
            1900 + yraw
        }
    } else {
        yraw
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
    skip_spaces(b, &mut i);
    if let Some(len) = filler_len(&b[i..]) {
        // "31.12.2026 um 23:59 Uhr" - "um" before the time, "Uhr" after it.
        i += len;
        skip_spaces(b, &mut i);
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
        skip_spaces(b, &mut i);
    }
    if let Some(len) = filler_len(&b[i..]) {
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
                    "unrecognised timezone {:?} after a dotted date (write +HH:MM, or a name such as UTC, GMT, MEZ, MESZ)",
                    tail(&b[i..])
                ))
            }
        }
    }
    if i != b.len() {
        return Attempt::Bad("trailing characters after a dotted date".to_string());
    }
    if let Some(why) = check_time(hh, mi, ss) {
        return Attempt::Bad(why);
    }
    let days = days_from_civil(y, m as u32, d as u32);
    let epoch = days * 86_400 + hh * 3600 + mi * 60 + ss - tz as i64;
    finish_epoch(epoch, "de_date", tz, has_tz, frac_us)
}


// -- the one entry point ------------------------------------------------

fn attempt_only(a: Attempt, label: &str) -> Result<Stamp, String> {
    match a {
        Attempt::Hit(s) => Ok(s),
        Attempt::Bad(why) => Err(why),
        Attempt::Miss => Err(format!("not a valid {}", label)),
    }
}

/// Parse one timestamp. `hint` forces a single reader; `now` is the
/// reference for relative phrases.
pub fn parse_stamp(raw: &str, hint: &str, now: i64) -> Result<Stamp, String> {
    let t = raw.trim();
    let t = t.strip_prefix('\u{feff}').unwrap_or(t).trim();
    if t.is_empty() {
        return Err("empty timestamp".to_string());
    }
    if t.len() > MAX_INPUT_BYTES {
        return Err(format!(
            "timestamp too long: {} bytes (max {})",
            t.len(),
            MAX_INPUT_BYTES
        ));
    }
    if let Some(ch) = t.chars().find(|c| c.is_control()) {
        return Err(format!(
            "timestamp contains a control character (U+{:04X})",
            ch as u32
        ));
    }
    let hint = hint.trim();
    if !hint.is_empty() {
        return match hint {
            "iso" | "rfc3339" | "date-only" | "%Y-%m-%d" | "%Y-%m-%dT%H:%M:%S"
            | "%Y-%m-%d %H:%M:%S" | "%Y-%m-%dT%H:%M:%S%z" => {
                attempt_only(try_iso(t), "ISO-8601 timestamp")
            }
            "rfc2822" | "%a, %d %b %Y %H:%M:%S %z" => {
                attempt_only(try_rfc2822(t), "RFC-2822 date")
            }
            "epoch" => attempt_only(try_epoch(t, NumUnit::Sec), "unix epoch in seconds"),
            "epoch_ms" => attempt_only(try_epoch(t, NumUnit::Ms), "unix epoch in milliseconds"),
            "epoch_us" => attempt_only(try_epoch(t, NumUnit::Us), "unix epoch in microseconds"),
            "epoch_ns" => attempt_only(try_epoch(t, NumUnit::Ns), "unix epoch in nanoseconds"),
            "de" | "%d.%m.%Y" | "%d.%m.%Y %H:%M" | "%d.%m.%Y %H:%M:%S" => {
                attempt_only(try_dotted(t), "German dd.mm.yyyy date")
            }
            "en" | "%d. %B %Y" | "%d %B %Y" => {
                attempt_only(try_monthname(t), "date with a month name")
            }
            "relative" => attempt_only(try_relative(t, now), "relative phrase"),
            other => Err(format!("unsupported format hint: {}", other)),
        };
    }
    // No hint: try every reader, remember the most specific refusal.
    let mut refused: Option<String> = None;
    macro_rules! step {
        ($attempt:expr) => {
            match $attempt {
                Attempt::Hit(stamp) => return Ok(stamp),
                Attempt::Bad(why) => {
                    if refused.is_none() {
                        refused = Some(why);
                    }
                }
                Attempt::Miss => {}
            }
        };
    }
    step!(try_iso(t));
    step!(try_epoch(t, NumUnit::Auto));
    step!(try_rfc2822(t));
    step!(try_monthname(t));
    step!(try_dotted(t));
    step!(try_relative(t, now));
    Err(refused.unwrap_or_else(|| {
        "not a recognised timestamp (expected ISO-8601, RFC-2822, dd.mm.yyyy, a month-name date, a relative phrase or a unix epoch)"
            .to_string()
    }))
}

pub fn cmd_parse(input: &Json) -> Result<String, String> {
    let ts = text_arg(input, "ts")?;
    let hint = input.get("hint").and_then(Json::as_str).unwrap_or("");
    let now = now_arg(input)?;
    let mut out = String::with_capacity(192);
    match parse_stamp(&ts, hint, now) {
        Ok(st) => {
            out.push_str("{\"ok\":true,\"epoch\":");
            out.push_str(&st.epoch.to_string());
            out.push_str(",\"iso\":");
            json::escape_str(&mut out, &iso_utc(st.epoch));
            out.push_str(",\"kind\":");
            json::escape_str(&mut out, st.kind);
            out.push_str(",\"tz_offset_s\":");
            out.push_str(&st.tz_off.to_string());
            out.push_str(",\"assumed_utc\":");
            out.push_str(if st.assumed_utc() { "true" } else { "false" });
            out.push_str(",\"weekday\":");
            json::escape_str(&mut out, st.weekday);
            out.push_str(",\"weekday_ok\":");
            out.push_str(if st.weekday_ok { "true" } else { "false" });
            out.push_str(",\"frac_us\":");
            out.push_str(&st.frac_us.to_string());
            out.push('}');
        }
        Err(why) => {
            // A bad timestamp is an answer, not a crash: the caller logs the
            // reason and decides. Never a guess.
            out.push_str("{\"ok\":false,\"reason\":");
            json::escape_str(&mut out, &why);
            out.push('}');
        }
    }
    Ok(out)
}
