//! Cross-module tests, and the fixtures every unit test in this crate shares.
//!
//! A database is one decision away from any single statement — schema, pragmas,
//! statements and a domain rule all run in one test here — so the timing test
//! lives in this file rather than next to one module. Everything that tests one
//! module sits in that module's own `#[cfg(test)] mod tests`.
//!
//! Every test owns its database (in memory, or a directory under `temp_dir` named
//! after the process id), so they can run in parallel.

use std::path::PathBuf;

use crate::clock::now_iso;
use crate::error::MAX_ERROR_CHARS;
use crate::{perms, BUSY_TIMEOUT_MS, POSIX};
use crate::params::SqlValue;
use crate::Store;

/// A migrated in-memory archive.
pub(crate) fn store() -> Store {
    let store = Store::open_memory().expect("memory store");
    store.init().expect("init");
    store
}

pub(crate) fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

pub(crate) fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

pub(crate) fn insert_article<P: AsRef<str>>(
    store: &Store,
    title: &str,
    publisher: &str,
    published: P,
) -> i64 {
    let published = published.as_ref();
    store
        .insert(
            "INSERT INTO articles(title,publisher,published,relevant) VALUES(?,?,?,1)",
            &[text(title), text(publisher), text(published)],
        )
        .expect("insert article")
}

pub(crate) fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atlas-store-{}-{}", label, std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn two_thousand_articles_in_one_transaction_carry_their_own_timing() {
    // The measured number behind "one-transaction bulk insert is the default
    // path": 2 000 articles, indexed the way collector.py indexes them, on a real
    // file database (WAL, synchronous=NORMAL). Run with
    // `cargo test --release --offline -p atlas-store two_thousand -- --nocapture`
    // to see the numbers; the assertion only stops a catastrophic regression.
    use std::time::Instant;

    let dir = temp_dir("perf");
    let db = dir.join("atlas.db");
    let store = Store::open(&db).expect("open");
    store.init().expect("init");
    let published = now_iso();

    let mut articles: Vec<Vec<SqlValue>> = Vec::with_capacity(2_000);
    for index in 0..2_000 {
        let title = format!("Story {} about the harvest and the chip shortage", index);
        articles.push(vec![
            text(&title),
            text("summary line"),
            text("body text of the article, long enough to be a row"),
            text(&published),
            text("FRA"),
            int(1),
        ]);
    }

    let started = Instant::now();
    let inserted = store
        .insert_many(
            "INSERT INTO articles(title,summary,content,published,iso,relevant) \
             VALUES(?,?,?,?,?,?)",
            &articles,
        )
        .expect("bulk insert");
    let insert_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(inserted, 2_000);

    // Indexing needs the rowids, which only exist after the insert; this is the
    // second half of the same import and also one transaction.
    let ids: Vec<i64> = store
        .query("SELECT id,title,summary,content FROM articles ORDER BY id", &[])
        .expect("ids")
        .iter()
        .map(|row| row.int("id").unwrap_or(0))
        .collect();
    let mut fts_rows: Vec<Vec<SqlValue>> = Vec::with_capacity(ids.len());
    for id in &ids {
        fts_rows.push(vec![int(*id), text("Story about the harvest"), text("summary line"), text("body text")]);
    }
    let started = Instant::now();
    let indexed = store
        .insert_many(
            "INSERT INTO article_fts(rowid,title,summary,content) VALUES(?,?,?,?)",
            &fts_rows,
        )
        .expect("bulk index");
    let index_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(indexed, 2_000);

    let started = Instant::now();
    let recent = store
        .query(
            "SELECT id,title,published FROM articles WHERE relevant=1 \
             ORDER BY published DESC LIMIT 50",
            &[],
        )
        .expect("list query");
    let list_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(recent.len(), 50);

    let started = Instant::now();
    let hits = store.search("harvest", 30).expect("search");
    let search_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(hits.len(), 30);

    let started = Instant::now();
    let count = store
        .query_one("SELECT COUNT(*) c FROM articles WHERE iso='FRA'", &[])
        .expect("count")
        .and_then(|row| row.int("c"))
        .expect("count value");
    let count_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(count, 2_000);

    // A second, separate round trip pays the fsync once, not 2 000 times.
    let started = Instant::now();
    store
        .insert(
            "INSERT INTO meta(k,v) VALUES(?,?)",
            &[text("perf_marker"), text("1")],
        )
        .expect("marker");
    let commit_ms = started.elapsed().as_secs_f64() * 1_000.0;

    // The comparison the default exists for: the same 2 000 rows, one commit
    // each, through `exec` (which commits per statement, like `db.py`'s `exec`).
    let started = Instant::now();
    for index in 0..2_000 {
        store
            .exec(
                "INSERT INTO meta(k,v) VALUES(?,?)",
                &[text(&format!("p{}", index)), text("1")],
            )
            .expect("row per commit");
    }
    let per_row_ms = started.elapsed().as_secs_f64() * 1_000.0;

    // Measurements, not guesses: printed so a release run can be quoted.
    eprintln!(
        "atlas-store: 2000 articles in one transaction = {:.1} ms; \
         2000 FTS rows in one transaction = {:.1} ms; \
         list 50 recent = {:.2} ms; FTS search (30 of 2000) = {:.2} ms; \
         count = {:.2} ms; single committed insert = {:.2} ms; \
         the same 2000 rows with one commit each = {:.1} ms ({:.0}x slower)",
        insert_ms, index_ms, list_ms, search_ms, count_ms, commit_ms, per_row_ms,
        per_row_ms / insert_ms.max(0.001)
    );
    assert!(
        insert_ms < 60_000.0,
        "2 000 inserts took {:.0} ms in one transaction",
        insert_ms
    );
    assert!(
        search_ms < 5_000.0,
        "an FTS search over 2 000 articles took {:.0} ms",
        search_ms
    );
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pragmas_match_db_py() {
    let store = store();
    let journal = store
        .query_one("PRAGMA journal_mode", &[])
        .expect("journal_mode")
        .and_then(|row| match row.get_at(0).cloned() {
            Some(SqlValue::Text(mode)) => Some(mode),
            _ => None,
        })
        .expect("journal_mode value");
    assert_eq!(journal.to_ascii_lowercase(), "memory"); // in-memory database
    let busy = store
        .query_one("PRAGMA busy_timeout", &[])
        .expect("busy_timeout")
        .and_then(|row| row.int("timeout"))
        .expect("busy timeout value");
    assert_eq!(busy, BUSY_TIMEOUT_MS as i64);
    let foreign_keys = store
        .query_one("PRAGMA foreign_keys", &[])
        .expect("foreign_keys")
        .and_then(|row| row.int("foreign_keys"))
        .expect("foreign_keys value");
    assert_eq!(foreign_keys, 1);
}


#[test]
fn query_and_one_and_exec_report_what_they_did() {
    let store = store();
    assert_eq!(
        store
            .exec("INSERT INTO meta(k,v) VALUES(?,?)", &[text("a"), text("b")])
            .expect("exec"),
        1
    );
    let row = store
        .query_one("SELECT k,v FROM meta WHERE k=?", &[text("a")])
        .expect("one")
        .expect("row");
    assert_eq!(row.text("k"), Some("a"));
    assert_eq!(row.text("v"), Some("b"));
    assert_eq!(row.get_at(0), Some(&text("a")));
    assert_eq!(row.columns(), &["k".to_string(), "v".to_string()][..]);
    assert_eq!(
        store
            .query_one("SELECT k FROM meta WHERE k=?", &[text("zzz")])
            .expect("one"),
        None
    );
    // exec accepts a row-returning statement, like db.py does.
    assert_eq!(store.exec("SELECT 1", &[]).expect("exec select"), 0);
    // Binding is binding: a value that looks like SQL is just a value.
    store
        .exec(
            "INSERT INTO meta(k,v) VALUES(?,?)",
            &[text("quote'; DROP TABLE meta; --"), text("x")],
        )
        .expect("exec injection-looking value");
    assert_eq!(
        store
            .query("SELECT COUNT(*) c FROM meta", &[])
            .expect("count")[0]
            .int("c"),
        Some(2)
    );
    // Two statements in one exec is refused, like sqlite3.execute().
    assert!(store.exec("SELECT 1; SELECT 2", &[]).is_err());
}


#[test]
fn exec_many_is_one_transaction_and_counts_rows() {
    let store = store();
    let rows: Vec<Vec<SqlValue>> = (0..1_000)
        .map(|index| vec![text(&format!("k{}", index)), text(&format!("v{}", index))])
        .collect();
    let changed = store
        .exec_many("INSERT INTO meta(k,v) VALUES(?,?)", &rows)
        .expect("many");
    assert_eq!(changed, 1_000);
    assert_eq!(
        store
            .query("SELECT COUNT(*) c FROM meta", &[])
            .expect("count")[0]
            .int("c"),
        Some(1_000)
    );
    let rows: Vec<Vec<SqlValue>> = (0..250)
        .map(|index| vec![text(&format!("b{}", index)), text(&format!("v{}", index))])
        .collect();
    assert_eq!(
        store
            .exec_many_chunked("INSERT INTO meta(k,v) VALUES(?,?)", &rows, 100)
            .expect("chunks"),
        250
    );
    assert_eq!(
        store
            .query("SELECT COUNT(*) c FROM meta", &[])
            .expect("count")[0]
            .int("c"),
        Some(1_250)
    );
    // Empty input is not an error, like db.py's `if not seq: return None`.
    assert_eq!(
        store
            .exec_many("INSERT INTO meta(k,v) VALUES(?,?)", &[])
            .expect("empty"),
        0
    );
    // A failing batch rolls the whole transaction back. `article_tags` has a NOT
    // NULL column, so the second row fails and the first must go with it.
    let bad: Vec<Vec<SqlValue>> = vec![
        vec![int(1), text("grain")],
        vec![SqlValue::Null, text("grain")], // article_id NOT NULL
    ];
    assert!(store
        .exec_many("INSERT INTO article_tags(article_id,tag) VALUES(?,?)", &bad)
        .is_err());
    assert_eq!(
        store
            .query("SELECT COUNT(*) c FROM article_tags", &[])
            .expect("count")[0]
            .int("c"),
        Some(0),
        "the first row of a failed batch must not survive"
    );
}


#[test]
fn wal_synchronous_and_file_modes_on_a_file_database() {
    let dir = temp_dir("wal");
    let db = dir.join("atlas.db");
    let store = Store::open(&db).expect("open");
    store.init().expect("init");
    // `PRAGMA journal_mode` answers with a row; read the first column.
    let journal = store
        .query_one("PRAGMA journal_mode", &[])
        .expect("pragma")
        .and_then(|row| match row.get_at(0).cloned() {
            Some(SqlValue::Text(mode)) => Some(mode),
            _ => None,
        })
        .expect("mode");
    assert_eq!(journal.to_ascii_lowercase(), "wal");
    let synchronous = store
        .query_one("PRAGMA synchronous", &[])
        .expect("pragma")
        .and_then(|row| row.int("synchronous"))
        .expect("sync");
    assert_eq!(synchronous, 1, "NORMAL is 1");
    let report = store.permission_report();
    assert_eq!(report.posix, POSIX);
    if POSIX {
        assert!(report.exposed.is_empty(), "{:?}", report);
        assert!(report.ok, "{:?}", report);
        for file in store.files() {
            if file.is_file() {
                assert_eq!(
                    perms::permissions(&file).mode,
                    "0o600",
                    "{} must be 0600",
                    file.display()
                );
            }
        }
    }
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}


#[test]
fn a_bad_statement_is_an_error_not_a_panic() {
    let store = store();
    let err = store.query("SELECT nope FROM articles", &[]).unwrap_err();
    assert!(err.message().len() <= MAX_ERROR_CHARS + 1);
    assert!(store.exec("NOT SQL AT ALL", &[]).is_err());
}

#[test]
fn a_search_window_compares_timestamps_in_the_archive_format() {
    // The archive stores `2026-11-05T10:00:00Z` (`clock::now_iso`). A cutoff in any
    // other shape is a *string* comparison against a different shape, and
    // `datetime('now', '-48 hours')` hands back `2026-11-05 10:00:00`: `T` (0x54)
    // sorts above the space (0x20), so every article published on the cutoff's own
    // calendar day passed `published >= ?` whatever its hour. Measured on a copy of
    // the live archive: the same 72-hour window counted 264 rows through
    // `datetime('now')` and 263 through the archive's format.
    let store = store();
    let cutoff = crate::clock::iso_ago(48.0);
    // Midnight of the cutoff's own day: older than the cutoff unless the cutoff is
    // itself midnight, in which case there is nothing to tell apart and the case is
    // skipped rather than asserted on a coin flip.
    let midnight = format!("{}T00:00:00Z", &cutoff[..10]);
    if midnight == cutoff {
        return;
    }
    assert!(
        crate::clock::parse_epoch(&midnight) < crate::clock::parse_epoch(&cutoff),
        "the sample must be older than the cutoff: {} vs {}",
        midnight,
        cutoff
    );
    // Two articles, both answering "harvest": one an hour inside the window, one at
    // midnight of the cutoff's day (outside it).
    let inside = insert_article(&store, "Harvest inside", "AP", crate::clock::iso_ago(47.0));
    let boundary = insert_article(&store, "Harvest on the boundary day", "AP", &midnight);
    for (id, title) in [(inside, "Harvest inside"), (boundary, "Harvest on the boundary day")] {
        store
            .exec(
                "INSERT INTO article_fts(rowid,title,summary,content) VALUES(?,?,'','')",
                &[int(id), text(title)],
            )
            .expect("index it");
    }
    let rows = store
        .fts_query("harvest", 10, Some(48), None, false)
        .expect("windowed search");
    let ids: Vec<i64> = rows.iter().filter_map(|row| row.int("id")).collect();
    assert_eq!(
        ids,
        vec![inside],
        "the window must exclude an article older than the cutoff, even on the \
         cutoff's own calendar day (boundary row {}, cutoff {})",
        boundary,
        cutoff
    );
    // Without the window both rows answer, so the exclusion above is the window and
    // not the query.
    assert_eq!(
        store
            .fts_query("harvest", 10, None, None, false)
            .expect("no window")
            .len(),
        2
    );
}

#[test]
fn a_reader_answers_while_a_writer_holds_the_archive() {
    // The pragmas were only ever asserted as values. This is the behaviour they
    // exist for: one writer, readers that do not wait for it. In WAL a reader sees
    // the archive as of its own snapshot while a write transaction is open; in the
    // rollback journal it would block (and, past the busy timeout, fail). The
    // second connection is a second `Store`, so it is a second SQLite connection on
    // the same file — the shape a separate reader process has.
    let dir = temp_dir("wal-reader");
    let db = dir.join("atlas.db");
    let writer = Store::open(&db).expect("writer");
    writer.init().expect("init");
    writer
        .exec("INSERT INTO meta(k,v) VALUES(?,?)", &[text("k"), text("old")])
        .expect("seed");
    let reader = Store::open(&db).expect("reader");

    let read_during_write = writer
        .in_transaction(|conn| {
            conn.execute("INSERT INTO meta(k,v) VALUES('k2','new')", [])
                .map_err(crate::StoreError::from)?;
            // The other connection answers now, with the write still uncommitted.
            let seen = reader.query("SELECT COUNT(*) c FROM meta", &[])?;
            Ok(seen[0].int("c").unwrap_or(-1))
        })
        .expect("the write commits");
    assert_eq!(
        read_during_write,
        1,
        "a reader sees the pre-write snapshot, it does not block"
    );
    // After the commit the same reader sees both rows.
    let after = reader
        .query("SELECT COUNT(*) c FROM meta", &[])
        .expect("read after commit");
    assert_eq!(after[0].int("c"), Some(2));
    drop(reader);
    drop(writer);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn one_init_is_the_whole_archive_the_routes_read() {
    // The boot path calls `init` and nothing else, and `/api/alerts`,
    // `/api/monitors` and the check log read the monitor tables from the first
    // click — before any check has run. A second, older copy of that DDL used to
    // live in the server crate; this test is what keeps `init` honest as the one
    // place that decides the archive's shape.
    let store = Store::open_memory().expect("store");
    let report = store.init().expect("init");
    assert!(
        report.monitors.unique_url_index,
        "the duplicate-URL guarantee is part of the shape"
    );
    // The monitor half was created by this call, not by a second one.
    let tables: Vec<String> = store
        .query("SELECT name FROM sqlite_master WHERE type='table'", &[])
        .expect("tables")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    for table in ["alerts", "monitors", "monitor_seen", "monitor_runs"] {
        assert!(
            tables.iter().any(|current| current == table),
            "init did not create {}",
            table
        );
    }
    // And the statements the app runs on those tables work immediately: a monitor,
    // one check-log row and one alert, then the alert list the UI reads.
    let monitor = store
        .monitors()
        .add("Chips", "chip export", 6, 0, None, "2026-11-05T10:00:00Z")
        .expect("add monitor");
    store
        .monitors()
        .add_run(&crate::monitors::NewRun {
            monitor_id: monitor,
            reason: "button",
            started: "2026-11-05T10:00:00Z",
            finished: "2026-11-05T10:00:02Z",
            status: "ok",
            new_alerts: 1,
            judged: 1,
            window_hours: 72,
            scorer: "model",
            elapsed_ms: 2000,
            note: "",
        })
        .expect("check log");
    store
        .monitors()
        .add_alert(&crate::alerts::NewAlert {
            monitor_id: monitor,
            kind: "news",
            article_id: Some(1),
            url: "https://example.com/a",
            title: "A title",
            relevance: 8,
            summary: "",
            analysis: "why",
            entities_json: "[]",
            terms_json: "[\"chip\"]",
            created: "2026-11-05T10:00:02Z",
        })
        .expect("alert write")
        .expect("a new alert has an id");
    // `SELECT al.*` names every column, so this is what breaks if a column is
    // missing from the table `init` created.
    let alerts = store
        .monitors()
        .recent_alerts("2026-11-05T00:00:00Z", None, 10)
        .expect("alert list");
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].text("terms"), Some("[\"chip\"]"));
    assert_eq!(
        alerts[0].text("monitor_name"),
        Some("Chips"),
        "the alert list joins its monitor"
    );
    // A second init is the same shape, reported as already there.
    let again = store.init().expect("second init");
    assert_eq!(again.monitors.migrations_applied, 0);
    assert!(again.monitors.unique_url_index);
}


// ---------------------------------------------------------------------------
// The SQL boundary: what a value may and may not become
// ---------------------------------------------------------------------------
//
// The module docs of `lib.rs` claim "values are always bound; the only text
// assembled into a statement is a list of `?`". These four tests are that claim
// measured rather than asserted:
//
// * `no_value_reaches_a_statement_as_text` runs a hostile corpus through every
//   public query helper this crate publishes;
// * `a_function_in_a_tampered_schema_never_runs` and
//   `a_statement_cannot_rewrite_the_schema` prove the two connection flags that
//   make an injected *schema* inert;
// * `defensive_mode_leaves_the_fts5_commands_working` proves the flags did not
//   cost the product anything;
// * `every_archive_file_is_owner_only_including_the_wal` pins the at-rest modes.

/// Every hostile shape a request, a page, or a model can hand this crate.
///
/// The list is the alphabet of the attacks, not a vocabulary of banned words:
/// each entry is a *structural* attempt to end a bound value and start a
/// statement (quote + terminator + comment), to smuggle a wildcard into a `LIKE`
/// (`%`, `_`, `\`), to be FTS5 syntax, to be a `PRAGMA`/`ATTACH` statement, to
/// carry a NUL (which C APIs truncate at, so a truncated value is a different
/// value), or to be text that a naive encoder mangles (RTL override, combining
/// marks, emoji, CJK, the fullwidth quote).
const HOSTILE: &[&str] = &[
    "'",
    "\"",
    "`",
    "[",
    "]",
    "''",
    "''''",
    "'; DROP TABLE articles; --",
    "') OR 1=1 --",
    "\" OR \"\"=\"",
    "1; DELETE FROM meta WHERE 1=1; --",
    "x'; ATTACH DATABASE '/tmp/atlas-pwn.db' AS pwn; --",
    "'; PRAGMA writable_schema=ON; --",
    "PRAGMA journal_mode=DELETE",
    "PRAGMA schema_version=999999",
    "SELECT 1; SELECT 2",
    "--",
    ";",
    "/*",
    "*/",
    "/* quote */ ' /* still a value */",
    "%",
    "_",
    "\\",
    "\\'",
    "\\%",
    "100%",
    "a_b%c\\d",
    "{\"q\": \"'; DROP TABLE meta; --\"}",
    "${jndi:ldap://127.0.0.1/a}",
    "{{7*7}}",
    "NEAR(chip export)",
    "^title:harvest",
    "chip AND NOT (export OR *)",
    "with\u{0}a\u{0}nul",
    "\u{0}",
    "line\nbreak\tand\r\nreturn",
    "zero\u{200b}width",
    "rtl \u{202e}override",
    "combining e\u{301}te\u{301}",
    "ｆｕｌｌｗｉｄｔｈ 'quote'",
    "半導体 '缺口' --",
    "🛰️🗞️ emoji",
    "ﬁ ligature",
];

/// How many tables the archive has — the number a statement that escaped would
/// change.
fn table_count(store: &Store) -> i64 {
    store
        .query_one("SELECT COUNT(*) c FROM sqlite_master WHERE type='table'", &[])
        .expect("table count")
        .and_then(|row| row.int("c"))
        .expect("table count value")
}

/// One article row with `value` in every text column; returns its id.
fn article_of(store: &Store, value: &str) -> i64 {
    store
        .insert(
            "INSERT INTO articles(title,summary,content,url,publisher,published,iso) \
             VALUES(?,?,?,?,?,?,?)",
            &[
                text(value),
                text(value),
                text(value),
                text(value),
                text(value),
                text(value),
                text(value),
            ],
        )
        .expect("insert a hostile value")
}

#[test]
fn no_value_reaches_a_statement_as_text() {
    let store = store();
    // A table of this test's own for the `many` path: `meta` already holds these
    // keys through `set_meta` above, and a `UNIQUE` refusal would be a fact about
    // the fixture, not about the value. Created before the shape is counted.
    store
        .exec("CREATE TABLE probe_many(k TEXT PRIMARY KEY, v TEXT)", &[])
        .expect("scratch table");
    let tables = table_count(&store);

    for (index, value) in HOSTILE.iter().enumerate() {
        // 1. A stored row comes back byte for byte: the value was a value.
        let id = article_of(&store, value);
        let stored = store
            .query_one(
                "SELECT title,summary,content,url,publisher,published,iso FROM articles WHERE id=?",
                &[int(id)],
            )
            .expect("read back")
            .expect("the row exists");
        for column in [
            "title", "summary", "content", "url", "publisher", "published", "iso",
        ] {
            assert_eq!(
                stored.text(column),
                Some(*value),
                "column {} changed {:?}",
                column,
                value
            );
        }

        // 2. Every public helper takes it as an argument and answers `Ok`: a value
        //    that reached SQL text would raise a syntax error here, not return.
        store
            .find_duplicate(value, value, 6, None)
            .unwrap_or_else(|err| panic!("find_duplicate({value:?}): {}", err.message()));
        store
            .fts_query(value, 5, Some(72), Some(value), true)
            .unwrap_or_else(|err| panic!("fts_query({value:?}): {}", err.message()));
        store
            .search(value, 5)
            .unwrap_or_else(|err| panic!("search({value:?}): {}", err.message()));
        store
            .query_one("SELECT ? AS v, length(?) AS n", &[text(value), text(value)])
            .unwrap_or_else(|err| panic!("SELECT ? ({value:?}): {}", err.message()));

        // 3. `meta` is a key/value table: a hostile key is still a key. The value
        //    may come back `[redacted]` (atlas-guard's decision), never a row that
        //    vanished.
        store
            .set_meta(value, value)
            .unwrap_or_else(|err| panic!("set_meta({value:?}): {}", err.message()));
        let meta = store
            .get_meta(value)
            .unwrap_or_else(|err| panic!("get_meta({value:?}): {}", err.message()))
            .expect("the key was written");
        assert!(
            meta == *value || meta == "[redacted]",
            "meta value changed: {:?} -> {:?}",
            value,
            meta
        );

        // 4. The monitor and alert half, through the same view the routes use.
        let monitors = store.monitors();
        let monitor = monitors
            .add(value, value, 5, 0, None, "2026-11-05T10:00:00Z")
            .unwrap_or_else(|err| panic!("monitors.add({value:?}): {}", err.message()));
        let row = monitors
            .get(monitor)
            .unwrap_or_else(|err| panic!("monitors.get({value:?}): {}", err.message()))
            .expect("the monitor exists");
        assert_eq!(row.name, *value, "monitor name changed: {:?}", value);
        assert_eq!(row.query, *value, "monitor query changed: {:?}", value);
        monitors
            .list()
            .unwrap_or_else(|err| panic!("monitors.list({value:?}): {}", err.message()));
        monitors
            .articles_since(value, &[id], 5)
            .unwrap_or_else(|err| panic!("articles_since({value:?}): {}", err.message()));
        monitors
            .topic_name(monitor)
            .unwrap_or_else(|err| panic!("topic_name({value:?}): {}", err.message()));
        monitors
            .touch_run(monitor, value, index as i64)
            .unwrap_or_else(|err| panic!("touch_run({value:?}): {}", err.message()));
        let url = format!("https://example.test/{}", index);
        let alert = monitors
            .add_alert(&crate::alerts::NewAlert {
                monitor_id: monitor,
                kind: value,
                article_id: Some(id),
                url: &url,
                title: value,
                relevance: 9,
                summary: value,
                analysis: value,
                entities_json: value,
                terms_json: value,
                created: value,
            })
            .unwrap_or_else(|err| panic!("add_alert({value:?}): {}", err.message()));
        monitors
            .alert_exists(monitor, Some(id), Some(&url))
            .unwrap_or_else(|err| panic!("alert_exists({value:?}): {}", err.message()));
        if let Some(alert_id) = alert {
            monitors
                .mark_alert_seen(alert_id, true)
                .unwrap_or_else(|err| panic!("mark_alert_seen({value:?}): {}", err.message()));
        }
        monitors
            .recent_alerts(value, Some(value), 5)
            .unwrap_or_else(|err| panic!("recent_alerts({value:?}): {}", err.message()));
        monitors
            .recent_alerts("2026-11-05T00:00:00Z", Some(value), 5)
            .unwrap_or_else(|err| panic!("recent_alerts(some {value:?}): {}", err.message()));
        monitors
            .seen(monitor, value)
            .unwrap_or_else(|err| panic!("seen({value:?}): {}", err.message()));
        monitors
            .mark_seen(monitor, value)
            .unwrap_or_else(|err| panic!("mark_seen({value:?}): {}", err.message()));
        monitors
            .attach_entities(id, &[value.to_string()])
            .unwrap_or_else(|err| panic!("attach_entities({value:?}): {}", err.message()));
        monitors
            .unseen_count()
            .unwrap_or_else(|err| panic!("unseen_count({value:?}): {}", err.message()));

        // 5. The `many` path and the JSON parameter boundary.
        store
            .exec_many(
                "INSERT INTO probe_many(k,v) VALUES(?,?)",
                &[vec![text(value), text(value)]],
            )
            .unwrap_or_else(|err| panic!("exec_many({value:?}): {}", err.message()));
        let bound = crate::params::param_from_json(&serde_json::Value::String(value.to_string()))
            .unwrap_or_else(|err| panic!("param_from_json({value:?}): {}", err.message()));
        assert_eq!(bound, SqlValue::Text(value.to_string()));

        // Every exploit above would have changed the schema shape; none did.
        assert_eq!(
            table_count(&store),
            tables,
            "the table count changed after {:?}",
            value
        );
    }

    // 6. The archive is intact and every hostile article is still there, one row
    //    each: a value that had escaped would have dropped the table instead.
    let integrity = store
        .query_one("PRAGMA integrity_check", &[])
        .expect("integrity check")
        .and_then(|row| row.get_at(0).cloned());
    assert_eq!(integrity, Some(text("ok")));
    let articles = store
        .query_one("SELECT COUNT(*) c FROM articles", &[])
        .expect("count")
        .and_then(|row| row.int("c"));
    assert_eq!(articles, Some(HOSTILE.len() as i64));

    // 7. The two statements that summarise the archive still answer, over those
    //    rows: the returns, not just the calls.
    store.dedupe_titles(30, 6).expect("dedupe over hostile titles");
    store.prune_old(Some(3_650), 400).expect("prune over hostile rows");
    assert_eq!(table_count(&store), tables);
}

#[test]
fn a_statement_cannot_rewrite_the_schema() {
    // What `defensive` buys, measured. `PRAGMA writable_schema=ON` plus an
    // `UPDATE sqlite_master` is how SQL text authors a schema: a view or trigger
    // that runs a function, an index with an expression, a `CHECK` that is not a
    // check. With defensive mode the pragma does nothing and the write is refused,
    // so the worst a statement that ever took text could do is corrupt rows — never
    // the schema the next `init` runs from.
    let store = Store::open_memory().expect("store");
    store.init().expect("init");
    store.exec("CREATE TABLE scratch(a)", &[]).expect("scratch");

    let _ = store.exec("PRAGMA writable_schema=ON", &[]);
    let writable = store
        .query_one("PRAGMA writable_schema", &[])
        .expect("read writable_schema")
        .and_then(|row| row.int("writable_schema"));
    assert_eq!(writable, Some(0), "defensive mode left writable_schema=ON");

    let err = store
        .exec("UPDATE sqlite_master SET name='scratch2' WHERE name='scratch'", &[])
        .expect_err("the schema write must be refused");
    let message = err.message().to_ascii_lowercase();
    assert!(
        message.contains("may not be modified") || message.contains("readonly"),
        "unexpected refusal: {}",
        err.message()
    );
    // The table is still there under its own name.
    assert!(store.query_one("SELECT a FROM scratch", &[]).is_ok());
}

#[test]
fn a_view_in_a_stolen_archive_cannot_reach_a_function() {
    // `trusted_schema=OFF`, measured on the thing that makes it matter: schema
    // content calling code. `load_extension()` is the sharpest name a schema can
    // put in a view — it runs a shared library — and `pragma_database_list` is the
    // quiet one: a virtual table that reads every attached database. With SQLite's
    // default (trusted) both are legal inside a view; with the flag off the
    // *statement* is refused before anything runs.
    //
    // A view is not resolved when it is created, so `CREATE VIEW` succeeds on an
    // archive that was crafted elsewhere — which is exactly why the read is the
    // decision, and why this is a schema question and not a SQL-text one.
    let store = Store::open_memory().expect("store");
    store.init().expect("init");

    for (view, body) in [
        ("probe_ext", "SELECT load_extension('probe') AS v"),
        (
            "probe_dbs",
            "SELECT * FROM pragma_database_list",
        ),
    ] {
        store
            .exec(&format!("CREATE VIEW {} AS {}", view, body), &[])
            .expect("a view is DDL, accepted on an archive crafted elsewhere");
        let err = store
            .query(&format!("SELECT * FROM {}", view), &[])
            .expect_err("a schema object must not be able to call a function");
        let message = err.message().to_ascii_lowercase();
        assert!(
            message.contains("unsafe use"),
            "{} was not refused as unsafe: {}",
            view,
            err.message()
        );
    }

    // Not a blanket ban on views: a view over an innocuous function still answers,
    // so the flag costs the product no expressiveness. (`substr` is marked
    // innocuous by SQLite; nothing in this crate's schema is even that.)
    store
        .exec("CREATE VIEW probe_ok AS SELECT substr('abc',1,1) AS v", &[])
        .expect("innocuous view");
    assert_eq!(
        store
            .query_one("SELECT v FROM probe_ok", &[])
            .expect("read")
            .and_then(|row| row.text("v").map(|text| text.to_string())),
        Some("a".to_string())
    );

    // And the function a statement names is still off the table: extension loading
    // was never enabled on this connection, which is the second half of why a
    // hostile archive has nothing to reach for.
    let direct = store
        .query("SELECT load_extension('probe')", &[])
        .expect_err("extension loading must not be enabled");
    assert!(
        direct
            .message()
            .to_ascii_lowercase()
            .contains("not authorized"),
        "unexpected answer: {}",
        direct.message()
    );
}

#[test]
fn defensive_mode_leaves_the_fts5_commands_working() {
    // The two flags cost nothing: `'rebuild'` and `'integrity-check'` are commands
    // to the fts5 module, and the row delete goes through `xUpdate`, so defensive
    // mode's ban on *shadow-table SQL* never applies. If that ever changed, the
    // first index repair after a bad boot would fail and search would stay blind —
    // which is why this is asserted rather than assumed.
    let store = store();
    let id = article_of(&store, "Harvest report");
    store
        .exec(
            "UPDATE articles SET title='Harvest report', summary='grain', content='grain', \
             relevant=1 WHERE id=?",
            &[int(id)],
        )
        .expect("set the body");
    let sync = store.fts_rebuild().expect("rebuild under defensive mode");
    assert!(sync.in_sync(), "{:?}", sync);
    assert!(
        store.fts_integrity_ok(),
        "fts5 integrity-check refused under defensive mode"
    );
    assert_eq!(
        store.search("harvest", 5).expect("search").len(),
        1,
        "the rebuilt index answers"
    );
    // And the delete the prune path uses (`DELETE FROM article_fts WHERE rowid IN
    // (?)`) still goes through the module.
    store
        .exec("DELETE FROM article_fts WHERE rowid IN (?)", &[int(id)])
        .expect("fts row delete under defensive mode");
}

#[test]
fn every_archive_file_is_owner_only_including_the_wal() {
    // The ordering question, which the other permission test does not ask: SQLite
    // creates `-wal` and `-shm` on the *first write*, which is after `Store::open`
    // tightened the directory and the (then non-existent) side files, so their mode
    // comes from SQLite inheriting the database file's mode rather than from
    // `tighten`. Measured on this machine: all three are 0600 and the directory is
    // 0700, with no file left group- or world-readable.
    let dir = temp_dir("wal-modes");
    let db = dir.join("atlas.db");
    let store = Store::open(&db).expect("open");
    store.exec("CREATE TABLE t(a)", &[]).expect("first write");
    if POSIX {
        assert_eq!(perms::permissions(&dir).mode, "0o700", "data/ is {}", perms::permissions(&dir).mode);
        for file in store.files() {
            assert!(file.is_file(), "{} was never created", file.display());
            assert_eq!(
                perms::permissions(&file).mode,
                "0o600",
                "{} must be 0600",
                file.display()
            );
            assert!(
                !perms::permissions(&file).world_readable,
                "{} is readable by another account",
                file.display()
            );
        }
        let report = store.permission_report();
        assert!(report.ok, "{:?}", report);
        assert!(report.exposed.is_empty(), "{:?}", report);
    }
    drop(store);

    // An archive that arrives world-readable — restored from a backup, copied from
    // another machine — is repaired by the next open, not accepted as it is.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644))
            .expect("make it loose");
        assert_eq!(perms::permissions(&db).mode, "0o644");
        let reopened = Store::open(&db).expect("reopen");
        assert_eq!(
            perms::permissions(&db).mode,
            "0o600",
            "opening a loose archive must tighten it"
        );
        drop(reopened);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_search_over_the_whole_archive_is_what_store_fts_budgets() {
    // `atlas-bench` times `store.fts` as `store.search(term, 25)` on the archive
    // `docs/PERFORMANCE.md` describes: 5 000 articles, ~2 400-character bodies, one
    // term present in every one of them, so the index really has 5 000 candidates
    // and the answer is a page of 25. This is the same measurement in this crate, so
    // the number a budget is written against can be reproduced without the harness:
    //
    //   cargo test --release --offline -p atlas-store a_search_over_the_whole \
    //     -- --nocapture
    //
    // It prints p50/p95 over 30 warm runs (`atlas_bench::stats::RUNS` is 30) and
    // asserts only the structural facts: 25 rows come back, each with a snippet. A
    // wall-clock ceiling belongs in `atlas-bench`'s `budgets.rs`, which owns the
    // table; a bound here would fail on a machine compiling something else.
    //
    // Measured here (release, 2026-11), on a file archive like the harness's:
    // p50 3.3–3.9 ms, p95 3.7–4.0 ms. The statement this replaced — one `JOIN` and
    // `ORDER BY rank, published, id` — is the same fixture at p50 7.5–8.0 ms, p95
    // 7.7–8.6 ms, because it carried every matched article's whole row through a
    // temp b-tree to return 25. On an in-memory archive the same two are 2.0–2.3 and
    // 6.1–7.4 ms. See `search::index_sql`.
    use std::time::Instant;

    // On a file database, like the harness's archive: WAL, `synchronous=NORMAL`,
    // the page cache a real install has.
    let dir = temp_dir("fts-budget");
    let store = Store::open(dir.join("atlas.db")).expect("open");
    store.init().expect("init");
    let published = now_iso();
    // Bodies of filler tokens built arithmetically — no vocabulary list, and the one
    // shared token (`corpus`) is what the query below asks for. 400 tokens of ~6
    // characters is the ~2 400-character body `atlas-bench`'s fixture writes, so the
    // index holds the same number of postings to score.
    let bodies: Vec<String> = (0..5_000)
        .map(|index| {
            let mut text = String::from("corpus ");
            for k in 0..400 {
                text.push_str("t");
                text.push_str(&((index * 7 + k) % 600).to_string());
                text.push(' ');
            }
            text
        })
        .collect();
    let rows: Vec<Vec<SqlValue>> = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| {
            vec![
                text(&format!("Story {}", index + 1)),
                text("summary line"),
                text(body),
                text(&published),
                int(1),
            ]
        })
        .collect();
    store
        .insert_many(
            "INSERT INTO articles(title,summary,content,published,relevant) VALUES(?,?,?,?,?)",
            &rows,
        )
        .expect("seed 5 000 articles");
    // The index is what the search reads: without this the fallback answers, and the
    // measurement would be of the wrong path.
    assert!(store.fts_rebuild().expect("rebuild").in_sync());

    let started = Instant::now();
    let found = store.search("corpus", 25).expect("warm search");
    let cold = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(found.len(), 25);
    assert!(found.iter().all(|row| row.text("snippet").is_some()));

    let mut ms: Vec<f64> = Vec::with_capacity(30);
    for _ in 0..30 {
        let started = Instant::now();
        let found = store.search("corpus", 25).expect("search");
        ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(found.len(), 25);
    }
    ms.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    // Nearest rank, like `atlas_bench::stats::Samples::percentile`: rank =
    // ceil(p/100 * n), then the sample at `rank - 1`.
    let p50 = ms[14];
    let p95 = ms[28];
    println!(
        "atlas-store: store.search over 5 000 articles (2.4 kB bodies, 25 rows) = \
         p50 {p50:.2} ms, p95 {p95:.2} ms (first, cold: {cold:.2} ms)"
    );
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}
