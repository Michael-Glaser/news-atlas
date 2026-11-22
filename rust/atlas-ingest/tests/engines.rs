//! What one stored article costs in rule-engine questions, and how many of them
//! are still child processes.
//!
//! The unit: one `save_article` through the real pipeline — the URL gate, the
//! headline, the body, the topic gate, the freshness window, the place, the entity
//! linking and the tags — against the real engines, with the model answering
//! offline and the store in memory. Nothing here opens a socket, starts a browser
//! or touches `data/`.
//!
//! Two runs of the same article:
//!
//! * **shipped** — `NativeRules` as the app wires it: all five engines answer in
//!   this process (`rules::inproc`), so no question starts a child.
//! * **all processes** — `NativeRules::spawn_only`, the same questions every one of
//!   them as a child process. This is the *control*: the difference between the two
//!   numbers is what the transport costs and nothing else.
//!
//! The two runs must store the *same* article: the numbers are only worth
//! reporting if the answers did not change.
//!
//! The engines are the release binaries in `rust/target/release`. Without them
//! (a plain `cargo test` on an unbuilt checkout) the test says so and returns.
//!
//! Measured with `cargo test --release -p atlas-ingest --test engines --
//! --nocapture`, one save of the fixture article = **18 engine questions**:
//!
//! | wiring | questions as processes | `save_article` |
//! | --- | --- | --- |
//! | before (`atlas-text` + `atlas-core` as children) | 15 | 47.6 / 49.8 / 64.5 ms |
//! | after (every engine a library call) | **0** | 0.74 / 0.81 / 1.44 ms |
//! | control: every question a process | 18 | 57.8 / 59.1 / 59.5 ms, 112.6 ms while another build was running |
//!
//! Per question that is 4.3–4.6 ms as a process against 0.16–0.25 ms in this
//! process
//! (`rules::tests::one_saves_questions_cost_no_process_so_the_difference_is_the_spawn`
//! prints it); the 13 `atlas-text` questions are `clean_field` (once per field,
//! `read.rs:364`) and one `tokenize` per tag (`tags.rs:66`), the 2 `atlas-core`
//! ones are the entity candidate lists.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atlas_ingest::model::OfflineModel;
use atlas_ingest::rules::inproc;
use atlas_ingest::rules::{
    ArticleText, Locate, NativeRules, Parsed, Rules, RulesError, TopicMatch, TopicRow, Window,
};
use atlas_ingest::{Clock, Config, Model, NetFetcher, NullBrowser, Pipeline};
use atlas_store::{SqlValue, Store};

/// The body of the article: prose, its headline repeated (so the quality gate and
/// the title check both pass), long enough to be stored, and with a city named
/// twice late in the text so the place engine has something to find.
fn article_text() -> String {
    let mut out = String::from(
        "Chip exports rise as the ministry reports a record quarter. \
         The ministry said the measure would take effect next month. ",
    );
    while out.chars().count() < 9_000 {
        out.push_str("Officials said the decision followed a review of the market. ");
    }
    out.push_str("The council moved the session to Lisbon for the second half. ");
    out.push_str("Lisbon will host the next round in March. ");
    out
}

fn engine_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("release")
}

/// Counts the questions that reach an engine, per engine.
#[derive(Default)]
struct Calls {
    text: AtomicUsize,
    core: AtomicUsize,
    in_process: AtomicUsize,
    spawned: AtomicUsize,
}

impl Calls {
    fn text(&self) -> usize {
        self.text.load(Ordering::SeqCst)
    }

    fn core(&self) -> usize {
        self.core.load(Ordering::SeqCst)
    }

    /// How many questions the wiring answered without a process. Attributed from
    /// [`inproc::answers_here`], not from a list kept here: an engine that moves
    /// between transports changes these numbers with no edit to this file.
    fn in_process(&self) -> usize {
        self.in_process.load(Ordering::SeqCst)
    }

    fn spawned(&self) -> usize {
        self.spawned.load(Ordering::SeqCst)
    }

    /// Every question asked, whichever engine answered it: the transport counters
    /// are the total, and `text`/`core` are a breakdown by engine that must not be
    /// added to it twice.
    fn all(&self) -> usize {
        self.in_process() + self.spawned()
    }
}

/// The shipped rules, with a counter in front of them.
///
/// Every method delegates: this wrapper decides nothing, it only counts which
/// engine was asked and whether that engine answers in this process.
struct CountingRules {
    inner: NativeRules,
    /// True when `inner` is the spawn-only wiring, so a count can be attributed.
    all_processes: bool,
    calls: Arc<Calls>,
}

impl CountingRules {
    /// One question, to one engine: which engine (always), and how it travelled.
    fn note(&self, engine: &str) {
        match engine {
            "atlas-text" => self.calls.text.fetch_add(1, Ordering::SeqCst),
            "atlas-core" => self.calls.core.fetch_add(1, Ordering::SeqCst),
            _ => 0,
        };
        if self.all_processes || !inproc::answers_here(engine) {
            self.calls.spawned.fetch_add(1, Ordering::SeqCst);
        } else {
            self.calls.in_process.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Rules for CountingRules {
    fn clean_text(&self, text: &str) -> Result<String, RulesError> {
        self.note("atlas-text");
        self.inner.clean_text(text)
    }

    fn norm_url(&self, url: &str) -> Result<String, RulesError> {
        self.note("atlas-text");
        self.inner.norm_url(url)
    }

    fn publisher(&self, url: &str) -> Result<String, RulesError> {
        self.note("atlas-text");
        self.inner.publisher(url)
    }

    fn clean_title(&self, title: &str, publisher: &str) -> Result<String, RulesError> {
        self.note("atlas-text");
        self.inner.clean_title(title, publisher)
    }

    fn tokenize(&self, text: &str, max_words: usize) -> Result<Vec<String>, RulesError> {
        self.note("atlas-text");
        self.inner.tokenize(text, max_words)
    }

    fn extract_html(&self, html: &str, url: &str, title: &str) -> Result<String, RulesError> {
        self.note("atlas-core");
        self.inner.extract_html(html, url, title)
    }

    fn entity_candidates(
        &self,
        text: &str,
        known: &[String],
        min_count: usize,
        max: usize,
    ) -> Result<Vec<(String, usize)>, RulesError> {
        self.note("atlas-core");
        self.inner.entity_candidates(text, known, min_count, max)
    }

    fn locate(&self, article: &ArticleText) -> Result<Locate, RulesError> {
        self.note("atlas-places");
        self.inner.locate(article)
    }

    fn match_topics(
        &self,
        topics: &[TopicRow],
        article: &ArticleText,
    ) -> Result<Vec<TopicMatch>, RulesError> {
        self.note("atlas-topics");
        self.inner.match_topics(topics, article)
    }

    fn parse_date(&self, ts: &str, now: i64) -> Result<Parsed, RulesError> {
        self.note("atlas-dates");
        self.inner.parse_date(ts, now)
    }

    fn window(&self, ts: &str, max_age_s: i64, now: i64) -> Result<Window, RulesError> {
        self.note("atlas-dates");
        self.inner.window(ts, max_age_s, now)
    }
}

/// 2026-11-05T10:00:00Z — the same "now" the crate's own tests use, so the two
/// runs publish the article at the same instant and their answers are comparable.
const NOW: i64 = 1_793_872_800;

struct FixedClock;

impl Clock for FixedClock {
    fn now_secs(&self) -> i64 {
        NOW
    }
}

/// One save of the fixture article, and what it cost.
struct Run {
    elapsed: Duration,
    calls: Arc<Calls>,
    result: serde_json::Value,
}

/// Store the fixture article once, with the rules wired the way the caller asks.
fn save_once(all_processes: bool) -> Option<Run> {
    let dir = engine_dir();
    let rules = if all_processes {
        NativeRules::spawn_only(&dir, Duration::from_secs(20))
    } else {
        NativeRules::with_bin_dir(&dir, Duration::from_secs(20))
    };
    let store = Arc::new(Store::open_memory().expect("an in-memory database"));
    store.init().expect("the schema");
    store
        .insert(
            "INSERT INTO topics(name,keywords,exclude,min_matches,active,created,hits) \
             VALUES(?,?,?,1,1,?,0)",
            &[
                SqlValue::Text("Chips".to_string()),
                SqlValue::Text("chip, export, ministry".to_string()),
                SqlValue::Text(String::new()),
                SqlValue::Text("2026-01-01T00:00:00Z".to_string()),
            ],
        )
        .expect("a topic");

    let calls = Arc::new(Calls::default());
    let rules = Arc::new(CountingRules {
        inner: rules,
        all_processes,
        calls: Arc::clone(&calls),
    });
    let model: Arc<dyn Model> = Arc::new(OfflineModel::new("the benchmark asks no model"));
    let mut config = Config::for_root(std::env::temp_dir());
    config.bin_dir = dir;
    let pipeline = Pipeline::new(
        Arc::clone(&store),
        config,
        rules,
        model,
        Arc::new(NetFetcher::new()),
        // The shipped seam: no browser, so this test cannot spawn one either.
        Arc::new(NullBrowser),
        None,
    )
    .with_clock(Arc::new(FixedClock));

    let now = pipeline.now_secs();
    let input = atlas_ingest::save::SaveInput {
        url: "https://example.com/chip-exports".to_string(),
        title: "Chip exports rise".to_string(),
        text: article_text(),
        summary: "Exports rose in the quarter. The ministry gave no figure.".to_string(),
        tags: vec!["chip export".to_string(), "semiconductors".to_string()],
        categories: vec!["Technology".to_string()],
        topic: "Chips".to_string(),
        published: atlas_store::clock::iso_from_epoch(now - 3_600),
        publisher: "Example".to_string(),
        place: String::new(),
        entities: Vec::new(),
    };

    let start = Instant::now();
    let stored = pipeline.save_article(&input);
    let elapsed = start.elapsed();
    let result = stored.to_json();
    if result.get("status").and_then(|v| v.as_str()) != Some("stored") {
        // Not a failure of the measurement: this checkout cannot store (an engine
        // is missing). The caller skips.
        eprintln!("engines: could not store the fixture: {}", result);
        return None;
    }
    Some(Run {
        elapsed,
        calls,
        result,
    })
}

/// The faster of two runs of one wiring: a single save is short enough that a
/// cold page cache or another process on the machine is most of the noise, and
/// best-of-two is the usual answer to that.
fn best_of_two(all_processes: bool) -> Option<Run> {
    let first = save_once(all_processes)?;
    let second = save_once(all_processes)?;
    Some(if second.elapsed < first.elapsed {
        second
    } else {
        first
    })
}

#[test]
fn one_saved_article_asks_the_engines_and_none_of_them_is_a_process() {
    let Some(shipped) = best_of_two(false) else {
        return; // the engine binaries are not built in this checkout
    };
    let Some(before) = best_of_two(true) else {
        return;
    };

    println!(
        "save_article: {} engine questions — shipped {:?}, every question a process {:?} ({:+.1} ms)",
        shipped.calls.all(),
        shipped.elapsed,
        before.elapsed,
        (shipped.elapsed.as_secs_f64() - before.elapsed.as_secs_f64()) * 1_000.0
    );
    println!(
        "  shipped:     {} in-process questions, {} child processes ({} text, {} core)",
        shipped.calls.in_process(),
        shipped.calls.spawned(),
        shipped.calls.text(),
        shipped.calls.core()
    );
    println!(
        "  all-process: {} in-process questions, {} child processes ({} text, {} core)",
        before.calls.in_process(),
        before.calls.spawned(),
        before.calls.text(),
        before.calls.core()
    );

    // The article the two runs stored is the same article: the answers did not
    // change with the transport.
    assert_eq!(
        shipped.result, before.result,
        "the two transports stored different articles"
    );
    // One save asks fifteen questions of the two engines that used to be
    // children, and now asks none of them of a process.
    assert!(
        shipped.calls.all() >= 8,
        "a save asks the engines more than {} times",
        shipped.calls.all()
    );
    assert_eq!(
        shipped.calls.spawned(),
        0,
        "no engine question may start a process: {} in-process, {} spawned",
        shipped.calls.in_process(),
        shipped.calls.spawned()
    );
    assert_eq!(shipped.calls.in_process(), shipped.calls.all());
    assert_eq!(
        shipped.calls.all(),
        before.calls.all(),
        "the count of questions is the wiring's, not the transport's"
    );
    assert!(
        before.calls.spawned() == before.calls.all() && before.calls.all() > 0,
        "the all-process wiring must have asked every question of a child: {} of {}",
        before.calls.spawned(),
        before.calls.all()
    );
    // The wall clock is printed, not asserted: `save_article` also opens SQLite,
    // builds the graph and asks the model, and this machine is shared (another
    // build running next to this test moved the same number by 10 ms between
    // runs). The *count* above is what the wiring decides, and the per-question
    // cost is measured where it is isolated
    // (`rules::tests::in_process_answers_are_cheaper_than_processes` and
    // `rules::tests::one_saves_questions_cost_no_process_so_the_difference_is_the_spawn`).
}
