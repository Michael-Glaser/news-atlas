//! Making an entity's stored type the model's answer: detach, ask, restore.
//!
//! Decisions: an entity type is re-derived only by a model pass, so the article's links
//! are detached before the pass (otherwise `atlas-graph` sees an already-typed entity and,
//! by its own documented rule, never relabels it), the pre-state is written to `meta`
//! **before** anything is detached so an interrupted run is finished by the next one, and
//! every count and timestamp `link_article` increments is set back afterwards so the pass
//! leaves no drift it did not intend. A type the model does not restate stays empty — the
//! documented unclassified state — and loses the category that came from the old guess.
//!
//! It does not: decide a type, keep a guess alive, delete an entity, or touch an entity no
//! article of this pass links.

use atlas_store::{SqlValue, Store};

use crate::error::Result;
use crate::ledger::{self, EntityState, Snapshot};
use crate::queries;

/// What one restore did, in the words the run summary prints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// Stored types the model replaced with a different answer.
    pub replaced: usize,
    /// Stored types the model answered with the same type it had before.
    pub confirmed: usize,
    /// Entities the model left empty: the documented unclassified state.
    pub unclassified: usize,
}

/// Restore the links, types and bookkeeping of one article from its snapshot.
///
/// A type that is non-empty after the pass is the model's answer whether or not it differs
/// from the stored one, so it is recorded as such: without that receipt the *next* pass
/// sees an unconfirmed type, clears it, and — if that pass's model is silent — the
/// classification the model gave is gone. That is the difference between a pass that can be
/// re-run and one that eats its own work.
pub fn restore(store: &Store, snapshot: &Snapshot) -> Result<Restored> {
    let linked: Vec<i64> = store
        .query(queries::ARTICLE_ENTITY_IDS, &[SqlValue::Integer(snapshot.article)])?
        .iter()
        .filter_map(|row| row.int("id"))
        .collect();
    let mut done = Restored::default();
    for link in &snapshot.links {
        if !linked.contains(&link.id) {
            store.exec(
                queries::RESTORE_ARTICLE_ENTITY,
                &[
                    SqlValue::Integer(snapshot.article),
                    SqlValue::Integer(link.id),
                    SqlValue::Real(link.weight),
                ],
            )?;
        }
        store.exec(
            queries::RESTORE_ENTITY_BOOKKEEPING,
            &[
                SqlValue::Integer(link.count),
                link.first_seen
                    .clone()
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null),
                link.last_seen
                    .clone()
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null),
                SqlValue::Integer(link.id),
            ],
        )?;
        let current = store
            .query_one(queries::ENTITY_TYPE, &[SqlValue::Integer(link.id)])?
            .and_then(|row| row.text("type").map(|text| text.to_string()))
            .unwrap_or_default();
        if current.is_empty() {
            // The model did not name this entity: unclassified, and its category may
            // not keep remembering the type that was guessed.
            if !link.kind.is_empty() {
                done.unclassified += 1;
            }
            let _ = store.exec(queries::CLEAR_ENTITY_CATEGORY, &[SqlValue::Integer(link.id)]);
            let _ = ledger::mark_entity(store, link.id, "unclassified", "");
            continue;
        }
        // Non-empty after the pass: the model's answer. Recorded even when it matches the
        // stored type, so a later pass treats it as confirmed instead of a guess.
        if current == link.kind {
            done.confirmed += 1;
        } else {
            done.replaced += 1;
        }
        let _ = ledger::mark_entity(store, link.id, "model", &current);
        if current != link.kind {
            if let Some(category) = pipeline_category(store, &current) {
                let _ = store.exec(
                    queries::SET_ENTITY_CATEGORY,
                    &[SqlValue::Integer(category), SqlValue::Integer(link.id)],
                );
            }
        }
    }
    // An interrupted run may have left a `reason:` row for a label it never got to
    // write, or one whose label a later pass cleared. This pass heals that as it goes:
    // a model's own row survives (its label is there), an orphan goes.
    let ids: Vec<i64> = snapshot.links.iter().map(|link| link.id).collect();
    let _ = drop_orphan_reasons(store, &ids);
    Ok(done)
}

/// The default category id for a type, using the two lookups
/// `atlas-ingest`'s `categorise_entities` uses: the documented type→label map and the
/// store's own category row.
fn pipeline_category(store: &Store, kind: &str) -> Option<i64> {
    let label = atlas_ai::prompts::category_by_type(kind);
    store
        .query_one(queries::CATEGORY_ID, &[SqlValue::Text(label.to_string())])
        .ok()
        .flatten()
        .and_then(|row| row.int("id"))
}

/// Drop the label-provenance rows that describe a label the edge no longer carries.
///
/// `atlas-ingest` records *why* an edge was labelled in `meta` under `reason:<a>:<b>` —
/// `{"article": N, "label": "…", "source": "relations"|"types"}` — so a label can be
/// traced to the answer that produced it. The heuristic fallback writes the row and the
/// label together, and [`PreState::revert`] un-labels the edge to undo that heuristic.
/// The row used to outlive the label, and then the archive asserted that an edge had
/// been labelled for a reason while carrying no label at all — a `meta` row left behind
/// by a pass whose every other write was reverted. Measured before this guard, on the
/// inconsistency fixture with a model that answered nothing: `meta` went from 0 to 2
/// rows (`reason:1:2` for article 3, `reason:2:3` for article 4) for an article whose
/// labels, tags, categories and summary were all put back.
///
/// The rule is structural and needs no pre-state: **a `reason:` row may exist only for a
/// pair whose edge carries a label.** So a row whose pair has no label — or no edge at
/// all — is stale whoever wrote it, which is also what heals an archive an interrupted
/// run left in that state. A model's own provenance is untouched: its label is there,
/// so the row that explains it stays.
pub fn drop_orphan_reasons(store: &Store, entity_ids: &[i64]) -> Result<usize> {
    let mut ids: Vec<i64> = entity_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut dropped = 0usize;
    for (index, a) in ids.iter().enumerate() {
        // `skip(index)`, not `skip(index + 1)`: a self-edge is a real row in an archive
        // that needs restoring (the fixture has one, `edges(4,4)`), and `atlas-ingest`
        // labels it and writes `reason:4:4` for it like any other pair. Measured: this
        // loop's first version skipped the diagonal and left exactly that row behind.
        for b in ids.iter().skip(index) {
            // The key `atlas-ingest` writes, whichever way round the pair is stored.
            let key = format!("reason:{}:{}", a, b);
            if store.get_meta(&key)?.is_none() {
                continue;
            }
            let label = pair_label(store, *a, *b)?;
            if label.trim().is_empty() {
                store.exec(queries::DELETE_META_KEY, &[SqlValue::Text(key)])?;
                dropped += 1;
            }
        }
    }
    Ok(dropped)
}

/// The label of one pair, whichever direction the edge was stored in.
fn pair_label(store: &Store, a: i64, b: i64) -> Result<String> {
    for (x, y) in [(a, b), (b, a)] {
        let found = store
            .query_one(
                queries::EDGE_LABEL_FOR_PAIR,
                &[SqlValue::Integer(x), SqlValue::Integer(y)],
            )?
            .and_then(|row| row.text("label").map(|text| text.to_string()));
        if let Some(label) = found {
            return Ok(label);
        }
    }
    Ok(String::new())
}

/// What one article looked like before the pass: the five places a heuristic or a
/// detach could have written.
pub struct PreState {
    pub links: Vec<EntityState>,
    categories: Vec<i64>,
    all_categories: Vec<i64>,
    tags: Vec<String>,
    summary: String,
    edges: Vec<(i64, i64, String)>,
}

impl PreState {
    pub fn capture(store: &Store, article_id: i64) -> Result<PreState> {
        let links = store
            .query(queries::ARTICLE_ENTITIES, &[SqlValue::Integer(article_id)])?
            .iter()
            .map(|row| EntityState {
                id: row.int("id").unwrap_or(0),
                kind: row.text("type").unwrap_or("").to_string(),
                count: row.int("count").unwrap_or(0),
                first_seen: row.text("first_seen").map(|text| text.to_string()),
                last_seen: row.text("last_seen").map(|text| text.to_string()),
                category_id: row.int("category_id"),
                weight: row.real("weight").unwrap_or(1.0),
            })
            .collect();
        let categories = store
            .query(queries::ARTICLE_CATEGORY_IDS, &[SqlValue::Integer(article_id)])?
            .iter()
            .filter_map(|row| row.int("id"))
            .collect();
        let tags = store
            .query(queries::ARTICLE_TAG_NAMES, &[SqlValue::Integer(article_id)])?
            .iter()
            .filter_map(|row| row.text("tag").map(|tag| tag.to_string()))
            .collect();
        let all_categories = store
            .query(queries::CATEGORY_IDS, &[])?
            .iter()
            .filter_map(|row| row.int("id"))
            .collect();
        let summary = store
            .query_one(queries::ARTICLE_SUMMARY, &[SqlValue::Integer(article_id)])?
            .and_then(|row| row.text("s").map(|text| text.to_string()))
            .unwrap_or_default();
        let edges = store
            .query(
                queries::ARTICLE_EDGE_LABELS,
                &[SqlValue::Integer(article_id), SqlValue::Integer(article_id)],
            )?
            .iter()
            .map(|row| {
                (
                    row.int("a").unwrap_or(0),
                    row.int("b").unwrap_or(0),
                    row.text("label").unwrap_or("").to_string(),
                )
            })
            .collect();
        Ok(PreState {
            links,
            categories,
            all_categories,
            tags,
            summary,
            edges,
        })
    }

    /// Undo every write the heuristic fallback made, and nothing else.
    ///
    /// The heuristic receipt in `atlas-ingest` can add categories, add tags, fill a
    /// NULL entity category, fill empty edge labels and write a summary when the
    /// article had none. Each of those is reverted to the captured value; a value the
    /// article already had is never touched.
    pub fn revert(&self, store: &Store, article_id: i64) -> Result<usize> {
        let mut undone = 0usize;
        let now = store.query(queries::ARTICLE_CATEGORY_IDS, &[SqlValue::Integer(article_id)])?;
        for row in now {
            let id = row.int("id").unwrap_or(0);
            if !self.categories.contains(&id) {
                store.exec(
                    queries::DELETE_ARTICLE_CATEGORY,
                    &[SqlValue::Integer(article_id), SqlValue::Integer(id)],
                )?;
                undone += 1;
            }
        }
        let now = store.query(queries::ARTICLE_TAG_NAMES, &[SqlValue::Integer(article_id)])?;
        for row in now {
            let tag = row.text("tag").unwrap_or("").to_string();
            if !self.tags.contains(&tag) {
                store.exec(
                    queries::DELETE_TAG,
                    &[SqlValue::Integer(article_id), SqlValue::Text(tag)],
                )?;
                undone += 1;
            }
        }
        for link in &self.links {
            if link.category_id.is_none() {
                let current = store
                    .query_one(queries::ENTITY_CATEGORY_ID, &[SqlValue::Integer(link.id)])?
                    .and_then(|row| row.int("id"));
                if current.is_some() {
                    store.exec(queries::CLEAR_ENTITY_CATEGORY, &[SqlValue::Integer(link.id)])?;
                    undone += 1;
                }
            }
        }
        let now = store.query(
            queries::ARTICLE_EDGE_LABELS,
            &[SqlValue::Integer(article_id), SqlValue::Integer(article_id)],
        )?;
        for row in now {
            let a = row.int("a").unwrap_or(0);
            let b = row.int("b").unwrap_or(0);
            let label = row.text("label").unwrap_or("");
            let before = self
                .edges
                .iter()
                .find(|(x, y, _)| *x == a && *y == b)
                .map(|(_, _, label)| label.as_str())
                .unwrap_or("");
            if before.is_empty() && !label.is_empty() {
                store.exec(
                    queries::CLEAR_EDGE_LABEL,
                    &[SqlValue::Integer(a), SqlValue::Integer(b)],
                )?;
                undone += 1;
            }
        }
        // A category the heuristic created for a type the model never gave is a label
        // invented from a guess; it goes, as long as nothing points at it.
        let now_categories: Vec<i64> = store
            .query(queries::CATEGORY_IDS, &[])?
            .iter()
            .filter_map(|row| row.int("id"))
            .collect();
        for id in now_categories {
            if !self.all_categories.contains(&id) {
                store.exec(queries::DELETE_UNUSED_CATEGORY, &[SqlValue::Integer(id)])?;
                undone += 1;
            }
        }
        let current = store
            .query_one(queries::ARTICLE_SUMMARY, &[SqlValue::Integer(article_id)])?
            .and_then(|row| row.text("s").map(|text| text.to_string()))
            .unwrap_or_default();
        if self.summary.trim().is_empty() && !current.trim().is_empty() {
            store.exec(
                queries::SET_SUMMARY,
                &[SqlValue::Text(String::new()), SqlValue::Integer(article_id)],
            )?;
            undone += 1;
        }
        // The label's provenance goes with the label: the edge was un-labelled above, so
        // the `reason:` row that says why it was labelled is now a claim about nothing.
        let ids: Vec<i64> = self.links.iter().map(|link| link.id).collect();
        undone += drop_orphan_reasons(store, &ids)?;
        Ok(undone)
    }
}

