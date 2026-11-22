//! Dates written in words: month names, weekdays, filler words, and the phrases
//! a feed writes instead of a timestamp.
//!
//! Decides:
//!
//! * which month and weekday names are understood — the four languages feeds are
//!   written in (English, German, French, Spanish), full or abbreviated, with or
//!   without a trailing dot (`November`, `Nov`, `Nov.`, `März`, `Mrz`, `novembre`,
//!   `noviembre`, `sábado`, `Samstag`) — and that an abbreviation that could mean
//!   two months is *refused* rather than guessed (`ju` is June or July);
//! * the words that are not part of the date but sit inside it — `um`, `Uhr`,
//!   `at`, `de`/`del`/`le`/`à` (Spanish and French), `AM`, `PM`, `a.m.` — and
//!   what a 12-hour hour means after them;
//! * how `21.`, `21st`, `1er`, `21º`, `21` are all the day `21`;
//! * the two numeric date shapes that are made of words: `21 November 2026` and
//!   `November 21, 2026`, each optionally followed by a clock and a zone;
//! * relative phrases: `vor 2 Stunden`, `2 hours ago`, `yesterday`, `gestern`,
//!   `just now`, and the fixed length of a displayed month (30 d) and year
//!   (365 d).
//!
//! Does not decide: what "now" is (that is `crate::args`), whether the date is in
//! range (that is `crate::parse`), or how an age is displayed
//! (`crate::format`). Month and year units are display-grade only: they never
//! feed the freshness window.

use crate::calendar::days_from_civil;
use crate::data::months::MONTH_NAMES;
use crate::data::weekdays::WEEKDAY_NAMES;
use crate::parse::{check_fields, check_time, finish_epoch, Attempt, WD};
use crate::scanners::{connector_len, is_all_digits, named_zone, parse_clock, word_at};

const MONTH_WORD_DAYS: i64 = 2_592_000; // 30 d — relative phrases only
const YEAR_WORD_DAYS: i64 = 31_536_000; // 365 d — relative phrases only

/// Longest month name we will accept as an abbreviation: `septembre` is spelled
/// out in the table, so only a *prefix* of a table entry is ever resolved.
const PREFIX_MAX_CHARS: usize = 5;

/// The month a name means, or why there is no answer.
///
/// Three steps, in this order: a trailing dot/comma/ordinal mark is dropped; the
/// name is looked up exactly; and only then, for 3..=5 letters, is a *prefix* of
/// a table entry resolved — and only when every entry it could be names the same
/// month. `septe` is September in three languages, so it is answered; `ju` could
/// be June or July, so it is refused. The previous version guessed in both
/// directions (it also accepted a token that *starts with* a month name, so
/// `novel` was November) and returned the first table hit, which made the answer
/// depend on the table's order.
pub(crate) fn month_from_name(tok: &str) -> Option<u32> {
    let core = tok.trim_matches(|c: char| c == '.' || c == ',' || c == 'º' || c == '°');
    if core.chars().count() < 3 {
        return None;
    }
    // Every month name in the four languages starts with a letter, and this
    // guard is what keeps a numeric or punctuated token from costing a table
    // scan: a stamp has more numbers in it than month names.
    if !core.chars().next()?.is_alphabetic() {
        return None;
    }
    // Feeds are overwhelmingly ASCII, so the common path allocates nothing.
    if core.is_ascii() {
        return month_exact(core).or_else(|| month_prefix(core));
    }
    // Umlauts and accents are multi-byte: fold them on chars, once.
    let lower: String = core.chars().flat_map(|c| c.to_lowercase()).collect();
    month_exact(&lower).or_else(|| month_prefix(&lower))
}

/// `word` starts with `prefix`, ASCII case-insensitively. Byte-wise, so the
/// ASCII path needs no lowercased copy of the token; a multi-byte prefix only
/// matches when its bytes do, which is what an exact accent spelling is.
fn ci_starts_with(word: &str, prefix: &str) -> bool {
    let (w, p) = (word.as_bytes(), prefix.as_bytes());
    p.len() <= w.len() && w[..p.len()].eq_ignore_ascii_case(p)
}

fn month_exact(name: &str) -> Option<u32> {
    MONTH_NAMES
        .iter()
        .find(|(word, _)| word.eq_ignore_ascii_case(name) || *word == name)
        .map(|(_, month)| *month)
}

fn month_prefix(name: &str) -> Option<u32> {
    if !(3..=PREFIX_MAX_CHARS).contains(&name.chars().count()) {
        return None;
    }
    let mut found: Option<u32> = None;
    for (word, month) in MONTH_NAMES {
        if !ci_starts_with(word, name) {
            continue;
        }
        match found {
            // Two different months answer: this is a guess, so refuse it.
            Some(known) if known != *month => return None,
            _ => found = Some(*month),
        }
    }
    found
}

/// The weekday a name means, 0 = Monday. Exact first, then a *unique* prefix, so
/// `Tues` and `Thurs` are read and `do` (German Thursday, from the table) is not
/// confused with the Spanish `domingo`, which also starts with `do`.
pub(crate) fn weekday_from_name(tok: &str) -> Option<usize> {
    let t = tok.trim_matches('.');
    // A weekday name starts with a letter; a number never is one, and this guard
    // keeps `21` from costing a 52-entry scan on every stamp.
    if !t.chars().next()?.is_alphabetic() {
        return None;
    }
    if t.is_ascii() {
        return weekday_lookup(t);
    }
    let lower: String = t.chars().flat_map(|c| c.to_lowercase()).collect();
    weekday_lookup(&lower)
}

fn weekday_lookup(name: &str) -> Option<usize> {
    if let Some((_, k)) = WEEKDAY_NAMES
        .iter()
        .find(|(word, _)| word.eq_ignore_ascii_case(name) || *word == name)
    {
        return Some(*k);
    }
    if !(2..=5).contains(&name.chars().count()) {
        return None;
    }
    let mut found: Option<usize> = None;
    for (word, k) in WEEKDAY_NAMES {
        if !ci_starts_with(word, name) {
            continue;
        }
        match found {
            Some(known) if known != *k => return None,
            _ => found = Some(*k),
        }
    }
    found
}

/// Strip a trailing `.`/ordinal mark and an English, French or Spanish ordinal
/// suffix: `21.`, `21st`, `1er`, `21º` → `21`.
pub(crate) fn strip_ordinal(tok: &str) -> &str {
    let t = tok.trim_matches(|c: char| c == '.' || c == 'º' || c == '°');
    for suffix in ["st", "nd", "rd", "th", "er", "ère", "ere", "e"] {
        if t.len() <= suffix.len() {
            continue;
        }
        if let Some(head) = t.strip_suffix(suffix) {
            let tail_ok = t
                .get(t.len() - suffix.len()..)
                .map(|s| s.eq_ignore_ascii_case(suffix))
                .unwrap_or(false);
            if tail_ok && is_all_digits(head) {
                return head;
            }
        }
    }
    t
}

/// How many `word` tokens the month-name reader keeps. A real stamp is
/// `[weekday,] day month year [um] clock [Uhr] [zone]` — eight. The cap exists
/// only to bound the scan; it is *checked*, because the old fixed six-token
/// buffer silently dropped the tail, and the tail is where the zone lives:
/// `21. November 2026 um 10:00 Uhr MEZ` was read as 10:00 UTC, an hour early,
/// with `assumed_utc: true` and no error.
const MAX_TOKENS: usize = 12;

/// `21. November 2026`, `21 November 2026`, `November 21, 2026`, `Nov 21 2026`,
/// `21 novembre 2026`, `1er janvier 2026`, `21 de noviembre de 2026` — each
/// optionally followed by a time and a zone.
pub(crate) fn meridiem_at(b: &[u8]) -> Option<(bool, usize)> {
    if let Some(n) = word_at(b, &["a.m.", "am"]) {
        return Some((false, n));
    }
    if let Some(n) = word_at(b, &["p.m.", "pm"]) {
        return Some((true, n));
    }
    None
}

/// The 12-hour marker a whole token means: `PM`, `p.m.`, `PM.` — the trailing
/// dot some feeds write must not hide it, or the hour is read as 1 o'clock.
fn token_meridiem(raw: &str) -> Option<bool> {
    let b = raw.as_bytes();
    if let Some((pm, len)) = meridiem_at(b) {
        if len == b.len() {
            return Some(pm);
        }
    }
    let trimmed = raw.trim_end_matches('.');
    let tb = trimmed.as_bytes();
    match meridiem_at(tb) {
        Some((pm, len)) if len == tb.len() => Some(pm),
        _ => None,
    }
}

/// 1..=12 plus an AM/PM marker → 0..=23. `None` when the hour cannot be one.
pub(crate) fn apply_meridiem(hh: i64, meridiem: Option<bool>) -> Option<i64> {
    match meridiem {
        None => Some(hh),
        Some(pm) => {
            if !(1..=12).contains(&hh) {
                return None;
            }
            Some(if pm {
                if hh == 12 {
                    12
                } else {
                    hh + 12
                }
            } else if hh == 12 {
                0
            } else {
                hh
            })
        }
    }
}

pub(crate) fn try_monthname(t: &str) -> Attempt {
    let b = t.as_bytes();
    let mut toks: [&str; MAX_TOKENS] = [""; MAX_TOKENS];
    let mut n = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        while i < b.len() && b[i] != b' ' && b[i] != b'\t' && b[i] != b',' {
            i += 1;
        }
        // Token bounds sit on ASCII separators, so this slice is char-clean.
        if let Some(tok) = t.get(start..i) {
            if n == MAX_TOKENS {
                // Refuse rather than drop the tail: the zone sits at the end.
                return Attempt::Bad(format!(
                    "date has more than {} parts; only a date, a time and a zone are read",
                    MAX_TOKENS
                ));
            }
            toks[n] = tok;
            n += 1;
        }
    }
    let mut mon: Option<u32> = None;
    let mut mon_at = 0usize;
    for (k, tok) in toks.iter().enumerate().take(n) {
        if let Some(m) = month_from_name(tok) {
            mon = Some(m);
            mon_at = k;
            break;
        }
    }
    let mon = match mon {
        Some(m) => m,
        None => return Attempt::Miss,
    };

    let mut day: Option<i64> = None;
    let mut year: Option<i64> = None;
    let mut clock: Option<(i64, i64, i64, u32)> = None;
    let mut tz = 0i32;
    let mut has_tz = false;
    let mut meridiem: Option<bool> = None;
    let mut wd: Option<usize> = None;
    for (k, raw) in toks.iter().enumerate().take(n) {
        if k == mon_at {
            continue;
        }
        let raw = *raw;
        let bytes = raw.as_bytes();
        // "Dienstag, 21. November 2026 um 10:00 Uhr", "Nov 21, 2026 10:00 AM",
        // "samedi 21 novembre 2026 à 10h30", "21 de noviembre de 2026".
        if connector_len(bytes).is_some() {
            continue;
        }
        if let Some(pm) = token_meridiem(raw) {
            meridiem = Some(pm);
            continue;
        }
        // A time of day is digits plus a separator: `10:00`, `10:00:00.5`,
        // `10h30`. A bare number (`2026`) never is.
        let clockish = bytes.first().map(|c| c.is_ascii_digit()).unwrap_or(false)
            && bytes
                .iter()
                .any(|c| *c == b':' || *c == b'h' || *c == b'H');
        if clockish {
            if clock.is_some() {
                return Attempt::Bad("two times of day in one date".to_string());
            }
            let mut j = 0usize;
            match parse_clock(bytes, &mut j) {
                Some(v) => clock = Some(v),
                None => return Attempt::Bad("unreadable time of day".to_string()),
            }
            if j != bytes.len() {
                return Attempt::Bad("trailing characters after a time of day".to_string());
            }
            continue;
        }
        let core = strip_ordinal(raw);
        if core.is_empty() {
            continue;
        }
        if is_all_digits(core) {
            if core.len() == 4 && year.is_none() {
                year = core.parse::<i64>().ok();
            } else if core.len() <= 2 && day.is_none() {
                day = core.parse::<i64>().ok();
            } else if year.is_none() {
                year = core.parse::<i64>().ok();
            } else {
                return Attempt::Bad(format!("unexpected number {} in a date", core));
            }
            continue;
        }
        // A word: a weekday in front of the date, or the zone after the time.
        // A weekday is accepted once, and only where no day has been read yet.
        if wd.is_none() && day.is_none() {
            if let Some(k2) = weekday_from_name(core) {
                wd = Some(k2);
                continue;
            }
        }
        match named_zone(core.as_bytes()) {
            Some((off, len)) if len == core.len() => {
                tz = off;
                has_tz = true;
            }
            _ => {
                return Attempt::Bad(format!(
                    "unexpected word {:?} in a date with a month name",
                    core
                ))
            }
        }
    }
    let year = match year {
        Some(y) => y,
        None => return Attempt::Bad("date with a month name but no year".to_string()),
    };
    let day = match day {
        Some(d) => d,
        None => return Attempt::Bad("date with a month name but no day".to_string()),
    };
    if let Some(why) = check_fields(year, mon as i64, day) {
        return Attempt::Bad(why);
    }
    let (hh, mi, ss, frac_us) = clock.unwrap_or((0, 0, 0, 0));
    let hh = match apply_meridiem(hh, meridiem) {
        Some(h) => h,
        None => return Attempt::Bad(format!("hour {} is not on a 12-hour clock", hh)),
    };
    if let Some(why) = check_time(hh, mi, ss) {
        return Attempt::Bad(why);
    }
    let days = days_from_civil(year, mon, day as u32);
    let epoch = days * 86_400 + hh * 3600 + mi * 60 + ss - tz as i64;
    match finish_epoch(epoch, "month_name", tz, has_tz, frac_us) {
        Attempt::Hit(mut stamp) => {
            if let Some(k) = wd {
                stamp.weekday_ok = stamp.weekday == WD[k];
            }
            Attempt::Hit(stamp)
        }
        other => other,
    }
}

pub(crate) fn amount_word(word: &str) -> Option<i64> {
    let w = word.trim_matches('.').to_lowercase();
    if is_all_digits(&w) {
        return w.parse::<i64>().ok();
    }
    let n = match w.as_str() {
        "a" | "an" | "one" | "ein" | "eine" | "einer" | "einem" | "einen" => 1,
        "two" | "zwei" => 2,
        "three" | "drei" => 3,
        "four" | "vier" => 4,
        "five" | "fünf" | "fuenf" => 5,
        "six" | "sechs" => 6,
        "seven" | "sieben" => 7,
        "eight" | "acht" => 8,
        "nine" | "neun" => 9,
        "ten" | "zehn" => 10,
        _ => return None,
    };
    Some(n)
}

pub(crate) fn unit_seconds(unit: &str) -> Option<i64> {
    let u = unit.trim_matches('.').to_lowercase();
    let secs = match u.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" | "sek" | "sekunde" | "sekunden" => 1,
        // "m" is minutes, not months: the places feeds abbreviate are
        // "5 m ago" and "5 min".
        "m" | "min" | "mins" | "minute" | "minutes" | "minuten" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" | "std" | "stunde" | "stunden" => 3600,
        "d" | "day" | "days" | "tag" | "tage" | "tagen" => 86_400,
        "w" | "week" | "weeks" | "woche" | "wochen" => 604_800,
        "month" | "months" | "monat" | "monate" | "monaten" => MONTH_WORD_DAYS,
        "year" | "years" | "jahr" | "jahre" | "jahren" => YEAR_WORD_DAYS,
        _ => return None,
    };
    Some(secs)
}

/// `2 hours ago` → `(2, "hours")`, `vor einer Stunde` → `(1, "stunde")`.
pub(crate) fn split_amount(s: &str) -> Option<(i64, &str)> {
    let mut count = 0usize;
    let mut first = "";
    let mut last = "";
    for word in s.split_whitespace() {
        if count == 0 {
            first = word;
        }
        last = word;
        count += 1;
    }
    if count < 2 {
        return None;
    }
    let n = amount_word(first)?;
    Some((n, last))
}

/// `vor 2 Stunden`, `2 hours ago`, `yesterday`, `just now`, `gestern`.
pub(crate) fn try_relative(t: &str, now: i64) -> Attempt {
    let lower: String = t.chars().flat_map(|c| c.to_lowercase()).collect();
    let s = lower.trim();
    match s {
        "now" | "just now" | "right now" | "jetzt" | "gerade eben" | "eben" | "soeben"
        | "today" | "heute" => return stamp_at(now),
        "yesterday" | "gestern" => return stamp_at(now - 86_400),
        "the day before yesterday" | "vorgestern" => return stamp_at(now - 2 * 86_400),
        _ => {}
    }
    let (head, sign) = if let Some(h) = s.strip_suffix(" ago") {
        (h, -1i64)
    } else if let Some(h) = s.strip_prefix("vor ") {
        (h, -1i64)
    } else if let Some(h) = s.strip_prefix("in ") {
        // "in 2 hours": a future stamp, which `window` then calls skew.
        (h, 1i64)
    } else {
        return Attempt::Miss;
    };
    let (n, unit) = match split_amount(head) {
        Some(v) => v,
        None => {
            return Attempt::Bad("relative phrase without an amount and a unit".to_string())
        }
    };
    let secs = match unit_seconds(unit) {
        Some(v) => v,
        None => return Attempt::Bad(format!("unknown unit {:?} in a relative phrase", unit)),
    };
    if n > 1_000_000 {
        return Attempt::Bad(format!("relative amount {} is too large", n));
    }
    stamp_at(now + sign * n * secs)
}

pub(crate) fn stamp_at(epoch: i64) -> Attempt {
    finish_epoch(epoch, "relative", 0, false, 0)
}
