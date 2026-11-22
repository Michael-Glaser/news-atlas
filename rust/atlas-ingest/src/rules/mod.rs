//! The rule engines, behind one trait.
//!
//! Five ATLAS crates own a decision this pipeline has to apply and must never
//! re-derive: `atlas-text` (URL canonical form, publisher, headline tail),
//! `atlas-core` (HTML → text, entity candidates), `atlas-places` (where a story
//! happened), `atlas-topics` (does it match a topic the reader collects) and
//! `atlas-dates` (is this timestamp current).
//!
//! # Two transports, one code path
//!
//! Every one of the five is both a crate and a binary, and the binary's program is
//! its library's `run(op, raw)`: argv[1] is the op, stdin is one JSON object,
//! stdout is one JSON object, `{"error": …}` plus exit 1 on refusal. Three of them
//! spell it as `atlas_lib::cli::serve(crate::run)`; `atlas-text` and `atlas-core`
//! own the same shape in their own envelope module, next to the caps it applies.
//!
//! | Engine | Transport | Its program is |
//! | --- | --- | --- |
//! | `atlas-text` | in-process | `atlas_text::run` (`src/envelope.rs`) |
//! | `atlas-core` | in-process | `atlas_core::run` (`src/entry.rs`) |
//! | `atlas-places` | in-process | `atlas_places::run` |
//! | `atlas-topics` | in-process | `atlas_topics::run` |
//! | `atlas-dates` | in-process | `atlas_dates::run` |
//!
//! A process per question cost 2–4 ms of spawn for tens of microseconds of work
//! (`rules::tests::in_process_answers_are_cheaper_than_processes` prints the
//! measured numbers), and one saved article asks 18 questions — so all five were
//! moved: the last two by exporting the envelope they already had, never by
//! writing a second one here. **No engine question is a process any more**
//! ([`SPAWNED`] is empty, `rules::tests::no_engine_question_starts_a_process`
//! proves it), and the binary is now only the way to ask an engine from outside
//! this app. The process path still exists for the parity witness
//! (`NativeRules::spawn_only`), which asks the same question both ways and
//! compares the answers: `rules::tests::the_in_process_answer_is_the_binary_answer`
//! field by field, `rules::tests::the_text_and_core_envelopes_are_the_binaries`
//! byte for byte.
//!
//! **There is no fallback.** A missing binary is a [`RulesError`] that names the
//! file and the command that builds it, because a rule that quietly answers
//! differently is worse than an app that says what is wrong. An engine that
//! refuses says why, and that reason is carried to the caller unchanged instead
//! of being replaced by "could not answer".
//!
//! The trait exists so a test can run the whole pipeline deterministically
//! without asking any engine — not so production can swap the rules out.
//!
//! # How much text each engine is given
//!
//! Each engine cuts the body at its own limit before scoring it, so sending more
//! cannot change an answer — it only moves bytes. Those limits are restated here
//! as [`PLACE_BODY_CHARS`] and [`TOPIC_BODY_CHARS`], each with a test that proves
//! the cut is the engine's own: a keyword or a city placed just past the limit is
//! not matched, and cutting the input at the limit changes nothing
//! (`rules::tests::the_body_caps_are_the_engines_own`).

mod answer;
pub mod inproc;
mod native;
mod spawn;

pub use native::NativeRules;

use serde_json::{json, Value};

/// A rule engine could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesError {
    message: String,
}

impl RulesError {
    pub fn new(message: impl Into<String>) -> RulesError {
        RulesError {
            message: crate::scrub(&message.into()),
        }
    }

    pub fn missing(engine: &str, path: &std::path::Path) -> RulesError {
        RulesError::new(format!(
            "{} is not built ({} is missing) - run: cargo build --release --manifest-path rust/Cargo.toml",
            engine,
            path.display()
        ))
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RulesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RulesError {}

/// One place verdict, as `atlas-places locate` answers it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Locate {
    pub place: String,
    pub iso: String,
    pub lat: f64,
    pub lon: f64,
    /// `city`, `country`, … — the precision the engine is willing to claim.
    pub prec: String,
    pub conf: f64,
}

impl Locate {
    /// Where the story happened, or "nowhere we can name".
    pub fn named(&self) -> bool {
        !self.iso.is_empty() && !self.place.trim().is_empty()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "place": self.place,
            "iso": self.iso,
            "lat": self.lat,
            "lon": self.lon,
            "prec": self.prec,
            "conf": self.conf,
        })
    }
}

/// One topic as `atlas-topics match` reads it (`server/topics.py` columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicRow {
    pub id: i64,
    pub name: String,
    pub keywords: String,
    pub exclude: String,
    pub min_matches: i64,
    pub active: bool,
}

/// One topic the article matches.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicMatch {
    pub id: i64,
    pub name: String,
    pub score: f64,
    /// Which terms matched, in the engine's words.
    pub matched: Vec<String>,
}

impl TopicMatch {
    /// The `article_topics.matched` column: one text value, comma separated.
    pub fn matched_text(&self) -> String {
        self.matched.join(", ")
    }
}

/// What an article is matched against.
#[derive(Debug, Clone, Default)]
pub struct ArticleText {
    pub title: String,
    pub summary: String,
    pub content: String,
    pub url: String,
    pub publisher: String,
    pub entities: Vec<String>,
    pub tags: Vec<String>,
}

impl ArticleText {
    /// The article as `atlas-places` reads it: the body cut to the engine's own
    /// limit ([`PLACE_BODY_CHARS`]), because text past it cannot name a place.
    ///
    /// Both callers (`save`, `enrich`) built this by hand and both cut the body at
    /// 3 000 characters — a fifth of what the engine scans, so a city first named
    /// in the fourth paragraph of a wire story was never seen. One constructor,
    /// one limit, and the limit is the engine's.
    pub fn for_place(title: &str, summary: &str, body: &str, url: &str, publisher: &str) -> ArticleText {
        ArticleText {
            title: title.to_string(),
            summary: summary.to_string(),
            content: truncate(body, PLACE_BODY_CHARS),
            url: url.to_string(),
            publisher: publisher.to_string(),
            ..ArticleText::default()
        }
    }
}

/// `atlas-dates window` — the freshness verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub fresh: bool,
    /// `fresh`, `skew`, `stale` or `unparsed`.
    pub reason: String,
    pub age_h: f64,
    pub iso: String,
    pub note: String,
}

/// `atlas-dates parse` — a timestamp, or why it is not one.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub ok: bool,
    pub epoch: i64,
    pub iso: String,
    pub reason: String,
}

/// The five engines' answers.
pub trait Rules: Send + Sync {
    /// `atlas-text clean_text` — control/bidi/zero-width out, layout normalised.
    fn clean_text(&self, text: &str) -> Result<String, RulesError>;

    /// `atlas-text norm_url` — the canonical form used as a cache and dupe key.
    fn norm_url(&self, url: &str) -> Result<String, RulesError>;

    /// `atlas-text publisher` — a presentable outlet name from a URL.
    fn publisher(&self, url: &str) -> Result<String, RulesError>;

    /// `atlas-text clean_title` — drop the " - Site" tail search feeds add.
    fn clean_title(&self, title: &str, publisher: &str) -> Result<String, RulesError>;

    /// `atlas-text tokenize` — lowercase words, stopwords gone, no markup.
    fn tokenize(&self, text: &str, max_words: usize) -> Result<Vec<String>, RulesError>;

    /// `atlas-core extract` — HTML → article text.
    fn extract_html(&self, html: &str, url: &str, title: &str) -> Result<String, RulesError>;

    /// `atlas-core entities` — capitalised candidates with their counts.
    fn entity_candidates(
        &self,
        text: &str,
        known: &[String],
        min_count: usize,
        max: usize,
    ) -> Result<Vec<(String, usize)>, RulesError>;

    /// `atlas-places locate` — where the story happened.
    fn locate(&self, article: &ArticleText) -> Result<Locate, RulesError>;

    /// `atlas-topics match` — the reader's topics this article matches.
    fn match_topics(
        &self,
        topics: &[TopicRow],
        article: &ArticleText,
    ) -> Result<Vec<TopicMatch>, RulesError>;

    /// `atlas-dates parse` — one timestamp, or why it cannot be read.
    fn parse_date(&self, ts: &str, now: i64) -> Result<Parsed, RulesError>;

    /// `atlas-dates window` — fresh, stale or dated in the future.
    fn window(&self, ts: &str, max_age_s: i64, now: i64) -> Result<Window, RulesError>;
}

// ---------------------------------------------------------------------------
// what each engine can actually use
// ---------------------------------------------------------------------------

/// How much of the body `atlas-topics` can match against.
///
/// The engine folds the first 16 000 characters and scans the first 8 000 of the
/// folded text (`atlas-topics/src/model.rs`, `CONTENT_FOLD` / `CONTENT_SCAN`), so
/// nothing past 16 000 can match however much is sent — and the payload used to
/// carry 32 000, twice what could be read, on every save.
///
/// 16 000 and not 8 000: folding *shortens* text (combining marks are dropped), so
/// character 9 000 of the body can land inside the 8 000 that are scanned. The
/// fold's input cut is the only bound that cannot lose a match.
pub const TOPIC_BODY_CHARS: usize = 16_000;

/// How much of the body `atlas-places` scans (`atlas-places/src/score.rs`,
/// `BODY_CHARS`); past it, a city cannot be found.
pub const PLACE_BODY_CHARS: usize = 20_000;

/// The first `limit` characters of `text` — `chars`, not bytes, so a multi-byte
/// character is never cut in half. `text` itself comes back when it is already
/// short enough, without a copy of its characters.
pub fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect()
}

// ---------------------------------------------------------------------------
// the engine lists a status line and the boot check read
// ---------------------------------------------------------------------------

/// Every rule engine this pipeline asks, in one list — the *names*, not files.
pub const ENGINES: &[&str] = &[
    "atlas-text",
    "atlas-core",
    "atlas-places",
    "atlas-topics",
    "atlas-dates",
];

/// The engines that must exist as a file: **none of them**.
///
/// All five answer from this crate's own dependencies (`inproc`), so requiring
/// their binaries would refuse to start an app that works — the app now needs no
/// engine binary at all, only the model and the store. The list stays, empty, so
/// the boot check, the status line (`atlas-ingest/src/main.rs`) and the transport
/// test keep reading one place: an engine that ever has to go back to a child
/// process is added here and to [`inproc::ENGINES`]'s complement, and the
/// assertion that the two lists partition [`ENGINES`] still holds.
pub const SPAWNED: &[&str] = &[];

// ---------------------------------------------------------------------------
// what goes out (one request per engine operation)
// ---------------------------------------------------------------------------

/// The request `atlas-places locate` reads: title, summary, body, url, outlet.
pub(crate) fn locate_request(article: &ArticleText) -> Value {
    json!({
        "title": article.title,
        "summary": article.summary,
        "text": article.content,
        "url": article.url,
        "publisher": article.publisher,
    })
}

/// The request `atlas-topics match` reads: the reader's topics, and the article.
///
/// The body is cut to [`TOPIC_BODY_CHARS`] here as well as in the engine: past
/// that cut the engine reads nothing, so sending more only moves bytes over a pipe
/// (or copies them). Entities and tags travel as *evidence* — the engine reports
/// them next to the terms that matched and never scores them — and only when the
/// article carries any, so an article with no names is not padded with empties.
pub(crate) fn topics_request(topics: &[TopicRow], article: &ArticleText) -> Value {
    let rows: Vec<Value> = topics
        .iter()
        .map(|topic| {
            json!({
                "id": topic.id,
                "name": topic.name,
                "keywords": topic.keywords,
                "exclude": topic.exclude,
                "min_matches": topic.min_matches,
            })
        })
        .collect();
    let mut body = json!({
        "title": article.title,
        "summary": article.summary,
        "content": truncate(&article.content, TOPIC_BODY_CHARS),
    });
    if !article.entities.is_empty() {
        body["entities"] = json!(article.entities);
    }
    if !article.tags.is_empty() {
        body["tags"] = json!(article.tags);
    }
    if !article.url.is_empty() {
        body["url"] = json!(article.url);
    }
    if !article.publisher.is_empty() {
        body["publisher"] = json!(article.publisher);
    }
    json!({ "topics": rows, "article": body })
}

/// The request `atlas-dates parse` reads. `hint` is empty on purpose: the
/// pipeline has no format to force, and the engine tries every reader without it.
pub(crate) fn parse_request(ts: &str, now: i64) -> Value {
    json!({ "ts": ts, "hint": "", "now": now })
}

/// The request `atlas-dates window` reads.
pub(crate) fn window_request(ts: &str, max_age_s: i64, now: i64) -> Value {
    json!({ "ts": ts, "max_age_s": max_age_s, "now": now })
}

#[cfg(test)]
mod tests;
