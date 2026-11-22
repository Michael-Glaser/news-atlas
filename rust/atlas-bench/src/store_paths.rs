//! The store paths: the statements behind the screens, timed without the HTTP
//! layer.
//!
//! Decisions: which store work is worth a number of its own — the list query, an
//! FTS match, an entity lookup, a thread rebuild and a retention prune — and that
//! each one is called through the crate's own public API, never through a copy of
//! its SQL.
//!
//! It does not: re-derive a query. `Store::search` and
//! `atlas_graph::graphread::entity_profile` are the shipped code; if they get
//! faster the number moves with them.
//!
//! The entity path used to call `atlas_web::routes::articles::find_entity`, which
//! lived in the retired HTTP face. The window's entity screen calls
//! `atlas_graph::graphread::entity_profile`, which resolves the same name (exact,
//! then alias, then closest) and then reads the profile, so the budget below was
//! re-measured against that call rather than against the deleted route.
//!
//! The prune is destructive, so [`StorePaths::run`] measures it last, after every
//! read path: the archive it deletes from is thrown away at the end of the run.

use crate::stats::{measure, Samples};
use atlas_graph::graphdb::Context;
use atlas_graph::graphlink::recompute_threads;
use atlas_store::{Row, SqlValue, Store, CHUNK};

use crate::fixture::{empty_context, Archive, ARTICLES};

/// The article the single-article paths centre on.
pub const CENTRE: i64 = (ARTICLES / 2) as i64;

/// The entity the lookup paths centre on.
pub const ENTITY: i64 = 7;

/// The store paths.
pub struct StorePaths {
    pub list: Samples,
    pub search: Samples,
    pub entity: Samples,
    pub threads: Samples,
    pub prune: Samples,
    /// The encoding the list routes used before this change: a `Value` per row,
    /// then a serialiser walking that tree.
    pub encode_value: Samples,
    /// The encoding they use now: one pass into one buffer.
    pub encode_text: Samples,
}

/// Time the list statement the article screen runs, on the real schema.
///
/// The statement is the route's own: it is reached through `store.query` here
/// only because the HTTP layer above it is measured separately, and timing the
/// same SQL twice through two layers would report the HTTP cost twice.
fn list(store: &Store, limit: i64) -> (Vec<Row>, usize) {
    let sql = "SELECT ar.id,ar.title,ar.summary,ar.published,ar.iso,ar.url,ar.place,ar.geo_src,\
               COALESCE(NULLIF(ar.publisher,''), s.name) AS source,at2.thread_id, \
               (SELECT GROUP_CONCAT(t.tag, ' · ') FROM article_tags t WHERE t.article_id=ar.id) AS tags, \
               (SELECT GROUP_CONCAT(t.name, ' · ') FROM article_topics att JOIN topics t ON t.id=att.topic_id \
               WHERE att.article_id=ar.id) AS topics \
               FROM articles ar LEFT JOIN sources s ON s.id=ar.source_id \
               LEFT JOIN article_threads at2 ON at2.article_id=ar.id \
               WHERE ar.relevant=1 AND ar.published>=datetime('now',?) \
               ORDER BY ar.published DESC LIMIT ?";
    let rows = store
        .query(
            sql,
            &[
                SqlValue::Text("-72 hours".to_string()),
                SqlValue::Integer(limit),
            ],
        )
        .expect("the list statement");
    (rows, 0)
}

impl StorePaths {
    /// Measure all five. The prune runs last and on its own copy of the counts.
    pub fn run(archive: &Archive) -> StorePaths {
        let store = archive.store();

        let (list_samples, _) = measure("store.list", || {
            let (rows, _) = list(store, 200);
            (rows, 0)
        });

        // A term every synthetic article carries, so the match set is the whole
        // window rather than one row: a fast number on an empty result is not a
        // measurement.
        let (search, _) = measure("store.fts", || {
            let rows = store.search("report", 25).expect("fts");
            (rows, 0)
        });

        let (entity, _) = measure("store.entity", || {
            let profile = atlas_graph::graphread::entity_profile(
                store,
                &format!("Entity {}", ENTITY),
            )
            .expect("entity profile");
            (profile, 0)
        });

        let ctx: Context = Context::from_json(&empty_context());
        let (threads, _) = measure("store.threads", || {
            let done = recompute_threads(store, &ctx, 0.20, 40).expect("thread rebuild");
            (done, 0)
        });

        // The two encodings, on the same rows: what the routes used to pay and what
        // they pay now. Both are the store's public API over the same 200 rows, so
        // the difference is the encoding and nothing else.
        let (rows, _) = list(store, 200);
        let (encode_value, _) = measure("store.encode_value", || {
            let value = serde_json::Value::Array(rows.iter().map(|row| row.as_json()).collect());
            let body = serde_json::to_vec(&value).expect("serialise");
            (body, 0)
        });
        let (encode_text, _) = measure("store.encode_text", || {
            let body = atlas_store::rows_to_json_text(&rows).into_bytes();
            (body, 0)
        });
        assert!(
            !encode_value.times.is_empty() && encode_text.times.len() == encode_value.times.len(),
            "both encodings must be measured"
        );

        // Destructive: last, and only over rows outside the retention window.
        let (prune, _) = measure("store.prune", || {
            let removed = store.prune_old(None, CHUNK).expect("prune");
            (removed, 0)
        });

        StorePaths {
            list: list_samples,
            search,
            entity,
            threads,
            prune,
            encode_value,
            encode_text,
        }
    }

    /// The five numbers, in report order.
    pub fn all(&self) -> Vec<&Samples> {
        vec![
            &self.list,
            &self.search,
            &self.entity,
            &self.threads,
            &self.prune,
            &self.encode_value,
            &self.encode_text,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_store_path_really_did_its_work() {
        let archive = Archive::build("store-paths", true);
        let store = archive.store();
        let (rows, _) = list(store, 100);
        assert_eq!(rows.len(), 100);
        // The first row is as wide as the screen needs it.
        for column in ["id", "title", "source", "thread_id", "tags", "topics"] {
            assert!(
                rows[0].columns().iter().any(|name| name == column),
                "missing column {}",
                column
            );
        }
        let found = store.search("report", 25).expect("fts");
        assert!(!found.is_empty(), "the FTS index answers");
        let profile =
            atlas_graph::graphread::entity_profile(store, &format!("Entity {}", ENTITY))
                .expect("entity profile")
                .expect("a known entity");
        assert!(
            profile.contains(&format!("Entity {}", ENTITY)),
            "the profile names the entity it was asked for"
        );
        let removed = store.prune_old(None, CHUNK).expect("prune");
        assert!(removed > 0, "the seed carries rows outside retention");
    }
}
