//! The archive layer, against a temporary SQLite file built here.
//!
//! `atlas_desktop::data` is where the window's numbers come from, and every one of
//! them is a SQL statement: the freshness window, the `relevant = 1` / `dupe = 0`
//! pair, the place pins, the relations among a set of entities, the topics' order. A
//! fixture archive of a dozen rows pins all six at once, which is the only way to see
//! the statements rather than the pixels.
//!
//! # How this file reaches the code under test
//!
//! `use atlas_desktop::data::Archive` — the crate's library half. That is worth a
//! sentence because it was not always possible: while `atlas-desktop` was a binary
//! only, every test in this directory reached into `src/` with `#[path]`, which meant
//! a test could not see a `pub(crate)` item, one file's `#[cfg(test)] mod tests;`
//! resolved against the *test's* directory (`E0583: file not found for module tests`),
//! and a symlink under `tests/support/` had to anchor that lookup. The lib target
//! (`src/lib.rs`) removed all three, so this target links the crate and the shim is
//! gone.
//!
//! # The database
//!
//! Created per test in the system temp directory, never in `data/`. The schema is
//! spelled out below with *only the columns the app reads*, mirroring
//! `atlas-store/src/schema.rs` (base tables plus the migrations that added
//! `publisher`, `relevant`, `dupe`, `full_text`) and `atlas-store/src/monitors.rs`
//! (`monitors`, `alerts`). A column this app selects and this fixture omits would
//! turn a query into an empty `Vec` (every read in `data.rs` answers an error with
//! an empty list), so the fixture has to be honest about the shape.
//!
//! # The state of the archive layer when this file was written
//!
//! `Archive::open` could not open any archive at all for a while — it read
//! `PRAGMA query_only` (an INTEGER) into a `String`, and `rusqlite` refuses the
//! conversion, so every open answered
//! `cannot set query_only: Invalid column type Integer at index: 0, name: query_only`.
//! It now *sets* the pragma (`conn.pragma_update(None, "query_only", true)`) and the
//! tests below run against a real file. `archive()` still names the reason when an
//! open fails, so a regression reads as one sentence instead of six broken asserts.


use std::path::{Path, PathBuf};

use rusqlite::Connection;

use atlas_desktop::data::Archive;

// --------------------------------------------------------------------------- fixture

/// A temp database, removed when the test ends.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    /// A database with the columns the window reads and nothing in it.
    fn new(name: &str) -> TempDb {
        let path = std::env::temp_dir().join(format!(
            "atlas-desktop-archive-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("a temp file is writable");
        conn.execute_batch(SCHEMA).expect("the fixture schema");
        drop(conn);
        TempDb { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// The fixture's own handle, for inserting rows. `Archive` is opened read-only,
    /// so rows are written here and read there — the same split the app has.
    fn write(&self) -> Connection {
        Connection::open(&self.path).expect("the fixture is openable twice")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.path.display(), suffix));
        }
    }
}

/// The columns the app selects. Source of truth: `atlas-store/src/schema.rs`
/// (`SCHEMA` + the `MIGRATIONS` that add `publisher`, `relevant`, `dupe`,
/// `full_text`) and `atlas-store/src/monitors.rs` (`monitors`, `alerts`).
const SCHEMA: &str = "
CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT, url TEXT, active INTEGER DEFAULT 1);
CREATE TABLE articles(
  id INTEGER PRIMARY KEY, source_id INTEGER, url TEXT, title TEXT, summary TEXT,
  content TEXT, published TEXT, fetched TEXT, iso TEXT, lat REAL, lon REAL,
  relevant INTEGER DEFAULT 0, dupe INTEGER DEFAULT 0, full_text INTEGER DEFAULT 0,
  publisher TEXT, place TEXT, geo_src TEXT);
CREATE TABLE entities(
  id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER DEFAULT 1,
  first_seen TEXT, last_seen TEXT);
CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL DEFAULT 1, type TEXT, label TEXT);
CREATE TABLE article_links(a INTEGER, b INTEGER, shared INTEGER, score REAL, why TEXT);
CREATE TABLE topics(
  id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, active INTEGER DEFAULT 1,
  hits INTEGER DEFAULT 0, last_hit TEXT);
CREATE TABLE monitors(
  id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER DEFAULT 1,
  cadence_minutes INTEGER DEFAULT 0, last_run TEXT, hits INTEGER DEFAULT 0);
CREATE TABLE alerts(id INTEGER PRIMARY KEY, monitor_id INTEGER, article_id INTEGER);
";

/// `now - hours` as the archive stores it: UTC, second precision, `2026-11-21T09:25:29Z`.
///
/// Fixture data only — this is how the *test* places a row at a known age. The code
/// under test derives its own bound (`data::since_iso`, private), which is the thing
/// `the_window_is_the_last_n_hours` is measuring.
fn iso_ago(hours: f64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs() as i64;
    let stamp = now - (hours * 3_600.0) as i64;
    let (days, secs) = (stamp.div_euclid(86_400), stamp.rem_euclid(86_400));
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

/// One article, with every column the app reads spelled out so a test can vary one
/// of them.
struct Row<'a> {
    title: &'a str,
    published: String,
    relevant: i64,
    dupe: i64,
    lat: Option<f64>,
    lon: Option<f64>,
    iso: &'a str,
    source_id: Option<i64>,
    publisher: Option<&'a str>,
    full_text: i64,
}

impl<'a> Row<'a> {
    /// The ordinary case: relevant, not a duplicate, full text, one place.
    fn fresh(title: &'a str, hours: f64) -> Row<'a> {
        Row {
            title,
            published: iso_ago(hours),
            relevant: 1,
            dupe: 0,
            lat: Some(52.5),
            lon: Some(13.4),
            iso: "DEU",
            source_id: Some(1),
            publisher: None,
            full_text: 1,
        }
    }
}

fn insert(conn: &Connection, row: &Row<'_>) {
    conn.execute(
        "INSERT INTO articles(url,title,summary,content,published,iso,lat,lon,relevant,dupe,\
         full_text,source_id,publisher) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        rusqlite::params![
            format!("https://news.example.invalid/{}", row.title),
            row.title,
            format!("Summary of {}.", row.title),
            format!("Body of {}. Chip export rules.", row.title),
            row.published,
            row.iso,
            row.lat,
            row.lon,
            row.relevant,
            row.dupe,
            row.full_text,
            row.source_id,
            row.publisher,
        ],
    )
    .expect("the fixture accepts the row");
}

/// The archive under test, or a panic that names the one line blocking it.
fn archive(db: &TempDb) -> Archive {
    match Archive::open(db.path()) {
        Ok(archive) => archive,
        Err(error) => panic!("a valid fixture archive must open: {error}"),
    }
}

// --------------------------------------------------------------------------- tests

#[test]
fn an_archive_that_is_not_there_is_a_sentence() {
    // The window opens on a fresh machine and says what is missing instead of
    // panicking: `Archive::open` returns the sentence, `main` shows it in the row.
    let missing = std::env::temp_dir().join("atlas-desktop-archive-not-here.db");
    let _ = std::fs::remove_file(&missing);
    let error = match Archive::open(&missing) {
        Ok(_) => panic!("a path with no file must not open"),
        Err(error) => error,
    };
    assert!(
        error.contains("no archive at") && error.contains("start ATLAS once"),
        "the sentence has to say what to do: {error}"
    );
}

#[test]
fn the_window_is_the_last_n_hours_of_relevant_non_dupes() {
    let db = TempDb::new("window");
    let conn = db.write();
    conn.execute(
        "INSERT INTO sources(id,name,url) VALUES(1,'Wire','https://wire.example.invalid')",
        [],
    )
    .unwrap();
    insert(&conn, &Row::fresh("one hour", 1.0));
    insert(&conn, &Row::fresh("seventy one hours", 71.0));
    insert(&conn, &Row::fresh("seventy three hours", 73.0));
    // A duplicate, a story the model did not keep, and a story with no timestamp:
    // none of the three is in any window.
    insert(&conn, &Row { title: "a duplicate", dupe: 1, ..Row::fresh("a duplicate", 2.0) });
    insert(&conn, &Row { title: "not relevant", relevant: 0, ..Row::fresh("not relevant", 2.0) });
    insert(&conn, &Row {
        title: "no stamp",
        published: String::new(),
        ..Row::fresh("no stamp", 2.0)
    });
    drop(conn);

    let archive = archive(&db);
    let rows = archive.articles(72, 50, "");
    let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(
        titles,
        vec!["one hour", "seventy one hours"],
        "72 hours is 1 and 71, newest first; 73 is outside, the dupe, the \
         irrelevant story and the unstamped row are never in a window"
    );
    assert_eq!(rows[0].source, "Wire", "the feed name is the publisher's fallback");
    assert!(rows[0].full_text, "full_text comes back as a bool");
    assert_eq!(rows[0].published.len(), 20, "the stamp keeps the archive's shape");

    // The limit is a limit, not a filter.
    assert_eq!(archive.articles(72, 1, "").len(), 1);
    // The needle is `LIKE` over title and content, and an empty needle is all rows.
    assert_eq!(archive.articles(72, 50, "seventy one").len(), 1);
    assert_eq!(archive.articles(72, 50, "Chip export").len(), 2, "the body matches too");
    assert!(archive.articles(72, 50, "nothing says this").is_empty());
}

#[test]
fn pins_drop_every_coordinate_that_is_not_a_place_on_earth() {
    let db = TempDb::new("pins");
    let conn = db.write();
    insert(&conn, &Row::fresh("berlin", 1.0));
    // 0,0 is the archive saying "no place" — the Gulf of Guinea is not a story.
    insert(&conn, &Row { lat: Some(0.0), lon: Some(0.0), iso: "XKX", ..Row::fresh("null island", 1.0) });
    // … but zero on one axis is a real place: the equator, or Greenwich.
    insert(&conn, &Row { lat: Some(0.0), lon: Some(13.4), iso: "GAB", ..Row::fresh("equator", 1.0) });
    insert(&conn, &Row { lat: Some(91.0), lon: Some(10.0), iso: "XKX", ..Row::fresh("north of the pole", 1.0) });
    insert(&conn, &Row { lat: Some(-95.0), lon: Some(10.0), iso: "XKX", ..Row::fresh("south of the pole", 1.0) });
    insert(&conn, &Row { lat: Some(10.0), lon: Some(181.0), iso: "XKX", ..Row::fresh("past the date line", 1.0) });
    insert(&conn, &Row { lat: Some(10.0), lon: Some(-181.5), iso: "XKX", ..Row::fresh("before the date line", 1.0) });
    // The edges of the domain are places: 90 and 180 are inclusive.
    insert(&conn, &Row { lat: Some(90.0), lon: Some(180.0), iso: "XKX", ..Row::fresh("the corner", 1.0) });
    // A story with no coordinate at all is not a pin either.
    insert(&conn, &Row { lat: None, lon: None, iso: "XKX", ..Row::fresh("nowhere", 1.0) });
    // Out of the window, a valid coordinate: pins share the window.
    insert(&conn, &Row { lat: Some(-33.9), lon: Some(151.2), iso: "AUS", ..Row::fresh("too old", 100.0) });
    drop(conn);

    let archive = archive(&db);
    let pins = archive.pins(72, 50);
    let mut places: Vec<(String, f64, f64)> = pins
        .iter()
        .map(|pin| (pin.place.clone(), pin.lat, pin.lon))
        .collect();
    // Every pin here has one story, so the statement's order is only `n DESC` and
    // the ties are the database's business: the *set* is what this test is about.
    places.sort_by(|left, right| left.partial_cmp(right).expect("coordinates compare"));
    assert_eq!(
        places,
        vec![
            ("DEU".to_string(), 52.5, 13.4),
            ("GAB".to_string(), 0.0, 13.4),
            ("XKX".to_string(), 90.0, 180.0),
        ],
        "one pin per place: 0,0 dropped, past either pole dropped, past either \
         date line dropped, no coordinate dropped, and the 100-hour story is out \
         of the window"
    );
    assert!(
        pins.iter().all(|pin| pin.count >= 1),
        "a pin counts the stories that named it"
    );
    // Two stories in the same place are one pin with a count of two.
    let conn = db.write();
    insert(&conn, &Row::fresh("berlin again", 2.0));
    drop(conn);
    let counted = archive.pins(72, 50);
    let berlin = counted
        .iter()
        .find(|pin| pin.place == "DEU")
        .expect("Berlin is a pin");
    assert_eq!(berlin.count, 2, "the pin aggregates its stories");
    assert_eq!(counted[0].place, "DEU", "the strongest place comes first");
}

#[test]
fn edges_only_join_the_ids_that_were_asked_for() {
    let db = TempDb::new("edges");
    let conn = db.write();
    for (id, name, count) in [(1, "Alpha Group", 30), (2, "Bravo Group", 20), (3, "Charlie Group", 10), (4, "Delta Group", 5)] {
        conn.execute(
            "INSERT INTO entities(id,name,type,count,last_seen) VALUES(?1,?2,'ORG',?3,?4)",
            rusqlite::params![id, name, count, iso_ago(1.0)],
        )
        .unwrap();
    }
    for (a, b, weight, label) in [(1, 2, 0.9, "chips"), (2, 3, 0.5, "trade"), (3, 4, 0.7, "export")] {
        conn.execute(
            "INSERT INTO edges(a,b,weight,label) VALUES(?1,?2,?3,?4)",
            rusqlite::params![a, b, weight, label],
        )
        .unwrap();
    }
    drop(conn);

    let archive = archive(&db);
    let drawn: Vec<(i64, i64, f64, String)> = archive
        .edges(&[1, 2, 3], 99)
        .into_iter()
        .map(|edge| (edge.a, edge.b, edge.weight, edge.label))
        .collect();
    assert_eq!(
        drawn,
        vec![
            (1, 2, 0.9, "chips".to_string()),
            (2, 3, 0.5, "trade".to_string()),
        ],
        "3-4 is out because 4 was not drawn, and the strongest edge comes first"
    );
    // A set of one id has no relations in it, and the empty set asks nothing.
    assert!(archive.edges(&[2], 99).is_empty());
    assert!(archive.edges(&[], 99).is_empty());
    // The limit is the limit.
    assert_eq!(archive.edges(&[1, 2, 3], 1).len(), 1);
    // A wide set sees every edge inside it.
    assert_eq!(archive.edges(&[1, 2, 3, 4], 99).len(), 3);
}

#[test]
fn topics_and_monitors_are_ordered_active_first() {
    let db = TempDb::new("topics");
    let conn = db.write();
    // Active first, then by hits — so a paused topic with more hits is still last.
    conn.execute(
        "INSERT INTO topics(id,name,keywords,active,hits,last_hit) VALUES\
         (1,'paused but busy','chips',0,50,'2026-01-01T00:00:00Z'),\
         (2,'quiet','trade',1,1,'2026-01-02T00:00:00Z'),\
         (3,'live','export',1,9,'2026-01-03T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO monitors(id,name,query,active,cadence_minutes,last_run,hits) VALUES\
         (1,'paused watch','chips',0,60,'2026-01-01T00:00:00Z',5),\
         (2,'live watch','trade',1,15,'2026-01-02T00:00:00Z',2),\
         (3,'second live watch','export',1,30,'2026-01-03T00:00:00Z',7)",
        [],
    )
    .unwrap();
    drop(conn);

    let archive = archive(&db);
    let topics: Vec<(i64, bool)> = archive.topics().iter().map(|t| (t.id, t.active)).collect();
    assert_eq!(
        topics,
        vec![(3, true), (2, true), (1, false)],
        "a paused topic cannot outrank a live one, however many hits it has"
    );
    let monitors: Vec<(i64, bool)> = archive.monitors().iter().map(|m| (m.id, m.active)).collect();
    assert_eq!(monitors, vec![(3, true), (2, true), (1, false)]);
    assert_eq!(archive.monitors()[0].cadence, 30, "cadence_minutes is read");
    assert_eq!(archive.topics()[0].last_hit, "2026-01-03T00:00:00Z");
}

#[test]
fn counts_count_the_whole_archive_not_the_window() {
    let db = TempDb::new("counts");
    let conn = db.write();
    insert(&conn, &Row::fresh("in the window", 1.0));
    insert(&conn, &Row { title: "a duplicate", dupe: 1, ..Row::fresh("a duplicate", 2.0) });
    insert(&conn, &Row { title: "too old", ..Row::fresh("too old", 500.0) });
    insert(&conn, &Row { title: "not relevant", relevant: 0, ..Row::fresh("not relevant", 3.0) });
    conn.execute("INSERT INTO entities(id,name,type,count) VALUES(1,'Alpha Group','ORG',3)", [])
        .unwrap();
    conn.execute("INSERT INTO edges(a,b,weight) VALUES(1,2,1.0)", []).unwrap();
    conn.execute("INSERT INTO article_links(a,b,shared,score) VALUES(1,2,1,0.5)", []).unwrap();
    conn.execute("INSERT INTO topics(id,name,active) VALUES(1,'live',1)", []).unwrap();
    conn.execute("INSERT INTO monitors(id,name,active) VALUES(1,'watch',1)", []).unwrap();
    conn.execute("INSERT INTO alerts(id,monitor_id) VALUES(1,1)", []).unwrap();
    drop(conn);

    let archive = archive(&db);
    let counts = archive.counts();
    assert_eq!(counts.articles, 4, "every row, duplicate and old and irrelevant");
    assert_eq!(counts.relevant, 3, "`relevant = 1` alone is the status row's number");
    assert_eq!((counts.entities, counts.edges, counts.links), (1, 1, 1));
    assert_eq!((counts.topics, counts.monitors, counts.alerts), (1, 1, 1));
}
