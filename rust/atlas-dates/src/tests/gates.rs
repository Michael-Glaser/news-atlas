//! The two gates on a date: the freshness window (`crate::window`) and the
//! display label (`crate::format`). Both must refuse to call a future-dated item
//! current, and they share one tolerance so they cannot disagree.

use super::{run, E2026_END};
use crate::DEFAULT_SKEW_S;

#[test]
fn only_one_of_the_two_gates_may_say_now() {
    // The freshness gate and the display label are two separate gates, and a
    // future-dated item must fail both: before this test the window said `skew`
    // while the label said "now", so the UI presented a 2027 report as current.
    let now = E2026_END;
    for ahead in [301i64, 3_600, 86_400, 5 * 86_400 * 365] {
        let ts = now + ahead;
        let verdict = run("window", &format!("{{\"ts\":{},\"now\":{}}}", ts, now));
        assert!(verdict.contains("\"fresh\":false"), "{} -> {}", ahead, verdict);
        assert!(verdict.contains("\"reason\":\"skew\""), "{} -> {}", ahead, verdict);
        let label = run("format", &format!("{{\"ts\":{},\"now\":{}}}", ts, now));
        assert!(label.contains("\"future\":true"), "{} -> {}", ahead, label);
        assert!(
            !label.contains("\"text\":\"now\""),
            "a stamp {} s ahead must not read as now: {}",
            ahead,
            label
        );
        assert!(label.contains("\"text\":\"in "), "{} -> {}", ahead, label);
    }
    // Inside the 300 s skew both gates tolerate a clock that runs fast, and they
    // agree about it, because they share the constant.
    let verdict = run("window", &format!("{{\"ts\":{},\"now\":{}}}", now + 300, now));
    assert!(verdict.contains("\"fresh\":true"), "{}", verdict);
    let label = run("format", &format!("{{\"ts\":{},\"now\":{}}}", now + 120, now));
    assert!(label.contains("\"future\":false"), "{}", label);
    assert!(!label.contains("\"in "), "{}", label);
    let label = run("format", &format!("{{\"ts\":{},\"now\":{}}}", now + 30, now));
    assert!(label.contains("\"text\":\"now\""), "{}", label);
    assert!(label.contains("\"future\":false"), "{}", label);
}

#[test]
fn assert_the_skew_constant_is_shared() {
    // One number for both gates: if someone widens the window's tolerance, the
    // label widens with it, and this test fails if they ever diverge.
    assert_eq!(DEFAULT_SKEW_S, 300);
    // At exactly the skew the stamp is still "now-ish": no direction, and the
    // magnitude reads like any other 300 seconds.
    let label = run("format", "{\"age_s\":-300}");
    assert!(label.contains("\"future\":false"), "{}", label);
    assert!(!label.contains("\"in "), "{}", label);
    let label = run("format", "{\"age_s\":-301}");
    assert!(label.contains("\"future\":true"), "{}", label);
    assert!(label.contains("\"text\":\"in 5 min\""), "{}", label);
    // A second ahead is inside the 60 s floor, so it reads "now" with no
    // direction - the same two characters as a stamp published a moment ago.
    let label = run("format", "{\"age_s\":-30}");
    assert!(label.contains("\"text\":\"now\""), "{}", label);
    assert!(label.contains("\"future\":false"), "{}", label);
}

#[test]
fn window_fresh_inside_72_hours() {
    let now = E2026_END;
    let out = run(
        "window",
        &format!("{{\"ts\":\"2026-12-31T20:00:00Z\",\"now\":{}}}", now),
    );
    assert!(out.contains("\"fresh\":true"), "{}", out);
    assert!(out.contains("\"reason\":\"fresh\""), "{}", out);
    assert!(out.contains("\"age_s\":14399"), "{}", out); // 3 h 59 min 59 s
    assert!(out.contains("\"max_age_s\":259200"), "{}", out);
    assert!(out.contains("\"age_h\":3.9997"), "{}", out);
}

#[test]
fn window_stale_after_the_default_window() {
    let now = E2026_END;
    // 72 h + 1 s is the exact boundary that must read as stale.
    let out = run(
        "window",
        &format!("{{\"ts\":{},\"now\":{}}}", now - 259_200 - 1, now),
    );
    assert!(out.contains("\"fresh\":false"), "{}", out);
    assert!(out.contains("\"reason\":\"stale\""), "{}", out);
    // Exactly 72 h old is still inside the window.
    let out = run(
        "window",
        &format!("{{\"ts\":{},\"now\":{}}}", now - 259_200, now),
    );
    assert!(out.contains("\"fresh\":true"), "{}", out);
    // A caller's own window wins over the default.
    let out = run(
        "window",
        &format!("{{\"ts\":{},\"now\":{},\"max_age_s\":600}}", now - 700, now),
    );
    assert!(out.contains("\"reason\":\"stale\""), "{}", out);
}

#[test]
fn window_future_is_skew_not_fresh() {
    let now = E2026_END;
    let out = run(
        "window",
        &format!("{{\"ts\":\"2027-06-01T00:00:00Z\",\"now\":{}}}", now),
    );
    assert!(out.contains("\"fresh\":false"), "{}", out);
    assert!(out.contains("\"reason\":\"skew\""), "{}", out);
    assert!(out.contains("in the future"), "{}", out);
    // Inside the skew allowance it is still current news.
    let out = run("window", &format!("{{\"ts\":{},\"now\":{}}}", now + 120, now));
    assert!(out.contains("\"fresh\":true"), "{}", out);
    // A caller can widen (or close) the allowance.
    let out = run(
        "window",
        &format!("{{\"ts\":{},\"now\":{},\"skew_s\":3600}}", now + 120, now),
    );
    assert!(out.contains("\"fresh\":true"), "{}", out);
    let out = run(
        "window",
        &format!("{{\"ts\":{},\"now\":{},\"skew_s\":0}}", now + 1, now),
    );
    assert!(out.contains("\"reason\":\"skew\""), "{}", out);
}

#[test]
fn window_unparsed_is_not_fresh() {
    let out = run("window", "{\"ts\":\"some time last week\",\"now\":1798761599}");
    assert!(out.contains("\"fresh\":false"), "{}", out);
    assert!(out.contains("\"reason\":\"unparsed\""), "{}", out);
    // The old assertion here pinned `"age_s":0`, which was a lie with a cost: a
    // reader that looks at `age_h` before `fresh` (two callers do exactly that:
    // `atlas-ingest::rules::Window`, `atlas-search::rules::Window`) saw an
    // unreadable feed date as "just published". A stamp that could not be read
    // has no age, so both age fields are null.
    assert!(out.contains("\"age_s\":null"), "{}", out);
    assert!(out.contains("\"age_h\":null"), "{}", out);
    assert!(out.contains("recognised"), "{}", out);
}

#[test]
fn window_missing_and_mistyped_keys() {
    assert!(crate::run("window", "{}").is_err());
    assert!(crate::run("window", "{\"ts\":42}").is_ok()); // numbers are accepted
    assert!(crate::run("window", "{\"ts\":true}").is_err());
    assert!(crate::run("window", "{\"ts\":\"2026-01-01\",\"max_age_s\":-1}").is_err());
    assert!(crate::run("window", "{\"ts\":\"2026-01-01\",\"skew_s\":\"x\"}").is_err());
    assert!(crate::run("window", "[1,2]").is_err());
}

#[test]
fn format_units_match_the_ui() {
    let cases = [
        (0i64, "now"),
        (-30, "now"), // inside the skew: a clock a little fast is not a future item
        (59, "now"),
        (60, "1 min"),
        (3599, "59 min"),
        (3600, "1 h"),
        (14_400, "4 h"),
        (86_399, "23 h"),
        (86_400, "1 d"),
        (172_800, "2 d"),
        (30 * 86_400, "30 d"),
    ];
    for (age, want) in cases {
        let out = run("format", &format!("{{\"age_s\":{}}}", age));
        assert!(
            out.contains(&format!("\"text\":\"{}\"", want)),
            "{} -> {}",
            age,
            out
        );
    }
    // The old case here was `(-5000, "now")`: a stamp dated 83 minutes ahead
    // printed the same label as one published a second ago, so the UI called a
    // future-dated item current. The label now carries the direction, using the
    // same 300 s skew the freshness window uses.
    for (age, want) in [(-5000i64, "in 1 h"), (-172_800, "in 2 d"), (-90_000, "in 1 d")] {
        let out = run("format", &format!("{{\"age_s\":{}}}", age));
        assert!(
            out.contains(&format!("\"text\":\"{}\"", want)),
            "{} -> {}",
            age,
            out
        );
        assert!(out.contains("\"future\":true"), "{} -> {}", age, out);
    }
    assert!(run("format", "{\"age_s\":3600}").contains("\"future\":false"));
}

#[test]
fn format_from_a_timestamp() {
    let out = run("format", "{\"ts\":\"2026-12-31T20:00:00Z\",\"now\":1798761599}");
    assert!(out.contains("\"text\":\"3 h\""), "{}", out);
    assert!(out.contains("\"age_s\":14399"), "{}", out);
    assert!(crate::run("format", "{}").is_err());
    assert!(crate::run("format", "{\"ts\":\"nonsense\"}").is_err());
}
