//! Cross-module cases, grouped by what they pin: the readers by family here,
//! the languages a date can be written in in [`languages`], the two freshness
//! gates in [`gates`], and the wire plus the nasty input in [`wire`].
//!
//! The calendar's own tests live in `crate::calendar`, next to the arithmetic
//! they check, and the month/weekday tables carry theirs in `crate::data`.

use crate::calendar::weekday_idx;
use crate::iso::iso_utc;
use crate::parse::parse_stamp;

mod gates;
mod languages;
mod wire;

/// 2026-01-01T00:00:00Z — the anchor the other 2026 values are built from.
pub(super) const E2026: i64 = 1_767_225_600;
/// 2026-12-31T23:59:59Z
pub(super) const E2026_END: i64 = 1_798_761_599;

pub(super) fn p(ts: &str) -> i64 {
    match parse_stamp(ts, "", 0) {
        Ok(s) => s.epoch,
        Err(e) => panic!("{:?} should parse, got: {}", ts, e),
    }
}

pub(super) fn bad(ts: &str) -> String {
    match parse_stamp(ts, "", 0) {
        Ok(s) => panic!("{:?} should not parse, got epoch {}", ts, s.epoch),
        Err(e) => e,
    }
}

pub(super) fn iso(ts: &str) -> String {
    iso_utc(p(ts))
}

pub(super) fn run(cmd: &str, payload: &str) -> String {
    super::run(cmd, payload).expect("subcommand should succeed")
}

#[test]
fn epoch_is_exact_for_known_anchors() {
    assert_eq!(p("1970-01-01T00:00:00Z"), 0);
    assert_eq!(p("2000-01-01T00:00:00Z"), 946_684_800);
    assert_eq!(p("2020-01-01T00:00:00Z"), 1_577_836_800);
    assert_eq!(p("2024-01-01T00:00:00Z"), 1_704_067_200);
    assert_eq!(p("2026-01-01T00:00:00Z"), E2026);
    assert_eq!(p("2026-12-31T23:59:59Z"), E2026_END);
    assert_eq!(p("2100-01-01T00:00:00Z"), 4_102_444_800);
    // No float anywhere: 2100-12-31T23:59:59Z stays exact.
    assert_eq!(
        p("2100-12-31T23:59:59Z"),
        4_102_444_800 + 364 * 86_400 + 86_399
    );
}

#[test]
fn weekday_is_right() {
    assert_eq!(weekday_idx(0), 3); // 1970-01-01 was a Thursday
    assert_eq!(parse_stamp("2026-12-31T00:00:00Z", "", 0).unwrap().weekday, "Thu");
    assert_eq!(parse_stamp("2024-02-29T00:00:00Z", "", 0).unwrap().weekday, "Thu");
    assert_eq!(parse_stamp("2026-11-21T00:00:00Z", "", 0).unwrap().weekday, "Sat");
}

#[test]
fn iso_z_basic() {
    let st = parse_stamp("2026-12-31T23:59:59Z", "", 0).unwrap();
    assert_eq!(st.epoch, E2026_END);
    assert_eq!(st.kind, "iso");
    assert!(st.has_tz);
    assert_eq!(iso_utc(st.epoch), "2026-12-31T23:59:59Z");
}

#[test]
fn iso_offset_plus_two() {
    assert_eq!(p("2026-12-31T23:59:59+02:00"), E2026_END - 2 * 3600);
    assert_eq!(iso("2026-12-31T23:59:59+02:00"), "2026-12-31T21:59:59Z");
    assert_eq!(p("2026-12-31T23:59:59+0200"), E2026_END - 2 * 3600);
    assert_eq!(p("2026-12-31T23:59:59+02"), E2026_END - 2 * 3600);
    assert_eq!(p("2026-12-31T23:59:59-02:00"), E2026_END + 2 * 3600);
}

#[test]
fn iso_minus_five_crosses_the_year() {
    // 2026-12-31T23:59:59-05:00 is 2027-01-01T04:59:59Z.
    assert_eq!(p("2026-12-31T23:59:59-05:00"), E2026_END + 5 * 3600);
    assert_eq!(iso("2026-12-31T23:59:59-05:00"), "2027-01-01T04:59:59Z");
}

#[test]
fn iso_half_hour_offset() {
    assert_eq!(p("2026-06-01T12:00:00+05:30"), p("2026-06-01T06:30:00Z"));
    assert_eq!(iso("2026-06-01T12:00:00+05:30"), "2026-06-01T06:30:00Z");
    assert_eq!(p("2026-06-01T12:00:00+0530"), p("2026-06-01T06:30:00Z"));
    assert_eq!(p("2026-06-01T12:00:00+05:30"), p("2026-06-01T12:00:00+0530"));
    // 30-second offsets exist in old zone data; ±HH:MM cannot express them,
    // so they must be refused rather than silently rounded.
    assert!(parse_stamp("2026-06-01T12:00:00+05:30:30", "", 0).is_err());
}

#[test]
fn iso_fractional_seconds() {
    let st = parse_stamp("2026-01-01T00:00:00.123456Z", "", 0).unwrap();
    assert_eq!(st.epoch, E2026);
    assert_eq!(st.frac_us, 123_456);
    let st = parse_stamp("2026-01-01T00:00:00.5Z", "", 0).unwrap();
    assert_eq!(st.epoch, E2026);
    assert_eq!(st.frac_us, 500_000);
    // A fraction never moves the whole second.
    assert_eq!(iso("2026-01-01T00:00:00.999999Z"), "2026-01-01T00:00:00Z");
}

#[test]
fn iso_space_instead_of_t() {
    assert_eq!(p("2026-01-01 10:00:00"), E2026 + 36_000);
    assert_eq!(p("2026-01-01  10:00:00"), E2026 + 36_000);
    assert_eq!(p("2026-01-01 10:00"), E2026 + 36_000);
    // A naive stamp is read as UTC and says so.
    let st = parse_stamp("2026-01-01 10:00:00", "", 0).unwrap();
    assert!(!st.has_tz);
}

#[test]
fn iso_date_only() {
    assert_eq!(p("2026-01-01"), E2026);
    assert_eq!(p("2026-1-1"), E2026);
    assert_eq!(p("2026-01-01 UTC"), E2026);
    assert_eq!(p("2026-01-01 00:00:00 GMT"), E2026);
    assert_eq!(iso("2026-01-01"), "2026-01-01T00:00:00Z");
}

#[test]
fn iso_leap_day_2024_is_valid() {
    assert_eq!(p("2024-02-29T00:00:00Z"), 1_709_164_800);
    assert_eq!(p("2024-02-29"), 1_709_164_800);
    assert_eq!(p("2024-02-29T12:00:00+01:00"), 1_709_164_800 + 11 * 3600);
}

#[test]
fn iso_1900_02_29_is_invalid() {
    let why = bad("1900-02-29");
    assert!(why.contains("1900"), "reason should name the year: {}", why);
    assert!(why.contains("29"), "reason should name the day: {}", why);
    // 2000 is a leap year (divisible by 400), 1900 is not (divisible by 100).
    assert_eq!(p("2000-02-29"), 946_684_800 + 59 * 86_400);
}

#[test]
fn iso_utc_suffixes_and_z_case() {
    assert_eq!(p("2026-01-01T00:00:00z"), E2026);
    assert_eq!(p("2026-01-01T00:00:00Z"), p("2026-01-01T00:00:00+00:00"));
    assert_eq!(p("2026-01-01T00:00:00 UTC"), E2026);
    assert_eq!(p("2026-01-01T00:00:00 MEZ"), E2026 - 3600);
    assert_eq!(p("2026-01-01T00:00:00 CEST"), E2026 - 7200);
}

#[test]
fn iso_rejects_impossible_fields() {
    assert!(bad("2026-13-01").contains("month"));
    assert!(bad("2026-01-32").contains("day"));
    assert!(bad("2026-01-01T24:00:00Z").contains("hour"));
    assert!(bad("2026-01-01T23:60:00Z").contains("minute"));
    assert!(bad("2026-01-01T23:59:60Z").contains("second"));
    assert!(bad("2026-01-01T23:59:59+25:00").contains("timezone"));
}

#[test]
fn rfc2822_news_feed_style() {
    let st = parse_stamp("Sat, 21 Nov 2026 10:00:00 +0100", "", 0).unwrap();
    assert_eq!(st.kind, "rfc2822");
    assert_eq!(st.tz_off, 3600);
    assert_eq!(iso_utc(st.epoch), "2026-11-21T09:00:00Z");
    assert!(st.weekday_ok, "2026-11-21 really is a Saturday");
}

#[test]
fn rfc2822_variants() {
    let base = p("Sat, 21 Nov 2026 09:00:00 GMT");
    assert_eq!(p("21 Nov 2026 09:00:00 GMT"), base);
    assert_eq!(p("Sat, 21 Nov 2026 09:00 GMT"), base);
    assert_eq!(p("Sat, 21 Nov 2026 09:00:00 +0000"), base);
    assert_eq!(p("Sat, 21 Nov 2026 09:00:00 -0000"), base);
    assert_eq!(p("Sat, 21 Nov 2026 09:00:00 GMT (CET)"), base);
    assert_eq!(iso_utc(base), "2026-11-21T09:00:00Z");
    // Two-digit RFC-2822 years: 00..49 -> 2000s, 50..99 -> 1900s.
    assert_eq!(p("21 Nov 26 09:00:00 GMT"), base);
    assert_eq!(p("21 Nov 99 09:00:00 GMT"), p("1999-11-21T09:00:00Z"));
    // US zone names from old mail archives.
    assert_eq!(p("Sat, 21 Nov 2026 09:00:00 EST"), base + 5 * 3600);
}

#[test]
fn rfc2822_weekday_mismatch_is_reported_not_fatal() {
    let st = parse_stamp("Mon, 21 Nov 2026 09:00:00 GMT", "", 0).unwrap();
    assert_eq!(st.weekday, "Sat");
    assert!(!st.weekday_ok, "a wrong weekday must be reported");
    // A leading word that is not a weekday is not an RFC-2822 date at all.
    assert!(parse_stamp("Xyz, 21 Nov 2026 09:00:00 GMT", "", 0).is_err());
}

#[test]
fn relative_german_hours() {
    let now = E2026_END;
    let st = parse_stamp("vor 2 Stunden", "", now).unwrap();
    assert_eq!(st.epoch, now - 2 * 3600);
    assert_eq!(st.kind, "relative");
    assert_eq!(parse_stamp("vor einer Stunde", "", now).unwrap().epoch, now - 3600);
    assert_eq!(parse_stamp("vor 3 Tagen", "", now).unwrap().epoch, now - 3 * 86_400);
    assert_eq!(parse_stamp("vor 5 Minuten", "", now).unwrap().epoch, now - 300);
    assert_eq!(parse_stamp("vor 2 Wochen", "", now).unwrap().epoch, now - 14 * 86_400);
}

#[test]
fn relative_english_hours() {
    let now = E2026_END;
    assert_eq!(parse_stamp("2 hours ago", "", now).unwrap().epoch, now - 7200);
    assert_eq!(parse_stamp("an hour ago", "", now).unwrap().epoch, now - 3600);
    assert_eq!(parse_stamp("5 min ago", "", now).unwrap().epoch, now - 300);
    assert_eq!(parse_stamp("30 seconds ago", "", now).unwrap().epoch, now - 30);
    assert_eq!(parse_stamp("3 days ago", "", now).unwrap().epoch, now - 3 * 86_400);
}

#[test]
fn relative_keywords() {
    let now = E2026_END;
    assert_eq!(parse_stamp("yesterday", "", now).unwrap().epoch, now - 86_400);
    assert_eq!(parse_stamp("gestern", "", now).unwrap().epoch, now - 86_400);
    assert_eq!(parse_stamp("today", "", now).unwrap().epoch, now);
    assert_eq!(parse_stamp("just now", "", now).unwrap().epoch, now);
    assert_eq!(parse_stamp("gerade eben", "", now).unwrap().epoch, now);
    // A future phrase still parses; `window` is what calls it skew.
    assert_eq!(parse_stamp("in 2 hours", "", now).unwrap().epoch, now + 7200);
}

#[test]
fn relative_without_a_unit_is_an_error() {
    assert!(parse_stamp("2 hours", "", 0).is_err());
    let why = match parse_stamp("2 blorks ago", "", 0) {
        Ok(s) => panic!("expected an error, got {}", s.epoch),
        Err(e) => e,
    };
    assert!(why.contains("blorks"), "reason should name the unit: {}", why);
}

#[test]
fn epoch_seconds_and_millis() {
    assert_eq!(p("1798761599"), E2026_END);
    assert_eq!(p("1798761599000"), E2026_END);
    assert_eq!(p("1798761599.75"), E2026_END);
    assert_eq!(p("1798761599000.4"), E2026_END);
    assert_eq!(iso("1798761599"), "2026-12-31T23:59:59Z");
    assert_eq!(parse_stamp("1798761599000", "", 0).unwrap().kind, "epoch_ms");
    assert_eq!(parse_stamp("1798761599", "", 0).unwrap().kind, "epoch");
}

#[test]
fn epoch_ambiguous_bare_numbers_are_refused() {
    assert!(bad("2026").contains("ambiguous"));
    assert!(bad("20261231").contains("ambiguous"));
    assert!(bad("0").contains("ambiguous"));
    // With a hint the caller says what it means, and it is honoured exactly.
    assert_eq!(parse_stamp("2026", "epoch", 0).unwrap().epoch, 2026);
    assert_eq!(parse_stamp("0", "epoch", 0).unwrap().epoch, 0);
    assert_eq!(
        parse_stamp("253402300799", "epoch", 0).unwrap().epoch,
        253_402_300_799
    );
    assert_eq!(iso_utc(253_402_300_799), "9999-12-31T23:59:59Z");
}
