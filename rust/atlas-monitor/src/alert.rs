//! `alert` — one decision: may this article alert on this monitor, and what does
//! the alert say?
//!
//! Decisions: that an alert is only raised for an article the reader can open
//! ([`reader_can_open`]), that an alert is unique per monitor and article (the
//! store checks and the unique index backs it up), what an alert carries ("why it
//! fired": the topic, the score, the terms the model chose), and that the entities
//! an alert names are attached to the article's graph entry when they already
//! exist.
//!
//! It does not: decide the threshold ([`Verdict::fired`]), judge the article
//! ([`crate::judge`]), or write a check-log row. It is called *after* a verdict
//! crossed, never before.
//!
//! Nothing an alert stores comes straight from a page: the title and summary are
//! cleaned as untrusted text and capped, and the model's reason goes through the
//! same path. The url is the one field kept as it was stored on the article — see
//! [`reader_can_open`] for why it is checked instead of rewritten.

use atlas_store::monitors::NewAlert;
use atlas_store::{SqlValue, Store};

use crate::candidates::Article;
use crate::error::{Error, Result};
use crate::spec::Monitor;
use crate::verdict::{clean_field, Verdict, MAX_SUMMARY_CHARS, MAX_TITLE_CHARS};

/// Why an alert fired: the topic it belongs to, the score, the threshold that was
/// crossed, and the words behind the score.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Why {
    /// The monitor's name (or its query, when it has no name).
    pub topic: String,
    pub score: i64,
    pub threshold: i64,
    /// The terms the model chose — the user's query terms in the fallback path.
    pub terms: Vec<String>,
    /// Who scored: `model` or `query-terms`.
    pub scorer: &'static str,
}

impl Why {
    pub fn of(monitor: &Monitor, verdict: &Verdict) -> Why {
        Why {
            topic: monitor.label().to_string(),
            score: verdict.score,
            threshold: monitor.threshold,
            terms: verdict.terms.clone(),
            scorer: verdict.scorer.as_str(),
        }
    }

    /// One line for the reader: `Chips · 8/10 (threshold 6) · export, licence`.
    pub fn line(&self) -> String {
        let mut out = format!(
            "{} · {}/10 (threshold {})",
            self.topic, self.score, self.threshold
        );
        if !self.terms.is_empty() {
            out.push_str(" · ");
            out.push_str(&self.terms.join(", "));
        }
        if self.scorer == "query-terms" {
            out.push_str(" · scored without the model");
        }
        clean_field(&out, 300)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "topic": self.topic,
            "score": self.score,
            "threshold": self.threshold,
            "terms": self.terms,
            "scorer": self.scorer,
            "line": self.line(),
        })
    }
}

/// One alert, as it was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub id: i64,
    pub monitor_id: i64,
    pub kind: String,
    pub article_id: Option<i64>,
    pub url: String,
    pub title: String,
    pub summary: String,
    pub entities: Vec<String>,
    pub seen: bool,
    pub created: String,
    pub why: Why,
}

impl Alert {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "monitor_id": self.monitor_id,
            "kind": self.kind,
            "article_id": self.article_id,
            "url": self.url,
            "title": self.title,
            "summary": self.summary,
            "entities": self.entities,
            "seen": self.seen,
            "created": self.created,
            "why": self.why.to_json(),
        })
    }
}

/// Raise one alert for one article. `None` means the monitor already had one —
/// the duplicate suppression, which is a refusal and not a failure.
///
/// An article the reader cannot open is an error, not a `None`: the check reports
/// it (status `partial`, the sentence in `monitor_runs.note`) instead of counting
/// it as a story that already alerted.
pub fn raise(
    store: &Store,
    monitor: &Monitor,
    article: &Article,
    verdict: &Verdict,
    created: &str,
) -> Result<Option<Alert>> {
    reader_can_open(store, article)?;
    let why = Why::of(monitor, verdict);
    let entities_json = serde_json::to_string(&verdict.entities)
        .map_err(|error| Error::new(format!("entities did not encode: {}", error)))?;
    let terms_json = serde_json::to_string(&why.terms)
        .map_err(|error| Error::new(format!("terms did not encode: {}", error)))?;
    // Bound before the row is built: the store borrows these, so they have to
    // outlive the call.
    let title = clean_field(&article.title, MAX_TITLE_CHARS);
    let summary = clean_field(
        if verdict.summary.is_empty() {
            &article.summary
        } else {
            &verdict.summary
        },
        MAX_SUMMARY_CHARS,
    );
    let analysis = verdict.why_text(monitor);
    let row = NewAlert {
        monitor_id: monitor.id,
        kind: "news",
        article_id: Some(article.id),
        url: &article.url,
        title: &title,
        relevance: verdict.score,
        summary: &summary,
        analysis: &analysis,
        entities_json: &entities_json,
        terms_json: &terms_json,
        created,
    };
    let Some(id) = store.monitors().add_alert(&row)? else {
        return Ok(None);
    };
    // The alert keeps the model's entities in its own column; the graph only ever
    // gets names it already knows.
    let _ = store.monitors().attach_entities(article.id, &verdict.entities);
    Ok(Some(Alert {
        id,
        monitor_id: monitor.id,
        kind: "news".to_string(),
        article_id: Some(article.id),
        url: article.url.clone(),
        title,
        summary,
        entities: verdict.entities.clone(),
        seen: false,
        created: created.to_string(),
        why,
    }))
}

/// Can the reader open what this alert would point at?
///
/// An alert row is a button. The reader's two ways in are in
/// `atlas-ui/src/views/monitors.rs`: the stored article by `data-art`, which is
/// `GET /api/articles/{id}` — `SELECT * FROM articles WHERE id=?` in
/// `atlas-web/src/routes/articles.rs`, and needs no network at all — and, only when
/// there is no article id, `window.open()` on the alert's own url. So an alert with
/// no row behind it *and* a url this app refuses to open is a click that loses:
/// `<javascript:…>`, `file:`, `data:`, a loopback address, or an empty string,
/// none of which `atlas_guard::urlsafe::check` accepts. That is the case refused
/// here, with the gate's own reason in the sentence the check log keeps.
///
/// The url is checked, never rewritten and never blanked. It is the article's own
/// stored url, and `alerts(monitor_id,url)` is the unique index that keeps one alert
/// per story: two unrelated articles with an unopenable url would collide on `''`
/// and the second alert would be dropped as a duplicate. A stored article with an
/// odd url keeps that url and still alerts, because the row is what the reader opens.
fn reader_can_open(store: &Store, article: &Article) -> Result<()> {
    let stored = store
        .query_one(
            "SELECT 1 AS found FROM articles WHERE id=?",
            &[SqlValue::Integer(article.id)],
        )
        .map_err(|error| Error::new(format!("the archive did not answer: {}", error)))?;
    if stored.is_some() {
        return Ok(());
    }
    // The token, not the bare answer: `Gated::verify` is the same
    // `urlsafe::check(url, false)` this used to call, but it is the only way a
    // caller outside `atlas-guard` may ask, and it hands back something that
    // cannot be a string on the way to a request (`atlas-guard/tests/audit_gate.rs
    // ::the_raw_gate_has_one_caller` is the rule, and it was red on this line).
    match atlas_guard::Gated::verify(&article.url) {
        Ok(_) => Ok(()),
        Err(reason) => Err(Error::new(format!(
            "no alert: article {} is not in the archive and its url is not one this app \
             opens ({})",
            article.id, reason
        ))),
    }
}

/// One `alerts` row as JSON for a reader that can still check the archive:
/// [`row_json`], minus a way in that no longer exists.
///
/// An alert outlives its article. `atlas-store`'s retention
/// (`prune_old_detailed` over `ARTICLE_REFS`) deletes the article row and every
/// table that points at it, and `alerts` is deliberately not one of them: the alert
/// is the record that something fired, and the story's title, summary and url are
/// still worth reading. What the row must not do is keep advertising an article id
/// that the reader's `GET /api/articles/{id}` answers with nothing — the row's click
/// is `data-art` first and its url only when there is no id, so a dangling id is a
/// button that loses while a perfectly good url sits next to it.
///
/// So the id is dropped (with `article_gone` saying why) and the url takes over —
/// unless the gate refuses that url, which is possible for a row written before
/// [`reader_can_open`] existed. Then the url goes too: the alert stays readable, and
/// it stops offering a `javascript:`/`file:`/loopback link.
pub fn row_json_for_reader(store: &Store, row: &atlas_store::Row) -> serde_json::Value {
    let mut out = row_json(row);
    let Some(article_id) = row.int("article_id") else {
        return out;
    };
    let stored = store
        .query_one(
            "SELECT 1 AS found FROM articles WHERE id=?",
            &[SqlValue::Integer(article_id)],
        )
        .ok()
        .flatten();
    if stored.is_some() {
        return out;
    }
    let serde_json::Value::Object(map) = &mut out else {
        return out;
    };
    map.insert("article_id".to_string(), serde_json::Value::Null);
    map.insert("article_gone".to_string(), serde_json::Value::Bool(true));
    let url = map
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    // The token, not the bare answer (see `reader_can_open`): `Gated::verify` is
    // the one public door to `urlsafe::check(url, false)`.
    if atlas_guard::Gated::verify(&url).is_err() {
        map.insert("url".to_string(), serde_json::Value::String(String::new()));
        map.insert("url_not_openable".to_string(), serde_json::Value::Bool(true));
    }
    out
}

/// One `alerts` row as JSON, with its entity and term lists decoded and the stored
/// `analysis` also offered as `why`.
///
/// The row comes from `atlas_store::monitors::recent_alerts`; the JSON columns are
/// text there, and a reader wants lists.
pub fn row_json(row: &atlas_store::Row) -> serde_json::Value {
    let mut out = row.as_json();
    if let serde_json::Value::Object(map) = &mut out {
        for key in ["entities", "terms"] {
            if let Some(serde_json::Value::String(raw)) = map.get(key) {
                let decoded = serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!([]));
                map.insert(key.to_string(), decoded);
            }
        }
        if let Some(serde_json::Value::String(analysis)) = map.get("analysis") {
            map.insert(
                "why".to_string(),
                serde_json::Value::String(analysis.clone()),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::Scorer;

    fn store() -> Store {
        let store = Store::open_memory().expect("memory store");
        store.init().expect("schema");
        store.init_monitors().expect("monitor schema");
        store
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
            ..Monitor::default()
        }
    }

    fn article() -> Article {
        Article {
            id: 4,
            title: "New chip export rules".to_string(),
            summary: "the ministry said".to_string(),
            content: String::new(),
            url: "https://example.com/a".to_string(),
            published: "2026-11-05T09:00:00Z".to_string(),
            fetched: "2026-11-05T09:01:00Z".to_string(),
            publisher: String::new(),
        }
    }

    /// The articles row an alert is allowed to point at. Before this pass the
    /// tests raised alerts for id 4 without one, which is exactly the row the
    /// reader cannot open — see `an_alert_whose_article_the_reader_cannot_open_is_not_raised`.
    fn article_row(store: &Store, id: i64, url: &str) {
        store
            .exec(
                "INSERT INTO articles(id,url,title,summary,content,published,fetched,relevant) \
                 VALUES(?,?,?,?,?,?,?,1)",
                &[
                    atlas_store::SqlValue::Integer(id),
                    atlas_store::SqlValue::Text(url.to_string()),
                    atlas_store::SqlValue::Text("New chip export rules".to_string()),
                    atlas_store::SqlValue::Text("the ministry said".to_string()),
                    atlas_store::SqlValue::Text("the body of the story".to_string()),
                    atlas_store::SqlValue::Text("2026-11-05T09:00:00Z".to_string()),
                    atlas_store::SqlValue::Text("2026-11-05T09:01:00Z".to_string()),
                ],
            )
            .expect("insert article");
    }

    #[test]
    fn an_alert_is_written_once_and_carries_why_it_fired() {
        let store = store();
        article_row(&store, 4, "https://example.com/a");
        let monitor = monitor(&store);
        let verdict = Verdict::new(
            Scorer::Model,
            8,
            vec!["export".to_string()],
            "new rule",
            "",
            vec!["Ministry".to_string()],
        );
        let first = raise(&store, &monitor, &article(), &verdict, "2026-11-05T10:00:00Z")
            .expect("raise")
            .expect("written");
        assert_eq!(first.why.score, 8);
        assert_eq!(first.why.terms, vec!["export".to_string()]);
        assert!(first.why.line().contains("Chips · 8/10 (threshold 6)"));
        let again = raise(&store, &monitor, &article(), &verdict, "2026-11-05T10:05:00Z")
            .expect("raise again");
        assert!(again.is_none());
        assert_eq!(store.monitors().unseen_count().expect("count"), 1);
    }

    #[test]
    fn an_untitled_article_still_produces_a_usable_alert() {
        let store = store();
        article_row(&store, 4, "https://example.com/a");
        let monitor = monitor(&store);
        let mut ugly = article();
        ugly.title = "  \u{7}line\nbreak  ".to_string();
        let alert = raise(
            &store,
            &monitor,
            &ugly,
            &Verdict::new(Scorer::QueryTerms, 7, vec![], "", "", vec![]),
            "2026-11-05T10:00:00Z",
        )
        .expect("raise")
        .expect("written");
        assert_eq!(alert.title, "line break");
        assert!(alert.why.line().contains("scored without the model"));
    }

    // -- an alert is something the reader can open ---------------------------

    #[test]
    fn an_alert_whose_article_the_reader_cannot_open_is_not_raised() {
        // The reader's two ways in (atlas-ui/src/views/monitors.rs on an alert row):
        // the stored article by `data-art`, and — only when there is no article id —
        // `window.open()` on the alert's own url. An alert with neither is a row the
        // reader clicks and loses, and its url is the one field this crate put in
        // front of `window.open` without asking whether the app would open it.
        let store = store();
        let monitor = monitor(&store);
        let verdict = Verdict::new(Scorer::Model, 9, vec![], "it is the story", "", vec![]);
        for url in [
            "javascript:alert(document.domain)",
            "file:///etc/passwd",
            "data:text/html,<script>x()</script>",
            "http://127.0.0.1:8787/api/alerts",
            "",
        ] {
            let article = Article {
                id: 77,
                url: url.to_string(),
                ..article()
            };
            let raised = raise(&store, &monitor, &article, &verdict, "2026-11-05T10:00:00Z");
            match &raised {
                Err(error) => {
                    assert!(
                        error.message().contains("not one this app opens"),
                        "the refusal says why: {}",
                        error.message()
                    );
                    // The sentence reaches `monitor_runs.note`, so it says what went
                    // wrong and quotes nothing the page wrote back.
                    if !url.is_empty() {
                        assert!(!error.message().contains(url), "{}", error.message());
                    }
                }
                Ok(other) => panic!(
                    "{:?} became an alert the reader cannot open: {}",
                    url,
                    other.is_some()
                ),
            }
        }
        assert_eq!(
            store.monitors().unseen_count().expect("count"),
            0,
            "no row was written"
        );

        // The same article, stored: the reader opens the archive copy, which needs no
        // network at all. Refusing this one would remove a feature, not add a check.
        article_row(&store, 77, "javascript:alert(document.domain)");
        let alert = raise(&store, &monitor, &article_with_id(77), &verdict, "2026-11-05T10:00:00Z")
            .expect("raise")
            .expect("written");
        assert_eq!(alert.article_id, Some(77));
    }

    #[test]
    fn an_article_that_is_not_stored_alerts_only_through_a_url_the_app_opens() {
        // The other half of the same rule: no row, but a url the gate accepts, so the
        // reader's "Open source" works.
        let store = store();
        let monitor = monitor(&store);
        let verdict = Verdict::new(Scorer::Model, 9, vec![], "", "", vec![]);
        let article = Article {
            id: 88,
            url: "https://example.com/88".to_string(),
            ..article()
        };
        let alert = raise(&store, &monitor, &article, &verdict, "2026-11-05T10:00:00Z")
            .expect("raise")
            .expect("written");
        assert_eq!(alert.url, "https://example.com/88");
        assert_eq!(store.monitors().unseen_count().expect("count"), 1);
    }

    /// The fixture at another id — the url and text stay the same.
    fn article_with_id(id: i64) -> Article {
        Article {
            id,
            ..article()
        }
    }

    // -- a hostile page cannot break the alert's JSON -------------------------

    #[test]
    fn a_hostile_title_or_reason_stays_one_json_value() {
        // Everything on an alert row came from a page, a model, or both. The row is
        // read by the wasm frontend as `serde_json`, so the one thing it may never do
        // is stop being JSON: a quote, a backslash, a newline or a `</script>` in the
        // title must arrive as a string, and the row must keep exactly its own keys.
        let store = store();
        article_row(&store, 4, "https://example.com/a");
        let monitor = monitor(&store);
        let mut ugly = article();
        ugly.title = "quote \" backslash \\ newline\nhere </script><script>x()</script> \u{2028}sep"
            .to_string();
        let verdict = Verdict::new(
            Scorer::Model,
            9,
            vec!["ex\"port".to_string(), "line\u{2028}sep".to_string()],
            "why \" quoted\nand \\ escaped",
            "summary \" with \u{7}control",
            vec!["A\"B".to_string()],
        );
        let alert = raise(&store, &monitor, &ugly, &verdict, "2026-11-05T10:00:00Z")
            .expect("raise")
            .expect("written");
        let json = alert.to_json();
        let encoded = serde_json::to_string(&json).expect("the alert encodes");
        let decoded: serde_json::Value = serde_json::from_str(&encoded).expect("and decodes");
        assert_eq!(decoded, json, "a round trip is lossless: {encoded}");
        assert_eq!(
            decoded.as_object().map(|map| map.len()),
            json.as_object().map(|map| map.len()),
            "no value added a key of its own: {encoded}"
        );
        assert_eq!(decoded["title"], serde_json::json!(alert.title));
        assert_eq!(decoded["why"]["terms"], serde_json::json!(verdict.terms));
        assert_eq!(decoded["entities"], serde_json::json!(verdict.entities));

        // The row the reader is handed takes the same trip: the two JSON columns are
        // arrays of strings, not text that has to be re-parsed by the page.
        let rows = store
            .monitors()
            .recent_alerts("2026-11-05T00:00:00Z", None, 5)
            .expect("alerts");
        assert_eq!(rows.len(), 1);
        let row = row_json(&rows[0]);
        let encoded = serde_json::to_string(&row).expect("the row encodes");
        let decoded: serde_json::Value = serde_json::from_str(&encoded).expect("and decodes");
        assert!(decoded["entities"].is_array(), "{encoded}");
        assert!(decoded["terms"].is_array(), "{encoded}");
        assert_eq!(decoded["entities"][0], serde_json::json!("A\"B"));
        assert_eq!(decoded["terms"][0], serde_json::json!("ex\"port"));
        assert!(decoded["analysis"].is_string(), "{encoded}");
        assert_eq!(
            decoded["why"],
            decoded["analysis"],
            "the reader's `why` is the stored analysis, one string"
        );
        // Nothing in the stored analysis closed the string it lives in.
        assert!(!decoded["analysis"]
            .as_str()
            .unwrap_or_default()
            .contains('\n'));
    }

    // -- an alert that outlived its article --------------------------------

    #[test]
    fn a_pruned_article_is_no_longer_advertised_by_its_alert() {
        // Retention deletes the article row and every table that points at it
        // (`atlas-store::prune`, `ARTICLE_REFS`) — `alerts` is not one of them, by
        // design: the alert is the record that something fired. What must not happen
        // is the row still naming an article id that `GET /api/articles/{id}` answers
        // 404 for, because that is the click the reader makes first.
        let store = store();
        article_row(&store, 4, "https://example.com/a");
        let monitor = monitor(&store);
        let verdict = Verdict::new(Scorer::Model, 9, vec![], "it is the story", "", vec![]);
        raise(&store, &monitor, &article(), &verdict, "2026-11-05T10:00:00Z")
            .expect("raise")
            .expect("written");
        store
            .exec("DELETE FROM articles WHERE id=4", &[])
            .expect("prune the article");

        let rows = store
            .monitors()
            .recent_alerts("2026-11-05T00:00:00Z", None, 5)
            .expect("alerts");
        assert_eq!(rows.len(), 1, "the alert is not pruned with the story");
        let row = row_json_for_reader(&store, &rows[0]);
        assert_eq!(row["article_id"], serde_json::Value::Null);
        assert_eq!(row["article_gone"], serde_json::json!(true));
        assert_eq!(
            row["url"], "https://example.com/a",
            "the way in that still works is the one that is offered"
        );
        assert!(row["url_not_openable"].is_null());
        // And the alert's own copy of the story is still readable.
        assert_eq!(row["title"], "New chip export rules");
    }

    #[test]
    fn an_alert_whose_url_the_app_would_not_open_offers_no_url_at_all() {
        // A row written before `reader_can_open` existed, or by another writer, can
        // carry a url the gate refuses. Once its article is gone there is nothing
        // left to click, so the url is dropped instead of being handed to
        // `window.open`.
        let store = store();
        article_row(&store, 4, "javascript:alert(document.domain)");
        let monitor = monitor(&store);
        let mut odd = article();
        odd.url = "javascript:alert(document.domain)".to_string();
        raise(
            &store,
            &monitor,
            &odd,
            &Verdict::new(Scorer::Model, 9, vec![], "", "", vec![]),
            "2026-11-05T10:00:00Z",
        )
        .expect("the stored article is what the reader opens")
        .expect("written");
        store
            .exec("DELETE FROM articles WHERE id=4", &[])
            .expect("prune the article");

        let rows = store
            .monitors()
            .recent_alerts("2026-11-05T00:00:00Z", None, 5)
            .expect("alerts");
        let row = row_json_for_reader(&store, &rows[0]);
        assert_eq!(row["url"], "");
        assert_eq!(row["url_not_openable"], serde_json::json!(true));
        assert_eq!(row["article_id"], serde_json::Value::Null);
    }
}
