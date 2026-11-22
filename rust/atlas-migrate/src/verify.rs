//! The verification pass: open read-only, read the documented statements, report.
//!
//! Decisions: what is checked (row counts, sampled rows, full-text health, references,
//! the invariants the data model states, publisher spellings, stored places, tag
//! labels), the order the report prints them in, and that a check whose engine is
//! missing is reported as *not run* with the reason — never as agreement.
//!
//! It does not: write, repair, or decide the exit code; [`crate::normalize`] plans
//! from the report, and [`crate::cli`] sets the code.

use std::path::Path;

use crate::engines;
use crate::error::Result;
use crate::findings::{Finding, Kind};
use crate::fts;
use crate::invariants;
use crate::open::Archive;
use crate::place;
use crate::publisher;
use crate::refs;
use crate::report::Report;
use crate::sample;
use crate::tags;

/// How far one `verify` run goes. Every bound is priced: a place row costs one
/// `atlas-places` process, an empty-publisher row costs one `atlas-text` process, a
/// full-text probe costs two reads.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Sampled rows per table.
    pub sample: usize,
    /// Rows the place check may examine.
    pub place: usize,
    /// Rows the full-text staleness probe may read.
    pub probe: usize,
    /// Empty-publisher rows the fill may ask `atlas-text` about: one process each.
    pub publisher: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            sample: 20,
            place: 400,
            probe: 20,
            publisher: 400,
        }
    }
}

/// Verify `path`. The connection is read-only, so this cannot change the archive.
pub fn verify(
    path: &Path,
    rules: &dyn atlas_ingest::Rules,
    limits: Limits,
) -> Result<Report> {
    let archive = Archive::open_read_only(path)?;
    let mut report = Report {
        database: path.display().to_string(),
        ..Report::default()
    };
    report.tables = sample::counts(&archive)?;
    report.samples = sample::samples(&archive, limits.sample)?;

    let (health, findings) = fts::check(&archive, limits.probe)?;
    report.fts = health;
    report.findings.extend(findings);
    report.findings.extend(refs::check(&archive)?);
    report.findings.extend(invariants::check(&archive)?);

    // Publisher spellings need no engine; filling an *empty* publisher does.
    report.findings.extend(publisher::check(&archive)?);
    match publisher::fill_empty(&archive, rules, limits.publisher) {
        Ok(Some(finding)) => report.findings.push(finding),
        Ok(None) => {}
        Err(error) => report.skipped.push(format!("publisher fill: {}", error.message())),
    }

    // Tag labels are a structural check (`atlas-ingest`'s own predicate): no engine.
    report.findings.extend(tags::check(&archive)?);

    // The place check costs one `atlas-places` process per unsupported row.
    if let Some(reason) = engines::answering(rules) {
        report.skipped.push(format!("place check: {}", reason));
    } else {
        let outcome = place::check(&archive, rules, limits.place)?;
        report.place_examined = outcome.examined;
        // A row `atlas-places` refused to answer is not a row that agreed: say so.
        if outcome.refused > 0 {
            report.skipped.push(format!(
                "place check: {} of the {} rows read got no answer from the engine; they \
                 were left exactly as they were",
                outcome.refused, outcome.examined
            ));
        }
        report.findings.extend(outcome.findings);
    }

    if let Ok(Some(note)) = tags::provenance(&archive) {
        report.findings.push(note);
    }

    sort_findings(&mut report.findings);
    Ok(report)
}

/// Broken first, then notes; inside a group, by kind id so two runs print the same
/// order.
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        let severity = |finding: &Finding| match finding.is_broken() {
            true => 0,
            false => 1,
        };
        severity(a)
            .cmp(&severity(b))
            .then_with(|| a.kind.id().cmp(b.kind.id()))
            .then_with(|| a.subject.cmp(&b.subject))
            .then_with(|| b.count.cmp(&a.count))
    });
}

/// Counts per kind, for the one-line summary a CLI prints after a fix.
pub fn kinds(report: &Report) -> Vec<(Kind, usize)> {
    let mut out: Vec<(Kind, usize)> = report
        .findings
        .iter()
        .map(|finding| (finding.kind, finding.count))
        .collect();
    out.sort_by_key(|(kind, _)| kind.id());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FakeRules;

    #[test]
    fn a_clean_database_verifies_clean() {
        let path = crate::fixture::clean_database("verify-clean");
        let report = verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
        assert!(report.is_clean(), "{}", report.to_text(false));
        assert_eq!(report.broken().len(), 0);
        assert!(report.fts.complete());
    }

    #[test]
    fn every_inconsistency_is_found_once_and_named() {
        let path = crate::fixture::database_with_inconsistencies("verify-all");
        let report = verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
        let ids: Vec<&str> = report.findings.iter().map(|f| f.kind.id()).collect();
        for expected in [
            "fts.missing",
            "fts.orphan",
            "fts.stale",
            "ref.articles",
            "ref.entities",
            "ref.aliases",
            "publisher.variant",
            "place.unsupported",
            "tag.invalid",
            "relevant.dupe",
            "edge.order",
            "entity.orphan",
        ] {
            assert!(ids.contains(&expected), "{} not reported: {:?}", expected, ids);
        }
        assert!(!report.is_clean());
    }

    #[test]
    fn a_report_of_the_archive_names_no_body_text() {
        let path = crate::fixture::database_with_inconsistencies("verify-redaction");
        let report = verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
        let text = report.to_text(true);
        // The fixture's bodies all carry this marker; it must not appear anywhere.
        assert!(
            !text.contains("BODYMARKER"),
            "article text reached the report"
        );
    }
}
