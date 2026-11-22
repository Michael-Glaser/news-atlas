//! The verification report: one structure, two renderings.
//!
//! Decisions: what a report is allowed to say (counts, ids, schema names and reasons
//! — never article text, never a prompt), that a `Broken` finding is what makes a run
//! fail and a `Note` never does, and that the JSON form carries the same fields as the
//! text form so a script and a human read one truth.
//!
//! It does not: run a check, decide an exit code ([`crate::cli`]), or write a file.

use serde_json::{json, Value};

use crate::findings::{Finding, Severity};
use crate::fts::FtsHealth;
use crate::sample::{TableCount, TableSample};

/// Everything `verify` saw.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The database path, as given.
    pub database: String,
    pub tables: Vec<TableCount>,
    pub samples: Vec<TableSample>,
    /// Findings, broken first, then notes, each group in kind order.
    pub findings: Vec<Finding>,
    /// Checks that could not run, and why — never silent.
    pub skipped: Vec<String>,
    pub fts: FtsHealth,
    /// Rows examined by the place check, for the measurement behind the claim.
    pub place_examined: usize,
}

impl Report {
    pub fn broken(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.is_broken())
            .collect()
    }

    pub fn notes(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|finding| !finding.is_broken())
            .collect()
    }

    /// Nothing disagrees.
    pub fn is_clean(&self) -> bool {
        self.broken().is_empty()
    }

    /// How many rows every fix would touch, if it were run.
    pub fn fixable_rows(&self) -> usize {
        self.findings
            .iter()
            .filter_map(|finding| finding.batch.as_ref())
            .map(|batch| batch.rows())
            .sum()
    }

    /// How many rows the archive holds for one kind, as the checks counted them.
    fn count_of(&self, kind: crate::findings::Kind) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .map(|finding| finding.count)
            .sum()
    }

    /// `complete`, or the two directions named separately.
    ///
    /// The old line printed `articles - documents`, which reads as "1 missing or orphaned"
    /// whether one article is unindexed or one stray document is indexed, and prints a
    /// *negative* count when the index holds more entries than `articles`. Two counts
    /// cannot be wrong in the same way.
    fn membership_text(&self) -> String {
        let missing = self.count_of(crate::findings::Kind::FtsMissing);
        let orphaned = self.count_of(crate::findings::Kind::FtsOrphan);
        if missing == 0 && orphaned == 0 {
            return if self.fts.articles == self.fts.documents {
                "complete".to_string()
            } else {
                // The counts disagree but no finding named a row: say what is measured
                // rather than a difference that may point the wrong way.
                "the index and the articles table disagree; see the findings".to_string()
            };
        }
        let mut parts: Vec<String> = Vec::new();
        if missing > 0 {
            parts.push(match missing {
                1 => "1 article not indexed".to_string(),
                other => format!("{} articles not indexed", other),
            });
        }
        if orphaned > 0 {
            parts.push(match orphaned {
                1 => "1 index entry with no article".to_string(),
                other => format!("{} index entries with no article", other),
            });
        }
        parts.join(" · ")
    }

    /// The text report: monochrome, English, no colour and no gradient.
    pub fn to_text(&self, samples: bool) -> String {
        let mut out = String::new();
        out.push_str("ATLAS archive verify — read-only\n");
        out.push_str(&format!("database: {}\n", self.database));
        out.push_str("\ntables (rows)\n");
        let widest = self
            .tables
            .iter()
            .map(|count| count.table.len())
            .max()
            .unwrap_or(8);
        for count in &self.tables {
            out.push_str(&format!(
                "  {:<width$} {:>8}\n",
                count.table,
                count.rows,
                width = widest
            ));
        }
        out.push_str("\nfull text\n");
        out.push_str(&format!(
            "  articles {} · indexed documents {} · {}\n",
            self.fts.articles,
            self.fts.documents,
            self.membership_text()
        ));
        out.push_str(&format!(
            "  ORDER BY rank {} · snippet() {} · {} probed · {} stale\n",
            yes_no(self.fts.rank_ok),
            yes_no(self.fts.snippet_ok),
            self.fts.probed,
            self.fts.stale
        ));
        if self.place_examined > 0 {
            out.push_str(&format!(
                "  place check read {} row{} (one place lookup each)\n",
                self.place_examined,
                plural(self.place_examined)
            ));
        }

        out.push_str(&format!(
            "\nfindings ({} broken, {} notes)\n",
            self.broken().len(),
            self.notes().len()
        ));
        if self.findings.is_empty() {
            out.push_str("  nothing disagrees\n");
        }
        for finding in &self.findings {
            out.push_str("  ");
            out.push_str(&finding.line());
            if !finding.ids.is_empty() {
                out.push_str(" ids ");
                out.push_str(&finding.ids.join(","));
            }
            if finding.fixable() {
                let rows = finding.batch.as_ref().map(|batch| batch.rows()).unwrap_or(0);
                out.push_str(&format!(" → fixable ({} row{})", rows, plural(rows)));
            }
            out.push('\n');
        }        if !self.skipped.is_empty() {
            out.push_str("\nchecks that did not run\n");
            for reason in &self.skipped {
                out.push_str(&format!("  - {}\n", reason));
            }
        }
        if samples {
            out.push_str("\nsamples (first rows by rowid)\n");
            for sample in &self.samples {
                out.push_str(&format!("  {} ({} shown)\n", sample.table, sample.rows.len()));
                for row in &sample.rows {
                    out.push_str("    ");
                    out.push_str(&TableSample::line(row));
                    out.push('\n');
                }
            }
        }
        out
    }

    /// The same report as one JSON object.
    pub fn to_json(&self) -> Value {
        let findings: Vec<Value> = self
            .findings
            .iter()
            .map(|finding| {
                json!({
                    "kind": finding.kind.id(),
                    "severity": match finding.kind.severity() {
                        Severity::Broken => "broken",
                        Severity::Note => "note",
                    },
                    "subject": finding.subject,
                    "count": finding.count,
                    "ids": finding.ids,
                    "detail": finding.detail,
                    "fixable": finding.fixable(),
                    "statement": finding.batch.as_ref().map(|batch| json!({
                        "sql": batch.sql,
                        "rows": batch.rows(),
                        "note": batch.note,
                    })),
                })
            })
            .collect();
        let samples: Vec<Value> = self
            .samples
            .iter()
            .map(|sample| {
                json!({
                    "table": sample.table,
                    "columns": sample.columns,
                    "rows": sample
                        .rows
                        .iter()
                        .map(|row| {
                            let mut map = serde_json::Map::new();
                            for (column, value) in row {
                                map.insert(column.clone(), json!(value));
                            }
                            Value::Object(map)
                        })
                        .collect::<Vec<Value>>(),
                })
            })
            .collect();
        json!({
            "database": self.database,
            "read_only": true,
            "tables": self.tables.iter().map(|count| json!({
                "table": count.table,
                "rows": count.rows,
            })).collect::<Vec<Value>>(),
            "full_text": {
                "articles": self.fts.articles,
                "documents": self.fts.documents,
                "complete": self.fts.complete(),
                "order_by_rank": self.fts.rank_ok,
                "snippet": self.fts.snippet_ok,
                "probed": self.fts.probed,
                "stale": self.fts.stale,
            },
            "findings": findings,
            "broken": self.broken().len(),
            "notes": self.notes().len(),
            "fixable_rows": self.fixable_rows(),
            "skipped": self.skipped,
            "place_rows_examined": self.place_examined,
            "samples": samples,
        })
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "ok"
    } else {
        "failed"
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Batch, Kind};

    #[test]
    fn a_clean_archive_verifies_clean() {
        let path = crate::fixture::clean_database("report-clean");
        let report = crate::verify::verify(&path, &crate::fixture::FakeRules::new(), crate::verify::Limits::default())
            .expect("verify");
        assert!(report.to_text(false).contains("complete"), "{}", report.to_text(false));
    }

    #[test]
    fn the_full_text_line_never_prints_a_negative_or_ambiguous_count() {
        // Two failures, two directions: a straggling index document and an unindexed
        // article both used to read as the single number `articles - documents`, which is
        // one too few in one direction and *below zero* in the other.
        let mut report = Report::default();
        report.fts.articles = 6;
        report.fts.documents = 7;
        report.findings.push(Finding::new(
            crate::findings::Kind::FtsOrphan,
            "article_fts",
            2,
        ));
        let text = report.to_text(false);
        assert!(!text.contains("-1"), "a negative count reached the report: {}", text);
        assert!(
            text.contains("2 index entries with no article"),
            "{}",
            text
        );
        assert!(!text.contains("missing or orphaned"), "{}", text);

        let mut both = Report::default();
        both.fts.articles = 6;
        both.fts.documents = 6;
        both.findings.push(Finding::new(
            crate::findings::Kind::FtsMissing,
            "article_fts",
            1,
        ));
        both.findings.push(Finding::new(
            crate::findings::Kind::FtsOrphan,
            "article_fts",
            1,
        ));
        let text = both.to_text(false);
        assert!(text.contains("1 article not indexed"), "{}", text);
        assert!(text.contains("1 index entry with no article"), "{}", text);
    }

    #[test]
    fn a_note_never_fails_the_run_and_a_broken_finding_does() {
        let mut report = Report::default();
        report
            .findings
            .push(Finding::new(Kind::EntityUntyped, "entities", 4));
        assert!(report.is_clean(), "a coverage note is not a disagreement");
        report
            .findings
            .push(Finding::new(Kind::DanglingArticle, "article_tags", 2));
        assert!(!report.is_clean());
        let text = report.to_text(false);
        assert!(text.contains("1 broken, 1 notes"));
        assert!(text.contains("entity.untyped"));
    }

    #[test]
    fn the_json_form_names_every_field_a_script_needs() {
        let mut report = Report::default();
        report.database = "data/atlas.db".to_string();
        let mut batch = Batch::new(Kind::PublisherVariant, "articles", "UPDATE articles SET publisher=? WHERE id=?");
        batch.push(vec![
            atlas_store::SqlValue::Text("Folha".to_string()),
            atlas_store::SqlValue::Integer(4),
        ]);
        report.findings.push(
            Finding::new(Kind::PublisherVariant, "articles", 1).with_batch(batch),
        );
        let value = report.to_json();
        assert_eq!(value["read_only"], json!(true));
        assert_eq!(value["findings"][0]["statement"]["rows"], json!(1));
        assert_eq!(value["fixable_rows"], json!(1));
        assert_eq!(value["broken"], json!(1));
    }
}
