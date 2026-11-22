//! The database shape: tables, indexes, and the version-less migration scheme.
//!
//! Decisions: what a new archive looks like ([`SCHEMA`]), which column additions
//! an existing one gets and in what order ([`MIGRATIONS`]), and that a migration
//! which fails because the column is already there is *the version marker*, not an
//! error. [`apply`] is that whole sequence against one connection.
//!
//! It does not: open a connection, set a pragma, or decide when the schema runs —
//! [`crate::Store::init`] does, and it reports how many statements actually
//! changed the database.

use rusqlite::Connection;

use crate::error::{Result, StoreError};

/// Tables and indexes `db.py` creates on every connect. `IF NOT EXISTS`
/// everywhere, so this is idempotent by construction.
///
/// Two of `db.py`'s indexes are **not** here: `idx_links_a` and `idx_edges_a`.
/// Both are single-column indexes on the first column of a primary key that
/// already starts with it (`article_links(a,b)`, `edges(a,b,type)`), so the
/// primary key's own index answers every lookup they did — `EXPLAIN QUERY PLAN`
/// names `sqlite_autoindex_article_links_1` / `sqlite_autoindex_edges_1 (a=?)` for
/// `WHERE a IN (…)`, and the `a IN (…) OR b IN (…)` statements still resolve to a
/// `MULTI-INDEX OR` of the primary key and `idx_links_b`/`idx_edges_b`. What they
/// cost was a second B-tree on the two most-written tables in the archive:
/// measured with `sqlite3` over 60 000 rows, three runs each, writing
/// `article_links` took 17.1 / 26.1 / 29.0 ms with `idx_links_a` and
/// 6.4 / 5.8 / 7.8 ms without it, and upserting 60 000 `edges` took
/// 26.9 / 21.9 / 32.9 ms with `idx_edges_a` and 26.5 / 15.9 / 17.5 ms without.
/// An archive that already holds them loses them through
/// [`RETIRED_INDEXES`].
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sources(
  id INTEGER PRIMARY KEY, name TEXT, url TEXT, rss TEXT UNIQUE,
  country TEXT, iso TEXT, lat REAL, lon REAL, lang TEXT,
  active INTEGER DEFAULT 1, last_ok TEXT, last_error TEXT, n INTEGER DEFAULT 0,
  kind TEXT DEFAULT 'base', topic_id INTEGER);
CREATE TABLE IF NOT EXISTS articles(
  id INTEGER PRIMARY KEY, source_id INTEGER, url TEXT UNIQUE, guid TEXT,
  title TEXT, summary TEXT, content TEXT, author TEXT,
  published TEXT, fetched TEXT, lang TEXT, iso TEXT, lat REAL, lon REAL, hash TEXT,
  relevant INTEGER DEFAULT 0, dupe INTEGER DEFAULT 0,
  full_text INTEGER DEFAULT 0, fetch_tries INTEGER DEFAULT 0, fetched_url TEXT, read_at TEXT);
CREATE VIRTUAL TABLE IF NOT EXISTS article_fts USING fts5(
  title, summary, content, content='articles', content_rowid='id');
CREATE TABLE IF NOT EXISTS entities(
  id INTEGER PRIMARY KEY, name TEXT UNIQUE, type TEXT, category_id INTEGER,
  count INTEGER DEFAULT 1, first_seen TEXT, last_seen TEXT);
CREATE TABLE IF NOT EXISTS article_entities(
  article_id INTEGER, entity_id INTEGER, weight REAL DEFAULT 1,
  PRIMARY KEY(article_id, entity_id));
CREATE TABLE IF NOT EXISTS edges(
  a INTEGER, b INTEGER, weight REAL DEFAULT 1, type TEXT DEFAULT 'cooccur', label TEXT,
  PRIMARY KEY(a, b, type));
CREATE TABLE IF NOT EXISTS categories(
  id INTEGER PRIMARY KEY, name TEXT UNIQUE, kind TEXT, description TEXT, created TEXT);
CREATE TABLE IF NOT EXISTS article_categories(
  article_id INTEGER, category_id INTEGER, PRIMARY KEY(article_id, category_id));
CREATE TABLE IF NOT EXISTS entity_alias(
  alias TEXT UNIQUE, entity_id INTEGER);
CREATE TABLE IF NOT EXISTS article_links(
  a INTEGER, b INTEGER, shared INTEGER, score REAL, why TEXT, PRIMARY KEY(a, b));
CREATE TABLE IF NOT EXISTS threads(
  id INTEGER PRIMARY KEY, title TEXT, size INTEGER, updated TEXT);
CREATE TABLE IF NOT EXISTS article_threads(
  article_id INTEGER PRIMARY KEY, thread_id INTEGER);
CREATE TABLE IF NOT EXISTS article_countries(
  article_id INTEGER, iso TEXT, is_primary INTEGER DEFAULT 0,
  PRIMARY KEY(article_id, iso));
CREATE TABLE IF NOT EXISTS topics(
  id INTEGER PRIMARY KEY, name TEXT UNIQUE, keywords TEXT, exclude TEXT,
  min_matches INTEGER DEFAULT 1, active INTEGER DEFAULT 1,
  created TEXT, hits INTEGER DEFAULT 0, last_hit TEXT);
CREATE TABLE IF NOT EXISTS article_topics(
  article_id INTEGER, topic_id INTEGER, score REAL DEFAULT 1, matched TEXT,
  PRIMARY KEY(article_id, topic_id));
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);
CREATE INDEX IF NOT EXISTS idx_articles_published ON articles(published DESC);
CREATE INDEX IF NOT EXISTS idx_articles_iso ON articles(iso);
CREATE INDEX IF NOT EXISTS idx_articles_source ON articles(source_id);
CREATE INDEX IF NOT EXISTS idx_ae_entity ON article_entities(entity_id);
CREATE INDEX IF NOT EXISTS idx_ae_article ON article_entities(article_id);
CREATE INDEX IF NOT EXISTS idx_links_b ON article_links(b);
CREATE INDEX IF NOT EXISTS idx_entities_count ON entities(count DESC);
CREATE INDEX IF NOT EXISTS idx_edges_b ON edges(b);
CREATE INDEX IF NOT EXISTS idx_at_topic ON article_topics(topic_id);
"#;

/// Indexes that only exist once the columns they name do, plus the three ATLAS
/// adds that `db.py` never had.
///
/// * `idx_article_threads_thread` — `article_threads` is keyed on `article_id`, so
///   every `WHERE at2.thread_id=?` (the threads list, the thread view, the
///   timeline's join) scanned the whole table once per thread. Measured on the
///   5 000-article archive: `/api/threads` p95 23.09 ms → 1.41 ms.
/// * `idx_articles_window` — every count and aggregate the UI shares filters
///   `relevant=1 AND dupe=0 AND published>=?`, and `db.py`'s index is
///   `(relevant, published DESC)`: `dupe`, `full_text` and `iso` were not in it, so
///   each of those statements read the row of every match. Measured on the
///   5 000-article archive: the status page's relevant count 5.37 ms → 0.09 ms, its
///   full-text sum 4.0 ms → 0.13 ms, the map's per-country count 4.26 ms → 0.57 ms.
///   A covering index is what those three statements wanted; a `place`-carrying
///   variant was measured too and bought nothing (6.6 ms either way), so it is not
///   here.
/// * `idx_articles_retention` — the prune's cutoff is
///   `COALESCE(NULLIF(published,''),NULLIF(fetched,''),'') < ?`, an expression no
///   existing index could answer, so every prune pass scanned `articles`. Measured
///   three ways, all on the same code: on a 51 435-article probe archive with
///   `sqlite3`'s timer the statement **23.3 ms scanned, 0.047 ms** with this index
///   (the `LIMIT 400` becomes a covering range scan); in this crate's own test
///   `prune::tests::the_retention_cutoff_reads_an_index_and_here_is_what_it_bought`
///   over 20 000 articles **2.538 ms → 0.007 ms (361×)**, release build; and
///   `atlas-bench --only store.prune --no-startup --repeat 3` on its 5 000-article
///   fixture went from **p50 3.90 / p95 6.03 ms to p50 0.01 / p95 0.01 ms**.
///   What it costs is a B-tree update per article write, and that is inside the
///   noise of the write itself: 20 000 articles inserted into one transaction
///   measured 28.4 / 32.0 / 31.4 ms without the index and 34.5 / 25.4 / 29.5 ms
///   with it, three runs each. An expression index on
///   `(relevant, COALESCE(NULLIF(fetched,''),published) DESC)` for the monitors'
///   candidate window was measured too and **rejected**: the `ORDER BY published`
///   cannot use it, so the same statement read 0.29 ms → 3.96 ms (13× slower).
///
/// All of them are additive: an archive created before them gets them on the next
/// `Store::init`, and `IF NOT EXISTS` makes that idempotent.
pub const POST_MIGRATION_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_articles_relevant ON articles(relevant, published DESC)",
    "CREATE INDEX IF NOT EXISTS idx_sources_kind ON sources(kind)",
    "CREATE INDEX IF NOT EXISTS idx_articles_fulltext ON articles(full_text, published DESC)",
    "CREATE INDEX IF NOT EXISTS idx_article_threads_thread ON article_threads(thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_articles_window ON articles(relevant, dupe, published DESC, full_text, iso)",
    "CREATE INDEX IF NOT EXISTS idx_articles_retention \
     ON articles(COALESCE(NULLIF(published,''),NULLIF(fetched,''),''))",
];

/// Column additions, in `db.py` order. A statement that fails because the column
/// already exists is expected and counted, not raised — that is how this schema
/// tracks its own version. Any *other* failure is raised ([`is_version_marker`]
/// decides which is which), so an archive that could not be migrated says so
/// instead of half-reading afterwards.
pub const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS article_tags(article_id INTEGER NOT NULL, tag TEXT NOT NULL, \
     weight REAL DEFAULT 1, source TEXT, created TEXT, PRIMARY KEY(article_id, tag))",
    // The provenance column, for an archive whose `article_tags` was created before
    // the column existed: `CREATE TABLE IF NOT EXISTS` above is a no-op there, so
    // without this `ALTER` the table keeps the old shape and every statement that
    // names `source` (atlas-migrate's tag carry, atlas-ingest's tag write, the tag
    // read in `/api/article`) fails with "no such column: source" on a live archive.
    // Existing rows keep tag/weight/created and read `source` as NULL — "written
    // before the archive recorded provenance" — which is exactly what
    // `atlas-migrate`'s `tag.provenance` note reports.
    "ALTER TABLE article_tags ADD COLUMN source TEXT",
    "ALTER TABLE entities ADD COLUMN category_id INTEGER",
    "ALTER TABLE edges ADD COLUMN label TEXT",
    "ALTER TABLE articles ADD COLUMN relevant INTEGER DEFAULT 0",
    "ALTER TABLE articles ADD COLUMN dupe INTEGER DEFAULT 0",
    "ALTER TABLE articles ADD COLUMN full_text INTEGER DEFAULT 0",
    "ALTER TABLE articles ADD COLUMN fetch_tries INTEGER DEFAULT 0",
    "ALTER TABLE articles ADD COLUMN fetched_url TEXT",
    "ALTER TABLE articles ADD COLUMN read_at TEXT",
    "ALTER TABLE articles ADD COLUMN publisher TEXT",
    "ALTER TABLE articles ADD COLUMN place TEXT",
    "ALTER TABLE articles ADD COLUMN geo_conf REAL",
    "ALTER TABLE articles ADD COLUMN geo_src TEXT",
    "ALTER TABLE article_links ADD COLUMN why TEXT",
    "ALTER TABLE sources ADD COLUMN kind TEXT DEFAULT 'base'",
    "ALTER TABLE sources ADD COLUMN topic_id INTEGER",
];

/// Indexes an archive created before this version still holds and must lose.
///
/// A retired index is *dropped*, never created, so this list can only ever get
/// shorter in cost: `DROP INDEX IF EXISTS` on an archive that never had it is a
/// no-op, and running it twice changes nothing. It exists so the schema can be
/// corrected on a live archive instead of costing every user a fresh database.
///
/// * `idx_links_a` / `idx_edges_a` — redundant with the primary keys
///   `article_links(a,b)` and `edges(a,b,type)`; see [`SCHEMA`] for the measured
///   write cost of keeping them.
pub const RETIRED_INDEXES: &[&str] = &[
    "DROP INDEX IF EXISTS idx_links_a",
    "DROP INDEX IF EXISTS idx_edges_a",
];

/// What [`Store::init`] did, so a caller can log counts instead of prose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Init {
    /// Migrations that changed the database.
    pub migrations_applied: usize,
    /// Migrations that were already there (the normal case after the first run).
    pub migrations_skipped: usize,
    /// Post-migration indexes created or already present.
    pub indexes: usize,
    /// The monitor half of the same archive ([`crate::monitors::Applied`]): the
    /// `alerts`/`monitors`/`monitor_seen`/`monitor_runs` tables, their columns and
    /// their indexes. Reported here because [`Store::init`] creates them too — one
    /// call, one complete shape, so a caller never has to know there were once two.
    pub monitors: crate::monitors::Applied,
}

/// One migration list's outcome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Migrations {
    /// Statements that changed the database.
    pub applied: usize,
    /// Statements that failed *because they had already run*.
    pub skipped: usize,
}

/// Create the schema, run every migration, then the indexes that need the
/// migrated columns. Idempotent: a second call applies nothing and reports
/// [`Init::migrations_skipped`] instead.
///
/// Split out of [`crate::Store::init`] so the DDL and the connection lifetime are
/// two decisions in two files.
pub(crate) fn apply(conn: &Connection) -> Result<Init> {
    conn.execute_batch(SCHEMA).map_err(StoreError::from)?;
    let done = run_migrations(conn, MIGRATIONS)?;
    // A drop needs no column, but it runs in a fixed place in the sequence so two
    // `init` calls report the same thing: schema, migrations, retirements, indexes.
    for statement in RETIRED_INDEXES {
        conn.execute(statement, []).map_err(StoreError::from)?;
    }
    let mut report = Init {
        migrations_applied: done.applied,
        migrations_skipped: done.skipped,
        indexes: 0,
        monitors: Default::default(),
    };
    for statement in POST_MIGRATION_INDEXES {
        conn.execute(statement, []).map_err(StoreError::from)?;
        report.indexes += 1;
    }
    Ok(report)
}

/// Run one migration list: count what changed, count what was already there, and
/// **raise** anything else.
///
/// Split out because the rule has two halves and only one of them is about
/// counting: [`is_version_marker`] decides which failures mean "this migration has
/// run", and a failure that means anything else must reach the caller — an archive
/// whose migration silently "skipped" keeps the old columns, and every later read
/// of a missing column fails somewhere far away from here. `crate::monitors::init`
/// runs its own list through this, so the rule is written once.
pub(crate) fn run_migrations(conn: &Connection, statements: &[&str]) -> Result<Migrations> {
    let mut done = Migrations::default();
    for statement in statements {
        match conn.execute(statement, []) {
            Ok(_) => done.applied += 1,
            Err(err) if is_version_marker(&err.to_string()) => done.skipped += 1,
            // Anything else ("no such table", "database is locked", a full disk) is
            // not a version marker: the archive is now half-migrated and every
            // later read of the missing column would fail. Raise it with the
            // statement, which is DDL and carries no article text or value.
            Err(err) => {
                return Err(StoreError::new(format!(
                    "migration failed ({}): {}",
                    err,
                    crate::fields::truncate(statement, 120)
                )))
            }
        }
    }
    Ok(done)
}

/// Is this SQLite error the "this migration has already run" marker rather than a
/// real failure? Two messages mean that, and only they are swallowed:
///
/// * `duplicate column name: x` — a second `ALTER TABLE … ADD COLUMN`;
/// * `table x already exists` / `index x already exists` — a `CREATE` without
///   `IF NOT EXISTS` (the `article_tags` migration is one), or an archive where a
///   later migration re-creates something an earlier one made.
///
/// Everything else is a failure the caller has to see. `db.py` swallowed no
/// migration error at all (`except sqlite3.OperationalError: if "duplicate column
/// name" not in str(e): raise`), so this is its rule, stated once; the test
/// `only_the_version_marker_is_a_skipped_migration` pins both directions with the
/// messages SQLite actually produces on this build.
pub(crate) fn is_version_marker(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("duplicate column name")
        || message.contains("already exists")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use crate::Store;

#[test]
fn init_is_idempotent_and_creates_every_table() {
    let store = store();
    let again = store.init().expect("second init");
    // One migration is `CREATE TABLE IF NOT EXISTS article_tags`, which succeeds
    // every time; every `ALTER TABLE` statement after it is already done.
    assert_eq!(again.migrations_applied, 1);
    assert_eq!(again.migrations_skipped, MIGRATIONS.len() - 1);
    assert_eq!(again.indexes, POST_MIGRATION_INDEXES.len());
    // The monitor half is part of the same call and reports itself the same way.
    assert_eq!(again.monitors.migrations_applied, 0);
    assert_eq!(again.monitors.migrations_skipped, crate::monitors::MIGRATIONS.len());
    assert!(again.monitors.unique_url_index);
    // A third run reports exactly the same thing.
    let third = store.init().expect("third init");
    assert_eq!(third, again);
    let tables: Vec<String> = store
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
            &[],
        )
        .expect("tables")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    // Every table the app reads, from one `init`: the core shape `db.py` created
    // *and* the monitor tables `monitor.py` created, because the boot path calls
    // `init` and `/api/alerts` reads `alerts` before any check has run.
    for required in [
        "alerts",
        "article_categories",
        "article_countries",
        "article_entities",
        "article_links",
        "article_tags",
        "article_threads",
        "article_topics",
        "articles",
        "briefings",
        "categories",
        "edges",
        "entities",
        "entity_alias",
        "enrich_checked",
        "meta",
        "monitor_runs",
        "monitor_seen",
        "monitors",
        "sources",
        "threads",
        "topics",
    ] {
        assert!(
            tables.iter().any(|table| table == required),
            "missing table {}",
            required
        );
    }
    assert!(
        tables.iter().any(|table| table == "article_fts_data"),
        "FTS5 must be enabled in the bundled build"
    );
}


#[test]
fn migration_from_the_old_schema_adds_every_column() {
    let store = Store::open_memory().expect("store");
    // The pre-migration shape: articles and sources without the columns
    // MIGRATIONS adds.
    store
        .exec_batch_old_schema()
        .expect("old schema fixture");
    store.init().expect("init migrates");
    let columns: Vec<String> = store
        .query("PRAGMA table_info(articles)", &[])
        .expect("table_info")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    for column in [
        "relevant",
        "dupe",
        "full_text",
        "fetch_tries",
        "fetched_url",
        "read_at",
        "publisher",
        "place",
        "geo_conf",
        "geo_src",
    ] {
        assert!(
            columns.iter().any(|current| current == column),
            "missing column articles.{}",
            column
        );
    }
    let source_columns: Vec<String> = store
        .query("PRAGMA table_info(sources)", &[])
        .expect("table_info")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    assert!(source_columns.iter().any(|column| column == "kind"));
    assert!(source_columns.iter().any(|column| column == "topic_id"));
    // `article_tags` was created before provenance existed; the migration has to
    // add the column the archive's readers name.
    let tag_columns: Vec<String> = store
        .query("PRAGMA table_info(article_tags)", &[])
        .expect("table_info")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    for column in ["article_id", "tag", "weight", "source", "created"] {
        assert!(
            tag_columns.iter().any(|current| current == column),
            "missing column article_tags.{}",
            column
        );
    }
    // The indexes that need those columns exist afterwards.
    let indexes: Vec<String> = store
        .query("SELECT name FROM sqlite_master WHERE type='index'", &[])
        .expect("indexes")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    for index in [
        "idx_articles_relevant",
        "idx_sources_kind",
        "idx_articles_fulltext",
        "idx_article_threads_thread",
        "idx_articles_window",
        "idx_articles_retention",
    ] {
        assert!(
            indexes.iter().any(|current| current == index),
            "missing index {}",
            index
        );
    }
}

/// The `EXPLAIN QUERY PLAN` of one statement, as one uppercase line.
///
/// A plan is chosen before any value is bound, so each `?` in the statement is
/// bound to `NULL` and the plan is the one the real call would get.
fn plan(store: &Store, sql: &str) -> String {
    let bound: Vec<crate::SqlValue> =
        vec![crate::SqlValue::Null; sql.matches('?').count()];
    store
        .query(&format!("EXPLAIN QUERY PLAN {}", sql), &bound)
        .expect("query plan")
        .iter()
        .filter_map(|row| row.text("detail").map(|detail| detail.to_string()))
        .collect::<Vec<String>>()
        .join(" | ")
        .to_ascii_uppercase()
}

#[test]
fn the_thread_lookups_use_an_index_instead_of_scanning() {
    let store = store();
    // Every route that asks "which articles are in thread N" must walk an index:
    // `article_threads` is keyed on `article_id`, so without
    // `idx_article_threads_thread` the threads list scanned the table once per
    // thread (23 ms p95 for 40 threads on a 5 000-article archive).
    for sql in [
        "SELECT article_id FROM article_threads WHERE thread_id=1",
        "SELECT ar.id FROM article_threads at2 JOIN articles ar ON ar.id=at2.article_id \
         WHERE at2.thread_id=1",
    ] {
        let joined = plan(&store, sql);
        assert!(
            joined.contains("IDX_ARTICLE_THREADS_THREAD"),
            "this statement scans article_threads: {} -> {}",
            sql,
            joined
        );
    }
}

/// The window every screen filters on: `relevant=1 AND dupe=0 AND published>=?`.
///
/// These are the statements the routes issue (counts, the map's per-country
/// group-by, the timeline's id scan) plus the store's own (the dedupe window and
/// the prune's cutoff). Each one has to stay a `SEARCH` on the index that was
/// added for it: the whole point of `idx_articles_window` and
/// `idx_articles_retention` is that this assertion fails the day a statement stops
/// using them, instead of the day somebody notices the status page is slow.
#[test]
fn the_window_scans_read_an_index_instead_of_the_table() {
    let store = store();
    // A few hundred rows, so the planner is choosing from statistics that look
    // like an archive rather than from an empty table.
    let mut rows: Vec<Vec<crate::SqlValue>> = Vec::with_capacity(400);
    for index in 0..400 {
        rows.push(vec![
            text(&format!("Story {}", index)),
            text("summary"),
            text("body"),
            text(&crate::clock::iso_ago(index as f64)),
            text("FRA"),
            int(1),
        ]);
    }
    store
        .insert_many(
            "INSERT INTO articles(title,summary,content,published,iso,relevant) VALUES(?,?,?,?,?,?)",
            &rows,
        )
        .expect("seed");

    // (statement, the index it must name)
    for (sql, index) in [
        (
            "SELECT COUNT(*) c FROM articles WHERE relevant=1 AND dupe=0 AND published>=?",
            "IDX_ARTICLES_WINDOW",
        ),
        (
            "SELECT COUNT(*) total, SUM(full_text) done FROM articles WHERE relevant=1 AND dupe=0",
            "IDX_ARTICLES_WINDOW",
        ),
        (
            "SELECT iso, COUNT(*) c FROM articles WHERE relevant=1 AND dupe=0 \
             AND iso IS NOT NULL AND iso!='' AND published>=? GROUP BY iso",
            "IDX_ARTICLES_WINDOW",
        ),
        (
            "SELECT id FROM articles WHERE relevant=1 AND published>=? ORDER BY published DESC LIMIT 200",
            "IDX_ARTICLES_RELEVANT",
        ),
        // `Store::find_duplicate`'s window (`articles.rs`).
        (
            "SELECT id,title,publisher FROM articles WHERE dupe=0 AND published>=? \
             ORDER BY published DESC LIMIT 5000",
            "IDX_ARTICLES_PUBLISHED",
        ),
        // `Store::prune_old_detailed`'s cutoff (`prune.rs`): the expression that
        // no index could answer before `idx_articles_retention` existed, and that
        // covered all 51 435 rows of a probe archive in 23.3 ms without it.
        (
            "SELECT id FROM articles WHERE COALESCE(NULLIF(published,''),NULLIF(fetched,''),'')<>'' \
             AND COALESCE(NULLIF(published,''),NULLIF(fetched,''),'') < ? LIMIT 400",
            "IDX_ARTICLES_RETENTION",
        ),
    ] {
        let joined = plan(&store, sql);
        assert!(
            joined.contains("SEARCH ARTICLES USING"),
            "this window statement scans articles instead of ranging over an index: {} -> {}",
            sql,
            joined
        );
        assert!(
            joined.contains(index),
            "this window statement no longer uses {}: {} -> {}",
            index,
            sql,
            joined
        );
    }
}

#[test]
fn the_redundant_link_and_edge_indexes_are_retired_and_the_primary_key_answers() {
    // `idx_links_a`/`idx_edges_a` repeat the first column of a primary key that
    // already starts with it, so they bought nothing on a read and cost a B-tree
    // on the two most-written tables (measured in `SCHEMA`'s doc comment).
    let store = store();
    let names: Vec<String> = store
        .query("SELECT name FROM sqlite_master WHERE type='index'", &[])
        .expect("indexes")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    for retired in ["idx_links_a", "idx_edges_a"] {
        assert!(
            !names.iter().any(|name| name == retired),
            "{} must not be created any more",
            retired
        );
    }
    // The lookups they used to answer still range over an index — the primary
    // key's own, or the `b` index the OR optimisation pairs it with.
    for (sql, index) in [
        (
            "SELECT b FROM article_links WHERE a IN (1,2,3)",
            "SQLITE_AUTOINDEX_ARTICLE_LINKS_1",
        ),
        (
            "SELECT a FROM article_links WHERE b IN (1,2,3)",
            "IDX_LINKS_B",
        ),
        (
            "SELECT b FROM edges WHERE a IN (1,2,3) OR b IN (1,2,3) ORDER BY weight DESC",
            "SQLITE_AUTOINDEX_EDGES_1",
        ),
        (
            "SELECT a,b,weight FROM edges WHERE a IN (1,2,3) AND b IN (1,2,3)",
            "SQLITE_AUTOINDEX_EDGES_1",
        ),
    ] {
        let joined = plan(&store, sql);
        assert!(
            joined.contains(index),
            "{} must be answered by the primary key ({}): {}",
            sql,
            index,
            joined
        );
    }
    // An archive that already holds them loses them on the next `init`, and keeps
    // its rows.
    let legacy = Store::open_memory().expect("store");
    legacy.init().expect("init");
    legacy
        .exec("CREATE INDEX idx_links_a ON article_links(a)", &[])
        .expect("the old link index");
    legacy
        .exec("CREATE INDEX idx_edges_a ON edges(a)", &[])
        .expect("the old edge index");
    legacy
        .exec(
            "INSERT INTO article_links(a,b,shared,score) VALUES(1,2,5,0.5)",
            &[],
        )
        .expect("a link row");
    legacy.init().expect("second init retires them");
    let names: Vec<String> = legacy
        .query("SELECT name FROM sqlite_master WHERE type='index'", &[])
        .expect("indexes")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    for retired in ["idx_links_a", "idx_edges_a"] {
        assert!(
            !names.iter().any(|name| name == retired),
            "{} survived a migration that retires it",
            retired
        );
    }
    assert_eq!(
        legacy
            .query("SELECT COUNT(*) c FROM article_links", &[])
            .expect("count")[0]
            .int("c"),
        Some(1),
        "dropping an index must not touch a row"
    );
}

#[test]
fn only_the_version_marker_is_a_skipped_migration() {
    // The messages are SQLite's own, produced here rather than typed from memory,
    // and the two directions are both pinned: the marker is swallowed (that is how
    // this schema tracks its version) and every other failure is raised (a
    // half-migrated archive that reads as "fine" is the worse outcome).
    let conn = Connection::open_in_memory().expect("connection");
    conn.execute_batch("CREATE TABLE t(x INTEGER)")
        .expect("table");
    let duplicate = conn
        .execute("ALTER TABLE t ADD COLUMN x INTEGER", [])
        .expect_err("the column is already there");
    assert!(
        is_version_marker(&duplicate.to_string()),
        "a second ALTER must read as \"already ran\": {}",
        duplicate
    );
    let exists = conn
        .execute("CREATE TABLE t(x INTEGER)", [])
        .expect_err("the table is already there");
    assert!(
        is_version_marker(&exists.to_string()),
        "a second CREATE is \"already there\": {}",
        exists
    );
    for not_a_marker in [
        conn.execute("ALTER TABLE missing_table ADD COLUMN z INTEGER", [])
            .expect_err("no such table"),
        conn.execute("ALTER TABLE t ADD COLUMN", [])
            .expect_err("syntax error"),
    ] {
        assert!(
            !is_version_marker(&not_a_marker.to_string()),
            "this is a real failure, not a version marker: {}",
            not_a_marker
        );
    }
    // And the rule is what `run_migrations` applies: a list containing a genuine
    // failure returns an error instead of counting it as "skipped".
    let failed = run_migrations(&conn, &["ALTER TABLE missing_table ADD COLUMN z INTEGER"])
        .expect_err("a real failure must reach the caller");
    assert!(
        failed.message().contains("migration failed"),
        "the error says what happened: {}",
        failed.message()
    );
    let clean = run_migrations(
        &conn,
        &[
            "ALTER TABLE t ADD COLUMN y INTEGER",
            "ALTER TABLE t ADD COLUMN y INTEGER",
        ],
    )
    .expect("the second statement is the version marker");
    assert_eq!((clean.applied, clean.skipped), (1, 1));
}

/// The schema as it looked before any migration ran.
trait OldSchemaFixture {
    fn exec_batch_old_schema(&self) -> Result<()>;
}

impl OldSchemaFixture for Store {
    fn exec_batch_old_schema(&self) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(StoreError::from)?;
        tx.execute_batch(
            "CREATE TABLE articles(
               id INTEGER PRIMARY KEY, source_id INTEGER, url TEXT UNIQUE, guid TEXT,
               title TEXT, summary TEXT, content TEXT, author TEXT,
               published TEXT, fetched TEXT, lang TEXT, iso TEXT, lat REAL, lon REAL, hash TEXT);
             CREATE TABLE sources(
               id INTEGER PRIMARY KEY, name TEXT, url TEXT, rss TEXT UNIQUE,
               country TEXT, iso TEXT, lat REAL, lon REAL, lang TEXT,
               active INTEGER DEFAULT 1, last_ok TEXT, last_error TEXT, n INTEGER DEFAULT 0);
             CREATE TABLE entities(id INTEGER PRIMARY KEY, name TEXT UNIQUE, type TEXT,
               count INTEGER DEFAULT 1, first_seen TEXT, last_seen TEXT);
             CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL DEFAULT 1,
               type TEXT DEFAULT 'cooccur', PRIMARY KEY(a,b,type));
             -- `article_tags` as it was created before provenance existed: no
             -- `source` column, one tag row already in it.
             CREATE TABLE article_tags(article_id INTEGER NOT NULL, tag TEXT NOT NULL,
               weight REAL DEFAULT 1, created TEXT, PRIMARY KEY(article_id, tag));
             INSERT INTO article_tags(article_id,tag,weight,created)
               VALUES(1,'grain',1.5,'2026-01-02T03:04:05Z');",
        )
        .map_err(StoreError::from)?;
        tx.commit().map_err(StoreError::from)?;
        Ok(())
    }
}

#[test]
fn an_old_archive_keeps_its_tag_rows_and_gains_provenance() {
    let store = Store::open_memory().expect("store");
    store
        .exec_batch_old_schema()
        .expect("old schema fixture");
    store.init().expect("init migrates");

    // The row that was already there is untouched, and its provenance reads NULL:
    // "written before the archive recorded where a tag came from". Nothing is
    // deleted or re-attributed by a migration.
    let kept = store
        .query_one(
            "SELECT article_id,tag,weight,created,source FROM article_tags WHERE tag=?",
            &[text("grain")],
        )
        .expect("tag read")
        .expect("the old tag row must survive the migration");
    assert_eq!(kept.int("article_id"), Some(1));
    assert_eq!(kept.real("weight"), Some(1.5));
    assert_eq!(kept.text("created"), Some("2026-01-02T03:04:05Z"));
    assert_eq!(kept.text("source"), None, "an old row has no provenance");

    // A writer that names provenance can use it now. This is the statement
    // atlas-ingest issues (`save.rs`, `enrich.rs`) and the one atlas-migrate's tag
    // review reads (`queries::TAGS`); on an archive like this one both failed with
    // "no such column: source" before the column was migrated in.
    store
        .exec(
            "INSERT INTO article_tags(article_id,tag,weight,source,created) VALUES(?,?,?,?,?)",
            &[
                int(2),
                text("harvest"),
                crate::SqlValue::Real(1.0),
                text("ai"),
                text("2026-11-05T10:00:00Z"),
            ],
        )
        .expect("a tag with provenance");
    let written = store
        .query_one(
            "SELECT source FROM article_tags WHERE article_id=2 AND tag=?",
            &[text("harvest")],
        )
        .expect("read")
        .expect("row");
    assert_eq!(written.text("source"), Some("ai"));

    // And a second init still changes nothing: the migration is idempotent on the
    // archive it just migrated, which is the property the boot path relies on.
    let again = store.init().expect("second init");
    assert_eq!(again.migrations_applied, 1, "only the CREATE TABLE re-applies");
    assert_eq!(
        store
            .query("SELECT COUNT(*) c FROM article_tags", &[])
            .expect("count")[0]
            .int("c"),
        Some(2),
        "neither migration may touch the rows"
    );
}

}
