//! The report: one table, the numbers in it, and the verdicts.
//!
//! Decisions: the column set (path, runs, min, p50, p95, max, response size,
//! budget, verdict), that a path with no budget says `-` instead of borrowing
//! someone else's, and that the machine-readable form is the same table.
//!
//! It does not: measure or decide a budget.

use crate::budgets::{self, TOLERANCE};
use crate::fixture::SeedReport;
use crate::stats::Samples;

/// Everything one run measured, plus what it measured it on.
pub struct Report {
    pub seed: SeedReport,
    pub database_bytes: u64,
    /// Where the seeded archive lived (already removed when the report is built).
    pub archive_root: String,
    pub rows: Vec<Samples>,
    /// Free-form notes: what could not be measured here, and why.
    pub notes: Vec<String>,
}

impl Report {
    /// The paths that broke their budget by more than [`TOLERANCE`].
    pub fn regressions(&self) -> Vec<(&'static str, f64, f64)> {
        let mut out: Vec<(&'static str, f64, f64)> = Vec::new();
        for samples in &self.rows {
            let p95 = Samples::ms(samples.p95());
            if let Some(budget) = budgets::regressed(samples.path, p95) {
                out.push((samples.path, p95, budget.p95_ms));
            }
        }
        out
    }

    /// The table, as text.
    pub fn table(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "archive: {} articles, {} entities, {} article links, {} article-entity links, \
             {} edges, {} threads, {:.1} MB\n",
            self.seed.articles,
            self.seed.entities,
            self.seed.links,
            self.seed.article_entities,
            self.seed.edges,
            self.seed.threads,
            self.database_bytes as f64 / 1_048_576.0
        ));
        out.push_str(&format!(
            "{:<24} {:>5} {:>9} {:>9} {:>9} {:>9} {:>10} {:>8}  {}\n",
            "path", "runs", "min", "p50", "p95", "max", "bytes", "budget", "verdict"
        ));
        out.push_str(&format!("{}\n", "-".repeat(96)));
        for samples in &self.rows {
            let p95 = Samples::ms(samples.p95());
            let (budget, verdict) = match budgets::of(samples.path) {
                None => ("-".to_string(), "no budget".to_string()),
                Some(budget) => {
                    let over = p95 > budget.p95_ms * TOLERANCE;
                    let verdict = if over {
                        format!("REGRESSION (> {:.0}% of {:.0})", TOLERANCE * 100.0, budget.p95_ms)
                    } else if p95 > budget.p95_ms {
                        "over budget".to_string()
                    } else {
                        "ok".to_string()
                    };
                    (format!("{:.1}", budget.p95_ms), verdict)
                }
            };
            out.push_str(&format!(
                "{:<24} {:>5} {:>7.2}ms {:>7.2}ms {:>7.2}ms {:>7.2}ms {:>10} {:>8}  {}\n",
                samples.path,
                samples.runs(),
                Samples::ms(samples.min()),
                Samples::ms(samples.p50()),
                p95,
                Samples::ms(samples.max()),
                samples.bytes,
                budget,
                verdict
            ));
        }
        if !self.notes.is_empty() {
            out.push_str("\nnotes\n");
            for note in &self.notes {
                out.push_str(&format!("  - {}\n", note));
            }
        }
        out
    }

    /// The same numbers as one JSON object, for a machine to diff.
    pub fn json(&self) -> serde_json::Value {
        let paths: Vec<serde_json::Value> = self
            .rows
            .iter()
            .map(|samples| {
                let budget = budgets::of(samples.path);
                serde_json::json!({
                    "path": samples.path,
                    "runs": samples.runs(),
                    "min_ms": Samples::ms(samples.min()),
                    "p50_ms": Samples::ms(samples.p50()),
                    "p95_ms": Samples::ms(samples.p95()),
                    "max_ms": Samples::ms(samples.max()),
                    "bytes": samples.bytes,
                    "budget_ms": budget.map(|budget| budget.p95_ms),
                    "regression": budgets::regressed(samples.path, Samples::ms(samples.p95())).is_some(),
                })
            })
            .collect();
        serde_json::json!({
            "seed": {
                "articles": self.seed.articles,
                "entities": self.seed.entities,
                "links": self.seed.links,
                "article_entities": self.seed.article_entities,
                "edges": self.seed.edges,
                "threads": self.seed.threads,
                "database_bytes": self.database_bytes,
            },
            "paths": paths,
            "archive_root": self.archive_root,
            "notes": self.notes,
        })
    }
}
