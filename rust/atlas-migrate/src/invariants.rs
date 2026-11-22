//! The invariants `docs/DATA-MODEL.md` states in prose, turned into statements.
//!
//! Decisions: which documented pairs must hold (`dupe=1` never with `relevant=1`;
//! `edges.a < edges.b`; `article_links.a < article_links.b`), that an article with no
//! URL or no headline is broken but not fixable here (the archive does not hold the
//! missing value), and that an entity nothing points at is a note, not a failure.
//!
//! It does not: decide what "relevant" or "dupe" mean (`atlas-store` does, at ingest
//! time), recompute a link, or touch a row.

use atlas_store::SqlValue;

use crate::error::Result;
use crate::findings::{Batch, Finding, Kind};
use crate::open::Archive;
use crate::queries;

/// Every invariant check.
pub fn check(archive: &Archive) -> Result<Vec<Finding>> {
    let mut findings: Vec<Finding> = Vec::new();

    if archive.has_table("articles")? {
        let rows = archive.query(queries::RELEVANT_DUPE, &[])?;
        if !rows.is_empty() {
            let mut batch =
                Batch::new(Kind::RelevantDupe, "articles", queries::FIX_RELEVANT_DUPE);
            batch.plan_rows(rows.len());
            findings.push(
                Finding::new(Kind::RelevantDupe, "articles", rows.len())
                    .with_ids(ids(&rows))
                    .with_detail("dupe=1 with relevant=1")
                    .with_batch(batch),
            );
        }
        let missing_url = archive.query(queries::ARTICLES_WITHOUT_URL, &[])?;
        if !missing_url.is_empty() {
            findings.push(
                Finding::new(Kind::ArticleWithoutUrl, "articles", missing_url.len())
                    .with_ids(ids(&missing_url))
                    .with_detail(
                        "the URL is not in the archive, so nothing here can rebuild it; these \
                         rows cannot be deduplicated, cached or re-read",
                    ),
            );
        }
        let missing_title = archive.query(queries::ARTICLES_WITHOUT_TITLE, &[])?;
        if !missing_title.is_empty() {
            findings.push(
                Finding::new(Kind::ArticleWithoutTitle, "articles", missing_title.len())
                    .with_ids(ids(&missing_title))
                    .with_detail("no headline stored; nothing here can invent one"),
            );
        }
    }

    if archive.has_table("edges")? {
        let self_edges = archive.query(queries::EDGE_SELF, &[])?;
        if !self_edges.is_empty() {
            let mut batch = Batch::new(Kind::EdgeSelf, "edges", queries::CLEAR_SELF_EDGES);
            batch.plan_rows(self_edges.len());
            findings.push(
                Finding::new(Kind::EdgeSelf, "edges", self_edges.len())
                    .with_ids(ids(&self_edges))
                    .with_detail("an edge from an entity to itself")
                    .with_batch(batch),
            );
        }
        let reversed = archive.query(queries::EDGE_ORDER, &[])?;
        if !reversed.is_empty() {
            let duplicated = archive.query(queries::EDGE_ORDER_MIRRORED, &[])?;
            let plain = archive.query(queries::EDGE_ORDER_PLAIN, &[])?;
            if !duplicated.is_empty() {
                // The correct row exists as well: swapping would collide with the
                // primary key, so the reversed copy is the one that goes.
                let mut batch = Batch::new(Kind::EdgeOrder, "edges", queries::CLEAR_MIRRORED_EDGES);
                batch.plan_rows(duplicated.len());
                findings.push(
                    Finding::new(Kind::EdgeOrder, "edges", duplicated.len())
                        .with_ids(ids(&duplicated))
                        .with_detail("a > b, and the same pair is already stored the right way round")
                        .with_batch(batch),
                );
            }
            if !plain.is_empty() {
                let mut batch = Batch::new(Kind::EdgeOrder, "edges", queries::FLIP_EDGES);
                batch.plan_rows(plain.len());
                findings.push(
                    Finding::new(Kind::EdgeOrder, "edges", plain.len())
                        .with_ids(ids(&plain))
                        .with_detail("a > b; the ends are swapped")
                        .with_batch(batch),
                );
            }
        }
    }

    if archive.has_table("article_links")? {
        let reversed = archive.query(queries::LINK_ORDER, &[])?;
        if !reversed.is_empty() {
            // No fix: swapping the ends of an `article_links` row would need the row's
            // own score and explanation recomputed by `atlas-graph`, and a carry-over
            // does not re-derive a similarity score.
            findings.push(
                Finding::new(Kind::LinkOrder, "article_links", reversed.len())
                    .with_ids(ids(&reversed))
                    .with_detail(
                        "a >= b; only atlas-graph can recompute the score and why line, so \
                         this row is reported and not rewritten",
                    ),
            );
        }
    }

    if archive.has_table("entities")? {
        let orphans = archive.query(queries::ORPHAN_ENTITIES, &[])?;
        if !orphans.is_empty() {
            findings.push(
                Finding::new(Kind::EntityOrphan, "entities", orphans.len())
                    .with_ids(ids(&orphans))
                    .with_detail(
                        "nothing links them, so search and the graph cannot reach them; a \
                         reader who searches the name will not find the article",
                    ),
            );
        }
        let untyped = archive.count(queries::UNTYPED_ENTITIES, &[])?;
        if untyped > 0 {
            // Named, not only counted: the note is the to-do list for the model pass, and
            // an id nobody can look up is not a to-do item.
            let rows = archive.query(
                queries::UNTYPED_ENTITY_IDS,
                &[SqlValue::Integer(20)],
            )?;
            findings.push(
                Finding::new(Kind::EntityUntyped, "entities", untyped as usize)
                    .with_ids(ids(&rows))
                    .with_detail(
                        "the state the model pass leaves a name it did not classify; run \
                         atlas-migrate carry --apply to classify them",
                    ),
            );
        }
    }

    Ok(findings)
}

fn ids(rows: &[atlas_store::Row]) -> Vec<String> {
    rows.iter()
        .take(20)
        .map(|row| match row.get("id") {
            Some(SqlValue::Integer(number)) => number.to_string(),
            Some(value) => crate::redact::cell("id", value),
            None => "?".to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_duplicate_marked_relevant_is_a_contradiction() {
        let path = crate::fixture::database_with_inconsistencies("invariants");
        let archive = Archive::open_read_only(&path).expect("open");
        let findings = check(&archive).expect("check");
        let dupe = findings
            .iter()
            .find(|finding| finding.kind == Kind::RelevantDupe)
            .expect("dupe/relevant conflict");
        assert!(dupe.fixable());
        assert!(dupe.count >= 1);
        let edge = findings
            .iter()
            .find(|finding| finding.kind == Kind::EdgeOrder)
            .expect("reversed edge");
        assert!(edge.fixable());
        assert!(findings
            .iter()
            .any(|finding| finding.kind == Kind::EntityOrphan));
    }
}
