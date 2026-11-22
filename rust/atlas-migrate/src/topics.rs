//! The deterministic topic pass: the reader's topics decide, `atlas-topics` matches.
//!
//! Decisions: an article is matched against the **active** topics with
//! `atlas-ingest`'s `Rules` (so the match rule exists once, in `atlas-topics`), a match is
//! written with the same statement `atlas-ingest::save` uses, and `relevant` is only ever
//! raised — a carry-over adds reach, it never hides a story a reader could see.
//!
//! It does not: decide what a topic is (the reader writes them), remove an assignment
//! that no longer matches (that is reported, not silently deleted), or call a model.

use atlas_ingest::{ArticleText, Pipeline, TopicRow};
use atlas_store::{SqlValue, Store};

use crate::error::{MigrateError, Result};
use crate::queries;

/// The deterministic topic pass: match, then upsert what changed.
pub fn assign(
    pipeline: &Pipeline,
    store: &Store,
    article_id: i64,
    topics: &[TopicRow],
) -> Result<usize> {
    if topics.is_empty() {
        return Ok(0);
    }
    let row = match store.query_one(queries::CARRY_ARTICLE, &[SqlValue::Integer(article_id)])? {
        Some(row) => row,
        None => return Ok(0),
    };
    let article = ArticleText {
        title: row.text("title").unwrap_or("").to_string(),
        summary: row.text("summary").unwrap_or("").to_string(),
        content: row.text("body").unwrap_or("").to_string(),
        url: row.text("url").unwrap_or("").to_string(),
        publisher: row.text("publisher").unwrap_or("").to_string(),
        ..ArticleText::default()
    };
    let matched = pipeline
        .rules()
        .match_topics(topics, &article)
        .map_err(|error| MigrateError::new(error.message()))?;
    if matched.is_empty() {
        return Ok(0);
    }
    let existing = store.query(
        queries::ARTICLE_TOPIC_ROWS,
        &[SqlValue::Integer(article_id)],
    )?;
    let mut written = 0usize;
    for hit in &matched {
        let same = existing.iter().any(|row| {
            row.int("id") == Some(hit.id)
                && row.text("matched").unwrap_or("") == hit.matched_text()
                && row.real("score").map(|score| (score - hit.score).abs() < 1e-9) == Some(true)
        });
        if same {
            continue;
        }
        store.exec(
            queries::UPSERT_ARTICLE_TOPIC,
            &[
                SqlValue::Integer(article_id),
                SqlValue::Integer(hit.id),
                SqlValue::Real(hit.score),
                SqlValue::Text(hit.matched_text()),
            ],
        )?;
        written += 1;
    }
    store.exec(queries::MARK_RELEVANT, &[SqlValue::Integer(article_id)])?;
    Ok(written)
}

