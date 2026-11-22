//! References to rows that are not there any more.
//!
//! Decisions: which tables point at an article, an entity, a topic, a category, a
//! thread or an alias ([`crate::queries`]), and that the fix for each is the one
//! `DELETE` that clears the orphan — never a cascade that would remove a parent the
//! archive still needs.
//!
//! It does not: delete a parent, decide that an article should exist, or touch a row
//! while checking: a finding carries the batch, and the batch runs only on
//! `--apply`.

use atlas_store::SqlValue;

use crate::error::Result;
use crate::findings::{Batch, Finding, Kind};
use crate::open::Archive;
use crate::queries;

/// The child tables that point at `articles`, in the order the report lists them. Each one
/// gets its own finding, so a reader sees the table that is wrong rather than one number
/// for seven tables.
const ARTICLE_CHILDREN: &[&str] = &[
    "article_entities",
    "article_links",
    "article_tags",
    "article_countries",
    "article_topics",
    "article_categories",
    "article_threads",
];

/// Every dangling reference, with the statement that clears it.
pub fn check(archive: &Archive) -> Result<Vec<Finding>> {
    let mut findings: Vec<Finding> = Vec::new();

    for table in ARTICLE_CHILDREN {
        if !archive.has_table(table)? {
            continue;
        }
        let (Some(select), Some(clear)) =
            (queries::dangling_articles(table), queries::clear_dangling_articles(table))
        else {
            continue;
        };
        let rows = archive.query(select, &[])?;
        if rows.is_empty() {
            continue;
        }
        // One statement with no bound values: the ids are in the statement's own
        // subquery, so the batch plans the count instead of binding thousands of ids.
        let mut batch = Batch::new(Kind::DanglingArticle, table, clear);
        batch.plan_rows(rows.len());
        findings.push(
            Finding::new(Kind::DanglingArticle, *table, rows.len())
                .with_ids(ids_of(&rows, 20))
                .with_detail("articles.id")
                .with_batch(batch),
        );
    }

    push(
        &mut findings,
        archive,
        queries::DANGLING_ENTITIES,
        queries::CLEAR_DANGLING_ENTITIES,
        Kind::DanglingEntity,
        "article_entities",
        "entities.id",
    )?;
    push(
        &mut findings,
        archive,
        queries::DANGLING_TOPICS,
        queries::CLEAR_DANGLING_TOPICS,
        Kind::DanglingTopic,
        "article_topics",
        "topics.id",
    )?;
    push(
        &mut findings,
        archive,
        queries::DANGLING_CATEGORIES,
        queries::CLEAR_DANGLING_CATEGORIES,
        Kind::DanglingCategory,
        "article_categories",
        "categories.id",
    )?;
    push(
        &mut findings,
        archive,
        queries::DANGLING_THREADS,
        queries::CLEAR_DANGLING_THREADS,
        Kind::DanglingThread,
        "article_threads",
        "threads.id",
    )?;
    push(
        &mut findings,
        archive,
        queries::DANGLING_EDGES,
        queries::CLEAR_DANGLING_EDGES,
        Kind::DanglingEntity,
        "edges",
        "entities.id",
    )?;
    push(
        &mut findings,
        archive,
        queries::DANGLING_ALIASES,
        queries::CLEAR_DANGLING_ALIASES,
        Kind::DanglingAlias,
        "entity_alias",
        "entities.id",
    )?;
    Ok(findings)
}

#[allow(clippy::too_many_arguments)]
fn push(
    findings: &mut Vec<Finding>,
    archive: &Archive,
    select: &str,
    clear: &'static str,
    kind: Kind,
    subject: &'static str,
    detail: &str,
) -> Result<()> {
    if !archive.has_table(subject)? {
        return Ok(());
    }
    let rows = archive.query(select, &[])?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut batch = Batch::new(kind, subject, clear);
    batch.plan_rows(rows.len());
    findings.push(
        Finding::new(kind, subject, rows.len())
            .with_ids(ids_of(&rows, 20))
            .with_detail(detail)
            .with_batch(batch),
    );
    Ok(())
}

fn ids_of(rows: &[atlas_store::Row], limit: usize) -> Vec<String> {
    rows.iter()
        .take(limit)
        .map(|row| match row.get("id") {
            Some(SqlValue::Text(text)) => crate::redact::cap(text, 40),
            Some(value) => crate::redact::cell("id", value),
            None => "?".to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dangling_reference_is_found_with_its_fix() {
        let path = crate::fixture::database_with_inconsistencies("refs");
        let archive = Archive::open_read_only(&path).expect("open");
        let findings = check(&archive).expect("check");
        let subjects: Vec<&str> = findings.iter().map(|f| f.subject.as_str()).collect();
        for expected in [
            "article_entities",
            "article_links",
            "article_tags",
            "article_topics",
            "entity_alias",
            "article_threads",
            "edges",
        ] {
            assert!(subjects.contains(&expected), "nothing found for {}", expected);
        }
        for finding in &findings {
            assert!(finding.fixable(), "{} has no statement", finding.subject);
            assert!(finding.count > 0);
        }
    }
}
