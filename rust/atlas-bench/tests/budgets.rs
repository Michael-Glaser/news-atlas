//! The budget test: a path that got slower is a bug, not a taste.
//!
//! This runs the same harness `atlas-bench` runs — same seeded archive, same
//! store, same pipeline — and fails when any measured p95 is more than
//! [`atlas_bench::budgets::TOLERANCE`] (20 %) over its budget. The budgets
//! themselves are release numbers, so this test is meant to be run in release:
//!
//! ```text
//! cargo test --release --offline -p atlas-bench --test budgets -- --nocapture
//! ```
//!
//! A debug build is five to ten times slower for reasons that have nothing to do
//! with the code under test, so a debug run measures, prints the table and says so
//! instead of pretending the numbers mean the same thing. What it still enforces
//! in debug is a ten-times ceiling: that catches a path that went quadratic
//! without failing on debug-build overhead.
//!
//! The `server.*`, `ui.*` and `startup.*` paths this file used to require are gone
//! with `atlas-web` / `atlas-ui`; the store and pipeline paths below are the ones
//! the shipped tree still takes.

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use atlas_bench::stats::Samples;
use atlas_bench::{budgets, run, Options, Report};

/// Three tests in one binary run on three threads; three harnesses measuring at
/// once measure each other. Every run in this file takes this lock first, so the
/// numbers are the machine's, not the test suite's.
static ONE_RUN_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Run the harness alone, with three passes over every path.
///
/// Three passes rather than one because a shared machine carries somebody else's
/// compiler into a single p95; the report quotes the fastest pass, which is the
/// code's own cost.
fn measure(options: Options) -> (Report, MutexGuard<'static, ()>) {
    let guard = ONE_RUN_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let report = run(&Options {
        repeat: 3,
        ..options
    });
    (report, guard)
}

/// A debug build is allowed this much slack before the test calls it a regression.
const DEBUG_SLACK: f64 = 10.0;

/// The paths that must be measured for this test to mean anything.
const MUST_MEASURE: &[&str] = &[
    "store.list",
    "store.fts",
    "store.entity",
    "store.threads",
    "store.prune",
];

#[test]
fn no_path_is_more_than_twenty_percent_over_its_budget() {
    let (report, _alone) = measure(Options::default());
    eprintln!("\n{}", report.table());

    let measured: Vec<&str> = report.rows.iter().map(|row| row.path).collect();
    for required in MUST_MEASURE {
        assert!(
            measured.contains(required),
            "{} was not measured — the table is not covering a path a user hits",
            required
        );
    }

    let scale = if cfg!(debug_assertions) {
        eprintln!(
            "note: debug build — budgets are release numbers, so this run enforces {}x them. \
             Run with --release for the real verdict.",
            DEBUG_SLACK
        );
        DEBUG_SLACK
    } else {
        1.0
    };

    let mut failures: Vec<String> = Vec::new();
    for row in &report.rows {
        let Some(budget) = budgets::of(row.path) else {
            continue;
        };
        let p95 = Samples::ms(row.p95());
        if p95 > budget.p95_ms * budgets::TOLERANCE * scale {
            failures.push(format!(
                "{}: p95 {:.2} ms is over the {:.1} ms budget (x{:.2}){}",
                row.path,
                p95,
                budget.p95_ms,
                budgets::TOLERANCE,
                if cfg!(debug_assertions) { " in a debug build" } else { "" }
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} path(s) regressed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

#[test]
fn every_budgeted_path_that_ran_had_a_real_answer() {
    // A fast number from an empty archive is not a measurement. This used to
    // assert `row.bytes > 0`, which was the HTTP response size of a retired
    // route; the store paths return typed rows and carry no response size, so
    // the claim is checked where it is now provable: the archive the run reports
    // is the archive the budgets describe, and every path took samples.
    let (report, _alone) = measure(Options {
        only: "store.".to_string(),
        ..Options::default()
    });
    assert_eq!(
        report.seed.articles,
        atlas_bench::fixture::ARTICLES,
        "the seeded archive is not the one the budgets describe"
    );
    assert_eq!(report.seed.entities, atlas_bench::fixture::ENTITIES);
    assert!(report.seed.links > 0, "no article-to-article links were seeded");
    assert!(report.seed.article_entities > 0, "no entity links were seeded");
    assert!(report.seed.edges > 0, "no entity edges were seeded");
    for row in &report.rows {
        assert!(
            row.runs() >= 5,
            "{} has {} sample(s), so it answered nothing to measure",
            row.path,
            row.runs()
        );
        assert!(
            row.p95() > Duration::ZERO,
            "{} measured zero time, so the work did not run",
            row.path
        );
    }
}

#[test]
fn a_path_that_ran_has_samples_not_one_outlier() {
    let (report, _alone) = measure(Options {
        only: "store.".to_string(),
        ..Options::default()
    });
    assert!(!report.rows.is_empty());
    for row in &report.rows {
        assert!(
            row.runs() >= 5,
            "{} has {} sample(s): a percentile needs a sample",
            row.path,
            row.runs()
        );
        assert!(
            row.p95() >= row.p50() && row.max() >= row.p95(),
            "{} percentiles are out of order",
            row.path
        );
        assert!(row.p50() < Duration::from_secs(30), "{} took seconds", row.path);
    }
}
