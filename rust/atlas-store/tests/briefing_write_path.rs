//! The `briefings` write path, audited at the SQL level against **copies**.
//!
//! The live archive is never opened here: every test copies `data/atlas.db` (and its `-wal`
//! and `-shm`, because a copy of a WAL database without them is a copy of an older state)
//! into a temporary directory and writes to that. The same for `data/requests.db`. A test
//! that finds no archive prints why and returns — the audit is about the real schema, so a
//! machine without the archive has nothing to say about it, and a suite that fails there
//! would be testing the developer's `data/` directory instead of the code.
//!
//! What is pinned, and why each is a *store* question rather than a caller's:
//!
//! 1. **The schema is the documented one.** Ten columns, all `NOT NULL`, the defaults the
//!    card relies on (`line ''`, `bullets '[]'`, `articles 0`, `model ''`, `ms 0`), the
//!    primary key `(scope, key, hours)` and the index `idx_briefings_scope` on
//!    `(scope, hours, created)` that the page's read uses. Measured on the live archive's
//!    copy, not reconstructed from the source: a migration that drifted would be invisible
//!    to a test that re-read the same constant the writer uses.
//! 2. **A put is an upsert of one row per slot**, so a refresh loop (a burst of stories, or
//!    the window asking twice for the same window) cannot turn the shelf into a log.
//! 3. **The caps are the store's**, not the prompt's: a line of 401 characters, a bullet of
//!    241, or nine bullets are refused with a sentence naming the number, and a caller that
//!    skipped `atlas_ai`'s parser still cannot put a card-sized lie in the table.
//! 4. **The claim path is not a second door.** A `brief` request is claimed from the queue
//!    as text and handed to the model; the store refuses an unknown scope whether or not
//!    `atlas-monitor`'s `Ask::parse` ran first, and the refusal leaves no row. Two asks for
//!    the same slot leave one row.
//!
//! The four scopes the window actually asked for are printed at the end of
//! [`every_stored_briefing_reads_back_and_nothing_broken_is_stored`] — evidence for the
//! report, not an assertion, because the archive fills as the app runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use atlas_store::briefings::{
    MAX_BULLETS, MAX_BULLET_CHARS, MAX_KEY_BYTES, MAX_LINE_CHARS, MIN_HOURS, SCOPES,
};
use atlas_store::{Briefing, Requests, SqlValue, Store};

// ------------------------------------------------------------------------ fixtures

/// A temporary root, removed when the test ends.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A directory no other test uses: the process id plus a counter.
fn temp_root(label: &str) -> Root {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-brief-write-{}-{}-{}",
        label,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp dir");
    Root(root)
}

/// The repository root: the crate lives at `rust/atlas-store`.
fn repo() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate_dir.to_path_buf())
}

/// Copy one SQLite database and the two files that make it current.
///
/// `atlas.db` alone is a snapshot from the last checkpoint, so the `-wal` carries the
/// writes since — and the `-shm` is the index into it. SQLite rebuilds the last one if it
/// is missing or stale, which is why the copy is safe either way.
fn copy_database(from: &Path, to: &Path) {
    std::fs::copy(from, to).expect("copy the database");
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{}", from.display(), suffix));
        if side.is_file() {
            let dest = PathBuf::from(format!("{}{}", to.display(), suffix));
            std::fs::copy(&side, &dest).expect("copy the side file");
        }
    }
}

/// The live archive, copied, or `None` when there is no `data/atlas.db` to copy.
fn archive_copy(root: &Root) -> Option<Store> {
    let source = repo().join("data").join("atlas.db");
    if !source.is_file() {
        eprintln!(
            "skipped: {} is not there, so there is no live schema to audit",
            source.display()
        );
        return None;
    }
    let target = root.0.join("atlas.db");
    copy_database(&source, &target);
    let store = Store::open(&target).expect("open the copy");
    Some(store)
}

/// The live queue, copied, or a temporary one seeded with the same three asks plus the
/// duplicate the window left, so the claim-path test means the same thing on a machine with
/// no `data/requests.db`.
fn queue_copy(root: &Root) -> Requests {
    let source = repo().join("data").join("requests.db");
    let target = root.0.join("requests.db");
    if source.is_file() {
        copy_database(&source, &target);
        return Requests::open(&target).expect("open the queue copy");
    }
    eprintln!("no {} — using a temporary queue with the same asks", source.display());
    let queue = Requests::open(&target).expect("open a temporary queue");
    for hours in [72, 24, 168, 72] {
        let payload = format!(
            "{{\"hours\":{hours},\"key\":\"\",\"scope\":\"window\",\"why\":\"the page about \
             the window has stories and no briefing yet\"}}"
        );
        queue.push("brief", &payload).expect("a queued ask");
    }
    queue
}

/// An archive of our own with the real schema, for the tests that are about the store's
/// rules rather than about the live archive's contents.
fn temp_archive(root: &Root) -> Store {
    let store = Store::open(root.0.join("atlas.db")).expect("open");
    store.init().expect("schema");
    store
}

/// A briefing inside every cap, for a slot no page reads.
fn probe(scope: &str, key: &str, hours: i64, line: &str) -> Briefing {
    Briefing {
        scope: scope.to_string(),
        key: key.to_string(),
        hours,
        watermark: 3444,
        created: "2026-11-22T12:00:00Z".to_string(),
        line: line.to_string(),
        bullets: vec![
            "The ministry put the package at 4.2 billion euros.".to_string(),
            "Two states want the money spent on fabs, not on design.".to_string(),
        ],
        articles: 12,
        model: "local".to_string(),
        ms: 1_840,
    }
}

fn rows(store: &Store) -> i64 {
    store
        .query_one("SELECT COUNT(*) AS n FROM briefings", &[])
        .expect("count")
        .and_then(|row| row.int("n"))
        .unwrap_or(-1)
}

fn stored(store: &Store, scope: &str, key: &str, hours: i64) -> Option<Briefing> {
    store
        .briefing_get(scope, key, hours)
        .unwrap_or_else(|error| panic!("briefing_get({scope},{key},{hours}): {}", error.message()))
}

/// The refusal sentence a put answers, or a panic that says it was accepted.
fn refusal(store: &Store, briefing: &Briefing) -> String {
    match store.briefing_put(briefing) {
        Ok(()) => panic!(
            "a briefing with {} line, {} bullets was stored (scope {:?}, key {:?})",
            briefing.line.chars().count(),
            briefing.bullets.len(),
            briefing.scope,
            briefing.key
        ),
        Err(error) => error.message().to_string(),
    }
}

// ------------------------------------------------------------------------ 1. the schema

/// The archive's `briefings` table is the one `atlas-store` documents: ten `NOT NULL`
/// columns with the defaults the card reads, the primary key `(scope, key, hours)`, the
/// `(scope, hours, created)` index the page's read uses, and nothing else — no trigger, no
/// second index, no nullable column that would let a half-row in.
///
/// Asserted from `PRAGMA table_info`, `PRAGMA index_list`, `PRAGMA index_info` and
/// `sqlite_master` on the copy, so it measures the archive the app will actually read.
#[test]
fn the_archive_copy_has_the_briefings_schema_the_store_documents() {
    let root = temp_root("schema");
    let Some(store) = archive_copy(&root) else {
        return;
    };

    // name, type, notnull, default, which column of the primary key (0 = not in it).
    let expected: [(&str, &str, i64, Option<&str>, i64); 10] = [
        ("scope", "TEXT", 1, None, 1),
        ("key", "TEXT", 1, None, 2),
        ("hours", "INTEGER", 1, None, 3),
        ("watermark", "INTEGER", 1, None, 0),
        ("created", "TEXT", 1, None, 0),
        ("line", "TEXT", 1, Some("''"), 0),
        ("bullets", "TEXT", 1, Some("'[]'"), 0),
        ("articles", "INTEGER", 1, Some("0"), 0),
        ("model", "TEXT", 1, Some("''"), 0),
        ("ms", "INTEGER", 1, Some("0"), 0),
    ];
    let columns = store
        .query("PRAGMA table_info(briefings)", &[])
        .expect("table_info");
    assert_eq!(
        columns.len(),
        expected.len(),
        "the table has {} columns, the store documents {}",
        columns.len(),
        expected.len()
    );
    for (row, (name, kind, not_null, default, pk)) in columns.iter().zip(expected) {
        assert_eq!(row.text("name"), Some(name), "column order or name");
        assert_eq!(row.text("type"), Some(kind), "{name}: declared type");
        assert_eq!(row.int("notnull"), Some(not_null), "{name}: NOT NULL");
        assert_eq!(row.text("dflt_value"), default, "{name}: default");
        assert_eq!(row.int("pk"), Some(pk), "{name}: position in the primary key");
    }

    // The primary key is the slot: (scope, key, hours), in that order.
    let mut pk: Vec<(i64, String)> = columns
        .iter()
        .filter_map(|row| {
            let position = row.int("pk").unwrap_or(0);
            if position == 0 {
                return None;
            }
            Some((position, row.text("name").unwrap_or("").to_string()))
        })
        .collect();
    pk.sort();
    assert_eq!(
        pk,
        vec![
            (1, "scope".to_string()),
            (2, "key".to_string()),
            (3, "hours".to_string())
        ],
        "the slot is not (scope, key, hours)"
    );

    // One index of ours, plus the one SQLite makes for the primary key.
    let indexes = store
        .query("PRAGMA index_list(briefings)", &[])
        .expect("index_list");
    let ours = indexes
        .iter()
        .find(|row| row.text("name") == Some("idx_briefings_scope"))
        .expect("the page's read index is there");
    assert_eq!(ours.int("unique"), Some(0), "the read index is not unique");
    assert_eq!(ours.text("origin"), Some("c"), "the index was created by us, not by a constraint");
    let theirs = indexes
        .iter()
        .find(|row| row.text("origin") == Some("pk"))
        .expect("the primary key has its automatic index");
    assert_eq!(theirs.int("unique"), Some(1));
    assert_eq!(indexes.len(), 2, "an unexpected index on the table");

    let shaped = |index: &str| -> Vec<String> {
        let mut columns: Vec<(i64, String)> = store
            .query(&format!("PRAGMA index_info('{index}')"), &[])
            .expect("index_info")
            .iter()
            .map(|row| {
                (
                    row.int("seqno").unwrap_or(0),
                    row.text("name").unwrap_or("").to_string(),
                )
            })
            .collect();
        columns.sort();
        columns.into_iter().map(|(_, name)| name).collect()
    };
    assert_eq!(shaped("idx_briefings_scope"), vec!["scope", "hours", "created"]);
    assert_eq!(shaped("sqlite_autoindex_briefings_1"), vec!["scope", "key", "hours"]);

    // Nothing else speaks for the table: no trigger, no view, no second index.
    let objects: Vec<(String, String)> = store
        .query(
            "SELECT type, name FROM sqlite_master WHERE tbl_name='briefings' ORDER BY type, name",
            &[],
        )
        .expect("sqlite_master")
        .iter()
        .filter_map(|row| {
            Some((
                row.text("type")?.to_string(),
                row.text("name")?.to_string(),
            ))
        })
        .collect();
    assert_eq!(
        objects,
        vec![
            ("index".to_string(), "idx_briefings_scope".to_string()),
            ("index".to_string(), "sqlite_autoindex_briefings_1".to_string()),
            ("table".to_string(), "briefings".to_string()),
        ],
        "the table has an object nobody documented"
    );
    let ddl = store
        .query_one(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='briefings'",
            &[],
        )
        .expect("ddl")
        .and_then(|row| row.text("sql").map(|text| text.to_string()))
        .expect("the table's DDL");
    assert!(
        ddl.contains("PRIMARY KEY(scope, key, hours)"),
        "the declared key is not the slot: {ddl}"
    );

    // And the DDL is the crate's own, whitespace aside: a table that drifted from the
    // constant the writer uses (an extra column with a default, an old spelling of the key)
    // would still pass the column check above if both were edited, but not this.
    let collapse = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let documented = collapse(
        &atlas_store::briefings::SCHEMA.replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE"),
    );
    assert_eq!(collapse(&ddl), documented, "the archive's table is not the declared DDL");
    let index_ddl = store
        .query_one(
            "SELECT sql FROM sqlite_master WHERE type='index' AND name='idx_briefings_scope'",
            &[],
        )
        .expect("index ddl")
        .and_then(|row| row.text("sql").map(|text| text.to_string()))
        .expect("the index's DDL");
    let documented_index = collapse(
        &atlas_store::briefings::INDEX.replace("CREATE INDEX IF NOT EXISTS", "CREATE INDEX"),
    );
    assert_eq!(
        collapse(&index_ddl),
        documented_index,
        "the archive's index is not the declared DDL"
    );

    // The table starts where the report says it does: printed, not asserted, because the
    // app fills it as it runs.
    println!(
        "measured: {} column(s), 2 index(es), {} stored briefing(s)",
        columns.len(),
        rows(&store)
    );
}

// ------------------------------------------------------------------------ 2. the upsert

/// A put is one row in its slot: three writes to `(place, probe, 72)` leave one row with the
/// last text, and a different window is a different slot. Measured on the archive's copy,
/// with the row count before and after, so "the table did not grow" is a number and not a
/// claim; the probe rows are deleted again for tidiness, since this is a copy of the live
/// archive.
#[test]
fn a_put_on_the_archive_copy_is_an_upsert_of_one_row_per_slot() {
    let root = temp_root("upsert");
    let Some(store) = archive_copy(&root) else {
        return;
    };
    let before = rows(&store);
    let key = "__audit_probe__";

    for index in 0..3 {
        let mut briefing = probe("place", key, 72, &format!("Probe, take {index}."));
        briefing.watermark = 3_444 + index;
        briefing.created = format!("2026-11-22T1{index}:00:00Z");
        store.briefing_put(&briefing).expect("a write");
        assert_eq!(rows(&store), before + 1, "a rewrite grew the table");
    }
    let read = stored(&store, "place", key, 72).expect("the row");
    assert_eq!(read.line, "Probe, take 2.", "the last write wins");
    assert_eq!(read.watermark, 3_446);

    // A second window is a second slot, and it does not disturb the first.
    store
        .briefing_put(&probe("place", key, 24, "Probe, last 24 hours."))
        .expect("a write");
    assert_eq!(rows(&store), before + 2, "a second window is a second slot");
    assert_eq!(
        stored(&store, "place", key, 72).map(|row| row.line),
        Some("Probe, take 2.".to_string())
    );
    // And a third write to the 72-hour slot still leaves two probe rows.
    store
        .briefing_put(&probe("place", key, 72, "Probe, again."))
        .expect("a write");
    assert_eq!(rows(&store), before + 2);

    // Leave the copy as it was for the other read below.
    for hours in [72, 24] {
        store
            .exec(
                "DELETE FROM briefings WHERE scope='place' AND key=? AND hours=?",
                &[SqlValue::Text(key.to_string()), SqlValue::Integer(hours)],
            )
            .expect("cleanup");
    }
    assert_eq!(rows(&store), before, "the probe rows were not cleaned up");
}

// ------------------------------------------------------------------------ 3. the caps

/// The storable-text caps are the store's, and the sentence names the number: a line of 401
/// characters, a bullet of 241, and nine bullets are each refused, and exactly 400 / 240 /
/// eight are each accepted.
///
/// This is the half a prompt cannot give: a caller that bypassed `atlas_ai::brief::parse`
/// (a hand-written row, a future writer, a test) must still be unable to put a card-sized
/// lie in the table, or the caps would be documentation rather than a rule. Measured on an
/// archive of the test's own, so it runs without `data/`.
#[test]
fn the_caps_refuse_a_hand_written_row_and_the_sentence_names_the_number() {
    let root = temp_root("caps");
    let store = temp_archive(&root);

    let long_line = probe("window", "", 72, &"a".repeat(MAX_LINE_CHARS + 1));
    let message = refusal(&store, &long_line);
    println!("measured: 401-character line -> {message}");
    assert!(
        message.contains(&MAX_LINE_CHARS.to_string())
            && message.contains(&(MAX_LINE_CHARS + 1).to_string()),
        "the sentence names the cap and the length: {message}"
    );
    assert_eq!(rows(&store), 0, "a refused line left a row");

    // The rule lives in `briefing_put`, not in the schema: the same row written with a plain
    // INSERT is accepted by SQLite, because the table has no CHECK on `line`. That is what
    // makes this test load-bearing — delete the store's check and the assertion above starts
    // failing with "was stored", while this INSERT keeps working.
    store
        .exec(
            "INSERT INTO briefings(scope, key, hours, watermark, created, line) \
             VALUES('window', '', 72, 1, '2026-11-22T12:00:00Z', ?)",
            &[SqlValue::Text("a".repeat(MAX_LINE_CHARS + 1))],
        )
        .expect("the table itself has no width: the check is the store's");
    assert_eq!(rows(&store), 1);
    store
        .exec("DELETE FROM briefings", &[])
        .expect("cleanup");

    let long_bullet = Briefing {
        bullets: vec!["b".repeat(MAX_BULLET_CHARS + 1)],
        ..probe("window", "", 72, "A line that fits.")
    };
    let message = refusal(&store, &long_bullet);
    println!("measured: 241-character bullet -> {message}");
    assert!(
        message.contains(&MAX_BULLET_CHARS.to_string())
            && message.contains(&(MAX_BULLET_CHARS + 1).to_string()),
        "the sentence names the bullet cap: {message}"
    );

    let too_many = Briefing {
        bullets: (0..MAX_BULLETS + 1).map(|index| format!("bullet {index}")).collect(),
        ..probe("window", "", 72, "A line that fits.")
    };
    let message = refusal(&store, &too_many);
    println!("measured: 9 bullets -> {message}");
    assert!(
        message.contains(&MAX_BULLETS.to_string())
            && message.contains(&(MAX_BULLETS + 1).to_string()),
        "the sentence names the bullet count: {message}"
    );
    assert_eq!(rows(&store), 0, "a refused briefing left a row");

    // The caps themselves are allowed: the rule is a width, not a target to undershoot.
    let at_the_caps = Briefing {
        line: "c".repeat(MAX_LINE_CHARS),
        bullets: (0..MAX_BULLETS)
            .map(|_| "d".repeat(MAX_BULLET_CHARS))
            .collect(),
        ..probe("window", "", 72, "unused")
    };
    store
        .briefing_put(&at_the_caps)
        .expect("400 characters, eight bullets of 240: exactly the card");
    let read = stored(&store, "window", "", 72).expect("the row");
    assert_eq!(read.line.chars().count(), MAX_LINE_CHARS);
    assert_eq!(read.bullets.len(), MAX_BULLETS);
    assert_eq!(read.bullets[0].chars().count(), MAX_BULLET_CHARS);

    // The store has **no upper** window: the page asks for 24, 72 and 168 hours
    // (`atlas-desktop/src/app.rs:931`), and a 168-hour slot is a slot like any other. Pinned
    // because the opposite would be silent — if a ceiling ever moved into this table, the
    // page's longest tab would stop being storable and would show "no briefing yet" forever.
    // (Ingest has one of its own: `atlas-ingest/src/brief.rs:109` clamps to `MAX_AGE_H`
    // = 72, so a 168-hour ask is answered with a 72-hour briefing under the 72-hour key.)
    for hours in [168, 720] {
        store
            .briefing_put(&probe("window", "", hours, "A line over a long window."))
            .unwrap_or_else(|error| panic!("{hours}h: {}", error.message()));
        assert_eq!(
            stored(&store, "window", "", hours).map(|row| row.hours),
            Some(hours),
            "a {hours}-hour slot did not read back as itself"
        );
    }

    // A scope outside the set is refused by name, and a window of zero hours with it: the
    // two rules a hand-written row would otherwise walk past.
    let unknown = Briefing {
        scope: "weather".to_string(),
        ..probe("weather", "", 72, "A line.")
    };
    let message = refusal(&store, &unknown);
    println!("measured: scope 'weather' -> {message}");
    assert!(
        message.contains("unknown briefing scope 'weather'") && message.contains(SCOPES.join(", ").as_str()),
        "the refusal names the scope and the set: {message}"
    );
    let no_window = Briefing {
        hours: MIN_HOURS - 1,
        ..probe("window", "", 0, "A line.")
    };
    let message = refusal(&store, &no_window);
    println!("measured: zero-hour window -> {message}");
    assert!(
        message.contains("at least 1 hour"),
        "the refusal names the window rule: {message}"
    );
    // A key past its column is refused with the number too.
    let long_key = Briefing {
        key: "k".repeat(MAX_KEY_BYTES + 1),
        ..probe("place", "", 72, "A line.")
    };
    let message = refusal(&store, &long_key);
    println!("measured: {} -byte key -> {message}", MAX_KEY_BYTES + 1);
    assert!(
        message.contains(&MAX_KEY_BYTES.to_string()),
        "the refusal names the key cap: {message}"
    );
}

// ------------------------------------------------------------------------ 4. the claim path

/// The marker this file's own asks carry, so a test can find them in a queue it does not
/// own.
///
/// The live `data/requests.db` is being drained while this suite runs — the app's agent
/// claims its pending rows — so a test that relied on the window's own pending ask would
/// pass or fail depending on when it ran. These tests put their asks in the *copied* queue
/// with this marker and serve only those; the live rows are printed as evidence, never
/// depended on. The payloads are the window's (`{\"scope\",\"key\",\"hours\",\"why\"}`), so
/// what is exercised is still the real shape.
const AUDIT_WHY: &str = "__audit_brief_write_path__";

/// Queue one ask in the window's payload shape.
fn queue_ask(queue: &Requests, scope: &str, key: &str, hours: i64) {
    let payload = serde_json::json!({
        "scope": scope,
        "key": key,
        "hours": hours,
        "why": AUDIT_WHY,
    })
    .to_string();
    queue.push("brief", &payload).expect("a queued ask");
}

/// Claim every request that carries [`AUDIT_WHY`], as `(scope, key, hours)`.
///
/// Staleness 0 because a row just pushed is claimable at once, and a limit wide enough that
/// the live queue's own rows cannot crowd ours out.
fn claimed_asks(queue: &Requests) -> Vec<(String, String, i64)> {
    queue
        .claim_due(&atlas_store::now_iso(), 0, 500)
        .expect("claim")
        .iter()
        .filter(|request| request.kind == "brief" && request.payload.contains(AUDIT_WHY))
        .filter_map(|request| {
            let payload: serde_json::Value = serde_json::from_str(&request.payload).ok()?;
            Some((
                payload.get("scope")?.as_str()?.to_string(),
                payload.get("key")?.as_str()?.to_string(),
                payload.get("hours")?.as_i64()?,
            ))
        })
        .collect()
}

/// A claimed `brief` request is text, and the store is the door that judges it.
///
/// The queue hands the payload over exactly as the window wrote it — the claim asks nothing
/// of it — so a claimer that skipped `atlas-monitor`'s `Ask::parse` would still reach
/// `briefing_put` with `"weather"` in the scope. This test takes that route on purpose: it
/// puts a hostile payload into the copied queue, claims it, builds the briefing from the
/// payload's own fields, and asserts the store refuses it by name and stores nothing. Then
/// the same route with a legal window ask, which lands — so the refusal is about the value
/// and not about the path.
#[test]
fn a_claimed_request_with_an_unknown_scope_cannot_reach_the_table() {
    let root = temp_root("claim");
    let Some(store) = archive_copy(&root) else {
        return;
    };
    let queue = queue_copy(&root);
    let before = rows(&store);

    // A payload a bug (or a hand) could write: the scope is page text, not a scope word.
    let hostile = "{\"hours\":72,\"key\":\"\",\"scope\":\"weather\",\"why\":\"probe\"}";
    queue
        .push("brief", hostile)
        .expect("the queue stores a payload it cannot judge");
    let claimed = queue
        .claim_due(&atlas_store::now_iso(), 0, 500)
        .expect("claim");
    let mine = claimed
        .iter()
        .find(|request| request.payload == hostile)
        .expect("the hostile request is claimable");
    assert_eq!(mine.kind, "brief");

    // What a claimer does next: read the payload's fields and hand them to the store.
    let payload: serde_json::Value = serde_json::from_str(&mine.payload).expect("payload JSON");
    let ask = Briefing {
        scope: payload
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        key: payload
            .get("key")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        hours: payload
            .get("hours")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        ..probe("window", "", 72, "A line the model would have written.")
    };
    let message = refusal(&store, &ask);
    println!("measured: claimed scope 'weather' -> {message}");
    assert!(
        message.contains("unknown briefing scope 'weather'"),
        "the store names the scope it refused: {message}"
    );
    assert_eq!(
        rows(&store),
        before,
        "a refused claim wrote a briefing anyway"
    );
    assert_eq!(
        stored(&store, "weather", "", 72),
        None,
        "a row exists for the scope the store refused"
    );

    // The same route with a legal ask of the window's shape: it lands, so the refusal above
    // is the scope's and not the claim's. The copy may already hold `window/72h` from the
    // running app, and a briefing may only replace a row that is **not newer** than itself
    // (`briefings.rs`, the watermark guard on the upsert) — so this write carries a watermark
    // above anything the archive can hold, which is what "the server just read the archive"
    // means. A stale write is refused on purpose, and `a_stale_write_does_not_replace_a_fresher…`
    // below pins that.
    queue_ask(&queue, "window", "", 72);
    let legal = claimed_asks(&queue);
    assert!(
        legal.contains(&("window".to_string(), String::new(), 72)),
        "the legal ask is claimable: {legal:?}"
    );
    let mut landing = probe(
        "window",
        "",
        72,
        "A line for the window, written through the claim path.",
    );
    landing.watermark = 9_999_999;
    store
        .briefing_put(&landing)
        .expect("the window's ask is a legal briefing");
    assert_eq!(
        stored(&store, "window", "", 72).map(|row| row.line),
        Some("A line for the window, written through the claim path.".to_string())
    );
}

/// Two asks for one slot leave one row: the window queued `window/72h` twice, and a claimer
/// that served both would otherwise write twice.
///
/// The asks are put in the copied queue here — the page's own three windows plus the
/// duplicate it left for 72 hours — because the live queue's pending rows are being claimed
/// by the running app while this test runs. Serving four asks over three distinct slots must
/// leave **three** rows, and serving the same slot again must leave three: measured by row
/// count, so a second row would be visible even where its text matched.
#[test]
fn a_repeated_ask_for_one_slot_leaves_one_row() {
    let root = temp_root("repeat");
    let Some(store) = archive_copy(&root) else {
        return;
    };
    let queue = queue_copy(&root);

    // The copy may already hold a briefing for one of these slots — the window has been
    // briefed since this audit started — so the three slots are cleared first. The copy is
    // disposable and the count below is then this test's writes and nothing else; the live
    // archive is not touched.
    for hours in [24, 72, 168] {
        store
            .exec(
                "DELETE FROM briefings WHERE scope='window' AND key='' AND hours=?",
                &[SqlValue::Integer(hours)],
            )
            .expect("clear the slot");
    }
    let before = rows(&store);

    for hours in [24, 72, 168, 72] {
        queue_ask(&queue, "window", "", hours);
    }
    let asks = claimed_asks(&queue);
    let duplicates = asks
        .iter()
        .filter(|(scope, _, hours)| scope == "window" && *hours == 72)
        .count();
    println!(
        "measured: {} claimed ask(s) of this test's own, {} of them window/72h",
        asks.len(),
        duplicates
    );
    assert_eq!(asks.len(), 4, "the four asks are claimable: {asks:?}");
    assert_eq!(duplicates, 2, "the 72-hour slot was asked for twice");

    // Serve every ask, the way a claimer would, into the archive's copy, remembering the
    // line each slot was last given so the stored text can be compared to it.
    let mut last: std::collections::BTreeMap<(String, i64), String> =
        std::collections::BTreeMap::new();
    for (index, (scope, key, hours)) in asks.iter().enumerate() {
        let line = format!("The window, served from ask {index}.");
        store
            .briefing_put(&probe(scope, key, *hours, &line))
            .unwrap_or_else(|error| panic!("ask {scope}/{hours}: {}", error.message()));
        last.insert((scope.clone(), *hours), line);
    }
    // One row per distinct slot is the ceiling.
    assert_eq!(
        rows(&store),
        before + 3,
        "four asks over three slots left {} rows",
        rows(&store) - before
    );
    for ((scope, hours), line) in &last {
        assert_eq!(
            stored(&store, scope, "", *hours).map(|row| row.line),
            Some(line.clone()),
            "{scope}/{hours}: the stored line is not the last one served"
        );
    }

    // And the duplicate served a second time: the table must not grow.
    store
        .briefing_put(&probe(
            "window",
            "",
            72,
            "The window, served a second time.",
        ))
        .expect("the second serve is legal");
    assert_eq!(
        rows(&store),
        before + 3,
        "a repeated ask for one slot grew the table"
    );
    assert_eq!(
        stored(&store, "window", "", 72).map(|row| row.line),
        Some("The window, served a second time.".to_string()),
        "the second serve did not overwrite the slot"
    );
}

// ------------------------------------------------------------------------ 5. what is stored

/// Every stored briefing on the archive's copy reads back, and every one of them is inside
/// the caps — a row that the API refused cannot be in the table, and a row that is there can
/// be drawn.
///
/// The slots the window asked for are printed: there is no assertion about their presence,
/// because a fresh archive legitimately has none, but a reader of the report wants to know
/// whether the four asks were served.
#[test]
fn every_stored_briefing_reads_back_and_nothing_broken_is_stored() {
    let root = temp_root("stored");
    let Some(store) = archive_copy(&root) else {
        return;
    };

    let slots: Vec<(String, String, i64)> = store
        .query(
            "SELECT scope, key, hours FROM briefings ORDER BY scope, key, hours",
            &[],
        )
        .expect("slots")
        .iter()
        .map(|row| {
            (
                row.text("scope").unwrap_or("").to_string(),
                row.text("key").unwrap_or("").to_string(),
                row.int("hours").unwrap_or(0),
            )
        })
        .collect();
    for (scope, key, hours) in &slots {
        let briefing = stored(&store, scope, key, *hours)
            .unwrap_or_else(|| panic!("{scope}/{key}/{hours} does not read back as a briefing"));
        assert!(
            briefing.line.chars().count() <= MAX_LINE_CHARS,
            "{scope}/{key}/{hours}: the line is {} characters",
            briefing.line.chars().count()
        );
        assert!(
            briefing.bullets.len() <= MAX_BULLETS,
            "{scope}/{key}/{hours}: {} bullets",
            briefing.bullets.len()
        );
        for bullet in &briefing.bullets {
            assert!(
                bullet.chars().count() <= MAX_BULLET_CHARS,
                "{scope}/{key}/{hours}: a bullet of {} characters",
                bullet.chars().count()
            );
        }
        assert!(briefing.hours >= MIN_HOURS, "{scope}/{key}/{hours}: a window below one hour");
    }

    // The four asks the window left, and what the archive holds for each.
    for hours in [24, 72, 168] {
        let row = stored(&store, "window", "", hours);
        println!(
            "measured: window/{hours}h -> {}",
            match &row {
                Some(briefing) => format!(
                    "line of {} chars, {} bullets, watermark {}, articles {}, model {:?}",
                    briefing.line.chars().count(),
                    briefing.bullets.len(),
                    briefing.watermark,
                    briefing.articles,
                    briefing.model
                ),
                None => "no briefing stored".to_string(),
            }
        );
    }
    println!("measured: {} stored briefing(s) in the archive's copy", slots.len());
}

/// A briefing may replace a row only when it is **at least as fresh**: the upsert carries
/// `WHERE excluded.watermark >= briefings.watermark`.
///
/// Two claimers can overlap on one slot — an ask retried while the first read is still in the
/// model — and without that guard the row went backwards: measured on a scratch table built
/// from these columns, a 3 446-watermark row was replaced by a 3 000-watermark one, so the
/// window showed a briefing older than the one it had just replaced. This pins both directions:
/// a staler write leaves the fresher text, an equal one lands (a retry of the same read is the
/// same briefing, and refusing it would leave a slot empty after a crash).
#[test]
fn a_stale_write_does_not_replace_a_fresher_briefing() {
    let root = temp_root("stale");
    let Some(store) = archive_copy(&root) else {
        return;
    };
    // A slot of this test's own, cleared first: the copy's live slots belong to the app.
    store
        .exec(
            "DELETE FROM briefings WHERE scope = 'topic' AND key = 'probe' AND hours = 6",
            &[],
        )
        .expect("clear the slot");

    let mut fresh = probe("topic", "probe", 6, "The fresher briefing.");
    fresh.watermark = 3_446;
    store.briefing_put(&fresh).expect("the first write lands");
    assert_eq!(
        stored(&store, "topic", "probe", 6).map(|row| row.line),
        Some("The fresher briefing.".to_string()),
        "the first write is the row"
    );

    let mut stale = probe("topic", "probe", 6, "An older briefing, written later.");
    stale.watermark = 3_000;
    store
        .briefing_put(&stale)
        .expect("a stale write is accepted and ignored, not an error");
    let row = stored(&store, "topic", "probe", 6).expect("the slot still holds a row");
    assert_eq!(
        row.line, "The fresher briefing.",
        "a briefing with watermark 3 000 replaced one with 3 446: the window would show a \
         summary older than the archive it replaced"
    );
    assert_eq!(row.watermark, 3_446, "the row's watermark went backwards");

    // The same read written again (a retry) is not a regression: equal watermarks land.
    let mut retry = probe("topic", "probe", 6, "The fresher briefing, written again.");
    retry.watermark = 3_446;
    store.briefing_put(&retry).expect("an equal watermark lands");
    assert_eq!(
        stored(&store, "topic", "probe", 6).map(|row| row.line),
        Some("The fresher briefing, written again.".to_string()),
        "a retry of the same read did not replace its own row"
    );

    // And a newer read wins, which is the everyday case.
    let mut newer = probe("topic", "probe", 6, "A newer briefing.");
    newer.watermark = 3_500;
    store.briefing_put(&newer).expect("a newer watermark lands");
    assert_eq!(
        stored(&store, "topic", "probe", 6).map(|row| row.line),
        Some("A newer briefing.".to_string()),
        "a newer read did not replace the older briefing"
    );

    store
        .exec(
            "DELETE FROM briefings WHERE scope = 'topic' AND key = 'probe' AND hours = 6",
            &[],
        )
        .expect("the probe slot is removed again");
}
