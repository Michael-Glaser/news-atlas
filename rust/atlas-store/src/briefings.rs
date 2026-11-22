//! The `briefings` table: the model's prepared summary of one scope of the archive.
//!
//! The reader's requirement is a sentence and a few bullets about whatever they are
//! looking at — a country, a place, a topic, a monitor, one entity, or the whole window
//! — written *before* they look, and refreshed as stories land. This table is where that
//! text waits: one row per `(scope, key, hours)`, replaced in place when the model has
//! written a newer one.
//!
//! Decisions in this file:
//!
//! * `scope` is a **closed set** ([`SCOPES`]) and is refused by name, because the agent
//!   dispatches on it: a row with a scope nothing reads is a model call nobody sees.
//! * `key` is the scope's own address — an ISO code, a place label, a topic id as text,
//!   or `""` for the window — and the store never interprets it. What `"DEU"` means is
//!   the reader's decision, in the same way a request payload's meaning is the agent's.
//! * `watermark` is `max(articles.id)` at the moment the text was written, and it is the
//!   freshness rule: a briefing is stale exactly when a **newer article** exists. Not a
//!   clock, so an archive that stopped collecting does not churn the model, and a burst
//!   of twelve stories refreshes one line once.
//! * The caps ([`MAX_KEY_BYTES`], [`MAX_LINE_CHARS`], [`MAX_BULLETS`],
//!   [`MAX_BULLET_CHARS`]) are **refusals, never truncations**: a briefing cut in half
//!   reads like a model that stopped mid-sentence, and the caller could not tell the
//!   difference. The caps are the size a card on screen holds.
//! * `bullets` is JSON text in the column and `Vec<String>` in Rust. A row whose JSON
//!   does not parse answers [`Store::briefing_get`]` → Ok(None)`: a hand-edited or
//!   half-written row must not break the page it would have appeared on (see the doc
//!   on that method).
//!
//! It does not: ask a model anything, decide which scope a page wants, expire a row, or
//! choose a window. `atlas-ingest::Pipeline::brief` writes these rows and the monitor's
//! serve loop asks for the refresh; this file is the shelf.

use serde_json::Value;

use crate::error::{Result, StoreError};
use crate::params::SqlValue;
use crate::Store;

/// The table. Spelled out here, next to the code that reads it, the way
/// [`crate::requests`] keeps its own.
pub const SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS briefings(
  scope TEXT NOT NULL,
  key TEXT NOT NULL,
  hours INTEGER NOT NULL,
  watermark INTEGER NOT NULL,
  created TEXT NOT NULL,
  line TEXT NOT NULL DEFAULT '',
  bullets TEXT NOT NULL DEFAULT '[]',
  articles INTEGER NOT NULL DEFAULT 0,
  model TEXT NOT NULL DEFAULT '',
  ms INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(scope, key, hours))"#;

/// The index a page's reads use: one scope's briefings, newest first.
///
/// `(scope, hours, created)` and not `(scope, key, …)`: the primary key already answers
/// a single row, and the read this index is for is "the newest text for this scope at
/// this window" — a country page or the window header asking what it has to show.
pub const INDEX: &str = "CREATE INDEX IF NOT EXISTS idx_briefings_scope \
     ON briefings(scope, hours, created)";

/// The scopes a briefing may be written for. A closed set, because a page reads them by
/// name: an unknown scope is refused at write time rather than stored where nothing
/// looks.
///
/// This is a protocol, not a vocabulary about the news. `window` is the whole front page
/// (`key` is `""`), `country` a country's page (an ISO code), `place` one place label,
/// `topic` a reader's topic (its id as text), `monitor` one monitor (its id as text) and
/// `entity` one entity of the graph (its id as text).
pub const SCOPES: [&str; 6] = ["window", "country", "place", "topic", "monitor", "entity"];

/// The longest scope ATLAS stores, in bytes.
///
/// The set above is fixed and every word in it is short, so this can only ever be a
/// guard against a caller that assembled a scope from page text. It is a cap *and* the
/// set check: both refuse, and neither shortens.
pub const MAX_SCOPE_BYTES: usize = 16;

/// The longest `key` ATLAS stores, in bytes.
///
/// A key is an address, not a document: an ISO code is 3 bytes, a place label is a name,
/// an id is a handful of digits. 128 is room around the longest place name on the map, so
/// a key past it is a page that handed over a sentence.
pub const MAX_KEY_BYTES: usize = 128;

/// The longest `line` ATLAS stores, in **characters**.
///
/// The line is the sentence the card shows above its bullets — one or two sentences of
/// prepared summary, read at a glance. 400 characters is that card's width; measured
/// against the model's own answers while this was written, a briefing line ran 90–220
/// characters, so this is room, not pressure.
pub const MAX_LINE_CHARS: usize = 400;

/// The most bullets one briefing may carry.
///
/// A card that needs more than eight is a page that needs a list, not a briefing. The
/// number is `atlas_ai::brief::MAX_BULLETS`, the same ceiling the parser keeps — the
/// prompt itself asks for fewer (`BULLET_RANGE`, 3..=6), so eight is the width of the
/// card rather than a target. The store **refuses** the ninth rather than dropping it:
/// `atlas_ai::brief::parse` already cuts a model's overflow (the line would be lost with
/// it), so a nine-bullet briefing reaching this table is a caller that bypassed the
/// parser, and telling it beats hiding the loss.
pub const MAX_BULLETS: usize = 8;

/// The longest one bullet ATLAS stores, in **characters**. Same rule as
/// [`MAX_LINE_CHARS`]: a bullet is a clause, not a paragraph.
pub const MAX_BULLET_CHARS: usize = 240;

/// The narrowest window a briefing may be built over, in hours.
///
/// A window of zero hours is not a window — it would be a row keyed `hours=0` that no
/// reader ever asks for, holding text about nothing. There is no upper cap: the callers
/// choose the window (72 hours today), and a larger number is simply a larger window.
pub const MIN_HOURS: i64 = 1;

/// One prepared summary, as a page reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Briefing {
    /// One of [`SCOPES`].
    pub scope: String,
    /// The scope's address (`"DEU"`, a place label, an id as text, `""` for the window).
    pub key: String,
    /// The window the text was built over, in hours.
    pub hours: i64,
    /// `max(articles.id)` when it was built: a newer article makes it stale.
    pub watermark: i64,
    /// ISO timestamp of the write.
    pub created: String,
    /// The sentence above the bullets.
    pub line: String,
    /// The bullets, in the order the model wrote them.
    pub bullets: Vec<String>,
    /// How many stories went into the text.
    pub articles: i64,
    /// The model's own name, as the caller asked it, or `""` when nobody recorded one.
    pub model: String,
    /// How long the model took, in milliseconds (`0` when unrecorded).
    pub ms: i64,
}

impl Store {
    /// Create the `briefings` table and its index. Idempotent.
    ///
    /// [`crate::Store::init`] calls this, so every archive has the table whether or not a
    /// briefing was ever written — a reader that tolerates its absence (every reader must,
    /// see the module doc) then simply finds no rows instead of a missing table. This
    /// takes the store's own lock, so it must not be called from inside a transaction.
    pub fn briefings_init(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch(SCHEMA).map_err(StoreError::from)?;
        conn.execute(INDEX, []).map_err(StoreError::from)?;
        Ok(())
    }

    /// The briefing for one slot, or `None`.
    ///
    /// `None` means three things, all of them "this page shows no prepared text yet":
    /// there is no row; the archive predates this table (every reader tolerates its
    /// absence, and this read must not create it); or the row cannot be read as a
    /// briefing — its `bullets` is not a JSON array of strings, or a column holds a type
    /// it should not. **None of them is an error.** The table is written by a
    /// model-driven pass and can be edited by hand; a row whose JSON a page cannot parse
    /// must cost that one card, never the page it would have been drawn on, so a broken
    /// row is reported as absent and the caller asks for a fresh briefing like any other
    /// missing one. A caller has no way to correct a row it cannot see, and an error here
    /// would turn a cosmetic problem into a blank screen.
    pub fn briefing_get(&self, scope: &str, key: &str, hours: i64) -> Result<Option<Briefing>> {
        let row = match self.query_one(
            "SELECT scope,key,hours,watermark,created,line,bullets,articles,model,ms \
             FROM briefings WHERE scope=? AND key=? AND hours=?",
            &[
                SqlValue::Text(scope.to_string()),
                SqlValue::Text(key.to_string()),
                SqlValue::Integer(hours),
            ],
        ) {
            Ok(row) => row,
            // An archive that predates the table: the contract's other half is that every
            // reader tolerates its absence, and this is the read that would otherwise turn
            // "no prepared text yet" into a failed page. Nothing is created here — reading
            // must not write.
            Err(err) if err.message().contains("no such table: briefings") => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(row.and_then(|row| {
            let bullets = parse_bullets(row.text("bullets").unwrap_or(""))?;
            Some(Briefing {
                scope: row.text("scope").unwrap_or("").to_string(),
                key: row.text("key").unwrap_or("").to_string(),
                hours: row.int("hours").unwrap_or(0),
                watermark: row.int("watermark").unwrap_or(0),
                created: row.text("created").unwrap_or("").to_string(),
                line: row.text("line").unwrap_or("").to_string(),
                bullets,
                articles: row.int("articles").unwrap_or(0),
                model: row.text("model").unwrap_or("").to_string(),
                ms: row.int("ms").unwrap_or(0),
            })
        }))
    }

    /// Write (or replace) one briefing.
    ///
    /// One statement, an upsert on the primary key: two writers racing over the same
    /// slot leave the later text, and a reader never sees a briefing half-replaced. The
    /// answer is `()` — *whether* there was an older row is not a fact a caller can act
    /// on, and a store that pre-read the slot to say so would open a race for a number
    /// nobody uses.
    ///
    /// The one guard on the upsert is the watermark: `WHERE excluded.watermark >=
    /// briefings.watermark`. Two claimers can overlap on one slot (an ask retried while the
    /// first read is still in the model), and without the guard the row went **backwards** —
    /// measured on a scratch table built from these columns: a 3 446-watermark row replaced by
    /// a 3 000-watermark one, so the window showed a briefing older than the one it replaced.
    /// A later write always wins; an earlier one leaves the fresher text alone.
    ///
    /// Every cap is checked here and **refused**, not applied silently — see
    /// [`MAX_LINE_CHARS`] and its siblings. A caller that hands over nine bullets is a
    /// caller whose prompt is not the one this archive was built around, and a store that
    /// cut the ninth would hide that from it.
    pub fn briefing_put(&self, briefing: &Briefing) -> Result<()> {
        check(briefing)?;
        let bullets = serde_json::to_string(&briefing.bullets).map_err(StoreError::from)?;
        self.exec(
            "INSERT INTO briefings(scope,key,hours,watermark,created,line,bullets,articles,model,ms) \
             VALUES(?,?,?,?,?,?,?,?,?,?) \
             ON CONFLICT(scope,key,hours) DO UPDATE SET watermark=excluded.watermark, \
             created=excluded.created, line=excluded.line, bullets=excluded.bullets, \
             articles=excluded.articles, model=excluded.model, ms=excluded.ms \
             WHERE excluded.watermark >= briefings.watermark",
            &[
                SqlValue::Text(briefing.scope.clone()),
                SqlValue::Text(briefing.key.clone()),
                SqlValue::Integer(briefing.hours),
                SqlValue::Integer(briefing.watermark),
                SqlValue::Text(briefing.created.clone()),
                SqlValue::Text(briefing.line.clone()),
                SqlValue::Text(bullets),
                SqlValue::Integer(briefing.articles),
                SqlValue::Text(briefing.model.clone()),
                SqlValue::Integer(briefing.ms),
            ],
        )?;
        Ok(())
    }
}

/// Every cap, in one place, so a refusal is a sentence the caller can act on.
fn check(briefing: &Briefing) -> Result<()> {
    let scope = briefing.scope.trim();
    if !SCOPES.contains(&scope) || briefing.scope.len() > MAX_SCOPE_BYTES {
        // Named, not paraphrased: the caller wrote it, and the set is the fix.
        return Err(StoreError::new(format!(
            "unknown briefing scope '{}' - expected one of {} (a scope is at most {} bytes)",
            crate::fields::truncate(&briefing.scope, 40),
            SCOPES.join(", "),
            MAX_SCOPE_BYTES
        )));
    }
    if briefing.key.len() > MAX_KEY_BYTES {
        return Err(StoreError::new(format!(
            "a briefing key is capped at {} bytes; this one is {}. A key is an address \
             (an ISO code, a place label, an id), not a document.",
            MAX_KEY_BYTES,
            briefing.key.len()
        )));
    }
    if briefing.hours < MIN_HOURS {
        return Err(StoreError::new(format!(
            "a briefing window is at least {} hour; this one is {}. A window of zero hours \
             is a row no reader asks for.",
            MIN_HOURS, briefing.hours
        )));
    }
    if briefing.created.trim().is_empty() {
        return Err(StoreError::new(
            "a briefing needs the ISO timestamp it was written at (`created`)",
        ));
    }
    let line_chars = briefing.line.chars().count();
    if line_chars > MAX_LINE_CHARS {
        return Err(StoreError::new(format!(
            "a briefing line is capped at {} characters; this one is {}. The line is the \
             sentence a card shows, not the article.",
            MAX_LINE_CHARS, line_chars
        )));
    }
    if briefing.bullets.len() > MAX_BULLETS {
        return Err(StoreError::new(format!(
            "a briefing carries at most {} bullets; this one carries {}",
            MAX_BULLETS,
            briefing.bullets.len()
        )));
    }
    for bullet in &briefing.bullets {
        let chars = bullet.chars().count();
        if chars > MAX_BULLET_CHARS {
            return Err(StoreError::new(format!(
                "a briefing bullet is capped at {} characters; this one is {}. A bullet is \
                 a clause, not a paragraph.",
                MAX_BULLET_CHARS, chars
            )));
        }
    }
    if briefing.articles < 0 {
        return Err(StoreError::new(format!(
            "a briefing cannot be built from a negative number of stories ({} were given)",
            briefing.articles
        )));
    }
    if briefing.ms < 0 {
        return Err(StoreError::new(format!(
            "a briefing cannot take a negative number of milliseconds ({} was given)",
            briefing.ms
        )));
    }
    Ok(())
}

/// The stored `bullets` text back as a list, or `None` when it is not one.
///
/// Strict on purpose: an array of strings, nothing else. A number, an object or a nested
/// array in that column is a row somebody wrote by hand, and drawing a debug
/// representation of it on a briefing card would be worse than drawing no card.
fn parse_bullets(text: &str) -> Option<Vec<String>> {
    match serde_json::from_str::<Value>(text).ok()? {
        Value::Array(items) => {
            let mut out: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(bullet) => out.push(bullet),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::store;

    /// A briefing in the shape the ingest pass writes: a sentence and three bullets.
    fn sample() -> Briefing {
        Briefing {
            scope: "country".to_string(),
            key: "DEU".to_string(),
            hours: 72,
            watermark: 3444,
            created: "2026-11-22T12:00:00Z".to_string(),
            line: "Germany is arguing about its chip subsidies this week.".to_string(),
            bullets: vec![
                "The ministry put the package at 4.2 billion euros.".to_string(),
                "Two states want the money spent on fabs, not on design.".to_string(),
                "A vote is expected before the winter break.".to_string(),
            ],
            articles: 12,
            model: "local".to_string(),
            ms: 1_840,
        }
    }

    #[test]
    fn a_briefing_goes_in_and_comes_back_with_its_bullets() {
        let store = store();
        let briefing = sample();
        store.briefing_put(&briefing).expect("a write");
        let read = store
            .briefing_get("country", "DEU", 72)
            .expect("a read")
            .expect("the row");
        assert_eq!(read, briefing, "the round trip lost a field");
        assert_eq!(read.bullets.len(), 3);
        // The bullets are JSON in the column, so a reader that is not this one (a
        // `sqlite3` shell, a future crate) can read them without knowing a format.
        let raw = store
            .query_one(
                "SELECT bullets FROM briefings WHERE scope='country' AND key='DEU' AND hours=72",
                &[],
            )
            .expect("a query")
            .expect("a row");
        assert_eq!(
            raw.text("bullets"),
            Some(r#"["The ministry put the package at 4.2 billion euros.","Two states want the money spent on fabs, not on design.","A vote is expected before the winter break."]"#)
        );
        // A slot that was never written is absent, not an error.
        assert!(store.briefing_get("country", "FRA", 72).expect("a read").is_none());
        assert!(store.briefing_get("window", "", 72).expect("a read").is_none());
    }

    #[test]
    fn a_second_briefing_replaces_the_first_in_the_same_slot() {
        let store = store();
        store.briefing_put(&sample()).expect("a write");
        let newer = Briefing {
            watermark: 3460,
            created: "2026-11-22T13:00:00Z".to_string(),
            line: "Germany approved the package on Thursday.".to_string(),
            bullets: vec!["The vote passed 402 to 311.".to_string()],
            articles: 14,
            ms: 1_210,
            ..sample()
        };
        store.briefing_put(&newer).expect("a write");
        let read = store
            .briefing_get("country", "DEU", 72)
            .expect("a read")
            .expect("the row");
        assert_eq!(read, newer, "the old text survived the replace");
        // One row, not two: the primary key is the slot.
        let count = store
            .query_one("SELECT COUNT(*) AS n FROM briefings", &[])
            .expect("a query")
            .expect("a row");
        assert_eq!(count.int("n"), Some(1));
        // A different window is a different slot — the reader asking for 24 hours is not
        // asking for the 72-hour text.
        let shorter = Briefing {
            hours: 24,
            ..sample()
        };
        store.briefing_put(&shorter).expect("a write");
        assert_eq!(
            store.briefing_get("country", "DEU", 24).expect("a read"),
            Some(shorter)
        );
    }

    #[test]
    fn an_unknown_scope_is_refused_and_the_sentence_names_it() {
        let store = store();
        let wrong = Briefing {
            scope: "weather".to_string(),
            ..sample()
        };
        let error = store.briefing_put(&wrong).expect_err("an unknown scope must not land");
        assert!(
            error.message().contains("unknown briefing scope 'weather'"),
            "the refusal does not name the scope: {}",
            error.message()
        );
        assert!(
            error.message().contains("window, country, place, topic, monitor, entity"),
            "the refusal does not offer the set: {}",
            error.message()
        );
        // A scope assembled from page text is refused by the same sentence.
        let long = Briefing {
            scope: "the whole front page please".to_string(),
            ..sample()
        };
        assert!(store
            .briefing_put(&long)
            .expect_err("a scope past the cap must not land")
            .message()
            .contains("unknown briefing scope"));
        // And nothing was stored by either refusal.
        let count = store
            .query_one("SELECT COUNT(*) AS n FROM briefings", &[])
            .expect("a query")
            .expect("a row");
        assert_eq!(count.int("n"), Some(0));
    }

    #[test]
    fn nine_bullets_are_refused_rather_than_cut() {
        let store = store();
        let too_many = Briefing {
            bullets: (0..9).map(|index| format!("bullet {index}")).collect(),
            ..sample()
        };
        let error = store
            .briefing_put(&too_many)
            .expect_err("a ninth bullet must be refused");
        assert!(
            error.message().contains("at most 8 bullets") && error.message().contains("9"),
            "the refusal does not count: {}",
            error.message()
        );
        assert_eq!(
            store.briefing_get("country", "DEU", 72).expect("a read"),
            None,
            "a refused briefing left a row"
        );
        // Exactly the cap goes in.
        let eight = Briefing {
            bullets: (0..MAX_BULLETS).map(|index| format!("bullet {index}")).collect(),
            ..sample()
        };
        store.briefing_put(&eight).expect("the cap itself is allowed");
        assert_eq!(
            store
                .briefing_get("country", "DEU", 72)
                .expect("a read")
                .map(|read| read.bullets.len()),
            Some(MAX_BULLETS)
        );
    }

    #[test]
    fn a_bullet_a_line_and_a_key_past_their_caps_are_refused() {
        let store = store();
        let long_bullet = Briefing {
            bullets: vec!["a".repeat(MAX_BULLET_CHARS + 1)],
            ..sample()
        };
        assert!(store
            .briefing_put(&long_bullet)
            .expect_err("a bullet past the cap must not land")
            .message()
            .contains("bullet is capped at 240 characters"));
        let long_line = Briefing {
            line: "a".repeat(MAX_LINE_CHARS + 1),
            ..sample()
        };
        assert!(store
            .briefing_put(&long_line)
            .expect_err("a line past the cap must not land")
            .message()
            .contains("line is capped at 400 characters"));
        let long_key = Briefing {
            key: "k".repeat(MAX_KEY_BYTES + 1),
            ..sample()
        };
        assert!(store
            .briefing_put(&long_key)
            .expect_err("a key past the cap must not land")
            .message()
            .contains("key is capped at 128 bytes"));
        // The caps are counted in characters, not bytes: a line of 400 umlauts is 800
        // bytes and still fits the card.
        let wide = Briefing {
            line: "ä".repeat(MAX_LINE_CHARS),
            ..sample()
        };
        store.briefing_put(&wide).expect("400 characters are 400 characters");
        assert_eq!(
            store
                .briefing_get("country", "DEU", 72)
                .expect("a read")
                .map(|read| read.line.chars().count()),
            Some(MAX_LINE_CHARS)
        );
    }

    #[test]
    fn a_window_and_a_count_that_cannot_be_true_are_refused() {
        let store = store();
        let no_window = Briefing {
            hours: 0,
            ..sample()
        };
        assert!(store
            .briefing_put(&no_window)
            .expect_err("a window of zero hours must not land")
            .message()
            .contains("at least 1 hour"));
        let negative = Briefing {
            articles: -1,
            ..sample()
        };
        assert!(store
            .briefing_put(&negative)
            .expect_err("a negative count must not land")
            .message()
            .contains("negative number of stories"));
        let negative_ms = Briefing {
            ms: -5,
            ..sample()
        };
        assert!(store
            .briefing_put(&negative_ms)
            .expect_err("a negative duration must not land")
            .message()
            .contains("negative number of milliseconds"));
        let no_stamp = Briefing {
            created: "  ".to_string(),
            ..sample()
        };
        assert!(store
            .briefing_put(&no_stamp)
            .expect_err("a briefing without its timestamp must not land")
            .message()
            .contains("ISO timestamp"));
    }

    #[test]
    fn an_archive_without_the_table_reads_as_absent_and_is_not_written_to() {
        // The contract's other half: every reader tolerates the table's absence. A store
        // that never ran `init` (an archive from before this table, opened read-mostly)
        // must answer "no prepared text", and the read must not create anything.
        let bare = Store::open_memory().expect("memory store");
        assert_eq!(
            bare.briefing_get("window", "", 72).expect("a read"),
            None,
            "a missing table must read as 'no briefing yet'"
        );
        let tables = bare
            .query_one(
                "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table' AND name='briefings'",
                &[],
            )
            .expect("a query")
            .expect("a row");
        assert_eq!(tables.int("n"), Some(0), "a read created the table");
        // And `briefings_init` is the one call that does create it — idempotently.
        bare.briefings_init().expect("the table");
        bare.briefings_init().expect("a second call changes nothing");
        bare.briefing_put(&sample()).expect("a write");
        assert!(bare.briefing_get("country", "DEU", 72).expect("a read").is_some());
    }

    #[test]
    fn a_row_with_broken_json_reads_as_absent_rather_than_an_error() {
        let store = store();
        // A row written by hand, or by a caller that stored `bullets` itself: the table
        // allows it, so the reader must survive it. Each of these is a page that shows no
        // card, not a page that fails.
        for broken in [
            "not json at all",
            "[1, 2, 3]",
            "{\"line\": \"an object\"}",
            "[\"one\", {\"two\": 2}]",
            "[] trailing",
        ] {
            store
                .exec(
                    "INSERT OR REPLACE INTO briefings(scope,key,hours,watermark,created,line,bullets,\
                     articles,model,ms) VALUES('place','Berlin',72,7,'2026-11-22T12:00:00Z','a line',\
                     ?,3,'local',10)",
                    &[SqlValue::Text(broken.to_string())],
                )
                .expect("a hand-written row");
            assert_eq!(
                store.briefing_get("place", "Berlin", 72).expect("a read"),
                None,
                "{broken} should have read as absent"
            );
        }
        // The same slot with real JSON reads normally again — the rule is about the row,
        // not about a row that failed once.
        let good = Briefing {
            scope: "place".to_string(),
            key: "Berlin".to_string(),
            ..sample()
        };
        store.briefing_put(&good).expect("a write");
        assert_eq!(
            store.briefing_get("place", "Berlin", 72).expect("a read"),
            Some(good)
        );
        // An empty array is a briefing with no bullets, not a broken row: the model may
        // legitimately answer only a line.
        let line_only = Briefing {
            scope: "window".to_string(),
            key: String::new(),
            bullets: Vec::new(),
            ..sample()
        };
        store.briefing_put(&line_only).expect("a write");
        assert_eq!(
            store.briefing_get("window", "", 72).expect("a read"),
            Some(line_only)
        );
    }

    #[test]
    fn every_scope_of_the_set_round_trips_and_the_set_is_what_the_pages_read() {
        let store = store();
        for scope in SCOPES {
            let briefing = Briefing {
                scope: scope.to_string(),
                key: match scope {
                    "window" => String::new(),
                    other => format!("{other}-1"),
                },
                ..sample()
            };
            store.briefing_put(&briefing).expect("every scope of the set goes in");
        }
        let count = store
            .query_one("SELECT COUNT(*) AS n FROM briefings", &[])
            .expect("a query")
            .expect("a row");
        assert_eq!(count.int("n"), Some(SCOPES.len() as i64));
        // The index the page reads through exists, so this is not a full scan.
        let plan = store
            .query(
                "EXPLAIN QUERY PLAN SELECT line FROM briefings WHERE scope='country' AND hours=72 \
                 ORDER BY created DESC LIMIT 1",
                &[],
            )
            .expect("a plan");
        let text: String = plan
            .iter()
            .filter_map(|row| row.text("detail"))
            .collect::<Vec<&str>>()
            .join(" | ");
        assert!(
            text.contains("idx_briefings_scope"),
            "the scope read is a scan: {text}"
        );
    }
}
