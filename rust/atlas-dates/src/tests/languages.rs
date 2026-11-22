//! Dates written in the languages real publishers write them in, and the
//! timezone that must come with them.
//!
//! Every case here is a stamp that was refused or misread before the pass that
//! added French and Spanish names, made the abbreviation lookup refuse a guess,
//! and stopped the month-name reader from truncating the zone off the end.

use super::{bad, iso, iso_utc, p, parse_stamp, E2026, E2026_END};

#[test]
fn french_month_names() {
    // Before this test existed, French was not read at all: `21 novembre 2026`
    // was refused, and `1er janvier 2026` failed twice over (month name and the
    // "er" ordinal). Both are the normal spelling in a French feed.
    assert_eq!(p("21 novembre 2026"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("1er janvier 2026"), p("2026-01-01T00:00:00Z"));
    assert_eq!(p("1er mars 2026"), p("2026-03-01T00:00:00Z"));
    assert_eq!(p("21 décembre 2026"), p("2026-12-21T00:00:00Z"));
    assert_eq!(p("21 déc 2026"), p("2026-12-21T00:00:00Z"));
    assert_eq!(p("21 août 2026"), p("2026-08-21T00:00:00Z"));
    assert_eq!(p("21 aout 2026"), p("2026-08-21T00:00:00Z"));
    assert_eq!(p("21 févr 2026"), p("2026-02-21T00:00:00Z"));
    assert_eq!(p("21 juillet 2026"), p("2026-07-21T00:00:00Z"));
    assert_eq!(p("21 juil 2026"), p("2026-07-21T00:00:00Z"));
    // A French weekday in front, and the French clock: "à 10h30".
    assert_eq!(p("samedi 21 novembre 2026"), p("2026-11-21T00:00:00Z"));
    let st = parse_stamp("samedi 21 novembre 2026 à 10h30", "", 0).unwrap();
    assert_eq!(iso_utc(st.epoch), "2026-11-21T10:30:00Z");
    assert!(st.weekday_ok, "21 November 2026 is a Saturday");
    // "10h" on its own is ten o'clock.
    assert_eq!(p("21 novembre 2026 10h"), p("2026-11-21T10:00:00Z"));
}

#[test]
fn spanish_month_names() {
    // Spanish writes the day between two "de"s; before this test the reader
    // stopped at the first "de" with "unexpected word".
    assert_eq!(p("21 de noviembre de 2026"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("1 de enero de 2026"), p("2026-01-01T00:00:00Z"));
    assert_eq!(p("31 de diciembre de 2026"), p("2026-12-31T00:00:00Z"));
    assert_eq!(p("21 de septiembre de 2026"), p("2026-09-21T00:00:00Z"));
    assert_eq!(p("21 de setiembre de 2026"), p("2026-09-21T00:00:00Z"));
    let st = parse_stamp("sábado, 21 de noviembre de 2026 10:00", "", 0).unwrap();
    assert_eq!(iso_utc(st.epoch), "2026-11-21T10:00:00Z");
    assert!(st.weekday_ok);
    // A time and a zone after the Spanish date.
    let st = parse_stamp("21 de noviembre de 2026, 10:00 CET", "", 0).unwrap();
    assert_eq!(iso_utc(st.epoch), "2026-11-21T09:00:00Z");
    assert_eq!(st.tz_off, 3600);
    assert!(!st.assumed_utc(), "the zone was written, so nothing was assumed");
}

#[test]
fn an_ambiguous_abbreviation_is_refused_not_guessed() {
    // `ju` could be June or July and `ma` March, May, or the German "Mai". The
    // old rule returned the first table hit, so the answer depended on the table
    // order - and it also accepted a token that merely *started with* a month
    // name, which made "novel" November. Both are refusals now.
    for guess in ["ju", "novel", "marchers", "maison"] {
        assert!(
            parse_stamp(&format!("21 {} 2026", guess), "", 0).is_err(),
            "{:?} must not be read as a month",
            guess
        );
    }
    // A unique prefix still resolves: "septe" is September in three languages.
    assert_eq!(p("1 septe 2026"), p("2026-09-01T00:00:00Z"));
    assert_eq!(p("1 janv 2026"), p("2026-01-01T00:00:00Z"));
}

#[test]
fn a_month_name_date_keeps_its_zone() {
    // The old reader kept six tokens and silently dropped the rest, and the rest
    // is where the zone sits: this stamp was answered as 10:00 UTC (an hour early)
    // with `assumed_utc: true` and no error at all. Seven tokens, so the cap bit.
    let st = parse_stamp("21. November 2026 um 10:00 Uhr MEZ", "", 0).unwrap();
    assert_eq!(st.tz_off, 3600);
    assert!(!st.assumed_utc());
    assert_eq!(iso_utc(st.epoch), "2026-11-21T09:00:00Z");
    let st = parse_stamp("Samstag, 21. November 2026 um 10:00 Uhr MESZ", "", 0).unwrap();
    assert_eq!(st.tz_off, 7200);
    assert_eq!(iso_utc(st.epoch), "2026-11-21T08:00:00Z");
    // Eight tokens, still read. Twelve is the cap and thirteen is refused rather
    // than truncated - a silently ignored tail is how the zone got lost. The
    // dotted day keeps the RFC-2822 reader out of the way so the month-name
    // reader's own refusal is the one that surfaces.
    let st = parse_stamp("Sat, 21 November 2026 at 10:00:00 AEST", "", 0).unwrap();
    assert_eq!(iso_utc(st.epoch), "2026-11-21T00:00:00Z");
    let why = bad("21. November 2026 1 1 1 1 1 1 1 1 1 1");
    assert!(why.contains("parts"), "{}", why);
}

#[test]
fn a_date_in_another_timezone_is_converted_not_assumed() {
    // Southern-hemisphere summer time: 21 Nov 2026 10:00 AEDT is the evening
    // before in UTC, and the weekday moves with it.
    let st = parse_stamp("21 Nov 2026 10:00:00 AEDT", "", 0).unwrap();
    assert_eq!(st.tz_off, 11 * 3600);
    assert_eq!(iso_utc(st.epoch), "2026-11-20T23:00:00Z");
    assert_eq!(st.weekday, "Fri");
    assert!(!st.assumed_utc());
    // The same instant written three ways agrees.
    assert_eq!(p("2026-11-20T23:00:00Z"), st.epoch);
    assert_eq!(p("21 Nov 2026 10:00:00 +1100"), st.epoch);
    assert_eq!(p("2026-11-21T10:00:00+11:00"), st.epoch);
    // A named zone plus an offset, as Java's mail writer prints it.
    let st = parse_stamp("Sat, 21 Nov 2026 10:00:00 GMT+0200", "", 0).unwrap();
    assert_eq!(st.tz_off, 7200);
    assert_eq!(iso_utc(st.epoch), "2026-11-21T08:00:00Z");
    // Across midnight into the previous day, and across the year.
    assert_eq!(p("2027-01-01T09:00:00+13:00"), p("2026-12-31T20:00:00Z"));
    // A zone that is not in the table is refused with the text it did not read,
    // not quietly read as UTC.
    let why = bad("21 Nov 2026 10:00:00 XYZ");
    assert!(why.contains("XYZ"), "reason should name the zone: {}", why);
}

#[test]
fn german_month_name() {
    assert_eq!(p("21. November 2026"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("21. November 2026 10:00"), p("2026-11-21T10:00:00Z"));
    assert_eq!(p("1. März 2026"), p("2026-03-01T00:00:00Z"));
    assert_eq!(p("1. Maerz 2026"), p("2026-03-01T00:00:00Z"));
    assert_eq!(p("1. Mrz 2026"), p("2026-03-01T00:00:00Z"));
    assert_eq!(p("24. Dezember 2026"), p("2026-12-24T00:00:00Z"));
    assert_eq!(p("3. Okt 2026"), p("2026-10-03T00:00:00Z"));
}

#[test]
fn english_month_names_both_orders() {
    assert_eq!(p("21 November 2026"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("November 21, 2026"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("November 21 2026 10:00:00"), p("2026-11-21T10:00:00Z"));
    assert_eq!(p("Nov 21 2026"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("21st November 2026"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("2nd January 2026"), p("2026-01-02T00:00:00Z"));
    assert_eq!(p("Sept 1 2026"), p("2026-09-01T00:00:00Z"));
    assert_eq!(p("November 21, 2026 10:00 GMT"), p("2026-11-21T10:00:00Z"));
}

#[test]
fn german_um_uhr_and_weekday_prefix() {
    // The German idiom feeds actually write.
    assert_eq!(p("21. November 2026 um 10:00 Uhr"), p("2026-11-21T10:00:00Z"));
    assert_eq!(p("31.12.2026 um 23:59:59 Uhr"), E2026_END);
    assert_eq!(p("31.12.2026 um 23:59 Uhr"), E2026_END - 59);
    assert_eq!(p("21. November 2026 um 10:00"), p("2026-11-21T10:00:00Z"));
    // A German weekday in front of the date is read, and checked.
    let st = parse_stamp("Samstag, 21. November 2026", "", 0).unwrap();
    assert!(st.weekday_ok, "2026-11-21 is a Saturday");
    let st = parse_stamp("Dienstag, 21. November 2026", "", 0).unwrap();
    assert!(!st.weekday_ok, "a wrong weekday must be reported, not ignored");
}

#[test]
fn german_dotted_date() {
    assert_eq!(p("31.12.2026"), E2026_END - 86_399);
    assert_eq!(p("31.12.2026 23:59:59"), E2026_END);
    assert_eq!(p("1.1.2026"), E2026);
    assert_eq!(p("01.01.2026 10:00"), E2026 + 36_000);
    assert_eq!(p("31.12.26"), E2026_END - 86_399);
    assert_eq!(iso("31.12.2026 23:59:59"), "2026-12-31T23:59:59Z");
    assert!(bad("31.13.2026").contains("month"));
    assert!(bad("31.11.2026").contains("day"));
}

#[test]
fn meridiem_12_hour_clock() {
    assert_eq!(p("Nov 21, 2026 10:00 AM"), p("2026-11-21T10:00:00Z"));
    assert_eq!(p("Nov 21, 2026 10:00 PM"), p("2026-11-21T22:00:00Z"));
    assert_eq!(p("Nov 21, 2026 12:00 AM"), p("2026-11-21T00:00:00Z"));
    assert_eq!(p("Nov 21, 2026 12:30 PM"), p("2026-11-21T12:30:00Z"));
    assert_eq!(p("Nov 21, 2026 1:30 p.m."), p("2026-11-21T13:30:00Z"));
    // A trailing dot after the marker must not hide it: "PM." is still 22:00.
    assert_eq!(p("Nov 21, 2026 10:00 PM."), p("2026-11-21T22:00:00Z"));
    // The RFC-2822 path reads the same markers.
    assert_eq!(p("21 Nov 2026 11:00 p.m. GMT"), p("2026-11-21T23:00:00Z"));
    // 13 o'clock is not on a 12-hour clock.
    assert!(bad("Nov 21, 2026 13:00 PM").contains("12-hour"));
}

#[test]
fn an_iso_date_may_carry_a_twelve_hour_clock() {
    // "2026-01-01 10:00 AM" used to be refused with "unrecognised timezone": the
    // marker was right there and the message blamed the zone.
    assert_eq!(p("2026-01-01 10:00 AM"), E2026 + 36_000);
    assert_eq!(p("2026-01-01 10:00 PM"), E2026 + 79_200);
    assert_eq!(p("2026-01-01 12:00 AM"), E2026);
    assert_eq!(p("2026-01-01T12:30:00 p.m."), E2026 + 45_000);
    assert_eq!(p("2026-01-01 10:00 AM UTC"), E2026 + 36_000);
    // 13 o'clock is not on a 12-hour clock, and says so.
    let why = bad("2026-01-01 13:00 PM");
    assert!(why.contains("12-hour"), "{}", why);
}

#[test]
fn every_zone_name_means_one_offset() {
    // The table is reference data, but reference data with a typo is a wrong
    // answer everywhere: a name may not be listed with two offsets, and the
    // offsets themselves must be round hours or half hours within ±14 h.
    use crate::scanners::named_zone;
    let mut seen: Vec<(Vec<u8>, i32)> = Vec::new();
    for name in [
        "UTC", "UT", "GMT", "Z", "WET", "WEST", "CET", "CEST", "MEZ", "MESZ", "EET", "EEST", "MSK",
        "IST", "BST", "EST", "EDT", "CST", "CDT", "MST", "MDT", "PST", "PDT", "AST", "ADT", "NST",
        "NDT", "AKST", "AKDT", "HST", "BRT", "ART", "CAT", "SAST", "JST", "KST", "HKT", "SGT",
        "PHT", "AWST", "ACST", "ACDT", "AEST", "AEDT", "NZST", "NZDT",
    ] {
        let (off, len) = named_zone(name.as_bytes()).unwrap_or_else(|| panic!("{} missing", name));
        assert_eq!(len, name.len(), "{} matched only {} bytes", name, len);
        assert!(off % 1800 == 0, "{} is not a whole half hour: {}", name, off);
        assert!(off.abs() <= 14 * 3600, "{} is out of range: {}", name, off);
        let key = name.to_lowercase().into_bytes();
        if let Some((_, previous)) = seen.iter().find(|(k, _)| *k == key) {
            assert_eq!(*previous, off, "{} is listed with two offsets", name);
        }
        seen.push((key, off));
    }
    // The longest name wins, so "CEST" is not read as "CET", and a name is only a
    // whole token.
    assert_eq!(named_zone(b"CEST").unwrap().0, 7200);
    assert_eq!(named_zone(b"CET").unwrap().0, 3600);
    assert!(named_zone(b"CETISH").is_none(), "a zone is a whole token");
    assert!(named_zone(b"2026").is_none(), "a number is not a zone");
}
