//! The wire shape of the three subcommands, and the input that must never be
//! answered with a guess: empty, garbage, impossible, enormous, or missing.

use atlas_lib::json;

use super::{bad, iso_utc, p, parse_stamp, run, E2026, E2026_END};

#[test]
fn empty_string_is_rejected() {
    assert!(bad("").contains("empty"));
    assert!(bad("   ").contains("empty"));
    assert!(bad("\u{a0}\u{a0}").contains("empty"));
    assert!(bad("\u{feff}").contains("empty"));
}

#[test]
fn huge_string_is_rejected_cheaply() {
    let giant = "9".repeat(10_000);
    assert!(bad(&giant).contains("too long"));
    let mut quoted = String::new();
    atlas_lib::json::escape_str(&mut quoted, &giant);
    let out = run("parse", &format!("{{\"ts\":{}}}", quoted));
    assert!(out.contains("\"ok\":false"), "{}", out);
    assert!(out.contains("too long"), "{}", out);
}

#[test]
fn non_ascii_digits_are_not_numbers() {
    // Arabic-Indic, Devanagari, fullwidth: none of these is a 2026.
    for weird in [
        "٢٠٢٦-٠١-٠١",
        "२०२६-०१-०१",
        "２０２６-０１-０１",
        "2０26-01-01",
        "2026-01-01T٠٠:٠٠:٠٠Z",
    ] {
        assert!(!bad(weird).is_empty(), "{:?} must be refused", weird);
    }
}

#[test]
fn garbage_is_rejected_with_a_reason() {
    for garbage in [
        "not a date",
        "????",
        "--",
        "1.2",
        "2026-",
        "2026-01-01T",
        "2026-01-01T10",
        "hello 2026-01-01 world",
        "2026/01/01",
        "31..12.2026",
        "\u{1f600}",
        "2026-01-01T00:00:00Z extra",
        "1798761599x",
    ] {
        assert!(!bad(garbage).is_empty(), "empty reason for {:?}", garbage);
    }
}

#[test]
fn a_garbage_date_is_refused_with_a_reason_that_names_it() {
    // Every one of these is a real feed bug shape: a truncated stamp, a word
    // where the date should be, a CMS placeholder, an impossible day.
    let cases = [
        ("2026-11-31", "day"),
        ("2026-02-30", "day"),
        ("2026-13-01", "month"),
        ("Wed, 32 Nov 2026 10:00:00 +0100", "day"),
        ("21 Nov 2026 25:00:00 GMT", "hour"),
        ("TBD", "not a recognised timestamp"),
        ("0000-00-00", "month"),
        ("N/A", "not a recognised timestamp"),
    ];
    for (ts, expected) in cases {
        let why = bad(ts);
        assert!(
            why.contains(expected),
            "{:?} -> {:?} (expected {:?})",
            ts,
            why,
            expected
        );
        // The reason never leaks an unescaped byte into the wire object.
        let mut quoted = String::new();
        json::escape_str(&mut quoted, ts);
        let out = run("parse", &format!("{{\"ts\":{}}}", quoted));
        assert!(atlas_lib::json::parse(&out).is_ok(), "invalid JSON: {}", out);
    }
    // A garbage date never becomes fresh news.
    let out = run("window", "{\"ts\":\"2026-02-30\",\"now\":1798761599}");
    assert!(out.contains("\"reason\":\"unparsed\""), "{}", out);
    assert!(out.contains("day 30 does not exist"), "{}", out);
}

#[test]
fn a_missing_date_is_answered_with_what_to_pass() {
    // Two different situations, two different answers.
    //
    // 1. A key that is absent or null is a caller bug: nothing was asked. It is a
    //    wire error, and the message names the argument and shows a usable value
    //    so the next call is right.
    for absent in ["{}", "{\"ts\":null}"] {
        let why = crate::run("parse", absent).unwrap_err();
        assert!(why.contains("\"ts\""), "{} -> {}", absent, why);
        assert!(why.contains("2026-11-21T09:00:00Z"), "{} -> {}", absent, why);
        // The freshness gate refuses the same way: it never invents an age.
        assert!(crate::run("window", absent).is_err(), "{}", absent);
    }
    // A structure where a date belongs is a caller bug too, and says what it got.
    let why = crate::run("parse", "{\"ts\":[\"2026-01-01\"]}").unwrap_err();
    assert!(why.contains("array or object"), "{}", why);
    // 2. An empty or blank value *is* an answer - "this card printed no date" -
    //    and both gates answer it in band, as undated and never as current.
    for blank in ["{\"ts\":\"\"}", "{\"ts\":\"   \"}", "{\"ts\":\"\u{a0}\"}"] {
        let out = run("parse", blank);
        assert!(out.contains("\"ok\":false"), "{} -> {}", blank, out);
        assert!(out.contains("empty"), "{} -> {}", blank, out);
        let out = run("window", blank);
        assert!(out.contains("\"fresh\":false"), "{} -> {}", blank, out);
        assert!(out.contains("\"reason\":\"unparsed\""), "{} -> {}", blank, out);
        assert!(out.contains("\"age_s\":null"), "{} -> {}", blank, out);
    }
}

#[test]
fn control_characters_are_rejected() {
    assert!(bad("2026-01-01\u{0}").contains("control"));
    assert!(bad("2026-01-01x\u{7}").contains("control"));
    // Surrounding whitespace (including a newline) is trimmed, not refused.
    assert_eq!(p("\n 2026-01-01 \t"), E2026);
}

#[test]
fn round_trip_is_idempotent() {
    for ts in [
        "2026-12-31T23:59:59Z",
        "2026-12-31T23:59:59-05:00",
        "2024-02-29T12:34:56+05:30",
        "1999-12-31T23:59:59Z",
        "2100-06-15T00:00:00Z",
        "Thu, 21 Nov 2026 10:00:00 +0100",
        "31.12.2026 23:59:59",
        "21. November 2026",
        "1798761599",
    ] {
        let once = iso_utc(p(ts));
        let twice = iso_utc(p(&once));
        assert_eq!(once, twice, "not idempotent for {:?}", ts);
        assert_eq!(p(&once), p(&twice));
    }
}

#[test]
fn hint_selects_one_reader_and_unknown_hints_are_refused() {
    assert_eq!(
        parse_stamp("1798761599000", "epoch_ms", 0).unwrap().epoch,
        E2026_END
    );
    assert!(parse_stamp("1798761599", "iso", 0).is_err()); // not ISO
    assert!(parse_stamp("2026-01-01", "%Y-%m-%d", 0).is_ok());
    assert!(parse_stamp("21. November 2026", "%d. %B %Y", 0).is_ok());
    assert!(parse_stamp("2026-01-01", "%Y%m%d", 0)
        .unwrap_err()
        .contains("unsupported"));
    // A hint never falls back to another reader: that would be a guess.
    assert!(parse_stamp("yesterday", "iso", 0).is_err());
}

#[test]
fn parse_subcommand_wire_format() {
    let out = run("parse", "{\"ts\":\"2026-12-31T23:59:59-05:00\"}");
    assert!(out.starts_with("{\"ok\":true"), "{}", out);
    assert!(out.contains("\"epoch\":1798779599"), "{}", out);
    assert!(out.contains("\"iso\":\"2027-01-01T04:59:59Z\""), "{}", out);
    assert!(out.contains("\"tz_offset_s\":-18000"), "{}", out);
    assert!(out.contains("\"assumed_utc\":false"), "{}", out);
    assert!(out.ends_with('}'));
    // A bad timestamp is in-band (exit 0), with a reason.
    let out = run("parse", "{\"ts\":\"\"}");
    assert!(out.contains("\"ok\":false"), "{}", out);
    assert!(out.contains("\"reason\""), "{}", out);
}

#[test]
fn parse_subcommand_missing_and_wrong_typed_keys() {
    assert!(crate::run("parse", "{}").is_err());
    assert!(crate::run("parse", "{\"ts\":null}").is_err());
    assert!(crate::run("parse", "{\"ts\":[\"2026-01-01\"]}").is_err());
    assert!(crate::run("parse", "not json").is_err());
    assert!(crate::run("nope", "{\"ts\":\"\"}").is_err());
    // A number for ts is an epoch, not an error.
    assert!(run("parse", "{\"ts\":1798761599}").contains("\"ok\":true"));
}

#[test]
fn quotes_and_newlines_never_break_the_json() {
    // A quote in the input reaches the reason string and must be escaped.
    let out = run("parse", "{\"ts\":\"November 21, 2026 x\\\"y\"}");
    assert!(out.contains("\"ok\":false"), "{}", out);
    assert!(out.contains("unexpected word"), "{}", out);
    assert!(out.contains("\\\""), "quote must be escaped: {}", out);
    assert!(atlas_lib::json::parse(&out).is_ok(), "invalid JSON: {}", out);
    // A real newline in the value is refused, and the output stays one line.
    let out = run("parse", "{\"ts\":\"a\\nb\"}");
    assert!(out.contains("\"ok\":false"), "{}", out);
    assert!(!out.contains('\n'), "output must stay on one line");
    assert!(atlas_lib::json::parse(&out).is_ok(), "invalid JSON: {}", out);
}

#[test]
fn performance_many_parses() {
    // Parsing is a fixed-cost scan bounded by MAX_INPUT_BYTES: 100 000
    // timestamps (a week of every feed) must stay far below a second.
    let inputs = [
        "Thu, 21 Nov 2026 10:00:00 +0100",
        "2026-12-31T23:59:59.123456-05:00",
        "31.12.2026 23:59",
        "vor 2 Stunden",
        "1798761599000",
    ];
    let started = std::time::Instant::now();
    let mut sink = 0i64;
    for _ in 0..20_000 {
        for ts in inputs {
            sink += parse_stamp(ts, "", E2026_END).map(|s| s.epoch).unwrap_or(0);
        }
    }
    let elapsed = started.elapsed();
    assert!(sink != 0);
    assert!(
        elapsed.as_millis() < 2_000,
        "100k parses took {:?} - something is quadratic",
        elapsed
    );
}

#[test]
fn assumed_utc_means_assumed_not_merely_absent() {
    // A wall-clock reading with no zone: UTC was a guess, and the answer says so.
    assert!(parse_stamp("2026-11-21 09:00:00", "", 0).unwrap().assumed_utc());
    assert!(parse_stamp("21. November 2026 09:00", "", 0).unwrap().assumed_utc());
    assert!(parse_stamp("21 Nov 2026 09:00:00", "", 0).unwrap().assumed_utc());
    // A zone was written: nothing is assumed.
    assert!(!parse_stamp("2026-11-21T09:00:00Z", "", 0).unwrap().assumed_utc());
    assert!(!parse_stamp("21 Nov 2026 09:00:00 +0100", "", 0).unwrap().assumed_utc());
    // An epoch number and a relative phrase are exact instants: there was no zone
    // to read and none was guessed, so `assumed_utc` is false. It used to be true
    // for both, which told a caller that UTC had been assumed where it could not
    // have been.
    assert!(!parse_stamp("1798761599", "", 0).unwrap().assumed_utc());
    assert!(!parse_stamp("1798761599000", "", 0).unwrap().assumed_utc());
    assert!(!parse_stamp("vor 2 Stunden", "", E2026_END).unwrap().assumed_utc());
    let out = run("parse", "{\"ts\":\"1798761599\"}");
    assert!(out.contains("\"assumed_utc\":false"), "{}", out);
}

#[test]
fn a_compact_date_time_is_refused_instead_of_read_as_an_epoch() {
    // 12 and 14 digits are in the millisecond range of the epoch heuristic, so
    // `20261231120000` used to be answered as 2612-01-20T23:45:20Z and
    // `202612311200` as 1976-06-03 — silently, and one of those looks stale and
    // the other looks like a typo for the future. Both are refusals now, and the
    // reason spells out both readings.
    for (compact, civil) in [
        ("20261231120000", "2026-12-31 12:00:00"),
        ("202612311200", "2026-12-31 12:00:00"),
        ("20250101000000", "2025-01-01 00:00:00"),
    ] {
        let why = bad(compact);
        assert!(why.contains("ambiguous"), "{} -> {}", compact, why);
        assert!(why.contains(civil), "{} should name {}: {}", compact, civil, why);
    }
    // A number that only *looks* long is still an epoch.
    assert_eq!(parse_stamp("1798761599000", "", 0).unwrap().kind, "epoch_ms");
    assert_eq!(parse_stamp("1798761599000000", "", 0).unwrap().kind, "epoch_us");
    assert_eq!(
        parse_stamp("1798761599000000000", "", 0).unwrap().kind,
        "epoch_ns"
    );
    // And the hint still forces a reading when the caller is sure.
    assert_eq!(parse_stamp("202612311200", "epoch_ms", 0).unwrap().epoch, 202612311);
}
