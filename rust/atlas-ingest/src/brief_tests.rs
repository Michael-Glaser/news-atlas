//! The briefing, proved without a model.
//!
//! What is real here: the archive (an in-memory `atlas-store` with the shipped schema and
//! rows this file seeds), the SQL every scope reads, the digest builder, the digest's caps,
//! the watermark and the row that lands in `briefings`. What is scripted: the model, because
//! the real one would need 8 GB of weights and a minute of thinking for a test that is about
//! what this crate does with the answer.
//!
//! Five promises, one per group below:
//!
//! * the digest names **only** the asked scope's stories;
//! * an offline model is a refusal with a sentence, not an empty briefing;
//! * an unknown scope is refused **by name**;
//! * the stored `watermark` is the newest article id **before** the call;
//! * sixty stories become forty lines and at most 24 000 characters.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use atlas_store::{SqlValue, Store};

use crate::brief::{brief_digest, BRIEF_CHARS, BRIEF_STORIES};
use crate::model::Model;
use crate::{Clock, Config, Pipeline};

/// A fixed instant, so `created` and the cutoff are facts instead of "now".
const NOW: i64 = 1_793_872_800;

/// The clock a briefing reads: one instant, so `iso_ago(72)` is arithmetic.
struct Fixed(i64);

impl Clock for Fixed {
    fn now_secs(&self) -> i64 {
        self.0
    }
}

/// A fetcher and a browser that are never reached: a briefing reads the archive.
struct Unused;

impl crate::Fetcher for Unused {
    fn fetch(&self, url: &str, _max_bytes: usize) -> crate::Fetched {
        crate::Fetched {
            final_url: url.to_string(),
            error: "a briefing reads the archive, it never opens a page".to_string(),
            ..crate::Fetched::default()
        }
    }
}

impl crate::BrowserReader for Unused {
    fn read(&self, _url: &str, _max_chars: usize) -> crate::BrowserPage {
        crate::BrowserPage {
            error: "a briefing reads the archive, it never opens a page".to_string(),
            ..crate::BrowserPage::default()
        }
    }
}

/// The model, scripted: what it answers, whether it is there, and what it was asked.
struct Scripted {
    online: bool,
    /// One answer per call, in order; the last one repeats.
    answers: Mutex<Vec<String>>,
    asked: Mutex<Vec<String>>,
    calls: AtomicUsize,
    id: String,
}

impl Scripted {
    fn answering(answers: &[&str]) -> Arc<Scripted> {
        Arc::new(Scripted {
            online: true,
            answers: Mutex::new(answers.iter().map(|text| text.to_string()).collect()),
            asked: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            id: "test-model".to_string(),
        })
    }

    /// The model with the JSON object a briefing is, ready to be stored.
    fn briefing() -> Arc<Scripted> {
        Scripted::answering(&[
            r#"{"line":"Chips are the story here.","bullets":["Exports fell 4% [Article #1]","Talks resumed [Article #2]"]}"#,
        ])
    }

    fn offline() -> Arc<Scripted> {
        Arc::new(Scripted {
            online: false,
            answers: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            id: "test-model".to_string(),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Model for Scripted {
    fn model_id(&self) -> String {
        self.id.clone()
    }

    fn online(&self) -> bool {
        self.online
    }

    fn verdict(
        &self,
        _title: &str,
        _text: &str,
        _url: &str,
        _publisher: &str,
    ) -> atlas_ai::prompts::NewsVerdict {
        atlas_ai::prompts::NewsVerdict::unchecked("a briefing does not ask the news gate")
    }

    /// The seam this test measures: the default [`Model::brief`] goes through here, so a
    /// scripted `chat` exercises the real path from the budget to the parsed answer.
    fn chat(&self, prompt: &str, _temperature: f32, _max_tokens: u32) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(prompt.to_string());
        let answers = self.answers.lock().unwrap();
        if answers.is_empty() {
            return Err("the scripted model has no answer left".to_string());
        }
        let index = self.calls.load(Ordering::SeqCst).saturating_sub(1);
        Ok(answers[index.min(answers.len() - 1)].clone())
    }

    fn json(&self, _prompt: &str, _max_tokens: u32) -> Option<Value> {
        None
    }
}

/// A pipeline over an archive this test owns, with the `briefings` table in it.
struct Bench {
    pipeline: Pipeline,
    store: Arc<Store>,
    model: Arc<Scripted>,
}

fn bench(model: Arc<Scripted>) -> Bench {
    let store = Arc::new(Store::open_memory().expect("an in-memory archive"));
    // `init` is also what creates `briefings` (the store's own shelf), so this bench reads a
    // briefing back through the same call the window will make — no table of this file's own.
    store.init().expect("the archive schema");
    store.init_monitors().expect("the monitor tables");
    let mut config = Config::for_root(std::env::temp_dir());
    config.search_engine = None;
    let pipeline = Pipeline::new(
        Arc::clone(&store),
        config,
        Arc::new(crate::tests::TestRules),
        model.clone(),
        Arc::new(Unused),
        Arc::new(Unused),
        None,
    )
    .with_clock(Arc::new(Fixed(NOW)));
    Bench {
        pipeline,
        store,
        model,
    }
}

/// One story, `hours_ago` before [`NOW`], in a country and optionally under a topic.
#[allow(clippy::too_many_arguments)]
fn story(
    store: &Store,
    id: i64,
    title: &str,
    summary: &str,
    iso: &str,
    place: &str,
    hours_ago: i64,
) {
    let published = atlas_store::clock::iso_from_epoch(NOW - hours_ago * 3600);
    store
        .exec(
            "INSERT INTO articles(id, source_id, url, title, summary, published, iso, place, \
             relevant, dupe) VALUES(?,?,?,?,?,?,?,?,1,0)",
            &[
                SqlValue::Integer(id),
                SqlValue::Integer(1),
                SqlValue::Text(format!("https://example.invalid/{id}")),
                SqlValue::Text(title.to_string()),
                SqlValue::Text(summary.to_string()),
                SqlValue::Text(published),
                SqlValue::Text(iso.to_string()),
                SqlValue::Text(place.to_string()),
            ],
        )
        .expect("a story");
}

fn source(store: &Store) {
    store
        .exec(
            "INSERT INTO sources(id, name) VALUES(1, 'Wire Daily')",
            &[],
        )
        .expect("a source");
}

/// The story ids in a prompt's **digest lines**, in order.
///
/// Only lines that begin with `[Article #` count: the prompt itself shows the model the
/// shape of a citation (`[Article #1234]`), and a helper that scanned the whole prompt would
/// read its own instructions back as stories.
fn ids_in(text: &str) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for line in text.lines().filter(|line| line.starts_with("[Article #")) {
        let digits: String = line
            .trim_start_matches("[Article #")
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if let Ok(id) = digits.parse::<i64>() {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// The row as the window will read it: through the store's own `briefing_get`, not this
/// file's SQL, so the test fails if the two disagree about the shelf.
fn stored(store: &Store, scope: &str, key: &str, hours: i64) -> Option<(String, i64, i64, i64)> {
    let row = store.briefing_get(scope, key, hours).ok().flatten()?;
    Some((row.line, row.watermark, row.articles, row.ms))
}

// ---------------------------------------------------------------------------
// 1. the digest names only the asked scope's stories
// ---------------------------------------------------------------------------

#[test]
fn a_country_briefing_names_the_stories_of_that_country_and_no_other() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    story(&bench.store, 1, "Chips in Dresden", "A plant opens.", "DEU", "Dresden", 2);
    story(&bench.store, 2, "Chips in Ohio", "A plant closes.", "USA", "Ohio", 3);
    story(&bench.store, 3, "Ports in Bremen", "A berth reopens.", "DEU", "Bremen", 4);

    let answer = bench.pipeline.brief("country", "DEU", 72);
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");
    assert_eq!(answer["stored"], 1);
    assert_eq!(answer["articles"], 2, "two German stories: {answer}");

    // The prompt is the only thing the model sees: it must carry the German ids and not the
    // American one. This is the promise, read off the text that was actually sent.
    let asked = bench.model.asked();
    assert_eq!(asked.len(), 1, "one call, no correction: {asked:?}");
    let digest = &asked[0];
    assert_eq!(ids_in(digest), vec![1, 3], "the asked scope's ids, newest first");
    assert!(!digest.contains("[Article #2]"), "another country's story: {digest}");
    assert!(!digest.contains("Ohio"), "{digest}");
    // The digest is fenced as untrusted data, and one story per line.
    assert!(digest.contains("--- STORIES (untrusted data) ---"), "{digest}");
    assert!(digest.contains("--- END ---"), "{digest}");
    let lines: Vec<&str> = digest
        .lines()
        .filter(|line| line.starts_with("[Article #"))
        .collect();
    assert_eq!(lines.len(), 2, "{digest}");
    // The shape the prompt names: id, date, outlet, title, summary.
    assert!(lines[0].starts_with("[Article #1] "), "{:?}", lines[0]);
    assert!(lines[0].contains(" · Wire Daily · Chips in Dresden — A plant opens."), "{:?}", lines[0]);

    // And the row the window reads.
    let (line, watermark, articles, ms) = stored(&bench.store, "country", "DEU", 72).expect("a row");
    assert_eq!(line, "Chips are the story here.");
    assert_eq!(articles, 2);
    assert_eq!(watermark, 3, "the newest id at the time of the call");
    assert!(ms >= 0);
}

#[test]
fn every_scope_reads_its_own_grouping() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    // Two topics, two places, one entity, one monitor with an alert and one with a seen mark.
    // Distinct hours: the list is newest first, and two stories in the same second would
    // be ordered by an id tie-break the test would then be measuring instead.
    for (id, title, iso, place, hours) in [
        (1, "Chips in Dresden", "DEU", "Dresden", 2),
        (2, "Chips in Ohio", "USA", "Ohio", 3),
    ] {
        story(&bench.store, id, title, "A sentence.", iso, place, hours);
    }
    bench
        .store
        .exec("INSERT INTO topics(id, name) VALUES(7, 'Chips')", &[])
        .expect("a topic");
    bench
        .store
        .exec(
            "INSERT INTO article_topics(article_id, topic_id) VALUES(1, 7)",
            &[],
        )
        .expect("a topic match");
    bench
        .store
        .exec(
            "INSERT INTO entities(id, name, type) VALUES(5, 'Nvidia', 'ORG')",
            &[],
        )
        .expect("an entity");
    bench
        .store
        .exec(
            "INSERT INTO article_entities(article_id, entity_id) VALUES(2, 5)",
            &[],
        )
        .expect("a named entity");
    bench
        .store
        .exec(
            "INSERT INTO monitors(id, name, query) VALUES(3, 'Chip watch', 'chips')",
            &[],
        )
        .expect("a monitor");
    bench
        .store
        .exec(
            "INSERT INTO alerts(monitor_id, article_id, title) VALUES(3, 1, 'Chips in Dresden')",
            &[],
        )
        .expect("an alert");
    bench
        .store
        .exec(
            "INSERT INTO monitor_seen(monitor_id, key) VALUES(3, 'a2')",
            &[],
        )
        .expect("a judged story");

    // The window sees both stories, newest first.
    let window = bench.pipeline.brief("window", "", 72);
    assert_eq!(window["ok"], Value::Bool(true), "{window}");
    assert_eq!(ids_in(&bench.model.asked()[0]), vec![1, 2], "the window, whole");

    // A topic by name, a place, an entity by name, a monitor by id.
    for (scope, key, expected) in [
        ("topic", "chips", vec![1]),
        ("place", "berlin-no", vec![]),
        ("place", "Dresden", vec![1]),
        ("entity", "nvidia", vec![2]),
        ("monitor", "3", vec![1, 2]),
        ("topic", "7", vec![1]),
    ] {
        let asked_before = bench.model.asked().len();
        let answer = bench.pipeline.brief(scope, key, 72);
        if expected.is_empty() {
            // A scope with no story is a refusal, and no call is made for it.
            assert_eq!(answer["ok"], Value::Bool(false), "{scope} {key}: {answer}");
            assert_eq!(bench.model.asked().len(), asked_before, "nobody was asked");
            continue;
        }
        assert_eq!(answer["ok"], Value::Bool(true), "{scope} {key}: {answer}");
        let asked = bench.model.asked();
        assert_eq!(
            ids_in(&asked[asked_before]),
            expected,
            "{scope} {key} read the wrong stories"
        );
    }
}

// ---------------------------------------------------------------------------
// 1b. every grouping selects the rows the window's own read selects
// ---------------------------------------------------------------------------
//
// The live bug: the ITA chip counted `articles.iso ∪ article_countries.iso` while the
// briefing read the column alone, so a country the window was showing stories for was
// refused. Each test below inserts one story that **no column of `articles`** connects to
// the scope — only a join table, or the place label's fallback — and requires it in the
// digest. A grouping that drifts from `atlas-desktop/src/data.rs` fails here, which is the
// point: the door has to open the list the number beside it counts.

/// The ids in the digest of one briefing call, in order, asserting it succeeded.
fn briefed(bench: &Bench, scope: &str, key: &str) -> Vec<i64> {
    let asked_before = bench.model.asked().len();
    let answer = bench.pipeline.brief(scope, key, 72);
    assert_eq!(answer["ok"], Value::Bool(true), "{scope} {key}: {answer}");
    let asked = bench.model.asked();
    ids_in(&asked[asked_before])
}

#[test]
fn a_country_briefing_reads_the_union_the_chip_counts() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    story(&bench.store, 1, "Rome", "A sentence.", "ITA", "Rome", 2);
    // Filed under France, but names Italy as a further place: the chip counts this row and
    // the plain column did not — the reported refusal, in one row.
    story(&bench.store, 2, "Summit in Rome", "A sentence.", "FRA", "Paris", 1);
    bench
        .store
        .exec(
            "INSERT INTO article_countries(article_id, iso) VALUES(2, 'ITA')",
            &[],
        )
        .expect("a second place");
    assert_eq!(briefed(&bench, "country", "ITA"), vec![2, 1], "newest first, both places");
    // The same rows the window's list reads: the country a story does not name stays empty,
    // and that is a refusal with a sentence rather than a briefing of nothing.
    let other = bench.pipeline.brief("country", "DEU", 72);
    assert_eq!(other["ok"], Value::Bool(false), "{other}");
}

#[test]
fn a_place_briefing_reads_the_label_the_pins_use() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    // A story the geocoder placed only in a country: `place` is empty, and the label falls
    // back to `iso` — the expression `atlas-desktop`'s pins group by (`PLACE_LABEL`).
    story(&bench.store, 1, "A dot on the map", "A sentence.", "Rome", "", 1);
    story(&bench.store, 2, "Filed by its own name", "A sentence.", "ITA", "Rome", 2);
    // Story 1 carries the label "Rome" through the fallback; story 2 through `place`.
    assert_eq!(briefed(&bench, "place", "Rome"), vec![1, 2]);
}

#[test]
fn a_topic_briefing_reads_the_join_and_not_the_text() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    bench
        .store
        .exec("INSERT INTO topics(id, name) VALUES(7, 'Chips')", &[])
        .expect("a topic");
    // Nothing in this story's words says "chips": the classifier's join is the only reason
    // it belongs to the topic, and re-reading the text would be a second rule.
    story(&bench.store, 1, "A plant opens", "A sentence.", "DEU", "Dresden", 1);
    bench
        .store
        .exec("INSERT INTO article_topics(article_id, topic_id) VALUES(1, 7)", &[])
        .expect("a topic match");
    assert_eq!(briefed(&bench, "topic", "Chips"), vec![1]);
}

#[test]
fn a_monitor_briefing_reads_the_mark_its_protocol_writes() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    bench
        .store
        .exec("INSERT INTO monitors(id, name, query) VALUES(3, 'Chip watch', 'chips')", &[])
        .expect("a monitor");
    story(&bench.store, 1, "Alerted", "A sentence.", "DEU", "Dresden", 3);
    story(&bench.store, 2, "Judged", "A sentence.", "DEU", "Dresden", 2);
    // A mark whose key is not `'a' || id` names a judged page, not a story. The window's
    // `m.key = 'a' || a.id` cannot match it, and neither may the briefing: a CAST of the
    // digits would have turned "a03" into article 3.
    story(&bench.store, 3, "Only a page", "A sentence.", "DEU", "Dresden", 1);
    bench
        .store
        .exec("INSERT INTO alerts(monitor_id, article_id, title) VALUES(3, 1, 'Alerted')", &[])
        .expect("an alert");
    bench
        .store
        .exec("INSERT INTO monitor_seen(monitor_id, key) VALUES(3, 'a2'), (3, 'a03')", &[])
        .expect("two marks");
    assert_eq!(briefed(&bench, "monitor", "3"), vec![2, 1], "the alert and the mark, not the page");
}

#[test]
fn an_entity_briefing_reads_the_join_and_not_the_text() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    bench
        .store
        .exec("INSERT INTO entities(id, name, type) VALUES(5, 'Nvidia', 'ORG')", &[])
        .expect("an entity");
    story(&bench.store, 1, "A plant opens", "A sentence.", "DEU", "Dresden", 1);
    bench
        .store
        .exec("INSERT INTO article_entities(article_id, entity_id) VALUES(1, 5)", &[])
        .expect("a named entity");
    assert_eq!(briefed(&bench, "entity", "Nvidia"), vec![1]);
}

#[test]
fn a_window_briefing_reads_a_story_no_join_table_names() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    // No country, no place, no topic, no entity, no monitor: the window is the one scope
    // that must not depend on a join, or a story the archive could not place disappears
    // from every briefing in the app.
    story(&bench.store, 1, "Nowhere in particular", "A sentence.", "", "", 1);
    assert_eq!(briefed(&bench, "window", ""), vec![1]);
}

#[test]
fn the_digest_strips_markup_and_keeps_a_headline_in_one_line() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    story(
        &bench.store,
        1,
        "<b>Chips</b> &amp; the <i>city</i>",
        "Line one.\nLine\ttwo.",
        "DEU",
        "Dresden",
        1,
    );
    assert_eq!(bench.pipeline.brief("country", "DEU", 72)["ok"], Value::Bool(true));
    let digest = &bench.model.asked()[0];
    assert!(!digest.contains("<b>"), "markup reached the model: {digest}");
    assert!(!digest.contains("<i>"), "{digest}");
    let line = digest
        .lines()
        .find(|line| line.starts_with("[Article #1]"))
        .expect("the story's line");
    assert!(line.contains("Chips & the city"), "{line}");
    assert_eq!(line.lines().count(), 1, "a summary with a newline stayed one line: {line}");
}

/// What the model actually reads, byte for byte: the fence, one line per story, the shape.
///
/// The other digest tests measure one property each (no markup, one line, the cap); this one
/// pins the whole block, because the line *is* the product — a separator that drifted or a
/// field that swapped places would still pass every property test and still be a worse
/// briefing.
#[test]
fn the_digest_sent_to_the_model_is_one_fenced_line_per_story() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    story(
        &bench.store,
        1,
        "<b>Chips</b> &amp; the <i>city</i>",
        "Line one.\nLine\ttwo.",
        "DEU",
        "Dresden",
        1,
    );
    story(&bench.store, 2, "Talks resume", "Both sides met.", "DEU", "Dresden", 2);
    assert_eq!(bench.pipeline.brief("country", "DEU", 72)["ok"], Value::Bool(true));
    let prompt = &bench.model.asked()[0];
    let block = prompt
        .rsplit_once("--- STORIES (untrusted data) ---\n")
        .map(|(_, rest)| rest)
        .expect("the stories fence");
    let newest = atlas_store::clock::iso_from_epoch(NOW - 3600);
    let older = atlas_store::clock::iso_from_epoch(NOW - 2 * 3600);
    assert_eq!(
        block,
        format!(
            "[Article #1] {newest} · Wire Daily · Chips & the city — Line one. Line two.\n\
             [Article #2] {older} · Wire Daily · Talks resume — Both sides met.\n\
             --- END ---"
        )
    );
}

// ---------------------------------------------------------------------------
// 2. an offline model is a sentence, not an empty briefing
// ---------------------------------------------------------------------------

#[test]
fn an_offline_model_is_refused_with_the_address_it_tried() {
    let bench = bench(Scripted::offline());
    source(&bench.store);
    story(&bench.store, 1, "Chips", "A sentence.", "DEU", "Dresden", 1);
    let answer = bench.pipeline.brief("country", "DEU", 72);
    assert_eq!(answer["ok"], Value::Bool(false), "{answer}");
    let sentence = answer["error"].as_str().expect("one sentence");
    assert!(sentence.contains("the local model is not answering"), "{sentence}");
    assert!(sentence.contains("127.0.0.1"), "the address is named: {sentence}");
    assert!(sentence.contains("atlas start"), "the fix is named: {sentence}");
    assert_eq!(bench.model.calls(), 0, "nothing was asked");
    assert!(stored(&bench.store, "country", "DEU", 72).is_none(), "a refusal wrote a row");
}

#[test]
fn a_model_that_never_writes_the_object_is_refused_after_one_correction() {
    let bench = bench(Scripted::answering(&["I think chips are the story.", "Still prose."]));
    source(&bench.store);
    story(&bench.store, 1, "Chips", "A sentence.", "DEU", "Dresden", 1);
    let answer = bench.pipeline.brief("country", "DEU", 72);
    assert_eq!(answer["ok"], Value::Bool(false), "{answer}");
    let sentence = answer["error"].as_str().expect("one sentence");
    assert!(sentence.contains("answered twice without the json object"), "{sentence}");
    assert_eq!(bench.model.calls(), 2, "one call and one correction, never a loop");
    // The correction says what it wants, in the words the contract fixes.
    let asked = bench.model.asked();
    assert!(asked[1].contains(crate::brief::ONLY_JSON), "{:?}", asked[1]);
    assert!(stored(&bench.store, "country", "DEU", 72).is_none(), "a refusal wrote a row");
}

#[test]
fn a_prose_answer_is_read_the_second_time_and_stored_once() {
    let bench = bench(Scripted::answering(&[
        "Here is the briefing: {\"line\":\"Chips.\",\"bullets\":[\"One [Article #1]\"]}",
        "unused",
    ]));
    source(&bench.store);
    story(&bench.store, 1, "Chips", "A sentence.", "DEU", "Dresden", 1);
    let answer = bench.pipeline.brief("country", "DEU", 72);
    assert_eq!(answer["ok"], Value::Bool(true), "prose around the object is read: {answer}");
    assert_eq!(bench.model.calls(), 1, "no correction when the object is there");
    assert_eq!(stored(&bench.store, "country", "DEU", 72).expect("a row").0, "Chips.");
}

// ---------------------------------------------------------------------------
// 3. an unknown scope is refused by name
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_scope_is_refused_and_the_six_are_named() {
    let bench = bench(Scripted::briefing());
    let answer = bench.pipeline.brief("weather", "DEU", 72);
    assert_eq!(answer["ok"], Value::Bool(false), "{answer}");
    let sentence = answer["error"].as_str().expect("one sentence");
    assert!(sentence.contains("unknown briefing scope \"weather\""), "{sentence}");
    for scope in crate::brief::SCOPES {
        assert!(sentence.contains(scope), "{scope} is not named: {sentence}");
    }
    assert_eq!(bench.model.calls(), 0);
}

#[test]
fn a_scope_with_no_story_says_which_one_it_looked_in() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    // A topic that exists and holds no story: the refusal is about the stories, not the name.
    bench
        .store
        .exec("INSERT INTO topics(id, name) VALUES(8, 'Empty')", &[])
        .expect("a topic");
    for (scope, key) in [("country", "FRA"), ("window", ""), ("topic", "Empty")] {
        let answer = bench.pipeline.brief(scope, key, 72);
        assert_eq!(answer["ok"], Value::Bool(false), "{scope}: {answer}");
        let sentence = answer["error"].as_str().expect("one sentence");
        assert!(sentence.contains("nothing to brief"), "{scope}: {sentence}");
        assert!(sentence.contains("last 72 hours"), "the window is named: {sentence}");
        assert_eq!(bench.model.calls(), 0, "no story, nobody asked");
    }
    // A name that does not resolve says so, and names what is missing.
    let answer = bench.pipeline.brief("topic", "Chips", 72);
    assert!(
        answer["error"].as_str().unwrap_or("").contains("no topic \"Chips\""),
        "{answer}"
    );
}

// ---------------------------------------------------------------------------
// 4. the watermark is the newest id before the call
// ---------------------------------------------------------------------------

#[test]
fn the_stored_watermark_is_the_newest_id_before_the_model_was_asked() {
    let bench = bench(Scripted::answering(&[
        r#"{"line":"Chips.","bullets":["One [Article #1]"]}"#,
    ]));
    source(&bench.store);
    story(&bench.store, 1, "Chips", "A sentence.", "DEU", "Dresden", 5);
    story(&bench.store, 9, "More chips", "A sentence.", "DEU", "Dresden", 4);

    // A story that lands *while* the model is thinking: it must not move the watermark,
    // because the watermark is what the briefing was written from.
    let bench = std::sync::Arc::new(bench);
    let store = Arc::clone(&bench.store);
    let writing = std::sync::Arc::clone(&bench);
    // The model's answer is asked for after the watermark is read; the write below happens
    // from this thread in between, which is the race the value has to survive.
    let inserter = std::thread::spawn(move || {
        story(&store, 10, "Later chips", "A sentence.", "DEU", "Dresden", 1);
    });
    inserter.join().expect("the concurrent write");
    let answer = writing.pipeline.brief("country", "DEU", 72);
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");

    let (_, watermark, articles, _) = stored(&bench.store, "country", "DEU", 72).expect("a row");
    assert_eq!(watermark, 10, "the watermark is the newest id when the call began");
    assert_eq!(articles, 3, "and the digest read what was there");
    // The answer says the same number the row holds: the window compares the two.
    assert_eq!(answer["watermark"], watermark);
}

#[test]
fn the_watermark_ignores_a_story_that_arrives_after_the_briefing_started() {
    // The same promise, measured without threads: a brief of an empty scope is refused
    // before any model call, and the watermark is read before the digest — so a row written
    // after `article_watermark()` cannot be in `articles` *or* in `watermark`.
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    for id in 1..=3 {
        story(&bench.store, id, "Chips", "A sentence.", "DEU", "Dresden", 10 - id);
    }
    let before = bench.pipeline.article_watermark();
    assert_eq!(before, 3);
    assert_eq!(bench.pipeline.brief("country", "DEU", 72)["ok"], Value::Bool(true));
    let (_, watermark, articles, _) = stored(&bench.store, "country", "DEU", 72).expect("a row");
    assert_eq!(watermark, before, "the watermark is what the call began with");
    assert_eq!(articles, 3);
    // The same scope, asked again with a new story: the row is replaced, not duplicated.
    story(&bench.store, 4, "A fourth", "A sentence.", "DEU", "Dresden", 1);
    assert_eq!(bench.pipeline.brief("country", "DEU", 72)["ok"], Value::Bool(true));
    let rows = bench
        .store
        .query("SELECT key FROM briefings WHERE scope='country'", &[])
        .expect("the rows");
    assert_eq!(rows.len(), 1, "one row per scope, key and window");
    assert_eq!(stored(&bench.store, "country", "DEU", 72).expect("a row").1, 4);
}

// ---------------------------------------------------------------------------
// 5. sixty stories are forty lines and at most 24 000 characters
// ---------------------------------------------------------------------------

#[test]
fn sixty_stories_are_cut_to_forty_and_never_past_the_character_budget() {
    // The builder's own caps, without a database: the statement asks for forty, and a
    // caller with more gets the same digest.
    let stories: Vec<crate::brief::Story> = (1..=60)
        .map(|id| crate::brief::Story {
            id,
            published: "2026-11-22T10:00:00Z".to_string(),
            source: "Wire Daily".to_string(),
            title: format!("Story {id}"),
            summary: "A sentence about the story.".to_string(),
        })
        .collect();
    let (digest, count) = brief_digest(&stories, BRIEF_CHARS);
    assert_eq!(count, BRIEF_STORIES, "forty stories, not sixty");
    assert_eq!(digest.lines().count(), 40);
    assert!(digest.chars().count() <= BRIEF_CHARS, "{} chars", digest.chars().count());
    assert!(digest.starts_with("[Article #1] "), "newest first, as the list arrives");
    assert!(!digest.contains("[Article #41]"), "the forty-first is cut: {digest}");

    // Five stories at the per-field caps still fit the shipped budget — the ceiling is a
    // promise, and this is the measurement that says forty lines cannot break it.
    let capped: Vec<crate::brief::Story> = (1..=40)
        .map(|id| crate::brief::Story {
            id,
            published: "2026-11-22T10:00:00Z".to_string(),
            source: "s".repeat(400),
            title: "x".repeat(400),
            summary: "y".repeat(400),
        })
        .collect();
    let (digest, count) = brief_digest(&capped, BRIEF_CHARS);
    assert_eq!(count, 40, "the field caps keep forty stories inside 24 000");
    assert!(digest.chars().count() <= BRIEF_CHARS, "{} chars", digest.chars().count());

    // The character cap itself, at a budget that binds: the line that would cross it is
    // left out whole, so no story is read as half a fact.
    let (short, kept) = brief_digest(&stories, 1_000);
    assert!(kept > 0 && kept < 40, "the budget cut some stories, not all: {kept}");
    assert!(short.chars().count() <= 1_000, "{} chars", short.chars().count());
    for line in short.lines() {
        assert!(line.ends_with("A sentence about the story."), "a line was cut mid-story: {line}");
    }
}

#[test]
fn a_briefing_of_the_window_asks_for_forty_stories_at_most() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    for id in 1..=60 {
        story(&bench.store, id, &format!("Story {id}"), "A sentence.", "DEU", "Dresden", 60 - id);
    }
    let answer = bench.pipeline.brief("window", "", 72);
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");
    assert_eq!(answer["articles"], 40, "the statement's own limit: {answer}");
    let digest = &bench.model.asked()[0];
    assert_eq!(ids_in(digest).len(), 40, "forty ids on the wire");
    let first = digest
        .lines()
        .find(|line| line.starts_with("[Article #"))
        .expect("a digest line");
    assert!(first.starts_with("[Article #60]"), "the newest story is first: {first}");
    assert!(!digest.contains("[Article #20]"), "the oldest stories are the ones cut: {digest}");
}

// ---------------------------------------------------------------------------
// 5b. the slot is the ask, not the ingest's own collection window
// ---------------------------------------------------------------------------
//
// The reported defect: `brief` clamped `hours` to the ingest's 72-hour *freshness* rule
// (`MAX_AGE_H`), so a 168-hour ask from the window's seven-day tab was written into the
// 72-hour slot. The request came back `done` with no error, and `window//168` stayed empty —
// the tab read a slot nothing ever wrote and said "no briefing yet" forever. The ask is the
// slot (`briefings` is keyed by `(scope, key, hours)`), so these tests assert two things at
// once: the row is stored under the number that was asked for, **and** nothing lands in a
// different one. A clamp that shortens the window would fail the first, and a clamp that
// stores somewhere else would fail the second.

/// The stored row's own `hours`, read through the store's accessor the window uses.
fn stored_hours(store: &Store, scope: &str, key: &str, hours: i64) -> Option<i64> {
    store.briefing_get(scope, key, hours).ok().flatten().map(|row| row.hours)
}

#[test]
fn a_168_hour_ask_is_stored_under_168_and_not_72() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    // Inside the seven-day window, outside the three-day one: only a 168-hour read sees both.
    story(&bench.store, 1, "A week of chip rules", "The first round.", "DEU", "Dresden", 100);
    story(&bench.store, 2, "Chips today", "A second round.", "DEU", "Dresden", 2);

    let answer = bench.pipeline.brief("window", "", 168);
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");
    assert_eq!(answer["stored"], 1);
    assert_eq!(
        answer["hours"], 168,
        "the answer echoes the slot it stored, not the ingest's 72-hour rule: {answer}"
    );
    assert_eq!(answer["articles"], 2, "a 168-hour ask reads 168 hours: {answer}");
    let asked = bench.model.asked();
    assert_eq!(ids_in(&asked[0]), vec![2, 1], "newest first, both stories: {:?}", asked[0]);

    // The row the seven-day tab reads, and the slot it must not have touched.
    assert_eq!(
        stored_hours(&bench.store, "window", "", 168),
        Some(168),
        "the ask is the slot"
    );
    assert!(
        stored(&bench.store, "window", "", 168).is_some(),
        "the row is readable under the ask"
    );
    assert_eq!(
        stored_hours(&bench.store, "window", "", 72),
        None,
        "a 168-hour ask must never be written into the 72-hour slot"
    );
}

#[test]
fn a_24_hour_ask_is_stored_under_24_and_not_72() {
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    // A day-old story is outside the 24-hour window and inside the 72-hour one: a read that
    // fell back to 72 would put it in the digest.
    story(&bench.store, 1, "Yesterday's rules", "An older round.", "DEU", "Dresden", 30);
    story(&bench.store, 2, "Chips in the last day", "A fresh round.", "DEU", "Dresden", 2);

    let answer = bench.pipeline.brief("country", "DEU", 24);
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");
    assert_eq!(answer["stored"], 1);
    assert_eq!(answer["hours"], 24, "the answer echoes the ask: {answer}");
    assert_eq!(answer["articles"], 1, "a 24-hour ask reads 24 hours: {answer}");
    let asked = bench.model.asked();
    assert_eq!(ids_in(&asked[0]), vec![2], "the day-old story is outside: {:?}", asked[0]);

    assert_eq!(
        stored_hours(&bench.store, "country", "DEU", 24),
        Some(24),
        "the ask is the slot"
    );
    assert_eq!(
        stored_hours(&bench.store, "country", "DEU", 72),
        None,
        "a 24-hour ask must never be written into the 72-hour slot"
    );
}

#[test]
fn an_ask_past_the_ceiling_is_clamped_to_the_door_and_named() {
    // `atlas-monitor`'s door takes 1..=720; this side clamps to the same 720 (`BRIEF_MAX_H`),
    // so a caller that asks for more gets the widest slot the queue would have allowed rather
    // than a number nobody can read back.
    let bench = bench(Scripted::briefing());
    source(&bench.store);
    story(&bench.store, 1, "Chips", "A round.", "DEU", "Dresden", 2);

    let answer = bench.pipeline.brief("window", "", 900);
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");
    assert_eq!(stored_hours(&bench.store, "window", "", 720), Some(720));
    assert_eq!(stored_hours(&bench.store, "window", "", 900), None, "the door's ceiling holds");

    // And the floor: 0 is one hour, not a briefing about everything ever. The story is from
    // this hour, so a one-hour read holds it and a zero-hour read would not.
    story(&bench.store, 2, "Chips now", "Another round.", "DEU", "Dresden", 0);
    let answer = bench.pipeline.brief("window", "", 0);
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");
    assert_eq!(stored_hours(&bench.store, "window", "", 1), Some(1));
}

// ---------------------------------------------------------------------------
// 6. the same statement on the user's own archive, read-only
// ---------------------------------------------------------------------------

/// The reported bug, measured on the archive the app is reading.
///
/// The live file is only ever opened by `fs::copy`: the pipeline reads and writes the copy
/// in the temp dir, so `data/atlas.db` is untouched (a read-only open of the original would
/// still let a future edit write through the plan cache; a copy cannot).
///
/// It skips **silently**, unlike `tests/redaction_corpus.rs`: a library module may not print
/// at all (`tests/search_privacy.rs` scans every file under `src/` for the print macros, and
/// a test module is scanned too), so a skip here has no line to say it. The archive lives
/// outside the crate, so a fresh tree has none and this test asserts nothing there.
///
/// The pair it briefs is chosen from the data, not hardcoded: the country with the fewest
/// stories among those where a story is in the join table but not in `articles.iso` — so the
/// digest cannot cut the join-only story off at its forty-line cap, and the test measures the
/// grouping rather than a limit. The live report's own country (`ITA`) is preferred when the
/// archive still holds it that way.
#[test]
fn the_live_archive_briefs_a_country_its_chip_counts_through_the_join() {
    use std::collections::BTreeMap;

    let live = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/atlas.db");
    if !live.is_file() {
        return;
    }
    let copy = std::env::temp_dir().join(format!("atlas-brief-live-{}.db", std::process::id()));
    std::fs::copy(&live, &copy).expect("copy the archive into the temp dir");
    let store = Arc::new(Store::open(&copy).expect("open the copy"));
    // The copy is the app's archive as it is on disk; a tree from before the briefing shelf
    // existed gets it here (on the copy), the same call `Store::init` makes for the app.
    store.briefings_init().expect("the briefings shelf on the copy");

    let since = atlas_store::iso_ago(72.0);
    let join_only = store
        .query(
            "SELECT c.iso AS iso, a.id AS id FROM article_countries c \
             JOIN articles a ON a.id = c.article_id \
             WHERE a.relevant = 1 AND a.dupe = 0 AND a.published >= ? AND a.iso <> c.iso \
             ORDER BY a.published DESC, a.id DESC",
            &[SqlValue::Text(since.clone())],
        )
        .expect("read the join table");
    let mut by_country: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for row in &join_only {
        if let Some(iso) = row.text("iso").filter(|iso| !iso.is_empty()) {
            by_country
                .entry(iso.to_string())
                .or_default()
                .push(row.int("id").unwrap_or(0));
        }
    }
    let mut countries: Vec<(String, i64)> = Vec::new();
    for iso in by_country.keys() {
        let counted = store
            .query_one(
                "SELECT count(*) AS n FROM articles a \
                 WHERE a.relevant = 1 AND a.dupe = 0 AND a.published >= ?1 \
                   AND (a.iso = ?2 OR EXISTS (SELECT 1 FROM article_countries c \
                        WHERE c.article_id = a.id AND c.iso = ?2))",
                &[SqlValue::Text(since.clone()), SqlValue::Text(iso.clone())],
            )
            .ok()
            .flatten()
            .and_then(|row| row.int("n"))
            .unwrap_or(0);
        if (1..=BRIEF_STORIES as i64).contains(&counted) {
            countries.push((iso.clone(), counted));
        }
    }
    countries.sort_by_key(|(_, counted)| *counted);
    // The live report's country first, when this archive still holds it that way; otherwise
    // the smallest candidate, so the test keeps measuring on a fresh archive with no Italy.
    let chosen = countries
        .iter()
        .find(|(iso, _)| iso == "ITA")
        .or_else(|| countries.first())
        .cloned();
    let Some((iso, counted)) = chosen else {
        return;
    };
    let wanted = by_country[&iso][0];

    let mut config = Config::for_root(std::env::temp_dir());
    config.search_engine = None;
    let model = Scripted::briefing();
    let pipeline = Pipeline::new(
        Arc::clone(&store),
        config,
        Arc::new(crate::tests::TestRules),
        model.clone(),
        Arc::new(Unused),
        Arc::new(Unused),
        None,
    );
    let answer = pipeline.brief("country", &iso, 72);
    assert_eq!(
        answer["ok"],
        Value::Bool(true),
        "{iso} is on the window's chip and the briefing refused it: {answer}"
    );
    let digest = &model.asked()[0];
    let ids = ids_in(digest);
    assert!(
        ids.contains(&wanted),
        "[Article #{wanted}] is in article_countries as {iso} and not in the digest: {digest}"
    );
    // Every story the digester kept is the statement's; a story with neither title nor
    // summary is dropped by `digest_line`, so the count is a ceiling, not an equality.
    assert!(ids.len() <= counted as usize, "{iso}: more digest lines than stories");
    drop(store);
    let _ = std::fs::remove_file(&copy);
}
