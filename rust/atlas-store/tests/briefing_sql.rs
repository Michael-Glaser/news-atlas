//! The `briefings` table's SQL surface: what a page's key may contain, what the caps
//! refuse, and what a hand-written row does to the reader.
//!
//! The table (`atlas-store/src/briefings.rs`) is written by a model-driven pass and read
//! by every page that shows prepared text, so three things have to hold at the boundary
//! rather than in the caller:
//!
//! 1. **A key is data.** `key` is the scope's own address — an ISO code, a place label, an
//!    id as text — and it arrives from page text, from a topic the reader typed, from a
//!    country name the model picked. So the statements that store and read it bind it
//!    (`briefing_put`'s `VALUES(?,?,…)`, `briefing_get`'s `WHERE scope=? AND key=? AND
//!    hours=?`), and the corpus below goes in and comes back unchanged with the table and
//!    its definition intact. `scope` is the opposite case: it is a closed set, so a scope
//!    that looks like SQL is not data at all — it is refused by name, and the refusal
//!    quotes it back without ever becoming a statement.
//! 2. **A cap is a refusal, never a truncation.** A key is an address, not a document:
//!    [`MAX_KEY_BYTES`] is the width, 128 bytes go in, 129 do not, and the sentence names
//!    both numbers. A briefing the store shortened would read like a model that stopped
//!    mid-sentence, and the caller could not tell the difference.
//! 3. **A row nobody wrote through the API costs one card, not the page.** `bullets` is
//!    JSON text; a hand-edited or half-written value answers `Ok(None)` from
//!    `briefing_get`, never an error, and the neighbouring slots still read.
//!
//! No network, no ports: every archive is a temporary file under `std::env::temp_dir()`,
//! created and removed by the test.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use atlas_store::briefings::{MAX_KEY_BYTES, SCOPES};
use atlas_store::{Briefing, SqlValue, Store};

// ------------------------------------------------------------------------ fixtures

/// A temporary root, removed when the test ends.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A directory no other test uses: the process id plus a counter, because two fixtures
/// alive at once must not share a database.
fn temp_root(label: &str) -> Root {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-store-briefing-{}-{}-{}",
        label,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    Root(root)
}

/// An archive with the real schema, `briefings` included (`Store::init` creates it).
fn archive(root: &Root) -> Store {
    let store = Store::open(root.0.join("data").join("atlas.db")).expect("open");
    store.init().expect("schema");
    store
}

/// A briefing in the shape `atlas-ingest`'s brief pass writes.
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
        ],
        articles: 12,
        model: "local".to_string(),
        ms: 1_840,
    }
}

/// How many rows the table holds — the number that says whether an upsert grew it.
fn rows(store: &Store) -> i64 {
    store
        .query_one("SELECT COUNT(*) AS n FROM briefings", &[])
        .expect("count")
        .and_then(|row| row.int("n"))
        .unwrap_or(-1)
}

/// Every table in the file, sorted: the shape an injection must not change.
fn tables(store: &Store) -> Vec<String> {
    store
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
            &[],
        )
        .expect("tables")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect()
}

/// The `briefings` table's own DDL, as SQLite stored it.
fn ddl(store: &Store) -> Option<String> {
    store
        .query_one(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='briefings'",
            &[],
        )
        .expect("ddl")
        .and_then(|row| row.text("sql").map(|text| text.to_string()))
}

// ------------------------------------------------------------------------ 1. a key is data

/// Keys that look like SQL are addresses, and they come back exactly as they went in.
///
/// Each of these is what a place label or a page's address can look like when the value
/// came from a model or from a reader's typing: a quote and a `DROP`, a boolean tautology,
/// a sub-select assembled into a string, a statement after a semicolon, a right-to-left
/// override, a newline, a NUL. If any of them were interpolated, the first would delete
/// the table the rest are written to — so the test asserts the table and its DDL survive
/// and that the row count is exactly one per key.
#[test]
fn a_key_with_sql_metacharacters_round_trips_as_data() {
    let root = temp_root("keys");
    let store = archive(&root);
    let tables_before = tables(&store);
    let ddl_before = ddl(&store).expect("the briefings table exists");

    let keys: [&str; 7] = [
        "'); DROP TABLE briefings; --",
        "\" OR 1=1 --",
        "place'||(SELECT line FROM briefings LIMIT 1)||'",
        "x'; DELETE FROM briefings; --",
        "\u{202e}DEU\u{202c}",
        "key with a\nnewline",
        "key with a \u{0} nul",
    ];
    for (index, key) in keys.iter().enumerate() {
        let briefing = Briefing {
            scope: "place".to_string(),
            key: key.to_string(),
            hours: 72,
            line: format!("briefing {index}"),
            ..sample()
        };
        store
            .briefing_put(&briefing)
            .unwrap_or_else(|error| panic!("key {key:?} was refused: {}", error.message()));
        let read = store
            .briefing_get("place", key, 72)
            .unwrap_or_else(|error| panic!("key {key:?} broke the read: {}", error.message()))
            .unwrap_or_else(|| panic!("key {key:?} did not come back"));
        assert_eq!(
            read.key, *key,
            "the key changed on the way through the table"
        );
        assert_eq!(read.line, briefing.line);
        assert_eq!(read.bullets, briefing.bullets);
    }

    // The table the first key tried to drop is still there, defined the same way, and the
    // schema is what it was.
    assert_eq!(tables(&store), tables_before, "a table was created or dropped");
    assert_eq!(ddl(&store), Some(ddl_before), "the DDL changed");
    assert_eq!(
        rows(&store),
        keys.len() as i64,
        "each key is one slot, and no statement added or removed a row"
    );
    let integrity = store
        .query("PRAGMA integrity_check", &[])
        .expect("integrity")
        .first()
        .and_then(|row| row.get_at(0))
        .map(|value| match value {
            SqlValue::Text(text) => text.clone(),
            _ => String::new(),
        });
    assert_eq!(integrity.as_deref(), Some("ok"), "the file is still sound");
}

/// A scope that looks like SQL is not data: `scope` is a closed set, so the write is
/// refused, the sentence quotes the value and lists the set, and nothing lands.
///
/// This is the other half of the rule above — a key may be anything, a scope may not —
/// and both have to hold at the same boundary: the scope check runs *before* the
/// statement, so the value in the sentence never becomes SQL either.
#[test]
fn a_scope_with_sql_metacharacters_is_refused_by_name_and_nothing_is_written() {
    let root = temp_root("scopes");
    let store = archive(&root);

    let scopes = [
        "weather",
        "'; DROP TABLE briefings; --",
        "the whole front page please",
        "COUNTRY",
    ];
    for scope in scopes {
        let briefing = Briefing {
            scope: scope.to_string(),
            ..sample()
        };
        let message = match store.briefing_put(&briefing) {
            Ok(()) => panic!("scope {scope:?} was stored"),
            Err(error) => error.message().to_string(),
        };
        assert!(
            message.contains("unknown briefing scope"),
            "the refusal says what it refused: {message}"
        );
        assert!(
            message.contains(SCOPES.join(", ").as_str()),
            "and offers the set a caller can correct to: {message}"
        );
        // The sentence quotes the caller's own value — the first 40 characters of it — so
        // a long hostile scope cannot become an essay in the log.
        let quoted = scope.chars().take(40).collect::<String>();
        assert!(
            message.contains(quoted.as_str()),
            "the refusal quotes {quoted:?}: {message}"
        );
    }
    assert_eq!(rows(&store), 0, "a refused scope left a row");
    assert!(tables(&store).contains(&"briefings".to_string()));
    // Every scope of the real set still works, so the refusal is about the value and not
    // about the table being closed for business.
    for scope in SCOPES {
        let briefing = Briefing {
            scope: scope.to_string(),
            ..sample()
        };
        store
            .briefing_put(&briefing)
            .unwrap_or_else(|error| panic!("scope {scope:?}: {}", error.message()));
    }
    assert_eq!(rows(&store), SCOPES.len() as i64);
}

// ------------------------------------------------------------------------ 2. the cap

/// 128 bytes of key are an address; 129 are a document, and the write is refused with both
/// numbers in the sentence.
///
/// The boundary is pinned at the constant, so tightening the cap later cannot quietly
/// start refusing a place name the map really holds, and loosening it cannot quietly
/// admit a page of text into a primary key.
#[test]
fn a_key_of_128_bytes_goes_in_and_129_is_refused() {
    let root = temp_root("keycap");
    let store = archive(&root);

    let at_cap = "k".repeat(MAX_KEY_BYTES);
    let briefing = Briefing {
        scope: "place".to_string(),
        key: at_cap.clone(),
        ..sample()
    };
    store
        .briefing_put(&briefing)
        .expect("a key at the cap is an address");
    assert_eq!(
        store
            .briefing_get("place", &at_cap, 72)
            .expect("read")
            .map(|read| read.key),
        Some(at_cap.clone()),
        "the key at the cap round-trips"
    );

    let over = "k".repeat(MAX_KEY_BYTES + 1);
    let refused = Briefing {
        scope: "place".to_string(),
        key: over.clone(),
        line: "a briefing that must not land".to_string(),
        ..sample()
    };
    let message = match store.briefing_put(&refused) {
        Ok(()) => panic!("a {}-byte key was stored", over.len()),
        Err(error) => error.message().to_string(),
    };
    assert!(
        message.contains(&MAX_KEY_BYTES.to_string()),
        "the sentence names the cap {MAX_KEY_BYTES}: {message}"
    );
    assert!(
        message.contains(&over.len().to_string()),
        "and the length it was handed: {message}"
    );
    assert_eq!(
        store.briefing_get("place", &over, 72).expect("read"),
        None,
        "a refused key left a row"
    );
    assert_eq!(rows(&store), 1, "the good slot is the only row");
}

// ------------------------------------------------------------------------ 3. bullets

/// Bullets are text that reaches JSON and back: quotes, a newline, a backslash and German
/// quotation marks survive unchanged, and the column holds **one line** of valid JSON —
/// a newline inside a bullet is escaped, so nothing in a bullet can break the row it is
/// stored in or the reader that parses it.
#[test]
fn bullets_with_quotes_and_a_newline_survive_as_json() {
    let root = temp_root("bullets");
    let store = archive(&root);

    let bullets = vec![
        "He said \"no\" to the plan, and 'maybe' to the other one.".to_string(),
        "A line\nwith a break in it.".to_string(),
        "A back\\slash and \u{201e}German\u{201c} quotes.".to_string(),
        "A tab\tand an emoji \u{1f4c8}.".to_string(),
    ];
    let briefing = Briefing {
        scope: "place".to_string(),
        key: "Berlin".to_string(),
        bullets: bullets.clone(),
        ..sample()
    };
    store.briefing_put(&briefing).expect("a write");
    assert_eq!(
        store
            .briefing_get("place", "Berlin", 72)
            .expect("read")
            .expect("the row")
            .bullets,
        bullets,
        "the bullets did not round-trip"
    );

    let raw = store
        .query_one(
            "SELECT bullets FROM briefings WHERE scope='place' AND key='Berlin' AND hours=72",
            &[],
        )
        .expect("query")
        .expect("row")
        .text("bullets")
        .expect("text")
        .to_string();
    // One line of JSON: the newline is an escape, not a byte in the column.
    assert!(
        !raw.contains('\n'),
        "a newline in a bullet reached the column: {raw}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("the column is JSON");
    let strings: Vec<String> = parsed
        .as_array()
        .expect("an array")
        .iter()
        .map(|item| item.as_str().unwrap_or("<not a string>").to_string())
        .collect();
    assert_eq!(strings, bullets, "the column is the list, as strings");
    // And a quote in a bullet is escaped in the JSON, not left to end the string early.
    assert!(
        raw.contains("\\\"no\\\""),
        "the quote was not escaped: {raw}"
    );
}

// ------------------------------------------------------------------------ 4. the upsert

/// A put is one row in its slot: three writes to the same `(scope, key, hours)` leave one
/// row carrying the last text, and a different window is a different slot.
///
/// The table is keyed the way a page reads it, so "never grows" is the property that keeps
/// a refresh loop (a burst of twelve stories) from turning the shelf into a log.
#[test]
fn the_upsert_replaces_its_slot_and_never_grows_the_table() {
    let root = temp_root("upsert");
    let store = archive(&root);

    for index in 0..3 {
        let briefing = Briefing {
            scope: "country".to_string(),
            key: "DEU".to_string(),
            hours: 72,
            watermark: 3444 + index,
            created: format!("2026-11-22T1{index}:00:00Z"),
            line: format!("Germany, take {index}."),
            bullets: vec![format!("the {index}. bullet")],
            ..sample()
        };
        store.briefing_put(&briefing).expect("a write");
        assert_eq!(rows(&store), 1, "a refresh grew the table");
    }
    let read = store
        .briefing_get("country", "DEU", 72)
        .expect("read")
        .expect("the row");
    assert_eq!(read.line, "Germany, take 2.", "the last write wins");
    assert_eq!(read.watermark, 3446);
    assert_eq!(read.created, "2026-11-22T12:00:00Z");

    // A different window is a different slot, and writing it does not touch the first.
    let shorter = Briefing {
        hours: 24,
        line: "Germany, last 24 hours.".to_string(),
        ..sample()
    };
    store.briefing_put(&shorter).expect("a write");
    assert_eq!(rows(&store), 2, "a second window is a second slot");
    assert_eq!(
        store
            .briefing_get("country", "DEU", 72)
            .expect("read")
            .map(|read| read.line),
        Some("Germany, take 2.".to_string()),
        "the 72-hour slot was overwritten by the 24-hour one"
    );
    // And re-writing the 72-hour slot still leaves two rows.
    store.briefing_put(&sample()).expect("a write");
    assert_eq!(rows(&store), 2);
}

// ------------------------------------------------------------------------ 5. a broken row

/// A row nobody wrote through the API answers `None`, not an error — and its neighbour
/// still reads.
///
/// `bullets` is JSON in a column that a person can edit and a crash can truncate. Each
/// value below is what such a row looks like; the contract is that it costs its own card
/// and no more, because a page that failed would hide every other briefing on it.
#[test]
fn a_hand_written_row_with_broken_json_answers_none_and_costs_one_card() {
    let root = temp_root("broken");
    let store = archive(&root);

    // A neighbour that is perfectly fine: it must read before and after each broken row.
    let good = Briefing {
        scope: "place".to_string(),
        key: "Munich".to_string(),
        ..sample()
    };
    store.briefing_put(&good).expect("a write");

    let broken = [
        "not json at all",
        "[1, 2, 3]",
        "{\"line\": \"an object\"}",
        "[\"one\", {\"two\": 2}]",
        "[] trailing",
        "\"a bare string\"",
        "null",
    ];
    for value in broken {
        store
            .exec(
                "INSERT OR REPLACE INTO briefings(scope,key,hours,watermark,created,line,bullets,\
                 articles,model,ms) VALUES('place','Berlin',72,7,'2026-11-22T12:00:00Z',\
                 'a hand-written line',?,3,'local',10)",
                &[SqlValue::Text(value.to_string())],
            )
            .expect("a hand-written row");
        assert_eq!(
            store.briefing_get("place", "Berlin", 72).expect("a read must not fail"),
            None,
            "the row holding {value:?} was read as a briefing"
        );
        assert_eq!(
            store.briefing_get("place", "Munich", 72).expect("a read"),
            Some(good.clone()),
            "the broken row cost its neighbour's card"
        );
    }

    // The same slot reads normally again once a real briefing is written to it, and an
    // empty array is a briefing with no bullets rather than a broken row.
    let repaired = Briefing {
        scope: "place".to_string(),
        key: "Berlin".to_string(),
        bullets: Vec::new(),
        ..sample()
    };
    store.briefing_put(&repaired).expect("a write");
    assert_eq!(
        store.briefing_get("place", "Berlin", 72).expect("a read"),
        Some(repaired)
    );
}
