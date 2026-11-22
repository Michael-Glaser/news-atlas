//! The budgets: how slow a path may be before it is a bug.
//!
//! Decisions: one number per path — the p95 a release build must stay under on the
//! archive `docs/PERFORMANCE.md` describes — and that a regression is defined as
//! *more than 20 % over* it, so run-to-run noise on a shared machine is not a
//! failure while a real regression is.
//!
//! It does not: measure (that is [`crate::stats`]) or print (that is
//! [`crate::report`]). `tests/budgets.rs` enforces this table; the binary prints
//! it beside every number.
//!
//! Every budget carries the reason it has that value. A budget with no reason is a
//! guess, and a guess that fails is not a finding.
//!
//! # What left the table
//!
//! The `server.*`, `ui.*` and `startup.*` rows went with the crates they timed:
//! `atlas-web` (the HTTP routes and the SSE stream), `atlas-ui` (the wasm list and
//! graph frames) and the `exec`-to-bound-socket measurement. Those paths no longer
//! exist in the shipped tree, so a budget for one would protect nothing and could
//! never fail for the right reason. The window's own paint costs are bounded in
//! `atlas-desktop/tests/ui_layout.rs`, on rects rather than milliseconds, because a
//! headless frame has no compositor to wait for.

/// How far over a budget a measurement may drift before the test fails.
pub const TOLERANCE: f64 = 1.20;

/// One path's ceiling.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// The path id a [`crate::stats::Samples`] carries.
    pub path: &'static str,
    /// p95 ceiling in milliseconds.
    pub p95_ms: f64,
    /// Why this number: what a user is doing while they wait for it.
    pub why: &'static str,
}

/// The table, in report order.
///
/// Each `p95_ms` is the measured p95 on this harness's archive (the numbers in
/// `docs/PERFORMANCE.md`), rounded up and given about 50 % room for a slower
/// machine; [`TOLERANCE`] on top of that means a test fails at roughly 1.8x the
/// measured number, which is a regression nobody would argue about — and not the
/// 1.3x a compiler argument between agents can produce on a shared machine.
pub const BUDGETS: &[Budget] = &[
    // ---- store -----------------------------------------------------------
    Budget {
        path: "store.list",
        p95_ms: 1.0,
        why: "the list statement alone, 200 rows, no HTTP; measured 0.26 ms",
    },
    Budget {
        path: "store.fts",
        p95_ms: 5.0,
        why: "one FTS5 match over the whole archive; measured 1.8 ms",
    },
    Budget {
        path: "store.entity",
        p95_ms: 4.0,
        why: "the model's `entity_profile` tool, not a bare lookup: it resolves the name (exact, then alias, then closest), then reads 12 stories, 25 neighbours and the aliases; measured 2.1 ms p95 on 5 000 articles / 20 000 entities / 60 000 edges. It was 0.5 ms while it timed `atlas-web`'s `find_entity` alone — that route was deleted and the budget now covers the call the shipped tool makes",
    },
    Budget {
        path: "store.threads",
        p95_ms: 90.0,
        why: "a full 60 000-link rebuild, after a collection pass and not per request; measured 46 ms p95",
    },
    Budget {
        path: "store.prune",
        p95_ms: 8.0,
        why: "retention runs on a schedule, in 400-row batches; measured 2.8 ms",
    },
    // ---- pipeline --------------------------------------------------------
    Budget {
        path: "pipeline.engines",
        p95_ms: 40.0,
        why: "one article through the shipped rules — three questions in-process, two binaries",
    },
    Budget {
        path: "pipeline.spawned",
        p95_ms: 90.0,
        why: "the same five questions, every one a subprocess: the witness for the spawn cost",
    },
    Budget {
        path: "pipeline.inprocess",
        p95_ms: 2.0,
        why: "three of those stages called as libraries; measured 0.39 ms",
    },
];

/// The budget for one path, or `None` if the table has no opinion.
pub fn of(path: &str) -> Option<Budget> {
    BUDGETS.iter().copied().find(|budget| budget.path == path)
}

/// Is `measured` a regression: more than [`TOLERANCE`] over the budget?
pub fn regressed(path: &str, p95_ms: f64) -> Option<Budget> {
    let budget = of(path)?;
    if p95_ms > budget.p95_ms * TOLERANCE {
        Some(budget)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_budget_states_a_reason_and_a_positive_number() {
        for budget in BUDGETS {
            assert!(budget.p95_ms > 0.0, "{} has no ceiling", budget.path);
            assert!(
                budget.why.len() > 12,
                "{} needs a reason, not a word",
                budget.path
            );
        }
    }

    #[test]
    fn there_is_exactly_one_budget_per_path() {
        let mut paths: Vec<&str> = BUDGETS.iter().map(|budget| budget.path).collect();
        let count = paths.len();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), count, "a path appears twice in the table");
    }

    #[test]
    fn a_regression_is_twenty_percent_over_and_not_less() {
        // `store.list` is budgeted at 1 ms: 1.19 ms is noise, 1.21 ms is a bug.
        // (The example used to be `server.status`, a path that no longer exists.)
        assert!(regressed("store.list", 1.0 * 1.10).is_none());
        assert!(regressed("store.list", 1.0 * 1.19).is_none());
        assert!(regressed("store.list", 1.0 * 1.21).is_some());
        assert!(regressed("no.such.path", 1_000.0).is_none());
    }
}
