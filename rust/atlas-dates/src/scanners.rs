//! The byte-level readers every date family shares: digits, a clock, a zone, a
//! word.
//!
//! Decides: how many digits a field may have and what happens on a shortfall
//! (the cursor resets, so the caller can keep scanning), how
//! `hh:mm[:ss[.ffffff]]` — or the French `10h30` — is read, which zone names and
//! numeric offsets are understood (`+02:00`, `+0200`, `Z`, `CET`, `MESZ`, `AEDT`,
//! `GMT+0200`, …), that the longest zone name wins and that a name is only ever a
//! whole token, and what "this word, as a whole word" means.
//!
//! Does not decide: which reader gets to try, or what the values mean once they
//! are read. Every function here is a cursor over the input bytes and allocates
//! nothing on the path it takes; only a refusal message ([`tail`]) builds a
//! string. The readers (`crate::parse`, `crate::iso`, `crate::phrases`) decide the
//! order and the messages.

/// Take `min_n..=max_n` ASCII digits. Resets `i` and yields `None` on a
/// shortfall so the caller can keep scanning from where it was.
pub(crate) fn take_digits(b: &[u8], i: &mut usize, min_n: usize, max_n: usize) -> Option<i64> {
    let start = *i;
    let mut v: i64 = 0;
    while *i < b.len() && b[*i].is_ascii_digit() && *i - start < max_n {
        v = v * 10 + (b[*i] - b'0') as i64;
        *i += 1;
    }
    if *i - start < min_n {
        *i = start;
        return None;
    }
    Some(v)
}

pub(crate) fn skip_spaces(b: &[u8], i: &mut usize) {
    while *i < b.len() && (b[*i] == b' ' || b[*i] == b'\t') {
        *i += 1;
    }
}

pub(crate) fn is_all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

/// `hh:mm[:ss[.ffffff]]` — 1 or 2 digits per field, like `strptime`, and `hh` alone
/// when the separator is the French `h` (`10h`, `10h30`), which is how French
/// feeds write a time of day.
pub(crate) fn parse_clock(b: &[u8], i: &mut usize) -> Option<(i64, i64, i64, u32)> {
    let h = take_digits(b, i, 1, 2)?;
    let french = match b.get(*i) {
        Some(b':') => false,
        Some(b'h') | Some(b'H') => true,
        _ => return None,
    };
    *i += 1;
    // "10h" on its own is ten o'clock; "10:00" must say its minutes. A trailing
    // "h30" is minutes only if the separator was the French one.
    let mi = if french && !matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
        0
    } else {
        take_digits(b, i, 1, 2)?
    };
    let mut ss = 0i64;
    let mut frac = 0u32;
    if b.get(*i) == Some(&b':') {
        *i += 1;
        ss = take_digits(b, i, 1, 2)?;
        if matches!(b.get(*i), Some(b'.') | Some(b',')) {
            *i += 1;
            let start = *i;
            let mut scale = 100_000u32;
            while *i < b.len() && b[*i].is_ascii_digit() {
                if scale > 0 {
                    frac += (b[*i] - b'0') as u32 * scale;
                    scale /= 10;
                }
                *i += 1;
            }
            if *i == start {
                return None;
            }
        }
    }
    Some((h, mi, ss, frac))
}

/// Named zones: the RFC-2822 obs-zone set, the two German abbreviations German
/// feeds write, and the abbreviations a publisher's CMS prints today.
///
/// Reference data — an offset per name, the same kind of table as the geodata.
/// It decides nothing about what counts as news. Where a name is genuinely
/// ambiguous in the wild it is resolved once, here, and named in a comment.
pub(crate) fn named_zone(name: &[u8]) -> Option<(i32, usize)> {
    const ZONES: &[(&str, i32)] = &[
        ("UTC", 0),
        ("UT", 0),
        ("GMT", 0),
        ("Z", 0),
        // European: winter/summer pairs.
        ("WET", 0),
        ("WEST", 3600),
        ("CET", 3600),
        ("CEST", 7200),
        ("MEZ", 3600),
        ("MESZ", 7200),
        ("EET", 7200),
        ("EEST", 3 * 3600),
        ("MSK", 3 * 3600),
        // "IST" is Ireland in winter and India all year, and both are written in
        // feeds. India is the reading an international desk means, so it wins;
        // a feed that means Dublin should write +00:00/+01:00.
        ("IST", 5 * 3600 + 1800),
        ("BST", 3600),
        // North America, including the Atlantic and Alaska pairs.
        ("EST", -5 * 3600),
        ("EDT", -4 * 3600),
        ("CST", -6 * 3600),
        ("CDT", -5 * 3600),
        ("MST", -7 * 3600),
        ("MDT", -6 * 3600),
        ("PST", -8 * 3600),
        ("PDT", -7 * 3600),
        ("AST", -4 * 3600),
        ("ADT", -3 * 3600),
        ("NST", -(3 * 3600 + 1800)),
        ("NDT", -(2 * 3600 + 1800)),
        ("AKST", -9 * 3600),
        ("AKDT", -8 * 3600),
        ("HST", -10 * 3600),
        // South America, Africa, Asia-Pacific.
        ("BRT", -3 * 3600),
        ("ART", -3 * 3600),
        ("CAT", 2 * 3600),
        ("SAST", 2 * 3600),
        ("JST", 9 * 3600),
        ("KST", 9 * 3600),
        ("HKT", 8 * 3600),
        ("SGT", 8 * 3600),
        ("PHT", 8 * 3600),
        ("AWST", 8 * 3600),
        ("ACST", 9 * 3600 + 1800),
        ("ACDT", 10 * 3600 + 1800),
        ("AEST", 10 * 3600),
        ("AEDT", 11 * 3600),
        ("NZST", 12 * 3600),
        ("NZDT", 13 * 3600),
    ];
    let mut best: Option<(i32, usize)> = None;
    // Every zone name is an ASCII letter run, so a number or a punctuated token
    // never reaches the table: a stamp has more numbers in it than zone names.
    if !matches!(name.first(), Some(c) if c.is_ascii_alphabetic()) {
        return None;
    }
    for (word, off) in ZONES {
        let n = word.len();
        if name.len() < n || !name[..n].eq_ignore_ascii_case(word.as_bytes()) {
            continue;
        }
        // Must be a whole token, never the start of a longer word. A sign is a
        // token end too: `GMT+0200` is the name plus an offset (see `parse_zone`).
        let next = name.get(n).copied();
        if !matches!(
            next,
            None | Some(b' ') | Some(b'\t') | Some(b'(') | Some(b')') | Some(b'+') | Some(b'-')
        ) {
            continue;
        }
        // Longest match wins: "CEST" must beat "CET", "AEDT" must beat nothing.
        if best.map(|(_, len)| n > len).unwrap_or(true) {
            best = Some((*off, n));
        }
    }
    best
}

/// `+HH:MM`, `+HHMM`, `+HH` → seconds east of UTC. `None` when the digits are
/// missing or the offset is not a real one, and the cursor is put back.
fn numeric_zone(b: &[u8], i: &mut usize) -> Option<i32> {
    let start = *i;
    let sign = b.get(*i).copied()?;
    if !matches!(sign, b'+' | b'-') {
        return None;
    }
    let neg = sign == b'-';
    *i += 1;
    let hh = match take_digits(b, i, 2, 2) {
        Some(v) => v,
        None => {
            *i = start;
            return None;
        }
    };
    let mut mm = 0i64;
    if b.get(*i) == Some(&b':') {
        *i += 1;
        match take_digits(b, i, 2, 2) {
            Some(v) => mm = v,
            None => {
                *i = start;
                return None;
            }
        }
    } else if matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
        match take_digits(b, i, 2, 2) {
            Some(v) => mm = v,
            None => {
                *i = start;
                return None;
            }
        }
    }
    if hh > 23 || mm > 59 {
        *i = start;
        return None;
    }
    let off = (hh * 3600 + mm * 60) as i32;
    Some(if neg { -off } else { off })
}

/// `Z`, `+02:00`, `+0200`, `+02`, `UTC`, `GMT`, `MEZ`, `AEDT`, `GMT+0200`, …
/// → seconds east of UTC.
pub(crate) fn parse_zone(b: &[u8], i: &mut usize) -> Option<i32> {
    match b.get(*i).copied()? {
        b'Z' | b'z' => {
            *i += 1;
            Some(0)
        }
        b'+' | b'-' => numeric_zone(b, i),
        _ => {
            let (off, len) = named_zone(&b[*i..])?;
            *i += len;
            // "GMT+0200" / "UTC-05:00": Java's RFC-2822 writer puts the offset
            // after the name, and some CMSs copy it. The offset is absolute, so
            // it replaces the name's own value only when the name said UTC.
            let after = *i;
            if let Some(extra) = numeric_zone(b, i) {
                if off == 0 {
                    return Some(extra);
                }
                // A named zone and a contradicting offset cannot both be right;
                // read the name (which the feed's own locale chose) and stop.
                *i = after;
                return Some(off);
            }
            Some(off)
        }
    }
}

/// A whole-word match from a small list, never the start of a longer word.
pub(crate) fn word_at(b: &[u8], words: &[&str]) -> Option<usize> {
    for word in words {
        let n = word.len();
        if b.len() < n || !b[..n].eq_ignore_ascii_case(word.as_bytes()) {
            continue;
        }
        let next = b.get(n).copied();
        if matches!(next, None | Some(b' ') | Some(b'\t') | Some(b'(') | Some(b')')) {
            return Some(n);
        }
    }
    None
}

/// German and English filler around a time: "um 10:00 Uhr".
pub(crate) fn filler_len(b: &[u8]) -> Option<usize> {
    word_at(b, &["um", "uhr", "at"])
}

/// The little words that are part of a written date without carrying a value:
/// `um`/`Uhr` (German), `at` (English), `de`/`del`/`den`/`der` (Spanish and
/// German), `le`/`à` (French).
///
/// Language grammar, not a news-word list: these say how a date is written, and
/// they are matched as whole words only, so `dev` is not `de`.
pub(crate) fn connector_len(b: &[u8]) -> Option<usize> {
    word_at(
        b,
        &[
            "um", "uhr", "at", "de", "del", "den", "der", "des", "le", "el", "y", "à",
        ],
    )
}

/// Up to 24 bytes of what is left of the input, cut on a char boundary, for a
/// message that names the text the reader did not understand. Allocates only on
/// the error path.
pub(crate) fn tail(b: &[u8]) -> String {
    let mut cut = b.len().min(24);
    while cut > 0 && std::str::from_utf8(&b[..cut]).is_err() {
        cut -= 1;
    }
    String::from_utf8_lossy(&b[..cut]).trim_end().to_string()
}

