//! Retention: drop what is older than the window, and nothing else.
//!
//! Decisions: the cutoff (published, else fetched), the batch size, the order of
//! deletion (every table that points at an article, then its full-text row, then
//! the row itself), and that an FTS5 refusal is *counted* rather than stopping the
//! prune — the one deliberate difference from `db.py`, documented at
//! [`crate::articles::FTS_DELETE`].
//!
//! It does not: recompute edges, links or threads (callers do that afterwards),
//! decide the retention window (the caller sets it), or ever prune when the window
//! is under a day — an accidental wipe is the one failure this cannot report after
//! the fact.

use crate::articles::{ARTICLE_REFS, FTS_DELETE};
use crate::clock::iso_ago;
use crate::error::{Result, StoreError};
use crate::params::{placeholders, SqlValue};
use crate::Store;

/// What [`Store::prune_old_detailed`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Articles removed, with every row that referenced them.
    pub removed: usize,
    /// Batches whose `article_fts` delete FTS5 refused (see [`FTS_DELETE`]).
    /// Non-zero means the archive holds articles that were never indexed, which
    /// a full-text rebuild fixes.
    pub fts_refused_batches: usize,
}

impl Store {
    /// Drop everything older than the retention window, in small chunks
    /// (`db.py: prune_old`).
    ///
    /// Each pass deletes at most `chunk` articles and every row that references
    /// them (full text, links, tags, entities, topics, categories, countries,
    /// threads), then the article itself, so no orphan survives. Callers
    /// recompute edges/links/threads afterwards. Returns how many articles were
    /// removed.
    pub fn prune_old(&self, days: Option<i64>, chunk: usize) -> Result<usize> {
        Ok(self.prune_old_detailed(days, chunk)?.removed)
    }

    /// [`Store::prune_old`], plus how many batches FTS5 refused (see
    /// [`FTS_DELETE`]). Counts only: a library reports, it does not log.
    pub fn prune_old_detailed(&self, days: Option<i64>, chunk: usize) -> Result<Pruned> {
        let mut report = Pruned::default();
        let days = days.unwrap_or(self.retention_days);
        if days < 1 {
            return Ok(report); // never wipe the archive by accident
        }
        let days = days.min(3_650);
        let cutoff = iso_ago(days as f64 * 24.0);
        let chunk = chunk.clamp(1, 2_000);
        let mut total = 0usize;
        loop {
            let rows = self.query(
                "SELECT id FROM articles \
                 WHERE COALESCE(NULLIF(published,''),NULLIF(fetched,''),'') <> '' \
                 AND COALESCE(NULLIF(published,''),NULLIF(fetched,''),'') < ? LIMIT ?",
                &[
                    SqlValue::Text(cutoff.clone()),
                    SqlValue::Integer(chunk as i64),
                ],
            )?;
            let ids: Vec<i64> = rows.iter().filter_map(|row| row.int("id")).collect();
            if ids.is_empty() {
                break;
            }
            let placeholders = placeholders(ids.len());
            let params: Vec<SqlValue> = ids.iter().map(|id| SqlValue::Integer(*id)).collect();
            for (label, template, both_ends) in ARTICLE_REFS {
                // `both_ends` needs the placeholder list twice; `replace` fills
                // every `{}` in the template, `replacen` only the one.
                let sql = if *both_ends {
                    template.replace("{}", &placeholders)
                } else {
                    template.replacen("{}", &placeholders, 1)
                };
                let bound: Vec<SqlValue> = if *both_ends {
                    let mut doubled = params.clone();
                    doubled.extend(params.iter().cloned());
                    doubled
                } else {
                    params.clone()
                };
                self.exec(&sql, &bound)
                    .map_err(|err| StoreError::new(format!("prune {}: {}", label, err)))?;
            }
            // Full text first, while `articles` still holds the values the index
            // was built from, then the row itself.
            let fts = FTS_DELETE.replacen("{}", &placeholders, 1);
            if self.exec(&fts, &params).is_err() {
                // FTS5 refused the batch (see FTS_DELETE). The statement is
                // atomic, so nothing changed; count it and keep pruning.
                report.fts_refused_batches += 1;
            }
            self.detach_alerts(&placeholders, &params)?;
            let delete = format!("DELETE FROM articles WHERE id IN ({})", placeholders);
            self.exec(&delete, &params)
                .map_err(|err| StoreError::new(format!("prune articles: {}", err)))?;
            total += ids.len();
            if ids.len() < chunk {
                break;
            }
        }
        if total > 0 {
            // A thread whose members are all gone is noise.
            self.exec(
                "DELETE FROM threads WHERE id NOT IN (SELECT thread_id FROM article_threads)",
                &[],
            )?;
        }
        report.removed = total;
        Ok(report)
    }

    /// Cut the `alerts` rows of articles that are about to go loose from their
    /// article id — the alert stays, the id becomes `NULL`.
    ///
    /// An alert is a record that the reader *was shown* something: the article
    /// ageing out of the window does not make the alert untrue, and deleting it
    /// would quietly rewrite the monitor's history (and the count `list_monitors`
    /// reports). What must not survive is the id: after `DELETE FROM articles`,
    /// SQLite hands that rowid to the next article it stores, so a stale
    /// `article_id` would point at a story the alert was never about — a wrong link
    /// is worse than no link. The alert's own `url` column is what its reader
    /// opens, and it is written for exactly this reason, so the record still says
    /// what it was about.
    ///
    /// Skipped when the archive has no `alerts` table: the monitor half is
    /// [`Store::init_monitors`]'s, and an archive that never ran a monitor check has
    /// nothing to detach — that is not a reason for retention to fail.
    fn detach_alerts(&self, placeholders: &str, params: &[SqlValue]) -> Result<()> {
        let present = self
            .query_one(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='alerts'",
                &[],
            )?
            .is_some();
        if !present {
            return Ok(());
        }
        let sql = format!(
            "UPDATE alerts SET article_id=NULL WHERE article_id IN ({})",
            placeholders
        );
        self.exec(&sql, params)
            .map_err(|err| StoreError::new(format!("prune alerts: {}", err)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use crate::clock::now_iso;

#[test]
fn prune_removes_articles_and_every_reference_to_them() {
    let store = store();
    let old = insert_article(&store, "Old story", "AP", "2020-01-01T00:00:00Z");
    let fresh = insert_article(&store, "Fresh story", "AP", now_iso());
    let entity = store
        .insert(
            "INSERT INTO entities(name,type) VALUES(?,?)",
            &[text("Harvest"), text("topic")],
        )
        .expect("entity");
    store
        .exec(
            "INSERT INTO article_entities(article_id,entity_id) VALUES(?,?)",
            &[int(old), int(entity)],
        )
        .expect("link");
    store
        .exec(
            "INSERT INTO article_tags(article_id,tag) VALUES(?,?)",
            &[int(old), text("grain")],
        )
        .expect("tag");
    store
        .exec(
            "INSERT INTO article_links(a,b,shared) VALUES(?,?,?)",
            &[int(fresh), int(old), int(1)],
        )
        .expect("links");
    store
        .exec(
            // Indexed exactly as `collector.py` indexes it: the same three values
            // the article row holds (summary and content are NULL here).
            "INSERT INTO article_fts(rowid,title) VALUES(?,?)",
            &[int(old), text("Old story")],
        )
        .expect("fts");
    let thread = store
        .insert(
            "INSERT INTO threads(title,size) VALUES(?,?)",
            &[text("t"), int(1)],
        )
        .expect("thread");
    store
        .exec(
            "INSERT INTO article_threads(article_id,thread_id) VALUES(?,?)",
            &[int(old), int(thread)],
        )
        .expect("thread link");

    let report = store.prune_old_detailed(Some(21), 400).expect("prune");
    assert_eq!(report.removed, 1);
    assert_eq!(report.fts_refused_batches, 0, "the article was indexed");
    store
        .exec(
            "INSERT INTO article_fts(article_fts) VALUES('integrity-check')",
            &[],
        )
        .expect("the index stays consistent across a prune");
    let left: Vec<i64> = store
        .query("SELECT id FROM articles", &[])
        .expect("left")
        .iter()
        .filter_map(|row| row.int("id"))
        .collect();
    assert_eq!(left, vec![fresh]);
    for table in [
        "article_entities",
        "article_tags",
        "article_links",
        "article_threads",
        // Not `article_fts`: a scan of an external-content FTS5 table reads the
        // content table, so it always counts the surviving articles. The index
        // itself is `article_fts_docsize`, one row per indexed document.
        "article_fts_docsize",
    ] {
        let count = store
            .query(&format!("SELECT COUNT(*) c FROM {}", table), &[])
            .expect("count")[0]
            .int("c");
        assert_eq!(
            count,
            Some(0),
            "{} kept a reference to a removed article",
            table
        );
    }
    // The thread had one member and it is gone, so the thread is noise.
    assert_eq!(
        store
            .query("SELECT COUNT(*) c FROM threads", &[])
            .expect("threads")[0]
            .int("c"),
        Some(0)
    );
    // Retention off (0 days) must never wipe the archive.
    assert_eq!(store.prune_old(Some(0), 400).expect("prune 0"), 0);
    assert_eq!(
        store
            .query("SELECT COUNT(*) c FROM articles", &[])
            .expect("count")[0]
            .int("c"),
        Some(1)
    );
}


#[test]
fn the_retention_cutoff_reads_an_index_and_here_is_what_it_bought() {
    // The cutoff is `COALESCE(NULLIF(published,''),NULLIF(fetched,''),'') < ?`, an
    // expression, so before `idx_articles_retention` every prune pass scanned the
    // whole table. This measures the same statement on the same 20 000-article
    // archive with the index dropped and then created again, prints both numbers,
    // and asserts the plan is a range read when the index is there. The assertion
    // is on the plan rather than on the milliseconds, because a shared machine
    // moves the milliseconds.
    use std::time::Instant;

    let store = store();
    let published = now_iso();
    let mut rows: Vec<Vec<SqlValue>> = Vec::with_capacity(20_000);
    for index in 0..20_000 {
        rows.push(vec![
            text(&format!("Story {}", index)),
            text("summary"),
            text("body text of the article"),
            text(&published),
            text(&published),
        ]);
    }
    store
        .insert_many(
            "INSERT INTO articles(title,summary,content,published,fetched) VALUES(?,?,?,?,?)",
            &rows,
        )
        .expect("seed 20 000 articles");
    let cutoff = iso_ago(21.0 * 24.0);
    let sql = "SELECT id FROM articles \
               WHERE COALESCE(NULLIF(published,''),NULLIF(fetched,''),'')<>'' \
               AND COALESCE(NULLIF(published,''),NULLIF(fetched,''),'') < ? LIMIT 400";
    let cutoff = SqlValue::Text(cutoff);
    let params = [cutoff.clone()];

    let time = |store: &Store| {
        let started = Instant::now();
        for _ in 0..20 {
            store.query(sql, &params).expect("cutoff query");
        }
        started.elapsed().as_secs_f64() * 1_000.0 / 20.0
    };

    store
        .exec("DROP INDEX IF EXISTS idx_articles_retention", &[])
        .expect("drop");
    let scanned = time(&store);
    let plan: String = store
        .query(&format!("EXPLAIN QUERY PLAN {}", sql), &params)
        .expect("plan")
        .iter()
        .filter_map(|row| row.text("detail").map(|detail| detail.to_string()))
        .collect::<Vec<String>>()
        .join(" | ")
        .to_ascii_uppercase();
    assert!(
        plan.contains("SCAN ARTICLES"),
        "without the index this is a scan, which is the point: {}",
        plan
    );

    store
        .exec(
            "CREATE INDEX IF NOT EXISTS idx_articles_retention \
             ON articles(COALESCE(NULLIF(published,''),NULLIF(fetched,''),''))",
            &[],
        )
        .expect("create");
    let ranged = time(&store);
    let plan: String = store
        .query(&format!("EXPLAIN QUERY PLAN {}", sql), &params)
        .expect("plan")
        .iter()
        .filter_map(|row| row.text("detail").map(|detail| detail.to_string()))
        .collect::<Vec<String>>()
        .join(" | ")
        .to_ascii_uppercase();
    assert!(
        plan.contains("SEARCH ARTICLES USING COVERING INDEX IDX_ARTICLES_RETENTION"),
        "the prune's cutoff must range over idx_articles_retention: {}",
        plan
    );
    eprintln!(
        "atlas-store: prune cutoff over 20 000 articles = {:.3} ms scanning, \
         {:.3} ms on the index ({:.0}x)",
        scanned,
        ranged,
        scanned / ranged.max(0.000_001)
    );
    assert!(
        ranged <= scanned,
        "the indexed statement must not be slower than the scan: {:.3} ms vs {:.3} ms",
        ranged,
        scanned
    );
}

#[test]
fn a_pruned_article_leaves_its_alert_in_place_with_no_article_id() {
    // Retention removes the article; the alert is a record that the reader *saw*
    // the story, and deleting it would rewrite the monitor's history. What must not
    // survive is the id: `articles.id` is a plain `INTEGER PRIMARY KEY`, so SQLite
    // hands a deleted rowid to the next insert — the alert would then link to a
    // story it was never about. `article_id` becomes NULL and the alert's own `url`
    // keeps saying what it was about.
    let store = store();
    store.init_monitors().expect("the monitor tables, as the app makes them");
    let old = insert_article(&store, "Old story", "AP", "2020-01-01T00:00:00Z");
    let monitor = store
        .insert(
            "INSERT INTO monitors(name,query,active,created) VALUES(?,?,1,?)",
            &[text("Grain watch"), text("grain"), text(&now_iso())],
        )
        .expect("monitor");
    let alert = store
        .insert(
            "INSERT INTO alerts(monitor_id,kind,article_id,url,title,relevance,created) \
             VALUES(?,'news',?,?,?,8,?)",
            &[
                int(monitor),
                int(old),
                text("https://example.com/old-story"),
                text("Old story"),
                text(&now_iso()),
            ],
        )
        .expect("alert");
    // The link is real before the prune: this test would pass vacuously otherwise.
    assert_eq!(
        store
            .query_one("SELECT article_id FROM alerts WHERE id=?", &[int(alert)])
            .expect("read")
            .and_then(|row| row.int("article_id")),
        Some(old)
    );

    let report = store.prune_old_detailed(Some(21), 400).expect("prune");
    assert_eq!(report.removed, 1);

    let kept = store
        .query_one("SELECT article_id,url,title FROM alerts WHERE id=?", &[int(alert)])
        .expect("the alert row is still there")
        .expect("a pruned article must not delete the alert it fired");
    assert_eq!(kept.int("article_id"), None, "a dangling id is not kept");
    assert_eq!(
        kept.text("url"),
        Some("https://example.com/old-story"),
        "the alert still names what it was about"
    );
    assert_eq!(kept.text("title"), Some("Old story"));
    // …and nothing points at an id that is no longer an article. (`article_id IS
    // NOT NULL` first: with an empty `articles` table, `NULL NOT IN ()` is true in
    // SQLite, so the NULL that is *correct* here would otherwise be counted.)
    assert_eq!(
        store
            .query(
                "SELECT COUNT(*) c FROM alerts \
                 WHERE article_id IS NOT NULL AND article_id NOT IN (SELECT id FROM articles)",
                &[]
            )
            .expect("count")[0]
            .int("c"),
        Some(0)
    );
}

#[test]
fn prune_falls_back_to_fetched_when_published_is_empty() {
    let store = store();
    store
        .exec(
            "INSERT INTO articles(title,published,fetched) VALUES(?,?,?)",
            &[text("Undated"), text(""), text("2019-05-05T00:00:00Z")],
        )
        .expect("insert");
    store
        .exec(
            "INSERT INTO articles(title,published,fetched) VALUES(?,?,?)",
            &[text("No dates at all"), text(""), text("")],
        )
        .expect("insert");
    // Neither article was ever indexed, so FTS5 refuses both deletes; the prune
    // counts that (db.py would raise and stop) and still removes the articles.
    let report = store.prune_old_detailed(Some(21), 400).expect("prune");
    assert_eq!(report.removed, 1);
    assert_eq!(report.fts_refused_batches, 1);
    // `prune_old` is db.py's signature: the count, nothing else.
    assert_eq!(store.prune_old(Some(21), 400).expect("prune again"), 0);
    let titles: Vec<String> = store
        .query("SELECT title FROM articles", &[])
        .expect("left")
        .iter()
        .filter_map(|row| row.text("title").map(|title| title.to_string()))
        .collect();
    assert_eq!(titles, vec!["No dates at all".to_string()]);
}


#[test]
fn fts_delete_refuses_a_row_it_never_indexed() {
    // Regression: FTS5 refuses to delete a row it never indexed, and the refusal
    // is atomic — the index still passes `integrity-check`.
    let store = store();
    let id = insert_article(&store, "Never indexed", "AP", now_iso());
    let refused = store.exec(&FTS_DELETE.replace("{}", "?"), &[int(id)]);
    assert!(refused.is_err(), "FTS5 deletes only what it indexed");
    store
        .exec(
            "INSERT INTO article_fts(article_fts) VALUES('integrity-check')",
            &[],
        )
        .expect("the index is still consistent after a refused delete");
    // The row is indexed: the same statement now succeeds and removes it.
    store
        .exec(
            "INSERT INTO article_fts(rowid,title,summary,content) VALUES(?,?,'','')",
            &[int(id), text("Never indexed")],
        )
        .expect("index it");
    store
        .exec(
            "UPDATE articles SET summary='', content='' WHERE id=?",
            &[int(id)],
        )
        .expect("match the indexed values");
    assert_eq!(
        store
            .exec(&FTS_DELETE.replace("{}", "?"), &[int(id)])
            .expect("delete the indexed row"),
        1
    );
    store
        .exec(
            "INSERT INTO article_fts(article_fts) VALUES('integrity-check')",
            &[],
        )
        .expect("consistent after a real delete");
}

}
