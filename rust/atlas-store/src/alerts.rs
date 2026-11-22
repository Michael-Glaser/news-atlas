//! `alerts` — what a monitor fired on, what the reader has seen, and the marks
//! that stop a second judgement.
//!
//! Decisions: that an alert is unique per monitor and article (the explicit check
//! *and* the `INSERT OR IGNORE … RETURNING id`), that `alerts` is read newest first
//! with `id` as the tie-break, and that a below-threshold or not-reporting verdict
//! leaves a `monitor_seen` mark so the next check does not pay for it again.
//!
//! It does not: decide a threshold, judge an article, or write a check-log row.
//! Every score, term and timestamp arrives from the caller (`atlas-monitor`).
//!
//! The statements hang off the same [`crate::monitors::Monitors`] view, so a caller
//! says `store.monitors().add_alert(…)` for both halves — one place to look, two
//! files to read.

use crate::error::Result;
use crate::monitors::Monitors;
use crate::params::{Row, SqlValue};

/// An alert about to be written. The JSON columns arrive already encoded, so
/// nothing in this crate decides what an entity or a term looks like.
#[derive(Clone, Debug)]
pub struct NewAlert<'a> {
    pub monitor_id: i64,
    pub kind: &'a str,
    pub article_id: Option<i64>,
    pub url: &'a str,
    pub title: &'a str,
    pub relevance: i64,
    pub summary: &'a str,
    pub analysis: &'a str,
    pub entities_json: &'a str,
    pub terms_json: &'a str,
    pub created: &'a str,
}

/// Alerts since an ISO cutoff, newest first, with the monitor's name and query
/// beside them.
///
/// `created DESC, id DESC`: two alerts of the same second still have one defined
/// order, which the alert list needs and a test can assert.
pub const RECENT_ALERTS_SQL: &str =
    "SELECT al.*, m.name AS monitor_name, m.query AS monitor_query \
     FROM alerts al LEFT JOIN monitors m ON m.id=al.monitor_id \
     WHERE al.created>=? ORDER BY al.created DESC, al.id DESC LIMIT ?";

impl<'a> Monitors<'a> {
    /// Has this monitor already raised an alert for this article?
    ///
    /// The URL decides when there is one, else the article id — the same order
    /// `monitor.py::_alert_exists` used, because two checks of one story can reach
    /// it through different paths.
    pub fn alert_exists(
        &self,
        monitor_id: i64,
        article_id: Option<i64>,
        url: Option<&str>,
    ) -> Result<bool> {
        let found = match url.filter(|text| !text.is_empty()) {
            Some(url) => self.store.query_one(
                "SELECT 1 FROM alerts WHERE monitor_id=? AND url=?",
                &[SqlValue::Integer(monitor_id), SqlValue::Text(url.to_string())],
            )?,
            None => match article_id {
                Some(article_id) => self.store.query_one(
                    "SELECT 1 FROM alerts WHERE monitor_id=? AND article_id=?",
                    &[SqlValue::Integer(monitor_id), SqlValue::Integer(article_id)],
                )?,
                None => None,
            },
        };
        Ok(found.is_some())
    }

    /// Write one alert, refusing a second copy for the same article.
    ///
    /// `None` means "already there": the explicit check and the unique index agree,
    /// and `RETURNING id` reports which of the two happened
    /// (`monitor.py::_store_alert`).
    pub fn add_alert(&self, alert: &NewAlert<'_>) -> Result<Option<i64>> {
        if self.alert_exists(
            alert.monitor_id,
            alert.article_id,
            Some(alert.url).filter(|url| !url.is_empty()),
        )? {
            return Ok(None);
        }
        let params = vec![
            SqlValue::Integer(alert.monitor_id),
            SqlValue::Text(alert.kind.to_string()),
            match alert.article_id {
                Some(id) => SqlValue::Integer(id),
                None => SqlValue::Null,
            },
            SqlValue::Text(alert.url.to_string()),
            SqlValue::Text(alert.title.to_string()),
            SqlValue::Integer(alert.relevance.clamp(0, 10)),
            SqlValue::Text(alert.summary.to_string()),
            SqlValue::Text(alert.analysis.to_string()),
            SqlValue::Text(alert.entities_json.to_string()),
            SqlValue::Text(alert.terms_json.to_string()),
            SqlValue::Text(alert.created.to_string()),
        ];
        let row = self.store.query_one(
            "INSERT OR IGNORE INTO alerts(monitor_id,kind,article_id,url,title,relevance,\
             summary,analysis,entities,terms,seen,created) VALUES(?,?,?,?,?,?,?,?,?,?,0,?) \
             RETURNING id",
            &params,
        )?;
        Ok(row.as_ref().and_then(|row| row.int("id")))
    }

    /// Alerts since `since`, newest first; `monitor` filters on a name or query
    /// substring (`monitor.py::recent_alerts`).
    ///
    /// The name is a *substring to find*, not a pattern: it goes through
    /// [`crate::search::like_pattern`], which escapes `%`, `_` and `\`, and the
    /// two `LIKE`s carry `ESCAPE '\'`, which is what makes the escaping real. A
    /// raw `format!("%{}%", name)` binds the name as a value and so cannot be SQL,
    /// but it does let a caller's `%` or `_` widen the match: measured on this
    /// archive, `recent_alerts(_, Some("%"), _)` returned *every* alert whose
    /// monitor existed, and a name that really is `Chips%` could not be asked for
    /// literally. `q` on `/api/articles` and `/api/search` already mean "these
    /// characters", so the alert filter means the same thing.
    pub fn recent_alerts(
        &self,
        since: &str,
        monitor: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Row>> {
        match monitor.filter(|name| !name.is_empty()) {
            Some(name) => {
                let like = crate::search::like_pattern(name);
                self.store.query(
                    "SELECT al.*, m.name AS monitor_name, m.query AS monitor_query \
                     FROM alerts al LEFT JOIN monitors m ON m.id=al.monitor_id \
                     WHERE al.created>=? \
                     AND (m.name LIKE ? ESCAPE '\\' OR m.query LIKE ? ESCAPE '\\') \
                     ORDER BY al.created DESC, al.id DESC LIMIT ?",
                    &[
                        SqlValue::Text(since.to_string()),
                        SqlValue::Text(like.clone()),
                        SqlValue::Text(like),
                        SqlValue::Integer(limit.clamp(1, 500)),
                    ],
                )
            }
            None => self.store.query(
                RECENT_ALERTS_SQL,
                &[
                    SqlValue::Text(since.to_string()),
                    SqlValue::Integer(limit.clamp(1, 500)),
                ],
            ),
        }
    }

    /// Seen or unseen, one alert.
    pub fn mark_alert_seen(&self, id: i64, seen: bool) -> Result<usize> {
        self.store.exec(
            "UPDATE alerts SET seen=? WHERE id=?",
            &[SqlValue::Integer(i64::from(seen)), SqlValue::Integer(id)],
        )
    }

    /// How many alerts the reader has not opened yet.
    pub fn unseen_count(&self) -> Result<i64> {
        Ok(self
            .store
            .query_one("SELECT COUNT(*) c FROM alerts WHERE seen=0", &[])?
            .and_then(|row| row.int("c"))
            .unwrap_or_default())
    }

    /// Was this candidate already judged below the threshold — or judged not to be
    /// reporting at all? Those two answers are remembered so a second check does not
    /// pay for them again (`monitor.py::_seen`).
    pub fn seen(&self, monitor_id: i64, key: &str) -> Result<bool> {
        Ok(self
            .store
            .query_one(
                "SELECT 1 x FROM monitor_seen WHERE monitor_id=? AND key=?",
                &[SqlValue::Integer(monitor_id), SqlValue::Text(key.to_string())],
            )?
            .is_some())
    }

    /// Remember that answer.
    pub fn mark_seen(&self, monitor_id: i64, key: &str) -> Result<usize> {
        self.store.exec(
            "INSERT OR IGNORE INTO monitor_seen(monitor_id,key) VALUES(?,?)",
            &[SqlValue::Integer(monitor_id), SqlValue::Text(key.to_string())],
        )
    }

    /// Attach the entities an alert named to the article it fired on, when those
    /// entities are already known (`monitor.py::_link_entities`).
    ///
    /// Nothing is invented here: an entity the graph does not know is left for
    /// `atlas-graph`'s enrich path, which is where a name becomes an entity.
    pub fn attach_entities(&self, article_id: i64, names: &[String]) -> Result<usize> {
        let mut attached = 0usize;
        for name in names {
            if name.trim().is_empty() {
                continue;
            }
            let found = self.store.query_one(
                "SELECT id FROM entities WHERE name=? COLLATE NOCASE",
                &[SqlValue::Text(name.clone())],
            )?;
            let Some(entity_id) = found.as_ref().and_then(|row| row.int("id")) else {
                continue;
            };
            attached += self.store.exec(
                "INSERT OR IGNORE INTO article_entities(article_id,entity_id) VALUES(?,?)",
                &[SqlValue::Integer(article_id), SqlValue::Integer(entity_id)],
            )?;
        }
        Ok(attached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn store() -> Store {
        let store = Store::open_memory().expect("memory store");
        store.init().expect("schema");
        store.init_monitors().expect("monitor schema");
        store
    }

    fn alert(monitor_id: i64) -> NewAlert<'static> {
        NewAlert {
            monitor_id,
            kind: "news",
            article_id: Some(7),
            url: "https://example.com/a",
            title: "A title",
            relevance: 8,
            summary: "",
            analysis: "why",
            entities_json: "[]",
            terms_json: "[]",
            created: "2026-11-05T10:01:00Z",
        }
    }

    #[test]
    fn an_alert_is_written_once_per_article() {
        let store = store();
        let id = store
            .monitors()
            .add("Chips", "chip export", 6, 0, None, "2026-11-05T10:00:00Z")
            .expect("add");
        assert!(store
            .monitors()
            .add_alert(&alert(id))
            .expect("first")
            .is_some());
        assert!(store
            .monitors()
            .add_alert(&alert(id))
            .expect("second")
            .is_none());
        assert_eq!(store.monitors().unseen_count().expect("count"), 1);
    }

    #[test]
    fn the_reader_flags_an_alert_seen_and_unseen_again() {
        let store = store();
        let id = store
            .monitors()
            .add("Chips", "chip export", 6, 0, None, "2026-11-05T10:00:00Z")
            .expect("add");
        let alert_id = store
            .monitors()
            .add_alert(&alert(id))
            .expect("write")
            .expect("id");
        assert_eq!(store.monitors().unseen_count().expect("count"), 1);
        store.monitors().mark_alert_seen(alert_id, true).expect("seen");
        assert_eq!(store.monitors().unseen_count().expect("count"), 0);
        store
            .monitors()
            .mark_alert_seen(alert_id, false)
            .expect("unseen");
        assert_eq!(store.monitors().unseen_count().expect("count"), 1);
    }

    #[test]
    fn alerts_of_one_second_keep_a_defined_order() {
        let store = store();
        let id = store
            .monitors()
            .add("Chips", "chip export", 6, 0, None, "2026-11-05T10:00:00Z")
            .expect("add");
        for index in 0..3 {
            // One URL each: the same URL twice is a duplicate and would not be
            // written at all.
            let url = format!("https://example.com/same-second-{}", index);
            let mut row = alert(id);
            row.article_id = Some(index);
            row.url = &url;
            row.created = "2026-11-05T10:01:00Z";
            store.monitors().add_alert(&row).expect("write");
        }
        let rows = store
            .monitors()
            .recent_alerts("2026-11-05T00:00:00Z", None, 10)
            .expect("recent");
        let ids: Vec<i64> = rows.iter().filter_map(|row| row.int("id")).collect();
        assert_eq!(ids, vec![3, 2, 1]);
    }
}

#[cfg(test)]
mod escaping {
    //! The filter is characters, not a pattern.
    //!
    //! Split out as its own module so the test names say what they pin: this is
    //! the wildcard half of the SQL-injection question. A monitor name reaches
    //! the statement as a *bound* value and can never be SQL (the hostile corpus
    //! in `crate::tests` runs it through every helper), but a `%` in it used to be
    //! a wildcard over every monitor in the archive.
    //!
    //! The filter matches the monitor's **name or its query**, so every fixture
    //! here gives each monitor its own query: two monitors sharing one query would
    //! make `name`-only assertions pass for a reason that has nothing to do with
    //! escaping (found the hard way when the first draft of this file reused
    //! `chip export` for both).

    use super::*;
    use crate::Store;

    fn store() -> Store {
        let store = Store::open_memory().expect("memory store");
        store.init().expect("schema");
        store
    }

    /// One monitor (name, query) and one alert on it; returns the alert id.
    fn alert_for(store: &Store, monitor: &str, query: &str, url: &str) -> i64 {
        let id = store
            .monitors()
            .add(monitor, query, 6, 0, None, "2026-11-05T10:00:00Z")
            .expect("add monitor");
        store
            .monitors()
            .add_alert(&NewAlert {
                monitor_id: id,
                kind: "news",
                article_id: None,
                url,
                title: "A title",
                relevance: 8,
                summary: "",
                analysis: "",
                entities_json: "[]",
                terms_json: "[]",
                created: "2026-11-05T10:01:00Z",
            })
            .expect("alert")
            .expect("new alert has an id")
    }

    fn filtered(store: &Store, monitor: &str) -> Vec<String> {
        store
            .monitors()
            .recent_alerts("2026-11-05T00:00:00Z", Some(monitor), 50)
            .expect("recent alerts")
            .iter()
            .filter_map(|row| row.text("monitor_name").map(|name| name.to_string()))
            .collect()
    }

    #[test]
    fn a_percent_in_the_filter_is_a_character_not_a_wildcard() {
        // Two monitors, one of them named with a real `%`, and two *different*
        // queries so the name is the only thing that can match. `%` as the filter
        // matches the monitor whose name contains one and not the other.
        // Before the escaping (`format!("%{}%", name)` and a bare `LIKE ?`) this
        // was `["Grain", "Chips%"]`: `%` matched every row with any monitor.
        let store = store();
        alert_for(&store, "Chips%", "chip export", "https://example.test/a");
        alert_for(&store, "Grain", "wheat harvest", "https://example.test/b");
        assert_eq!(filtered(&store, "%"), vec!["Chips%".to_string()]);
        // …and asking for the literal name still finds it.
        assert_eq!(filtered(&store, "Chips%"), vec!["Chips%".to_string()]);
        // A substring of a name still works: this is a filter a reader types.
        assert_eq!(filtered(&store, "chip"), vec!["Chips%".to_string()]);
        assert!(filtered(&store, "zzz").is_empty());
    }

    #[test]
    fn an_underscore_in_the_filter_is_a_character_not_one_character() {
        // `_` is LIKE's single-character wildcard. `Ch_p` matched `Chips%`
        // before; it is a literal now, and nothing in the archive contains it.
        let store = store();
        alert_for(&store, "Chips%", "chip export", "https://example.test/a");
        let wrong = filtered(&store, "Ch_p");
        assert!(wrong.is_empty(), "a wildcard got through: {:?}", wrong);
        // A name that really holds an underscore is still selectable literally.
        alert_for(&store, "chips_2026", "chip shortage", "https://example.test/c");
        assert_eq!(filtered(&store, "chips_2026"), vec!["chips_2026".to_string()]);
        // …and its underscore does not also select the underscore-free neighbour.
        assert!(!filtered(&store, "chips_2026").contains(&"Chips%".to_string()));
    }

    #[test]
    fn a_backslash_in_the_filter_cannot_escape_the_escape() {
        // `\` is the ESCAPE character. A filter that holds one must not turn the
        // trailing `%` of the pattern into a literal-only match, and must not make
        // the statement a syntax error either.
        let store = store();
        alert_for(&store, "back\\slash", "chip export", "https://example.test/a");
        assert_eq!(
            filtered(&store, "back\\slash"),
            vec!["back\\slash".to_string()]
        );
        assert_eq!(filtered(&store, "\\"), vec!["back\\slash".to_string()]);
        assert!(filtered(&store, "slash\\").is_empty());
    }
}
