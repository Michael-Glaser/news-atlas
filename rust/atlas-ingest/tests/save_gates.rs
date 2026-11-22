//! `save_article`'s gates, driven end to end through `Pipeline::save_article`.
//!
//! `docs/THREAT-MODEL.md`'s parity pass found this area with no test of its own:
//! five different refusals stand between a model's JSON and a row in the archive,
//! and nothing pinned them together. Each test here is one refusal and the
//! sentence the tool hands back — or, for the last one, the write:
//!
//! | gate | what is refused |
//! | --- | --- |
//! | 1. URL | a loopback address, a `file:` URL |
//! | 7. topic | an article that matches no active topic |
//! | 8. freshness | an article older than the window |
//! | 6. headline | the same story, same publisher, already stored |
//! | 10. write | nothing — a legitimate article is stored |
//!
//! What it costs and what it touches: one temporary directory under
//! `std::env::temp_dir()`, one `Store` inside it, and no network at all. The rules
//! are [`NativeRules`] — the engines as libraries, in this process — so the real
//! `atlas-dates` window and the real `atlas-topics` match decide the two gates that
//! ask them, and no engine binary has to be built (`Engines` answers every question
//! in-process; `SPAWNED` is empty, so `discover` checks no file). The model is
//! `OfflineModel`, whose verdict is "unchecked, accepted" — the gate under test is
//! not the model's opinion. The transport is a stub that panics if it is ever
//! asked: `save_article` reads no page, and a test that tried would be a bug.
//!
//! The refusal sentences are asserted, not just the status: a refusal a model
//! cannot act on ("error: refused") is the failure mode this file exists to stop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;

use atlas_ingest::fetcher::{Fetched, Fetcher};
use atlas_ingest::model::OfflineModel;
use atlas_ingest::rules::NativeRules;
use atlas_ingest::save::SaveInput;
use atlas_ingest::{Config, NullBrowser, Pipeline};
use atlas_store::{SqlValue, Store};

/// A `Fetcher` that is never called: `save_article` works on text the model
/// already read. Panicking (rather than answering empty) keeps a future gate that
/// starts fetching from quietly reaching the network inside a test.
struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, url: &str, _max_bytes: usize) -> Fetched {
        panic!("save_article must not fetch ({})", url);
    }
}

/// One temp archive, one pipeline, removed on drop.
struct Fixture {
    pipeline: Pipeline,
    store: Arc<Store>,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A directory no other test uses: the process id plus a counter, because two
/// fixtures alive at once must not share a database.
fn temp_root(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-ingest-save-gates-{}-{}-{}",
        label,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    root
}

fn fixture(label: &str) -> Fixture {
    let root = temp_root(label);
    let store = Arc::new(Store::open(root.join("data").join("atlas.db")).expect("open"));
    store.init().expect("schema");
    store.init_monitors().expect("monitor tables, as the app makes them");
    let config = Config::for_root(&root);
    let rules = NativeRules::discover(&config).expect("the engines answer in this process");
    let pipeline = Pipeline::new(
        store.clone(),
        config,
        Arc::new(rules),
        Arc::new(OfflineModel::new("save_gates: no model in this test")),
        Arc::new(NoFetch),
        Arc::new(NullBrowser),
        None,
    );
    Fixture {
        pipeline,
        store,
        root,
    }
}

impl Fixture {
    /// One active topic, the way `add_topic` writes it.
    fn add_topic(&self, name: &str, keywords: &str) {
        self.store
            .exec(
                "INSERT INTO topics(name,keywords,exclude,min_matches,active,created,hits) \
                 VALUES(?,?,'',1,1,?,0)",
                &[
                    SqlValue::Text(name.to_string()),
                    SqlValue::Text(keywords.to_string()),
                    SqlValue::Text(atlas_store::now_iso()),
                ],
            )
            .expect("topic");
    }

    fn articles(&self) -> i64 {
        self.store
            .query_one("SELECT COUNT(*) c FROM articles", &[])
            .expect("count")
            .and_then(|row| row.int("c"))
            .unwrap_or(-1)
    }
}

/// A body long enough to pass `MIN_CHARS` (700) and to read as prose: a wall or a
/// navigation dump is refused earlier, by a different gate.
fn body(sentence: &str) -> String {
    let mut text = String::new();
    for _ in 0..12 {
        text.push_str(sentence);
        text.push(' ');
    }
    text
}

/// The article every test starts from: a current story about chips, filed by a
/// publisher the URL names.
fn article(url: &str, published: &str) -> SaveInput {
    SaveInput {
        url: url.to_string(),
        title: "Chip exports rise sharply".to_string(),
        text: body("Chip exports rose sharply this quarter, the ministry said."),
        summary: "Exports rose. The ministry said chip shipments hit a record.".to_string(),
        tags: Vec::new(),
        categories: Vec::new(),
        topic: String::new(),
        published: published.to_string(),
        publisher: String::new(),
        place: String::new(),
        entities: Vec::new(),
    }
}

/// The sentence a refusal carries, whichever key it arrives in.
///
/// `note` before `reason`: a duplicate answers `reason: "title"` (a token, not a
/// sentence) beside a `note` that names the article it duplicates.
fn sentence(result: &atlas_ingest::SaveResult) -> String {
    let value: Value = result.to_json();
    for key in ["error", "note", "reason"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    panic!("a refusal with no sentence: {}", value);
}

#[test]
fn an_article_older_than_the_window_is_refused() {
    let fixture = fixture("stale");
    // The article has to match a topic to *reach* the freshness gate: the gates
    // run in the order the module documents, and a cheap refusal comes first.
    fixture.add_topic("Chips", "chip, export");
    let result = fixture
        .pipeline
        .save_article(&article("https://example.com/story/old", "2020-01-01T00:00:00Z"));
    assert_eq!(result.status(), "error", "{:?}", result.to_json());
    let message = sentence(&result);
    assert!(message.contains("not current reporting"), "{}", message);
    assert!(message.contains("older than the"), "{}", message);
    assert!(message.contains("2020-01-01"), "the date it read: {}", message);
    assert_eq!(fixture.articles(), 0, "a refused article leaves no row");
}

#[test]
fn an_article_that_matches_no_topic_is_refused() {
    let fixture = fixture("no-topic");
    // The archive has a topic; the article is not about it. ATLAS collects what
    // the reader asked for, so "no match" is a refusal and not a fallback.
    fixture.add_topic("Beef", "beef, cattle");
    let result = fixture
        .pipeline
        .save_article(&article("https://example.com/story/chips", &atlas_store::now_iso()));
    assert_eq!(result.status(), "error", "{:?}", result.to_json());
    let message = sentence(&result);
    assert!(message.contains("matches no active topic"), "{}", message);
    assert!(
        message.contains("Beef"),
        "the refusal names the topics there are: {}",
        message
    );
    assert_eq!(fixture.articles(), 0, "a refused article leaves no row");
}

#[test]
fn a_loopback_or_file_url_is_refused() {
    let fixture = fixture("url");
    fixture.add_topic("Chips", "chip, export");
    for url in [
        // The app's own port: a model that read a status page must not store it.
        "http://127.0.0.1:8787/api/status",
        // A local file is not a page on a public site.
        "file:///etc/passwd",
        // And the local name of this machine resolves to a loopback address.
        "http://localhost:8787/api/articles",
    ] {
        let result = fixture.pipeline.save_article(&article(url, &atlas_store::now_iso()));
        assert_eq!(result.status(), "error", "{}: {:?}", url, result.to_json());
        let message = sentence(&result);
        assert!(
            message.contains("url refused"),
            "{}: the refusal must say the URL was refused: {}",
            url,
            message
        );
        assert!(
            message.contains("public site"),
            "{}: and what to pass instead: {}",
            url,
            message
        );
    }
    assert_eq!(fixture.articles(), 0, "no refused URL left a row");
}

#[test]
fn a_duplicate_title_is_refused() {
    let fixture = fixture("duplicate");
    fixture.add_topic("Chips", "chip, export");
    let first = fixture
        .pipeline
        .save_article(&article("https://example.com/story/one", &atlas_store::now_iso()));
    assert_eq!(first.status(), "stored", "{:?}", first.to_json());
    let stored = first.id().expect("the first save has an id");
    assert_eq!(fixture.articles(), 1);

    // The same story, a different URL — a second outlet path, a redirect, a model
    // that re-worded the link. The headline (same publisher, inside the window) is
    // what says it is the same story.
    let again = fixture
        .pipeline
        .save_article(&article("https://example.com/story/two", &atlas_store::now_iso()));
    assert_eq!(again.status(), "duplicate", "{:?}", again.to_json());
    let value: Value = again.to_json();
    assert_eq!(value["reason"], serde_json::json!("title"), "{}", value);
    assert_eq!(value["id"], serde_json::json!(stored), "{}", value);
    let message = sentence(&again);
    assert!(
        message.contains(&format!("#{}", stored)),
        "the duplicate names the article it duplicates: {}",
        message
    );
    assert!(
        message.contains("tag_article"),
        "and what to do instead: {}",
        message
    );
    assert_eq!(fixture.articles(), 1, "a duplicate writes nothing");
}

#[test]
fn a_legitimate_article_is_stored() {
    let fixture = fixture("stored");
    fixture.add_topic("Chips", "chip, export");
    let result = fixture
        .pipeline
        .save_article(&article("https://example.com/story/kept", &atlas_store::now_iso()));
    assert_eq!(result.status(), "stored", "{:?}", result.to_json());
    let id = result.id().expect("a stored article has an id");
    assert_eq!(fixture.articles(), 1);

    // The row is what was asked for: the headline, the body, the topic, and the
    // search index that makes the article findable afterwards.
    let row = fixture
        .store
        .query_one("SELECT title,relevant,dupe FROM articles WHERE id=?", &[SqlValue::Integer(id)])
        .expect("read")
        .expect("the row exists");
    assert_eq!(row.text("title"), Some("Chip exports rise sharply"));
    assert_eq!(row.int("relevant"), Some(1));
    assert_eq!(row.int("dupe"), Some(0));
    let topics = fixture
        .store
        .query(
            "SELECT t.name FROM article_topics at JOIN topics t ON t.id=at.topic_id \
             WHERE at.article_id=?",
            &[SqlValue::Integer(id)],
        )
        .expect("topics");
    assert_eq!(
        topics.iter().filter_map(|row| row.text("name")).collect::<Vec<&str>>(),
        vec!["Chips"],
        "the article carries the topic that let it in"
    );
    let indexed = fixture
        .store
        .query("SELECT COUNT(*) c FROM article_fts_docsize", &[])
        .expect("index")[0]
        .int("c");
    assert_eq!(indexed, Some(1), "a stored article is searchable");
}
