//! The monitor tables and every statement that touches them.
//!
//! Port of `server/monitor.py`'s `SCHEMA` plus the SQL that lived inside its
//! functions (`list_monitors`, `add_monitor`, `_alert_exists`, `_store_alert`,
//! `recent_alerts`, `_mark_seen`, …). The statements are here rather than in
//! `atlas-monitor` for the same reason every other statement is: there is one
//! place that knows the shape of the archive.
//!
//! Decisions in this file:
//!
//! * the four tables — `monitors` (a saved query), `alerts` (what fired),
//!   `monitor_seen` (what was judged and will not be judged twice) and
//!   `monitor_runs` (one row per check, with the reason it ran);
//! * the two additions the port needs and `monitor.py` did not have:
//!   `monitors.topic_id` + `monitors.cadence_minutes`, and `alerts.terms`;
//! * that an alert is unique per `(monitor_id, url)` **and** per
//!   `(monitor_id, article_id)` — the check is explicit *and* the insert is
//!   `INSERT OR IGNORE … RETURNING id`, so two overlapping checks cannot both
//!   write the same article;
//! * that `alerts` is read newest first with `id` as the tie-break, so the order
//!   is defined even when two alerts share a second.
//!
//! It does not: decide a threshold, judge an article, schedule a check, or choose
//! a time window. Every timestamp, score and term arrives from the caller.
//!
//! `monitor_seen` and `enrich_checked` are created here because `monitor.py`
//! created them; `enrich_checked` belongs to the enrichment path and no statement
//! in this module uses it.
//!
//! The alert statements themselves — write, list, seen, the duplicate check — are
//! in [`crate::alerts`], on this same [`Monitors`] view. This file is the monitor
//! row and the check log.

use crate::error::{Result, StoreError};
use crate::params::{placeholders, Row, SqlValue};
use crate::Store;

/// The tables `monitor.py::init()` created, plus this port's two column
/// additions and the run log. `IF NOT EXISTS` everywhere, so it is idempotent.
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS monitors(
  id INTEGER PRIMARY KEY, name TEXT, query TEXT, threshold INTEGER DEFAULT 5,
  active INTEGER DEFAULT 1, created TEXT, last_run TEXT, hits INTEGER DEFAULT 0,
  topic_id INTEGER, cadence_minutes INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS alerts(
  id INTEGER PRIMARY KEY, monitor_id INTEGER, kind TEXT DEFAULT 'news',
  article_id INTEGER, url TEXT, title TEXT, relevance INTEGER DEFAULT 0,
  summary TEXT, analysis TEXT, entities TEXT, seen INTEGER DEFAULT 0, created TEXT,
  terms TEXT);
CREATE TABLE IF NOT EXISTS monitor_seen(
  monitor_id INTEGER, key TEXT, PRIMARY KEY(monitor_id, key));
CREATE TABLE IF NOT EXISTS enrich_checked(
  a INTEGER, b INTEGER, PRIMARY KEY(a, b));
CREATE TABLE IF NOT EXISTS monitor_runs(
  id INTEGER PRIMARY KEY, monitor_id INTEGER, reason TEXT, started TEXT, finished TEXT,
  status TEXT, new_alerts INTEGER DEFAULT 0, judged INTEGER DEFAULT 0,
  window_hours INTEGER DEFAULT 72, scorer TEXT, elapsed_ms INTEGER DEFAULT 0, note TEXT);
"#;

/// Columns an archive created before this port is missing. Each is an
/// `ALTER TABLE … ADD COLUMN`, so a second run fails on "duplicate column" and
/// that failure *is* the version marker (see [`crate::schema`]).
pub const MIGRATIONS: &[&str] = &[
    "ALTER TABLE monitors ADD COLUMN topic_id INTEGER",
    "ALTER TABLE monitors ADD COLUMN cadence_minutes INTEGER DEFAULT 0",
    "ALTER TABLE alerts ADD COLUMN terms TEXT",
    "ALTER TABLE alerts ADD COLUMN seen INTEGER DEFAULT 0",
];

/// Indexes, after the migrations that make their columns exist.
pub const INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_alerts_monitor ON alerts(monitor_id, created DESC)",
    "CREATE INDEX IF NOT EXISTS idx_alerts_article ON alerts(monitor_id, article_id)",
    "CREATE INDEX IF NOT EXISTS idx_alerts_unseen ON alerts(seen)",
    "CREATE INDEX IF NOT EXISTS idx_monitor_runs ON monitor_runs(monitor_id, id DESC)",
    "CREATE INDEX IF NOT EXISTS idx_monitors_active ON monitors(active, id)",
];

/// The hard guarantee behind the explicit duplicate check. It can fail on an
/// archive that already holds two alerts for one URL — `monitor.py` swallowed
/// that too, and the explicit check still runs, so only the guarantee is lost.
pub const UNIQUE_URL_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_alerts_monitor_url ON alerts(monitor_id, url)";

/// What [`Store::init_monitors`] did, so a caller can log counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Migrations that changed this archive (already-present columns fail).
    pub migrations_applied: usize,
    /// Migration statements that were already there.
    pub migrations_skipped: usize,
    /// Index statements accepted.
    pub indexes: usize,
    /// Whether the unique `(monitor_id, url)` index exists now.
    pub unique_url_index: bool,
}

/// One monitor as stored. Free of policy: the threshold *is* just a number here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorRow {
    pub id: i64,
    pub name: String,
    pub query: String,
    pub threshold: i64,
    pub active: bool,
    pub created: String,
    pub last_run: String,
    pub hits: i64,
    pub topic_id: Option<i64>,
    pub cadence_minutes: i64,
    pub alerts: i64,
    pub unseen: i64,
}

impl MonitorRow {
    /// Read one row of [`LIST_SQL`] or [`GET_SQL`]. A missing column reads as
    /// the column default, never as an error.
    pub fn from_row(row: &Row) -> MonitorRow {
        MonitorRow {
            id: row.int("id").unwrap_or_default(),
            name: row.text("name").unwrap_or_default().to_string(),
            query: row.text("query").unwrap_or_default().to_string(),
            threshold: row.int("threshold").unwrap_or(5),
            active: row.int("active").unwrap_or(1) != 0,
            created: row.text("created").unwrap_or_default().to_string(),
            last_run: row.text("last_run").unwrap_or_default().to_string(),
            hits: row.int("hits").unwrap_or_default(),
            topic_id: row.int("topic_id"),
            cadence_minutes: row.int("cadence_minutes").unwrap_or_default(),
            alerts: row.int("alerts").unwrap_or_default(),
            unseen: row.int("unseen").unwrap_or_default(),
        }
    }
}

/// The alert row type, re-exported so `atlas_store::monitors::NewAlert` keeps
/// working after the alert statements moved to [`crate::alerts`].
pub use crate::alerts::NewAlert;

/// One check, as logged. `reason` is why it ran (`button`, `scheduler`, `tool`,
/// `check-all`) and `status` is what came of it.
#[derive(Clone, Debug)]
pub struct NewRun<'a> {
    pub monitor_id: i64,
    pub reason: &'a str,
    pub started: &'a str,
    pub finished: &'a str,
    pub status: &'a str,
    pub new_alerts: i64,
    pub judged: i64,
    pub window_hours: i64,
    pub scorer: &'a str,
    pub elapsed_ms: i64,
    pub note: &'a str,
}

/// The monitors of this archive. Obtained from [`Store::monitors`].
pub struct Monitors<'a> {
    /// The archive every statement here runs against. `pub(crate)` so the alert
    /// statements in [`crate::alerts`] can hang off the same view.
    pub(crate) store: &'a Store,
}

impl Store {
    /// Create the monitor tables, run their migrations, then their indexes.
    /// Idempotent, and safe on an archive that predates this port.
    ///
    /// [`crate::Store::init`] calls this, so a caller that wants the whole archive
    /// shape asks for `init` and gets both halves. A failure that is not the
    /// "already there" marker is raised (`crate::schema::run_migrations`), because
    /// a monitor table left half-migrated fails later on a column nobody can see
    /// from here.
    pub fn init_monitors(&self) -> Result<Applied> {
        let conn = self.lock();
        let mut applied = Applied::default();
        conn.execute_batch(SCHEMA).map_err(StoreError::from)?;
        let done = crate::schema::run_migrations(&conn, MIGRATIONS)?;
        applied.migrations_applied = done.applied;
        applied.migrations_skipped = done.skipped;
        for sql in INDEXES {
            conn.execute_batch(sql).map_err(StoreError::from)?;
            applied.indexes += 1;
        }
        applied.unique_url_index = conn.execute(UNIQUE_URL_INDEX, []).is_ok();
        Ok(applied)
    }

    /// The monitor statements, as one view.
    pub fn monitors(&self) -> Monitors<'_> {
        Monitors { store: self }
    }
}

/// Every monitor, active first, newest first, with its alert counts
/// (`monitor.py::list_monitors`).
pub const LIST_SQL: &str = "SELECT m.*, \
     (SELECT COUNT(*) FROM alerts al WHERE al.monitor_id=m.id) AS alerts, \
     (SELECT COUNT(*) FROM alerts al WHERE al.monitor_id=m.id AND al.seen=0) AS unseen \
     FROM monitors m ORDER BY m.active DESC, m.id DESC";

/// One monitor with its counts.
pub const GET_SQL: &str = "SELECT m.*, \
     (SELECT COUNT(*) FROM alerts al WHERE al.monitor_id=m.id) AS alerts, \
     (SELECT COUNT(*) FROM alerts al WHERE al.monitor_id=m.id AND al.seen=0) AS unseen \
     FROM monitors m WHERE m.id=?";

/// Active monitors in id order — the set the scheduler walks
/// (`monitor.py::check_all`).
pub const ACTIVE_SQL: &str = "SELECT * FROM monitors WHERE active=1 ORDER BY id";

impl<'a> Monitors<'a> {
    /// Every monitor with its alert counts.
    pub fn list(&self) -> Result<Vec<MonitorRow>> {
        Ok(self
            .store
            .query(LIST_SQL, &[])?
            .iter()
            .map(MonitorRow::from_row)
            .collect())
    }

    /// One monitor, or `None` when it was deleted.
    pub fn get(&self, id: i64) -> Result<Option<MonitorRow>> {
        Ok(self
            .store
            .query_one(GET_SQL, &[SqlValue::Integer(id)])?
            .as_ref()
            .map(MonitorRow::from_row))
    }

    /// The active monitors the scheduler may consider.
    pub fn active(&self) -> Result<Vec<MonitorRow>> {
        Ok(self
            .store
            .query(ACTIVE_SQL, &[])?
            .iter()
            .map(MonitorRow::from_row)
            .collect())
    }

    /// Save a query the user owns. `last_run` stays empty — "never checked"
    /// until a check actually happens (`monitor.py::add_monitor`).
    pub fn add(
        &self,
        name: &str,
        query: &str,
        threshold: i64,
        cadence_minutes: i64,
        topic_id: Option<i64>,
        created: &str,
    ) -> Result<i64> {
        let params = vec![
            SqlValue::Text(name.to_string()),
            SqlValue::Text(query.to_string()),
            SqlValue::Integer(threshold),
            SqlValue::Text(created.to_string()),
            match topic_id {
                Some(id) => SqlValue::Integer(id),
                None => SqlValue::Null,
            },
            SqlValue::Integer(cadence_minutes.max(0)),
        ];
        self.store.insert(
            "INSERT INTO monitors(name,query,threshold,active,created,last_run,hits,\
             topic_id,cadence_minutes) VALUES(?,?,?,1,?,NULL,0,?,?)",
            &params,
        )
    }

    /// Switch monitoring on or off without losing the query.
    pub fn set_active(&self, id: i64, active: bool) -> Result<usize> {
        self.store.exec(
            "UPDATE monitors SET active=? WHERE id=?",
            &[SqlValue::Integer(i64::from(active)), SqlValue::Integer(id)],
        )
    }

    /// How often the scheduler may run this monitor. `0` means manual only.
    pub fn set_cadence(&self, id: i64, minutes: i64) -> Result<usize> {
        self.store.exec(
            "UPDATE monitors SET cadence_minutes=? WHERE id=?",
            &[SqlValue::Integer(minutes.max(0)), SqlValue::Integer(id)],
        )
    }

    /// Delete a monitor and everything that pointed at it
    /// (`monitor.py::delete_monitor`).
    pub fn delete(&self, id: i64) -> Result<usize> {
        let key = [SqlValue::Integer(id)];
        self.store.exec("DELETE FROM alerts WHERE monitor_id=?", &key)?;
        self.store.exec("DELETE FROM monitor_seen WHERE monitor_id=?", &key)?;
        self.store.exec("DELETE FROM monitor_runs WHERE monitor_id=?", &key)?;
        self.store.exec("DELETE FROM monitors WHERE id=?", &key)
    }

    /// Record that a check happened: `last_run` and the hit counter
    /// (`monitor.py::_run_check`'s update).
    pub fn touch_run(&self, id: i64, at: &str, hits: i64) -> Result<usize> {
        self.store.exec(
            "UPDATE monitors SET last_run=?, hits=hits+? WHERE id=?",
            &[
                SqlValue::Text(at.to_string()),
                SqlValue::Integer(hits),
                SqlValue::Integer(id),
            ],
        )
    }

    /// Ensure an active row exists in `topics` for `name` (case-insensitive)
    /// and return its id. An existing topic is re-activated without overwriting
    /// its keywords or hit history.
    pub fn ensure_topic(&self, name: &str, keywords: &str, created: &str) -> Result<i64> {
        if let Some(id) = self
            .store
            .query_one(
                "SELECT id FROM topics WHERE name=? COLLATE NOCASE",
                &[SqlValue::Text(name.to_string())],
            )?
            .and_then(|row| row.int("id"))
        {
            self.store
                .exec("UPDATE topics SET active=1 WHERE id=?", &[SqlValue::Integer(id)])?;
            return Ok(id);
        }
        self.store.insert(
            "INSERT INTO topics(name,keywords,exclude,min_matches,active,created,hits) \
             VALUES(?,?,'',1,1,?,0)",
            &[
                SqlValue::Text(name.to_string()),
                SqlValue::Text(keywords.to_string()),
                SqlValue::Text(created.to_string()),
            ],
        )
    }

    /// The name of the topic a monitor points at, or `None` when that topic is
    /// gone — a monitor outlives the topic it was made from, and a check has to be
    /// able to say so instead of failing.
    pub fn topic_name(&self, topic_id: i64) -> Result<Option<String>> {
        Ok(self
            .store
            .query_one(
                "SELECT name FROM topics WHERE id=?",
                &[SqlValue::Integer(topic_id)],
            )?
            .and_then(|row| row.text("name").map(|name| name.to_string())))
    }

    /// Articles a check may look at: those fetched since `since`, plus exactly the
    /// ids a research pass just stored (which may be older than `since` if the model
    /// stored something it had already read).
    ///
    /// The only text assembled into this statement is a list of `?`, sized by a
    /// count — never a value.
    pub fn articles_since(&self, since: &str, ids: &[i64], limit: i64) -> Result<Vec<Row>> {
        let limit = limit.clamp(1, 500);
        let mut params = vec![SqlValue::Text(since.to_string())];
        let sql = if ids.is_empty() {
            "SELECT * FROM articles WHERE relevant=1 \
             AND COALESCE(NULLIF(fetched,''), published)>=? \
             ORDER BY published DESC, id DESC LIMIT ?"
                .to_string()
        } else {
            for id in ids {
                params.push(SqlValue::Integer(*id));
            }
            format!(
                "SELECT * FROM articles WHERE relevant=1 AND \
                 (COALESCE(NULLIF(fetched,''), published)>=? OR id IN ({})) \
                 ORDER BY published DESC, id DESC LIMIT ?",
                placeholders(ids.len())
            )
        };
        params.push(SqlValue::Integer(limit));
        self.store.query(&sql, &params)
    }

    /// One row in the check log — the reason it ran, what it did, how long it
    /// took. Every check writes one, including the ones that were refused.
    pub fn add_run(&self, run: &NewRun<'_>) -> Result<i64> {
        self.store.insert(
            "INSERT INTO monitor_runs(monitor_id,reason,started,finished,status,new_alerts,\
             judged,window_hours,scorer,elapsed_ms,note) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            &[
                SqlValue::Integer(run.monitor_id),
                SqlValue::Text(run.reason.to_string()),
                SqlValue::Text(run.started.to_string()),
                SqlValue::Text(run.finished.to_string()),
                SqlValue::Text(run.status.to_string()),
                SqlValue::Integer(run.new_alerts),
                SqlValue::Integer(run.judged),
                SqlValue::Integer(run.window_hours),
                SqlValue::Text(run.scorer.to_string()),
                SqlValue::Integer(run.elapsed_ms),
                SqlValue::Text(run.note.to_string()),
            ],
        )
    }

    /// The newest checks, newest first. `monitor_id` `None` means every monitor.
    pub fn runs(&self, monitor_id: Option<i64>, limit: i64) -> Result<Vec<Row>> {
        match monitor_id {
            Some(id) => self.store.query(
                "SELECT * FROM monitor_runs WHERE monitor_id=? ORDER BY id DESC LIMIT ?",
                &[
                    SqlValue::Integer(id),
                    SqlValue::Integer(limit.clamp(1, 500)),
                ],
            ),
            None => self.store.query(
                "SELECT * FROM monitor_runs ORDER BY id DESC LIMIT ?",
                &[SqlValue::Integer(limit.clamp(1, 500))],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let store = Store::open_memory().expect("memory store");
        store.init().expect("schema");
        store.init_monitors().expect("monitor schema");
        store
    }

    #[test]
    fn init_is_idempotent_and_reports_what_it_did() {
        let store = store();
        let again = store.init_monitors().expect("second init");
        assert_eq!(again.migrations_applied, 0);
        assert_eq!(again.migrations_skipped, MIGRATIONS.len());
        assert!(again.unique_url_index);
    }

    // One `#[test]`, not two: the duplicate was a copy-paste slip that made the
    // compiler warn (`duplicate_macro_attributes`) on every build of this crate.
    #[test]
    fn deleting_a_monitor_takes_its_alerts_and_log_with_it() {
        let store = store();
        let id = store
            .monitors()
            .add("Chips", "chip export", 6, 0, None, "2026-11-05T10:00:00Z")
            .expect("add");
        store
            .monitors()
            .add_run(&NewRun {
                monitor_id: id,
                reason: "button",
                started: "2026-11-05T10:00:00Z",
                finished: "2026-11-05T10:00:01Z",
                status: "ok",
                new_alerts: 0,
                judged: 0,
                window_hours: 72,
                scorer: "model",
                elapsed_ms: 1000,
                note: "",
            })
            .expect("run");
        assert_eq!(store.monitors().delete(id).expect("delete"), 1);
        assert!(store.monitors().get(id).expect("get").is_none());
        assert!(store.monitors().runs(Some(id), 10).expect("runs").is_empty());
    }

    #[test]
    fn articles_of_the_window_and_the_stored_ids_both_come_back() {
        let store = store();
        store
            .exec(
                "INSERT INTO articles(id,url,title,summary,content,published,fetched,relevant) \
                 VALUES(1,'u1','new','','','2026-11-05T09:00:00Z','2026-11-05T09:00:00Z',1), \
                       (2,'u2','old','','','2026-10-01T09:00:00Z','2026-10-01T09:00:00Z',1), \
                       (3,'u3','irrelevant','','','2026-11-05T09:00:00Z','2026-11-05T09:00:00Z',0)",
                &[],
            )
            .expect("insert");
        let window = store
            .monitors()
            .articles_since("2026-11-01T00:00:00Z", &[], 25)
            .expect("window");
        let ids: Vec<i64> = window.iter().map(|row| row.int("id").unwrap_or(0)).collect();
        assert_eq!(ids, vec![1]);
        let with_stored = store
            .monitors()
            .articles_since("2026-11-01T00:00:00Z", &[2], 25)
            .expect("stored");
        let mut ids: Vec<i64> = with_stored
            .iter()
            .map(|row| row.int("id").unwrap_or(0))
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
    }
}
