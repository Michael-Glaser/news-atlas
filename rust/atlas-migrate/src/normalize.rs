//! Repairing what the migration broke: plan first, print every statement, then one
//! transaction.
//!
//! Decisions: a fix is only ever built from a finding the checks already proved
//! ([`crate::report::Report`] → [`Plan`]), the whole plan runs in **one** transaction
//! so a reader never sees half a repair, a full-text rebuild is FTS5's own
//! `'rebuild'` command, and running the same plan twice changes nothing the second
//! time because every statement is conditional on the state it fixes.
//!
//! It does not: find anything (`verify` does), delete a parent row, or write when the
//! caller did not pass `--apply` — `--dry-run` is the default and only prints.

use atlas_store::{SqlValue, Store};

use crate::error::Result;
use crate::findings::{Batch, Finding, Kind};
use crate::open;
use crate::queries;

/// The statements a run would execute.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub batches: Vec<Batch>,
    /// Findings with no fix in the archive itself, named so the report is complete.
    pub unfixable: Vec<Finding>,
}

impl Plan {
    /// How many rows the plan would change.
    pub fn rows(&self) -> usize {
        self.batches.iter().map(Batch::rows).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// The plan as text, one line per statement, with the values it will bind.
    ///
    /// This is "list every statement before running it": the SQL once, the row count,
    /// and up to eight bound tuples so a human can see which rows move.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if self.batches.is_empty() {
            out.push_str("no statement: nothing in the archive disagrees\n");
        }
        let mut index = 0usize;
        for batch in &self.batches {
            index += 1;
            out.push_str(&format!(
                "[{}] {} — {}, {} row{}\n",
                index,
                batch.kind.id(),
                batch.kind.reason(),
                batch.rows(),
                if batch.rows() == 1 { "" } else { "s" }
            ));
            out.push_str(&format!("    {}\n", statement_text(batch)));
            if !batch.note.is_empty() {
                out.push_str(&format!("    note: {}\n", batch.note));
            }
            for params in batch.params.iter().take(8) {
                out.push_str(&format!("    bound: {}\n", bind_text(batch, params)));
            }
            if batch.params.len() > 8 {
                out.push_str(&format!("    … and {} more rows\n", batch.params.len() - 8));
            }
        }
        if !self.unfixable.is_empty() {
            out.push_str("\nnot fixable here\n");
            for finding in &self.unfixable {
                out.push_str("  ");
                out.push_str(&finding.line());
                out.push('\n');
            }
        }
        out
    }
}

/// A statement plus the number of `?` it binds, so a reader can see the shape without
/// reading the code.
fn statement_text(batch: &Batch) -> String {
    let mut sql = batch.sql.replace('\n', " ");
    while sql.contains("  ") {
        sql = sql.replace("  ", " ");
    }
    sql
}

/// `(?1 …)` values, rendered through the report's redaction policy.
fn bind_text(_batch: &Batch, params: &[SqlValue]) -> String {
    if params.is_empty() {
        return "(no bound values — the statement's own subquery selects the rows)".to_string();
    }
    let rendered: Vec<String> = params
        .iter()
        .enumerate()
        .map(|(index, value)| crate::redact::cell(&format!("param{}", index), value))
        .collect();
    format!("({})", rendered.join(", "))
}

/// Build the plan from a report: pure, and the reason `--dry-run` cannot write.
pub fn plan(report: &crate::report::Report) -> Plan {
    let mut batches: Vec<Batch> = Vec::new();
    let mut unfixable: Vec<Finding> = Vec::new();

    // The full-text findings share one repair: FTS5's rebuild re-derives the whole
    // index from `articles`, which fixes a stale, missing or orphaned document in one
    // statement. It is listed once, whatever the checks found.
    let fts_broken = report.findings.iter().any(|finding| {
        matches!(
            finding.kind,
            Kind::FtsMissing | Kind::FtsOrphan | Kind::FtsStale | Kind::FtsRank | Kind::FtsSnippet
        )
    });
    if fts_broken {
        let mut batch = Batch::new(Kind::FtsStale, "article_fts", queries::FTS_REBUILD);
        let disagreeing: usize = report
            .findings
            .iter()
            .filter(|finding| {
                matches!(
                    finding.kind,
                    Kind::FtsMissing
                        | Kind::FtsOrphan
                        | Kind::FtsStale
                        | Kind::FtsRank
                        | Kind::FtsSnippet
                )
            })
            .map(|finding| finding.count)
            .sum();
        batch.plan_rows(disagreeing);
        // One statement re-derives *every* document from `articles`, so the number of rows
        // that disagreed (3) and the number of rows this statement writes (all of them) are
        // different questions. The note says both, in schema words.
        batch = batch.with_note(format!(
            "one statement: FTS5 re-derives all {} indexed document{} from articles \
             ({} disagreed)",
            report.fts.articles,
            if report.fts.articles == 1 { "" } else { "s" },
            disagreeing
        ));
        batches.push(batch);
    }

    for finding in &report.findings {
        match finding.batch.clone() {
            Some(batch) if !batch.is_empty() => batches.push(batch),
            Some(_) => {}
            None => {
                if finding.is_broken() {
                    unfixable.push(finding.clone());
                }
            }
        }
    }
    // A rebuild plus a dangling-reference batch both touch `article_fts`/
    // `article_*`: the order is the order the checks were reported in, which
    // `verify::sort_findings` fixed, so two runs plan identically.
    Plan { batches, unfixable }
}

/// What one apply did.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    pub statements: usize,
    pub rows: usize,
    pub fts_rebuilt: bool,
}

/// Run the plan in one transaction.
///
/// Every statement is parameterised and listed with the plan first; a failure rolls
/// the whole repair back, because a half-normalized archive is worse than one that
/// still disagrees.
///
/// Every statement in a plan is conditional on the state the check measured (`AND
/// TRIM(publisher)=?`, `… NOT IN (SELECT …)`, `WHERE dupe=1 AND relevant=1`), so run
/// `applied.rows` is the number of rows that actually changed: replaying a plan that was
/// built against an older archive reports `0` rather than rewriting a value it never saw.
pub fn apply(path: &std::path::Path, plan: &Plan) -> Result<Applied> {
    let store: Store = open::open_write(path)?;
    let mut applied = Applied::default();
    if plan.is_empty() {
        return Ok(applied);
    }
    let batches = plan.batches.clone();
    let outcome = store.in_transaction(|tx| {
        let mut done = Applied::default();
        for batch in &batches {
            if batch.params.is_empty() {
                // A statement that selects its own rows (`… NOT IN (SELECT …)`): run it
                // once, with nothing bound.
                tx.execute(batch.sql, [])
                    .map_err(atlas_store::StoreError::from)?;
                done.statements += 1;
                done.rows += tx.changes() as usize;
            } else {
                let mut statement =
                    tx.prepare(batch.sql).map_err(atlas_store::StoreError::from)?;
                for params in &batch.params {
                    statement
                        .execute(rusqlite::params_from_iter(params.iter()))
                        .map_err(atlas_store::StoreError::from)?;
                    done.statements += 1;
                    done.rows += tx.changes() as usize;
                }
            }
            if batch.sql == queries::FTS_REBUILD {
                done.fts_rebuilt = true;
            }
        }
        Ok(done)
    })?;
    applied.statements = outcome.statements;
    applied.rows = outcome.rows;
    applied.fts_rebuilt = outcome.fts_rebuilt;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FakeRules;
    use crate::verify::{verify, Limits};

    #[test]
    fn a_clean_archive_plans_nothing() {
        let path = crate::fixture::clean_database("plan-clean");
        let report = verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
        let planned = plan(&report);
        assert!(planned.is_empty(), "{}", planned.to_text());
        assert_eq!(planned.rows(), 0);
    }

    #[test]
    fn the_plan_lists_one_statement_per_repair_with_its_rows() {
        let path = crate::fixture::database_with_inconsistencies("plan-text");
        let report = verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
        let planned = plan(&report);
        let text = planned.to_text();
        assert!(text.contains("INSERT INTO article_fts(article_fts) VALUES('rebuild')"));
        assert!(text.contains("DELETE FROM article_tags WHERE article_id NOT IN"));
        assert!(text.contains("UPDATE articles SET publisher=? WHERE id=?"));
        // Every listed statement is bound or self-selecting, never a literal value.
        for batch in &planned.batches {
            assert!(!batch.sql.contains("Folha"), "a value reached the SQL text");
            assert!(!batch.sql.contains("NBC"));
        }
    }

    #[test]
    fn applying_twice_is_applying_once() {
        let path = crate::fixture::database_with_inconsistencies("plan-idempotent");
        let report = verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
        let planned = plan(&report);
        assert!(!planned.is_empty());
        let first = apply(&path, &planned).expect("apply");
        assert!(first.rows > 0);
        let again = verify(&path, &FakeRules::new(), Limits::default()).expect("verify again");
        let second = plan(&again);
        assert!(
            second.is_empty(),
            "a second run must find nothing to do: {}",
            second.to_text()
        );
    }
}
