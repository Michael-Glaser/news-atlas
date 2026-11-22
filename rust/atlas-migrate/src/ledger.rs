//! The carry-over's own bookkeeping: what has been carried, and the snapshot a
//! re-derivation must be able to undo.
//!
//! Decisions: the `meta` key namespace (`migrate:carry:`, `migrate:snap:`,
//! `migrate:type:`) so a resume never confuses the app's own markers with the
//! migration's, that a snapshot is written **before** a link is detached and deleted
//! **after** it is restored — so a run that is interrupted can be finished, not
//! guessed at — and that an entity's provenance is recorded per entity id.
//!
//! It does not: carry anything itself ([`crate::carry`] does), store article text (a
//! snapshot holds ids, types, counts and weights), or read a credential (a value that
//! looks like one is stored as `[redacted]` by `atlas-store`).

use atlas_store::{SqlValue, Store};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// One article's carry-over receipt: `meta` key [`carry_key`].
pub const CARRY_PREFIX: &str = "migrate:carry:1:";
/// The pre-state of one article's links and entity types: [`snapshot_key`].
pub const SNAPSHOT_PREFIX: &str = "migrate:snap:1:";
/// One entity's type provenance: [`entity_key`].
pub const ENTITY_PREFIX: &str = "migrate:type:1:";

pub fn carry_key(article_id: i64) -> String {
    format!("{}{}", CARRY_PREFIX, article_id)
}

pub fn snapshot_key(article_id: i64) -> String {
    format!("{}{}", SNAPSHOT_PREFIX, article_id)
}

pub fn entity_key(entity_id: i64) -> String {
    format!("{}{}", ENTITY_PREFIX, entity_id)
}

/// What one article's model pass did.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Record {
    /// When the pass ran (ISO 8601, UTC).
    pub at: String,
    /// `model` when the local model answered, `none` when it did not. There is no
    /// third value: a heuristic answer is reverted, not recorded.
    pub source: String,
    /// Entities the model named that the article did not link before.
    pub entities: usize,
    /// Categories the model added.
    pub categories: usize,
    /// Topic assignments written or refreshed by the deterministic topic pass.
    pub topics: usize,
    /// Writes a heuristic fallback attempted and the guard undid.
    pub reverted: usize,
}

impl Record {
    pub fn is_model(&self) -> bool {
        self.source == "model"
    }
}

/// One linked entity, as it was before the pass.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityState {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub count: i64,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub category_id: Option<i64>,
    pub weight: f64,
}

/// The pre-state of one article: everything the retype pass moves.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub article: i64,
    pub at: String,
    pub links: Vec<EntityState>,
}

/// Has this article been carried?
pub fn done(store: &Store, article_id: i64) -> bool {
    matches!(
        store.get_meta(&carry_key(article_id)),
        Ok(Some(_))
    )
}

/// Has this article been carried? Asked on the read-only connection, so a dry run needs
/// no write access at all.
pub fn done_readonly(archive: &crate::open::Archive, article_id: i64) -> bool {
    matches!(
        archive.query_one(
            crate::queries::META_KEY,
            &[SqlValue::Text(carry_key(article_id))],
        ),
        Ok(Some(_))
    )
}

/// Record what a pass did for one article.
pub fn record(store: &Store, article_id: i64, record: &Record) -> Result<()> {
    let value = serde_json::to_string(record).map_err(|_| {
        crate::error::MigrateError::new("a ledger record could not be written")
    })?;
    store.set_meta(&carry_key(article_id), &value)?;
    Ok(())
}

/// Read one article's record back.
pub fn read(store: &Store, article_id: i64) -> Option<Record> {
    let raw = store.get_meta(&carry_key(article_id)).ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Write a snapshot before detaching anything.
pub fn save_snapshot(store: &Store, snapshot: &Snapshot) -> Result<()> {
    let value = serde_json::to_string(snapshot).map_err(|_| {
        crate::error::MigrateError::new("a snapshot could not be written")
    })?;
    store.set_meta(&snapshot_key(snapshot.article), &value)?;
    Ok(())
}

pub fn take_snapshot(store: &Store, article_id: i64) -> Option<Snapshot> {
    let raw = store.get_meta(&snapshot_key(article_id)).ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

pub fn clear_snapshot(store: &Store, article_id: i64) -> Result<()> {
    store.exec(
        crate::queries::DELETE_META_KEY,
        &[SqlValue::Text(snapshot_key(article_id))],
    )?;
    Ok(())
}

/// Every snapshot an interrupted run left behind.
pub fn pending(store: &Store) -> Result<Vec<Snapshot>> {
    let rows = store.query(
        crate::queries::META_KEYS_LIKE,
        &[SqlValue::Text(format!("{}%", SNAPSHOT_PREFIX))],
    )?;
    let mut out: Vec<Snapshot> = Vec::new();
    for row in rows {
        let raw = row.text("v").unwrap_or("");
        if let Ok(snapshot) = serde_json::from_str::<Snapshot>(raw) {
            out.push(snapshot);
        }
    }
    Ok(out)
}

/// Has this entity's type come from the model (and not from a guess)?
///
/// The stored value is JSON (`{"state":…,"type":…}`); it is parsed, not substring-matched,
/// because a type that happens to contain the letters `model` (`"model press"`) would make
/// an unclassified entity look confirmed and stop the carry-over from ever asking again.
pub fn entity_verified(store: &Store, entity_id: i64) -> bool {
    match store.get_meta(&entity_key(entity_id)) {
        Ok(Some(raw)) => serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|value| {
                value
                    .get("state")
                    .and_then(serde_json::Value::as_str)
                    .map(|state| state == "model")
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Record the outcome for one entity: `model` with the type it chose, or
/// `unclassified` when the model did not name it.
pub fn mark_entity(store: &Store, entity_id: i64, state: &str, kind: &str) -> Result<()> {
    let value = serde_json::json!({ "state": state, "type": kind }).to_string();
    store.set_meta(&entity_key(entity_id), &value)?;
    Ok(())
}

/// The carry-over's receipts, for a report or a resume.
pub fn entries(store: &Store) -> Result<Vec<(String, Record)>> {
    let rows = store.query(crate::queries::LEDGER_ENTRIES, &[])?;
    let mut out: Vec<(String, Record)> = Vec::new();
    for row in rows {
        let key = row.text("k").unwrap_or("").to_string();
        let raw = row.text("v").unwrap_or("");
        if let Ok(record) = serde_json::from_str::<Record>(raw) {
            out.push((key, record));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_through_meta_and_says_what_happened() {
        let path = crate::fixture::clean_database("ledger");
        let store = crate::open::open_write(&path).expect("open");
        assert!(!done(&store, 7));
        let entry = Record {
            at: "2026-11-21T10:00:00Z".to_string(),
            source: "model".to_string(),
            entities: 3,
            categories: 1,
            topics: 1,
            reverted: 0,
        };
        record(&store, 7, &entry).expect("record");
        assert!(done(&store, 7));
        let read_back = read(&store, 7).expect("a record");
        assert!(read_back.is_model());
        assert_eq!(read_back.entities, 3);
        assert_eq!(entries(&store).expect("entries").len(), 1);
    }

    #[test]
    fn a_snapshot_is_pending_until_it_is_cleared_and_holds_no_text() {
        let path = crate::fixture::clean_database("ledger-snap");
        let store = crate::open::open_write(&path).expect("open");
        let snapshot = Snapshot {
            article: 4,
            at: "2026-11-21T10:00:00Z".to_string(),
            links: vec![EntityState {
                id: 11,
                kind: "ORG".to_string(),
                count: 5,
                first_seen: None,
                last_seen: Some("2026-11-20T10:00:00Z".to_string()),
                category_id: Some(2),
                weight: 1.0,
            }],
        };
        save_snapshot(&store, &snapshot).expect("save");
        let waiting = pending(&store).expect("pending");
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].links[0].id, 11);
        clear_snapshot(&store, 4).expect("clear");
        assert!(pending(&store).expect("pending again").is_empty());
        assert!(!entity_verified(&store, 11));
        mark_entity(&store, 11, "model", "PERSON").expect("mark");
        assert!(entity_verified(&store, 11));
    }

    #[test]
    fn only_the_state_field_decides_whether_a_type_was_confirmed() {
        // The stored state is JSON; a *type* that happens to contain the letters "model"
        // must not read as a model receipt, or the carry-over stops asking about an entity
        // it never classified.
        let path = crate::fixture::clean_database("ledger-state");
        let store = crate::open::open_write(&path).expect("open");
        mark_entity(&store, 21, "unclassified", "model press").expect("mark");
        assert!(
            !entity_verified(&store, 21),
            "a type spelling 'model' made an unclassified entity look confirmed"
        );
        mark_entity(&store, 21, "model", "ORG").expect("mark");
        assert!(entity_verified(&store, 21));
    }
}
