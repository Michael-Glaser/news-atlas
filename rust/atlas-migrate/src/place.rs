//! A stored place the article's own text does not support.
//!
//! Decisions: whether a stored `place` occurs in the article's title, summary or body
//! (case-insensitive substring — a place the text names is evidence, and is left
//! alone), and, when it does not, whether `atlas-places` can name a place from that
//! same text. Only a place the engine *names* replaces the stored one; when the
//! engine cannot name anything, the row is reported and nothing is written — a
//! carry-over may not invent a location, and clearing one it cannot replace would
//! lose the outlet anchor the map draws from.
//!
//! It does not: decide what a place is (that is `atlas-places`), keep a gazetteer, or
//! read a word list: the comparison is substring and the verdict is the engine's.

use atlas_store::SqlValue;

use crate::error::Result;
use crate::findings::{Batch, Finding, Kind};
use crate::open::Archive;
use crate::queries;

/// How much a place value claims: a city is more evidence than a country, and an outlet
/// anchor is the least evidence there is (it is where the newspaper sits, not where the
/// story happened).
pub fn precision_rank(prec: &str) -> u8 {
    match prec.trim() {
        "city" => 3,
        "country" => 2,
        "outlet" => 1,
        _ => 0,
    }
}

/// What one place check found.
#[derive(Clone, Debug, Default)]
pub struct PlaceOutcome {
    pub findings: Vec<Finding>,
    /// Rows where the stored place is not spelled out in the text but the engine
    /// names the same place: not a disagreement, so not a finding.
    pub confirmed: usize,
    /// Rows the engine was asked about, for the measurement behind the claim.
    pub examined: usize,
    /// Of those, the rows `atlas-places` gave no answer for at all (it refused, or the row
    /// vanished): counted, so the caller can say the check is partial instead of printing
    /// an examined count that reads like agreement.
    pub refused: usize,
}

/// Check the stored places the text does not support, up to `limit` rows.
///
/// `limit` exists because every row costs one `atlas-places` process: a bounded,
/// measured pass beats an unbounded one. The finding's detail says when the bound
/// was reached, so a truncated run is never silent.
pub fn check(
    archive: &Archive,
    rules: &dyn atlas_ingest::Rules,
    limit: usize,
) -> Result<PlaceOutcome> {
    let unsupported = archive.query(queries::PLACE_UNSUPPORTED, &[])?;
    let mut outcome = PlaceOutcome::default();
    if unsupported.is_empty() {
        return Ok(outcome);
    }
    let mut batch = Batch::new(Kind::PlaceUnsupported, "articles", queries::SET_PLACE);
    let mut replace_ids: Vec<String> = Vec::new();
    let mut unnamed: Vec<String> = Vec::new();
    let mut weaker: Vec<String> = Vec::new();
    let mut examined = 0usize;
    for row in unsupported.iter().take(limit) {
        let id = row.int("id").unwrap_or(0);
        let stored = row.text("place").unwrap_or("").trim().to_string();
        let stored_prec = precision_rank(row.text("geo_src").unwrap_or(""));
        let article = match archive.query_one(
            queries::ARTICLE_TEXT,
            &[SqlValue::Integer(id)],
        )? {
            Some(article) => article,
            None => {
                outcome.refused += 1;
                continue;
            }
        };
        examined += 1;
        outcome.examined += 1;
        let located = match rules.locate(&atlas_ingest::ArticleText {
            title: article.text("title").unwrap_or("").to_string(),
            summary: article.text("summary").unwrap_or("").to_string(),
            content: article.text("body").unwrap_or("").to_string(),
            url: article.text("url").unwrap_or("").to_string(),
            publisher: article.text("publisher").unwrap_or("").to_string(),
            ..atlas_ingest::ArticleText::default()
        }) {
            Ok(located) => located,
            // The engine refused this row: counted, and the caller reports the check as
            // partial. An engine that answers nothing is not an engine that agreed.
            Err(_) => {
                outcome.refused += 1;
                continue;
            }
        };
        if !located.named() {
            unnamed.push(id.to_string());
            continue;
        }
        if located.place.eq_ignore_ascii_case(&stored) {
            outcome.confirmed += 1;
            continue;
        }
        // A place the engine derived from the outlet's country says nothing about this
        // story, and a country is less evidence than the city already stored: neither
        // replaces what the archive has. Replacing a place with a weaker one is not a
        // repair.
        if precision_rank(&located.prec) < stored_prec {
            weaker.push(id.to_string());
            continue;
        }
        // The last bound value is the place the check rejected: the statement only writes
        // while the row still holds it, so a replayed plan cannot overwrite a place that
        // was re-derived in between.
        batch.push(vec![
            SqlValue::Text(located.place.clone()),
            SqlValue::Text(located.iso.clone()),
            SqlValue::Real(located.lat),
            SqlValue::Real(located.lon),
            SqlValue::Real(located.conf),
            SqlValue::Text(located.prec.clone()),
            SqlValue::Integer(id),
            SqlValue::Text(stored.clone()),
        ]);
        replace_ids.push(id.to_string());
    }
    let truncated = unsupported.len() > limit;
    if !batch.is_empty() {
        let detail = if truncated {
            format!(
                "{} of {} rows examined (limit reached)",
                examined,
                unsupported.len()
            )
        } else {
            format!("{} rows examined", examined)
        };
        outcome.findings.push(
            Finding::new(Kind::PlaceUnsupported, "articles", batch.rows())
                .with_ids(replace_ids)
                .with_detail(detail)
                .with_batch(batch),
        );
    }
    if !unnamed.is_empty() {
        outcome.findings.push(
            Finding::new(Kind::PlaceUnnamed, "articles", unnamed.len())
                .with_ids(unnamed)
                .with_detail("the place engine names nothing in this text"),
        );
    }
    if !weaker.is_empty() {
        outcome.findings.push(
            Finding::new(Kind::PlaceUnnamed, "articles", weaker.len())
                .with_ids(weaker)
                .with_detail("the engine's answer is less precise than the stored place"),
        );
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FakeRules;

    #[test]
    fn a_place_the_text_names_is_evidence_and_is_kept() {
        let path = crate::fixture::database_with_inconsistencies("place-kept");
        let archive = Archive::open_read_only(&path).expect("open");
        // The fixture's article 3 says Shenzhen and stores Shenzhen.
        let rules = FakeRules::new();
        let outcome = check(&archive, &rules, 50).expect("check");
        for finding in &outcome.findings {
            assert!(
                !finding.ids.contains(&"3".to_string()),
                "a place the text names must not be rewritten"
            );
        }
    }

    #[test]
    fn an_unsupported_place_is_replaced_only_when_the_engine_names_one() {
        let path = crate::fixture::database_with_inconsistencies("place-replace");
        let archive = Archive::open_read_only(&path).expect("open");
        // The fixture's article 4 stores Washington and talks about Berlin only;
        // the fake engine answers Berlin.
        let rules = FakeRules::new();
        let outcome = check(&archive, &rules, 50).expect("check");
        let replaced = outcome
            .findings
            .iter()
            .find(|finding| finding.kind == Kind::PlaceUnsupported)
            .expect("a replacement finding");
        assert!(replaced.ids.contains(&"4".to_string()));
        assert!(replaced.fixable());
        let batch = replaced.batch.as_ref().unwrap();
        let values: Vec<&str> = batch
            .params
            .iter()
            .filter_map(|row| match row.first() {
                Some(SqlValue::Text(text)) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(values.contains(&"Berlin"));
    }

    #[test]
    fn a_weaker_place_never_replaces_a_stronger_one() {
        let path = crate::fixture::database_with_inconsistencies("place-weaker");
        let archive = Archive::open_read_only(&path).expect("open");
        // Article 4 stores Washington as a city; an outlet anchor is less evidence, so
        // nothing is written and the row is reported as a note.
        let rules = FakeRules::outlet_only();
        let outcome = check(&archive, &rules, 50).expect("check");
        assert!(
            !outcome
                .findings
                .iter()
                .any(|finding| finding.kind == Kind::PlaceUnsupported),
            "a less precise place replaced a city"
        );
        let note = outcome
            .findings
            .iter()
            .find(|finding| {
                finding.kind == Kind::PlaceUnnamed
                    && finding.detail.contains("less precise")
            })
            .expect("the weaker answer is reported");
        assert!(note.ids.contains(&"4".to_string()));
        assert!(!note.fixable());
    }

    #[test]
    fn an_engine_that_refuses_is_counted_and_the_report_says_the_check_is_partial() {
        // A missing or crashing `atlas-places` used to be swallowed: the run printed
        // "examined N" and nothing else, which reads exactly like an engine that agreed.
        let path = crate::fixture::database_with_inconsistencies("place-refused");
        let rules = FakeRules::refusing_places();
        let archive = Archive::open_read_only(&path).expect("open");
        let outcome = check(&archive, &rules, 50).expect("check");
        assert!(outcome.refused > 0, "a refused row must be counted");
        assert_eq!(
            outcome.refused, outcome.examined,
            "every row read was refused, so none of them agreed"
        );
        assert!(
            outcome.findings.is_empty(),
            "a refusal is not a finding about the archive"
        );

        let report = crate::verify::verify(&path, &rules, crate::verify::Limits::default())
            .expect("verify");
        let partial = report
            .skipped
            .iter()
            .find(|reason| reason.contains("got no answer"))
            .expect("the report says the place check is partial");
        assert!(partial.contains("left exactly as they were"), "{}", partial);
    }

    #[test]
    fn precision_is_ordered_city_country_outlet_nothing() {        assert!(precision_rank("city") > precision_rank("country"));
        assert!(precision_rank("country") > precision_rank("outlet"));
        assert!(precision_rank("outlet") > precision_rank(""));
        assert_eq!(precision_rank("none"), 0);
    }

    #[test]
    fn an_engine_that_names_nothing_writes_nothing() {
        let path = crate::fixture::database_with_inconsistencies("place-unnamed");
        let archive = Archive::open_read_only(&path).expect("open");
        // Article 5 stores Lisbon and its text names no place at all.
        let rules = FakeRules::new();
        let outcome = check(&archive, &rules, 50).expect("check");
        let unnamed = outcome
            .findings
            .iter()
            .find(|finding| finding.kind == Kind::PlaceUnnamed)
            .expect("an unnameable place is reported");
        assert!(unnamed.ids.contains(&"5".to_string()));
        assert!(!unnamed.fixable(), "no engine answer, no statement");
    }
}
