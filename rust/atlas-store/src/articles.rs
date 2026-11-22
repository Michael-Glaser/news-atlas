//! The article rows themselves: is this story already stored, and is it stored
//! twice?
//!
//! Decisions: [`Store::find_duplicate`] (the same headline, preferring the same
//! publisher), [`Store::dedupe_titles`] (flag every second copy `dupe=1`, keeping
//! the fullest copy visible — never delete), and which tables point at an article
//! and must be cleared before its row goes ([`ARTICLE_REFS`], [`FTS_DELETE`]).
//!
//! It does not: prune (that is [`crate::prune`]), search, or compare anything but
//! the normalised headline key — [`crate::norm::norm_title`] owns the folding, so
//! a headline's identity is decided in one place.

use std::collections::HashMap;

use crate::clock::{iso_ago, now_iso, parse_epoch};
use crate::error::Result;
use crate::norm::norm_title;
use crate::params::{placeholders, SqlValue};
use crate::CHUNK;
use crate::Store;

/// Same story inside this window = duplicate, whichever outlet printed it.
/// (`db.py: DUPE_WINDOW_HOURS`.)
pub const DUPE_WINDOW_HOURS: i64 = 6;

/// Tables that point at an article, cleared before the article row itself.
/// `both_ends` is `db.py`'s `article_links` case (`a IN (…) OR b IN (…)`).
/// The label names the table in an error message — a table name is schema, not
/// content.
pub(crate) const ARTICLE_REFS: &[(&str, &str, bool)] = &[
    ("article_entities", "DELETE FROM article_entities WHERE article_id IN ({})", false),
    ("article_links", "DELETE FROM article_links WHERE a IN ({}) OR b IN ({})", true),
    ("article_tags", "DELETE FROM article_tags WHERE article_id IN ({})", false),
    ("article_countries", "DELETE FROM article_countries WHERE article_id IN ({})", false),
    ("article_topics", "DELETE FROM article_topics WHERE article_id IN ({})", false),
    ("article_categories", "DELETE FROM article_categories WHERE article_id IN ({})", false),
    ("article_threads", "DELETE FROM article_threads WHERE article_id IN ({})", false),
];

/// Remove the full-text rows of articles that are about to go — `db.py`'s
/// statement, unchanged.
///
/// FTS5 only tolerates this while the rowid really is in the index *and* the
/// values stored next to it match what the index was built from; otherwise it
/// refuses with "database disk image is malformed". Measured on the bundled
/// SQLite 3.54.1: the refusal is atomic (nothing is half-deleted and
/// `article_fts` still passes `integrity-check`), so [`Store::prune_old_detailed`]
/// counts such a batch and carries on instead of stopping the whole prune the way
/// `db.py` would.
pub(crate) const FTS_DELETE: &str = "DELETE FROM article_fts WHERE rowid IN ({})";

impl Store {
    /// Id of a stored article that already tells this story, or `None`
    /// (`db.py: find_duplicate`).
    pub fn find_duplicate(
        &self,
        title: &str,
        publisher: &str,
        hours: i64,
        exclude_id: Option<i64>,
    ) -> Result<Option<i64>> {
        let key = norm_title(title);
        if key.is_empty() {
            return Ok(None);
        }
        let hours = hours.max(1);
        let rows = self.query(
            "SELECT id,title,publisher FROM articles WHERE dupe=0 AND published>=? \
             ORDER BY published DESC LIMIT 5000",
            &[SqlValue::Text(iso_ago(hours as f64))],
        )?;
        let publisher = publisher.trim().to_lowercase();
        let mut fallback: Option<i64> = None;
        for row in rows {
            let id = match row.int("id") {
                Some(id) => id,
                None => continue,
            };
            if exclude_id == Some(id) {
                continue;
            }
            if norm_title(row.text("title").unwrap_or("")) != key {
                continue;
            }
            let row_publisher = row.text("publisher").unwrap_or("").trim().to_lowercase();
            if !publisher.is_empty() && row_publisher == publisher {
                return Ok(Some(id));
            }
            if fallback.is_none() {
                fallback = Some(id);
            }
        }
        Ok(fallback)
    }

    /// Flag the same story stored twice — never delete it (`db.py:
    /// dedupe_titles`).
    ///
    /// Inside `window_hours` a matching headline is the same story whichever
    /// outlet printed it; outside it only the same publisher counts as a reprint.
    /// The fullest copy stays visible, every other copy gets `dupe=1/relevant=0`
    /// so a repair script can always bring it back. Returns how many copies were
    /// flagged.
    pub fn dedupe_titles(&self, days: i64, window_hours: i64) -> Result<usize> {
        let days = days.max(1);
        let window = window_hours.max(1) * 3_600;
        let rows = self.query(
            "SELECT id,title,publisher,published,full_text FROM articles \
             WHERE dupe=0 AND published>=? ORDER BY published DESC, id DESC",
            &[SqlValue::Text(iso_ago(days as f64 * 24.0))],
        )?;

        #[derive(Clone)]
        struct Anchor {
            id: i64,
            ts: i64,
            publisher: String,
            full: bool,
        }

        let mut anchors: HashMap<String, Vec<Anchor>> = HashMap::new();
        let mut dups: Vec<i64> = Vec::new();
        for row in rows {
            let key = norm_title(row.text("title").unwrap_or(""));
            if key.is_empty() {
                continue;
            }
            let id = match row.int("id") {
                Some(id) => id,
                None => continue,
            };
            let ts = parse_epoch(row.text("published").unwrap_or(""));
            let publisher = row.text("publisher").unwrap_or("").trim().to_lowercase();
            let full = row.int("full_text").unwrap_or(0) != 0;
            let bucket = anchors.entry(key).or_default();
            let mut hit: Option<usize> = None;
            for (index, anchor) in bucket.iter().enumerate() {
                let same_outlet = !publisher.is_empty() && anchor.publisher == publisher;
                let same_story =
                    ts != 0 && anchor.ts != 0 && (ts - anchor.ts).abs() <= window;
                if same_outlet || same_story {
                    hit = Some(index);
                    break;
                }
            }
            let hit = match hit {
                Some(index) => index,
                None => {
                    if bucket.len() < 8 {
                        bucket.push(Anchor {
                            id,
                            ts,
                            publisher,
                            full,
                        });
                    }
                    continue;
                }
            };
            if full && !bucket[hit].full {
                // This copy is the better one: hide the earlier copy instead.
                dups.push(bucket[hit].id);
                bucket[hit] = Anchor {
                    id,
                    ts,
                    publisher,
                    full,
                };
            } else {
                dups.push(id);
            }
        }
        let mut flagged = 0usize;
        for part in dups.chunks(CHUNK) {
            let sql = format!(
                "UPDATE articles SET dupe=1, relevant=0 WHERE id IN ({})",
                placeholders(part.len())
            );
            let params: Vec<SqlValue> = part.iter().map(|id| SqlValue::Integer(*id)).collect();
            flagged += self.exec(&sql, &params)?;
        }
        if flagged > 0 {
            self.set_meta("last_dedupe", &now_iso())?;
        }
        Ok(dups.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;

#[test]
fn find_duplicate_prefers_the_same_publisher() {
    let store = store();
    // The collector hands `find_duplicate` a title with the publisher already
    // stripped, so the three copies differ only in case and punctuation.
    let first = insert_article(&store, "Chip exports hit a record", "Reuters", now_iso());
    insert_article(&store, "chip  exports hit a record!", "AP", now_iso());
    assert_eq!(
        store
            .find_duplicate("Chip exports hit a record", "AP", DUPE_WINDOW_HOURS, None)
            .expect("dupe"),
        Some(first + 1)
    );
    assert_eq!(
        store
            .find_duplicate("Chip exports hit a record", "Reuters", DUPE_WINDOW_HOURS, None)
            .expect("dupe"),
        Some(first)
    );
    assert_eq!(
        store
            .find_duplicate(
                "Chip exports hit a record",
                "Reuters",
                DUPE_WINDOW_HOURS,
                Some(first)
            )
            .expect("dupe"),
        Some(first + 1)
    );
    // Nothing worth comparing.
    assert_eq!(store.find_duplicate("", "AP", 6, None).expect("empty"), None);
    assert_eq!(
        store.find_duplicate("A", "AP", 6, None).expect("short"),
        None
    );
    // Outside the window it is not a duplicate.
    insert_article(&store, "Ancient story", "AP", "2020-01-01T00:00:00Z");
    assert_eq!(
        store.find_duplicate("Ancient story", "AP", 6, None).expect("old"),
        None
    );
}


#[test]
fn dedupe_flags_copies_and_keeps_the_fullest_visible() {
    let store = store();
    let plain = insert_article(&store, "Chip exports hit a record", "Reuters", now_iso());
    let full = store
        .insert(
            "INSERT INTO articles(title,publisher,published,relevant,full_text) VALUES(?,?,?,1,1)",
            &[
                text("Chip exports hit a record"),
                text("AP"),
                text(&now_iso()),
            ],
        )
        .expect("insert");
    let other = insert_article(&store, "Completely different story", "AP", now_iso());
    let flagged = store.dedupe_titles(45, DUPE_WINDOW_HOURS).expect("dedupe");
    assert_eq!(flagged, 1, "exactly one copy flagged");
    let rows = store
        .query("SELECT id,dupe,relevant FROM articles ORDER BY id", &[])
        .expect("rows");
    let by_id: HashMap<i64, (i64, i64)> = rows
        .iter()
        .map(|row| {
            (
                row.int("id").unwrap_or(-1),
                (
                    row.int("dupe").unwrap_or(-1),
                    row.int("relevant").unwrap_or(-1),
                ),
            )
        })
        .collect();
    assert_eq!(by_id[&plain], (1, 0), "the earlier plain copy is hidden");
    assert_eq!(by_id[&full], (0, 1), "the fullest copy stays visible");
    assert_eq!(by_id[&other], (0, 1));
    assert_eq!(
        store.get_meta("last_dedupe").expect("meta").is_some(),
        true,
        "a dedupe sets the marker"
    );
    // Idempotent: already-flagged rows are not re-flagged.
    assert_eq!(
        store.dedupe_titles(45, DUPE_WINDOW_HOURS).expect("again"),
        0
    );
}


#[test]
fn dedupe_ignores_a_reprint_outside_the_window() {
    let store = store();
    // Same headline, different outlet, a day apart: not the same story, so both
    // stay visible (db.py: the window decides).
    insert_article(&store, "Harvest outlook", "AP", "2026-11-05T10:00:00Z");
    insert_article(&store, "Harvest outlook", "Reuters", "2026-11-04T10:00:00Z");
    assert_eq!(store.dedupe_titles(45, DUPE_WINDOW_HOURS).expect("dedupe"), 0);
    // Same outlet a day apart is a reprint, so it is flagged.
    insert_article(&store, "Harvest outlook", "AP", "2026-11-03T10:00:00Z");
    assert_eq!(store.dedupe_titles(45, DUPE_WINDOW_HOURS).expect("dedupe"), 1);
}

}
