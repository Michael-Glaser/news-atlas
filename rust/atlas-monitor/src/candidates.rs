//! `candidates` — which articles one check is allowed to look at.
//!
//! Decisions: that "new" means *stored since the monitor's last check*, that the
//! ids a research pass just reported are always included (a stored page can be
//! older than the last check if the model had read it before), that an article
//! which already alerted — or was already judged below the threshold — is not
//! looked at again, and that a row with no summary and no body is not a candidate
//! at all (there is nothing to read, so nothing to judge).
//!
//! It does not: score anything, run research, or write the seen-marks it reads.
//! The SQL is `atlas_store::monitors::articles_since`; the window comes from the
//! caller, so the 72-hour rule exists once.

use std::collections::HashSet;

use atlas_store::{Row, Store};

use crate::error::Result;
use crate::spec::Monitor;

/// How much of a stored article is handed to the model (`ai.analyze_for_monitor`
/// sent 600 of summary and 1 500 of text).
pub const JUDGE_SUMMARY_CHARS: usize = 600;
pub const JUDGE_BODY_CHARS: usize = 1_500;

/// One stored article, as a check sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Article {
    pub id: i64,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub url: String,
    pub published: String,
    pub fetched: String,
    pub publisher: String,
}

impl Article {
    /// Read one `articles` row. Missing columns read as empty, never as an error:
    /// an old archive has fewer of them.
    pub fn from_row(row: &Row) -> Article {
        Article {
            id: row.int("id").unwrap_or_default(),
            title: row.text("title").unwrap_or_default().to_string(),
            summary: row.text("summary").unwrap_or_default().to_string(),
            content: row.text("content").unwrap_or_default().to_string(),
            url: row.text("url").unwrap_or_default().to_string(),
            published: row.text("published").unwrap_or_default().to_string(),
            fetched: row.text("fetched").unwrap_or_default().to_string(),
            publisher: row.text("publisher").unwrap_or_default().to_string(),
        }
    }

    /// What the model reads: the opening of the summary, then the opening of the
    /// text. Capped here so a check costs the same whatever a page weighed.
    pub fn body(&self) -> String {
        let summary: String = self.summary.chars().take(JUDGE_SUMMARY_CHARS).collect();
        let content: String = self.content.chars().take(JUDGE_BODY_CHARS).collect();
        if content.is_empty() {
            summary
        } else {
            format!("{}\n{}", summary, content)
        }
    }

    /// Is there anything to read? A row stored with neither a summary nor a body is
    /// a title and a URL: the news question would answer from the headline alone
    /// and the relevance question would score it on the same three words. Nothing
    /// was read, so nothing here can call it reporting; the check says how many
    /// candidates it left unread instead.
    pub fn has_text(&self) -> bool {
        !self.summary.trim().is_empty() || !self.content.trim().is_empty()
    }

    /// The mark this article leaves in `monitor_seen` (`monitor.py` used `a%d`).
    pub fn key(&self) -> String {
        format!("a{}", self.id)
    }
}

/// What one check selected, and what it refused to look at twice.
#[derive(Clone, Debug, Default)]
pub struct Selected {
    pub articles: Vec<Article>,
    /// Distinct rows the store offered, over every page a scan needed.
    pub considered: usize,
    /// Already alerted for this monitor — the duplicate-suppression count.
    pub skipped_alerted: usize,
    /// Already judged below the threshold, or judged not to be reporting.
    pub skipped_seen: usize,
    /// Dropped because the judge budget for one check was reached.
    pub skipped_budget: usize,
    /// Dropped because the archive holds no text for them — neither a summary nor a
    /// body. A page nothing was read from cannot be called reporting, so it is not
    /// handed to the model and not alerted on.
    pub skipped_unread: usize,
}

/// How far one scan may widen when its rows were all already judged.
/// `atlas_store::monitors::articles_since` clamps its own limit to this.
pub const MAX_SCAN_ROWS: usize = 500;

/// How much wider each further page is: the first page is the caller's
/// `scan_limit`, then `scan_limit * 4`, then the store's ceiling.
pub const SCAN_GROWTH: usize = 4;

/// Select what is new for one monitor.
///
/// `since` is the previous check's start; `stored` are the article ids the research
/// pass reported; `budget` caps how many articles one check judges.
///
/// # Why more than one page
///
/// One query returns the newest `scan_limit` rows of the window, and the
/// seen/alerted filter runs *here*, after it: when those rows had all been judged
/// already, a check found nothing to look at even though the window still held
/// unjudged articles — and `last_run` then moved past them, so they were lost for
/// good. The scan therefore widens (×[`SCAN_GROWTH`], up to [`MAX_SCAN_ROWS`])
/// while the page came back full and the budget is not yet filled.
pub fn new_since(
    store: &Store,
    monitor: &Monitor,
    since: &str,
    stored: &[i64],
    scan_limit: usize,
    budget: usize,
) -> Result<Selected> {
    let budget = budget.max(1);
    let mut limit = scan_limit.max(1).min(MAX_SCAN_ROWS);
    let mut selected = Selected::default();
    // A wider scan repeats the rows of the narrower one; each row is counted once.
    let mut scanned: HashSet<i64> = HashSet::new();
    loop {
        let rows = store
            .monitors()
            .articles_since(since, stored, limit as i64)?;
        let page_was_full = rows.len() >= limit;
        for row in &rows {
            let article = Article::from_row(row);
            if !scanned.insert(article.id) {
                continue;
            }
            selected.considered += 1;
            if store
                .monitors()
                .alert_exists(monitor.id, Some(article.id), Some(&article.url))?
            {
                selected.skipped_alerted += 1;
                continue;
            }
            if store.monitors().seen(monitor.id, &article.key())? {
                selected.skipped_seen += 1;
                continue;
            }
            if !article.has_text() {
                selected.skipped_unread += 1;
                continue;
            }
            if selected.articles.len() >= budget {
                selected.skipped_budget += 1;
                continue;
            }
            selected.articles.push(article);
        }
        if !page_was_full || selected.articles.len() >= budget {
            break;
        }
        let wider = limit.saturating_mul(SCAN_GROWTH).min(MAX_SCAN_ROWS);
        if wider == limit {
            break;
        }
        limit = wider;
    }
    Ok(selected)
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

    fn insert(store: &Store, id: i64, fetched: &str) {
        let sql = "INSERT INTO articles(id,url,title,summary,content,published,fetched,relevant,publisher) \
                   VALUES(?,?,?,?,?,?,?,1,'')";
        store
            .exec(
                sql,
                &[
                    atlas_store::SqlValue::Integer(id),
                    atlas_store::SqlValue::Text(format!("https://example.com/{}", id)),
                    atlas_store::SqlValue::Text(format!("title {}", id)),
                    atlas_store::SqlValue::Text("summary".to_string()),
                    atlas_store::SqlValue::Text("content".to_string()),
                    atlas_store::SqlValue::Text(fetched.to_string()),
                    atlas_store::SqlValue::Text(fetched.to_string()),
                ],
            )
            .expect("insert");
    }

    fn monitor(store: &Store) -> Monitor {
        let id = store
            .monitors()
            .add("Chips", "chip export", 6, 0, None, "2026-11-05T09:00:00Z")
            .expect("add");
        Monitor {
            id,
            name: "Chips".to_string(),
            query: "chip export".to_string(),
            threshold: 6,
            active: true,
            ..Monitor::default()
        }
    }

    #[test]
    fn an_article_that_already_alerted_is_not_selected_again() {
        let store = store();
        let monitor = monitor(&store);
        insert(&store, 1, "2026-11-05T10:00:00Z");
        store
            .monitors()
            .add_alert(&atlas_store::monitors::NewAlert {
                monitor_id: monitor.id,
                kind: "news",
                article_id: Some(1),
                url: "https://example.com/1",
                title: "title 1",
                relevance: 7,
                summary: "",
                analysis: "",
                entities_json: "[]",
                terms_json: "[]",
                created: "2026-11-05T10:01:00Z",
            })
            .expect("alert");
        let selected =
            new_since(&store, &monitor, "2026-11-05T00:00:00Z", &[], 25, 8).expect("select");
        assert!(selected.articles.is_empty());
        assert_eq!(selected.skipped_alerted, 1);
    }

    #[test]
    fn an_article_judged_below_the_threshold_is_not_selected_again() {
        let store = store();
        let monitor = monitor(&store);
        insert(&store, 2, "2026-11-05T10:00:00Z");
        store
            .monitors()
            .mark_seen(monitor.id, "a2")
            .expect("mark seen");
        let selected =
            new_since(&store, &monitor, "2026-11-05T00:00:00Z", &[], 25, 8).expect("select");
        assert!(selected.articles.is_empty());
        assert_eq!(selected.skipped_seen, 1);
    }

    #[test]
    fn the_budget_caps_one_check_and_says_what_it_left() {
        let store = store();
        let monitor = monitor(&store);
        for id in 1..=5 {
            insert(&store, id, "2026-11-05T10:00:00Z");
        }
        let selected =
            new_since(&store, &monitor, "2026-11-05T00:00:00Z", &[], 25, 2).expect("select");
        assert_eq!(selected.articles.len(), 2);
        assert_eq!(selected.skipped_budget, 3);
        assert_eq!(selected.considered, 5);
    }

    #[test]
    fn nothing_stored_since_the_last_check_means_no_candidates() {        let store = store();
        let monitor = monitor(&store);
        insert(&store, 1, "2026-11-01T10:00:00Z");
        let selected =
            new_since(&store, &monitor, "2026-11-05T00:00:00Z", &[], 25, 8).expect("select");
        assert!(selected.articles.is_empty());
        assert_eq!(selected.considered, 0);
    }

    #[test]
    fn the_body_the_model_reads_is_capped() {
        let article = Article {
            summary: "s".repeat(5_000),
            content: "c".repeat(9_000),
            ..Article::default()
        };
        let body = article.body();
        assert_eq!(body.chars().count(), JUDGE_SUMMARY_CHARS + 1 + JUDGE_BODY_CHARS);
    }

    #[test]
    fn an_article_with_no_text_is_not_a_candidate() {
        // Stored by a source that handed over a headline and nothing else: the
        // model would score the title, and a monitor would alert on a page nobody
        // read. It is counted instead, and never judged.
        let store = store();
        let monitor = monitor(&store);
        store
            .exec(
                "INSERT INTO articles(id,url,title,summary,content,published,fetched,relevant) \
                 VALUES(9,'https://example.com/9','A headline only','','',\
                 '2026-11-05T10:00:00Z','2026-11-05T10:00:00Z',1)",
                &[],
            )
            .expect("insert");
        let selected =
            new_since(&store, &monitor, "2026-11-05T00:00:00Z", &[], 25, 8).expect("select");
        assert!(selected.articles.is_empty());
        assert_eq!(selected.skipped_unread, 1);
        assert_eq!(selected.considered, 1);
    }

    #[test]
    fn the_scan_widens_when_the_newest_rows_were_all_judged() {
        // Forty articles in the window; the thirty newest were judged on an
        // earlier check. One page of twenty-five used to hold nothing but those,
        // so the older ten were never looked at — and the window moved past them.
        let store = store();
        let monitor = monitor(&store);
        for id in 1..=40 {
            let fetched = format!("2026-11-05T{:02}:00:00Z", 59 - id);
            insert(&store, id, &fetched);
        }
        for id in 1..=30 {
            store
                .monitors()
                .mark_seen(monitor.id, &format!("a{}", id))
                .expect("mark seen");
        }
        let selected =
            new_since(&store, &monitor, "2026-11-05T00:00:00Z", &[], 25, 5).expect("select");
        let ids: Vec<i64> = selected.articles.iter().map(|article| article.id).collect();
        assert_eq!(
            ids,
            vec![31, 32, 33, 34, 35],
            "the unjudged rows of the window, newest first"
        );
        assert_eq!(selected.skipped_seen, 30, "each judged row counted once");
        assert_eq!(selected.considered, 40);
    }

    /// Measured, not claimed: the widening scan over a busy window, timed.
    /// `cargo test --offline -p atlas-monitor -- --nocapture` prints the number.
    #[test]
    fn the_widened_scan_is_measured() {
        let store = store();
        let monitor = monitor(&store);
        for id in 1..=500 {
            let fetched = format!(
                "2026-11-05T{:02}:{:02}:00Z",
                23 - (id / 60) as i64,
                59 - (id % 60)
            );
            insert(&store, id, &fetched);
        }
        for id in 1..=490 {
            store
                .monitors()
                .mark_seen(monitor.id, &format!("a{}", id))
                .expect("mark seen");
        }
        let started = std::time::Instant::now();
        let selected =
            new_since(&store, &monitor, "2026-11-05T00:00:00Z", &[], 25, 8).expect("select");
        let elapsed = started.elapsed();
        println!(
            "atlas-monitor: widened scan, {} rows considered, {} candidates, 10 unjudged: {:?}",
            selected.considered,
            selected.articles.len(),
            elapsed
        );
        assert_eq!(selected.articles.len(), 8, "the budget, filled from the wide scan");
        assert!(
            elapsed.as_millis() < 500,
            "filling a check from a 500-row window must stay well inside a model call: {:?}",
            elapsed
        );
    }
}
