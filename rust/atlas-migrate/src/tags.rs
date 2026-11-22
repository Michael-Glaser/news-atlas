//! Tag rows that cannot stand.
//!
//! Decisions: a tag is dropped only for a **structural** reason — it fails
//! `atlas-ingest`'s own label check, it repeats another tag on the same article, or
//! it only repeats the outlet the article came from. Whether a tag is *true* is the
//! model's call (`tag_article`), and a carry-over that dropped tags by looking for
//! words in the text would be exactly the forbidden word-list rule.
//!
//! It does not: create a tag, re-word one, decide that an article needs tags, or
//! touch `article_categories` (a category is the model's answer with the same
//! provenance problem, and it is re-derived by the model pass rather than deleted).

use std::collections::BTreeSet;

use atlas_store::SqlValue;

use crate::error::Result;
use crate::findings::{Batch, Finding, Kind};
use crate::open::Archive;
use crate::queries;

/// One tag row.
#[derive(Clone, Debug)]
pub struct TagRow {
    pub article_id: i64,
    pub tag: String,
    pub source: String,
    pub publisher: String,
}

/// Every tag row in the archive.
pub fn rows(archive: &Archive) -> Result<Vec<TagRow>> {
    let rows = archive.query(queries::TAGS, &[])?;
    Ok(rows
        .iter()
        .map(|row| TagRow {
            article_id: row.int("article_id").unwrap_or(0),
            tag: row.text("tag").unwrap_or("").to_string(),
            source: row.text("source").unwrap_or("").to_string(),
            publisher: row.text("publisher").unwrap_or("").to_string(),
        })
        .collect())
}

/// The checks, in the order a row is attributed to one of them.
pub fn check(archive: &Archive) -> Result<Vec<Finding>> {
    let all = rows(archive)?;
    let mut invalid: Vec<(i64, String)> = Vec::new();
    let mut variant: Vec<(i64, String)> = Vec::new();
    let mut publisher: Vec<(i64, String)> = Vec::new();
    // Folded spelling → the first tag that carried it on that article.
    let mut first_of: Vec<((i64, String), (i64, String))> = Vec::new();
    for row in &all {
        if !atlas_ingest::tags::label_ok(&row.tag) {
            invalid.push((row.article_id, row.tag.clone()));
            continue;
        }
        let fold = crate::publisher::folded(&row.tag);
        let publisher_fold = crate::publisher::folded(&row.publisher);
        if !publisher_fold.is_empty() && fold == publisher_fold {
            publisher.push((row.article_id, row.tag.clone()));
            continue;
        }
        let key = (row.article_id, fold);
        if let Some((_, (_, kept))) = first_of.iter().find(|(existing, _)| *existing == key) {
            if kept != &row.tag {
                variant.push((row.article_id, row.tag.clone()));
            }
            continue;
        }
        first_of.push((key, (row.article_id, row.tag.clone())));
    }
    let mut findings: Vec<Finding> = Vec::new();
    for (kind, subject, rows, reason) in [
        (
            Kind::TagInvalid,
            "article_tags",
            invalid,
            "markup, punctuation or a length no label can have",
        ),
        (
            Kind::TagVariant,
            "article_tags",
            variant,
            "the same word as another tag on this article, spelled differently",
        ),
        (
            Kind::TagPublisher,
            "article_tags",
            publisher,
            "it only repeats the outlet the article came from",
        ),
    ] {
        if rows.is_empty() {
            continue;
        }
        let mut batch = Batch::new(kind, subject, queries::DELETE_TAG);
        for (article_id, tag) in &rows {
            batch.push(vec![
                SqlValue::Integer(*article_id),
                SqlValue::Text(tag.clone()),
            ]);
        }
        findings.push(
            Finding::new(kind, subject, rows.len())
                .with_ids(
                    rows.iter()
                        .take(20)
                        // `article:tag` — the row's own primary key, capped so a tag a page
                        // grew too long cannot break the line it is printed on.
                        .map(|(article_id, tag)| {
                            format!(
                                "{}:{}",
                                article_id,
                                crate::redact::cap(tag, crate::redact::CAP_CHARS)
                            )
                        })
                        .collect(),
                )
                .with_detail(reason)
                .with_batch(batch),
        );
    }
    Ok(findings)
}

/// Tags the archive cannot tell apart by provenance: `article_tags.source` is `ai`
/// for the model's own tags *and* for the tags `atlas-ingest`'s repair pass derives
/// from entity names, so a migration can neither confirm nor clear them. Reported as
/// a note, with the articles it is about, because a note nobody can act on is noise;
/// the model-owned tag pass is `tag_article`, driven by the agent.
pub fn provenance(archive: &Archive) -> Result<Option<Finding>> {
    let all = rows(archive)?;
    if all.is_empty() {
        return Ok(None);
    }
    let articles: BTreeSet<i64> = all.iter().map(|row| row.article_id).collect();
    let sources: BTreeSet<&str> = all.iter().map(|row| row.source.as_str()).collect();
    let ids: Vec<String> = archive
        .query(queries::TAG_ARTICLE_IDS, &[SqlValue::Integer(20)])?
        .iter()
        .filter_map(|row| row.int("id"))
        .map(|id| id.to_string())
        .collect();
    let mut detail = format!(
        "{} articles, source values stored: {}",
        articles.len(),
        sources.into_iter().collect::<Vec<&str>>().join(", ")
    );
    detail.push_str(
        "; the archive cannot say which of these the agent wrote, so re-derive them with \
         tag_article rather than deleting a column",
    );
    Ok(Some(
        Finding::new(Kind::TagProvenance, "article_tags", all.len())
            .with_ids(ids)
            .with_detail(detail),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_structural_tag_faults_are_dropped() {
        let path = crate::fixture::database_with_inconsistencies("tags");
        let archive = Archive::open_read_only(&path).expect("open");
        let findings = check(&archive).expect("check");
        let kinds: Vec<Kind> = findings.iter().map(|finding| finding.kind).collect();
        assert!(kinds.contains(&Kind::TagInvalid), "a markup tag is not a label");
        assert!(kinds.contains(&Kind::TagVariant), "a case variant repeats a tag");
        assert!(kinds.contains(&Kind::TagPublisher), "a tag equal to the outlet");
        // A tag the text never repeats is *not* a fault: only the model decides that.
        for finding in &findings {
            let batch = finding.batch.as_ref().expect("a statement");
            for row in &batch.params {
                let tag = match row.get(1) {
                    Some(SqlValue::Text(text)) => text.as_str(),
                    _ => continue,
                };
                assert_ne!(tag, "chips", "a plain tag with no text evidence stays");
            }
        }
    }

    #[test]
    fn provenance_is_reported_and_never_fixed() {
        let path = crate::fixture::database_with_inconsistencies("tag-provenance");
        let archive = Archive::open_read_only(&path).expect("open");
        let note = provenance(&archive).expect("provenance").expect("a note");
        assert_eq!(note.kind.severity(), crate::findings::Severity::Note);
        assert!(!note.fixable());
    }
}
