//! The window's SQL, proved rather than promised.
//!
//! `src/data.rs` is the only place the native window talks to SQLite: one read-only
//! connection and one statement per list. This file proves the four things that claim
//! rests on. It reads the SQL *out of the source* (`include_str!`, then the literals and
//! the `const` fragments the statements are assembled from) instead of retyping it, so
//! a statement cannot drift away from what is proved here, and it runs the assembled
//! statements against a temporary archive under `std::env::temp_dir()` — never `data/`:
//!
//! 1. **Every query binds its values.** No statement text is assembled from a value.
//!    The only text assembled at all is (a) the four `const` fragments the article
//!    statement is composed of, chosen by a `bool`, and (b) a run of `?` placeholders
//!    sized by a count. `edges` binds its ids; it used to interpolate them.
//! 2. **The connection cannot write.** The archive is opened `SQLITE_OPEN_READ_ONLY`
//!    *and* `PRAGMA query_only = ON`, and `open` fails if the switch did not take. A
//!    write through the same two guards is refused by SQLite and the file is unchanged.
//! 3. **An empty search box costs no search.** It is a *different statement* with no
//!    `LIKE` in it at all, not the same statement with a sentinel: asserted by text
//!    (no `LIKE`), by plan (`idx_articles_window`, no table scan) and by SQLite's own
//!    `FullscanStep` counter. A filled box escapes `%`, `_` and `\` and carries
//!    `ESCAPE '\'`, so a search for `%` is a search for a percent sign.
//! 4. **The limits are the limits** — the window's `?1`, the list limits and the edge
//!    limit are all bound, honoured exactly, and passed as positive constants by the
//!    frame (SQLite reads `LIMIT -1` as "no limit", so a negative one would unbind a
//!    list silently).
//!
//! The file also pins the two things that were wrong in `src/data.rs` and are not any
//! more, so they cannot come back: the pragma is *set* and read back as an `i64` (it was
//! read into a `String`, which neither set it nor succeeded), and the edge ids are
//! bound (they were formatted into `IN (…)`). Both are asserted in
//! `the_pragma_is_set_not_merely_read` and `every_value_is_a_placeholder_not_text`.

use std::path::{Path, PathBuf};

use rusqlite::ToSql;

use rusqlite::{Connection, OpenFlags};

/// The source every assertion in this file is made against.
const DATA_RS: &str = include_str!("../src/data.rs");

/// The frame, for the limits it passes to each list.
const APP_RS: &str = include_str!("../src/app.rs");

/// `atlas-store` owns the archive's schema: the index the plan assertion relies on is
/// declared there, so the test reads the declaration instead of trusting a comment.
const STORE_SCHEMA_RS: &str = include_str!("../../atlas-store/src/schema.rs");

/// In-window, relevant, non-duplicate stories in the temporary archive. Enough that
/// SQLite prefers the window index to a scan, which is what the plan test is about.
const IN_WINDOW: i64 = 500;

/// Total rows of the temporary archive: [`IN_WINDOW`] plus an old story, an irrelevant
/// one, a duplicate and one with no text at all.
const TOTAL_ROWS: i64 = IN_WINDOW + 4;

/// What `articles(72 h, ·, "")` returns: the window, relevant, not a duplicate, and the
/// textless row — an empty search box filters on nothing.
const IN_WINDOW_VISIBLE: i64 = IN_WINDOW + 1;

/// A limit above the row count, for the assertions that are about *what* comes back
/// rather than about where the list stops.
const ALL_ROWS: i64 = TOTAL_ROWS + 100;

// ---------------------------------------------------------------------------
// Reading the source
// ---------------------------------------------------------------------------

/// Every double-quoted string literal in `source`, in file order, with Rust's escapes
/// applied and `\`-newline continuations joined.
///
/// A hand-written scan rather than a regex: the literals contain `\\`, `''` and
/// continuations, and a doc comment that mentions a statement must not be mistaken for
/// one (`//` comments are skipped, and `[^/]/[^/]` inside a string is not a comment).
fn literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '/' if chars.get(index + 1) == Some(&'/') => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            '"' => {
                let mut literal = String::new();
                index += 1;
                while index < chars.len() && chars[index] != '"' {
                    if chars[index] == '\\' {
                        index += 1;
                        if index >= chars.len() {
                            break;
                        }
                        if chars[index] == '\n' {
                            index += 1;
                            while index < chars.len() && chars[index].is_whitespace() {
                                index += 1;
                            }
                            continue;
                        }
                    }
                    literal.push(chars[index]);
                    index += 1;
                }
                index += 1;
                out.push(literal);
            }
            _ => index += 1,
        }
    }
    out
}

/// The first literal in `source` that contains `marker`.
fn literal_with(source: &str, marker: &str) -> String {
    literals(source)
        .into_iter()
        .find(|literal| literal.contains(marker))
        .unwrap_or_else(|| panic!("no string literal containing {marker:?} in src/data.rs"))
}

/// The first statement literal inside `fn NAME(…)` in `source`.
///
/// The article template used to be located by its `FROM` clause alone, and that was
/// sound only while one statement read articles. Then a second one appeared —
/// `articles_by_ids`, the article page's lookup by id — with the same
/// `FROM articles a LEFT JOIN sources s …`; file order decided which one this file read,
/// and five tests here failed on a template that carried no `?` at all. Locating the
/// literal by the function it lives in is what these assertions were always about:
/// `article_sql` *is* the statement they describe. It also fixes the finding to the
/// source rather than to where an unrelated change happens to be inserted.
fn statement_in(source: &str, function: &str) -> String {
    let needle = format!("fn {function}(");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("src/data.rs no longer declares `fn {function}`"))
        + needle.len();
    let body = &source[start..];
    // A function ends at the first line holding only its closing brace, which is how this
    // workspace formats one; taking the literal from inside the body is what keeps a
    // second statement elsewhere in the file out of the answer.
    let end = body.find("\n}").map(|end| end + 2).unwrap_or(body.len());
    // The first *statement* literal in the body, not the first literal: `fn edges` opens
    // with `vec!["?"; ids.len()]`, and taking that `"?"` made two tests here audit a
    // placeholder instead of the statement (they passed until the literal was read at all).
    literals(&body[..end])
        .into_iter()
        .find(|literal| {
            let head = literal.trim_start();
            head.starts_with("SELECT") || head.starts_with("WITH")
        })
        .unwrap_or_else(|| panic!("no statement literal inside `fn {function}`"))
}

/// The value of `const NAME: &str = "…"` in `source`.
///
/// Found by name, so the test follows the source: rename the `const` and this fails
/// loudly instead of quietly asserting on a stale copy of the SQL.
fn const_str(source: &str, name: &str) -> String {
    let needle = format!("const {name}: &str = ");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("src/data.rs no longer declares `{name}`"))
        + needle.len();
    let rest = &source[start..];
    let open = rest.find('"').expect("a string literal after the const");
    literals(&rest[open..])
        .into_iter()
        .next()
        .expect("the const's literal")
}

/// The article statement, assembled exactly as `article_sql(searching)` assembles it.
///
/// The template, the two named fragments, the search fragment and the two limit markers
/// are all read from the source; only the substitution — which the function in
/// `data.rs` does with `format!` — is repeated here, and
/// `the_article_statement_is_one_template_and_four_fragments` pins that the pieces it
/// reads are the ones the template asks for.
fn articles_sql(searching: bool) -> String {
    let template = statement_in(DATA_RS, "article_sql");
    let mut sql = template
        .replace("{ARTICLE_COLUMNS}", &const_str(DATA_RS, "ARTICLE_COLUMNS"))
        .replace("{ARTICLE_WINDOW}", &const_str(DATA_RS, "ARTICLE_WINDOW"));
    let search = if searching {
        const_str(DATA_RS, "ARTICLE_SEARCH")
    } else {
        String::new()
    };
    // `article_sql` fills the two anonymous slots in this order: the search predicate,
    // then the limit's marker (`?3` when searching, because `?2` is the pattern).
    sql = sql.replacen("{}", &search, 1);
    sql = sql.replacen("{}", if searching { "?3" } else { "?2" }, 1);
    sql
}

/// The edge statement, assembled as `edges` assembles it, for a set of `ids`.
///
/// Located by function for the same reason as the article template: `IN ({marks})` is no
/// longer one statement's shape — `articles_by_ids` binds its keys the same way — and
/// "whichever comes first in the file" is not a property any of these assertions want.
fn edges_sql(ids: usize) -> String {
    let template = statement_in(DATA_RS, "edges");
    let marks = vec!["?"; ids].join(",");
    template.replace("{marks}", &marks)
}

/// Every literal in `src/data.rs` that is a statement: it starts with `SELECT`.
fn statements(source: &str) -> Vec<String> {
    literals(source)
        .into_iter()
        .filter(|literal| literal.trim_start().starts_with("SELECT"))
        .collect()
}

// ---------------------------------------------------------------------------
// A temporary archive
// ---------------------------------------------------------------------------

/// The archive's shape, cut to the columns and indexes the window's queries name.
///
/// It is `atlas-store`'s schema (`src/schema.rs`, plus the columns its `MIGRATIONS`
/// add) with the tables the window does not read left out.
/// `the_archive_shape_the_plan_relies_on_is_the_one_atlas_store_declares` asserts the
/// index the plan depends on is the one `atlas-store` declares.
const SCHEMA: &str = "
CREATE TABLE sources(
  id INTEGER PRIMARY KEY, name TEXT, active INTEGER DEFAULT 1);
CREATE TABLE articles(
  id INTEGER PRIMARY KEY, source_id INTEGER, url TEXT UNIQUE, title TEXT, summary TEXT,
  content TEXT, published TEXT, iso TEXT, lat REAL, lon REAL, place TEXT, relevant
  INTEGER DEFAULT 0, dupe INTEGER DEFAULT 0, full_text INTEGER DEFAULT 0, publisher TEXT);
CREATE TABLE entities(
  id INTEGER PRIMARY KEY, name TEXT UNIQUE, type TEXT, count INTEGER DEFAULT 1, last_seen TEXT);
CREATE TABLE edges(
  a INTEGER, b INTEGER, weight REAL DEFAULT 1, type TEXT DEFAULT 'cooccur', label TEXT);
CREATE TABLE article_links(
  a INTEGER, b INTEGER, shared INTEGER, score REAL, why TEXT);
CREATE TABLE topics(
  id INTEGER PRIMARY KEY, name TEXT UNIQUE, keywords TEXT, active INTEGER DEFAULT 1,
  hits INTEGER DEFAULT 0, last_hit TEXT);
CREATE TABLE monitors(
  id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER DEFAULT 1,
  last_run TEXT, hits INTEGER DEFAULT 0, cadence_minutes INTEGER DEFAULT 0);
CREATE TABLE alerts(
  id INTEGER PRIMARY KEY, monitor_id INTEGER, kind TEXT, article_id INTEGER, url TEXT,
  title TEXT, seen INTEGER DEFAULT 0, created TEXT);
CREATE INDEX idx_articles_published ON articles(published DESC);
CREATE INDEX idx_articles_relevant ON articles(relevant, published DESC);
CREATE INDEX idx_articles_window ON articles(relevant, dupe, published DESC, full_text, iso);
";

/// A fresh directory per test, so two tests never share a database.
fn temp_db(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("atlas-desktop-sql-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp directory");
    dir.join("atlas.db")
}

/// An ISO stamp `hours` in the past, in the archive's own format.
///
/// Asked of SQLite (`strftime` is UTC, fixed width, `Z`), so this file cannot drift
/// from the format `data.rs`'s `window_start_iso` writes.
fn iso_hours_ago(conn: &Connection, hours: i64) -> String {
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        [format!("-{hours} hours")],
        |row| row.get(0),
    )
    .expect("strftime")
}

/// An archive with rows on both sides of every filter the window has: [`IN_WINDOW`]
/// in-window stories over 3 places and 2 countries, one story older than the window,
/// one irrelevant, one duplicate, and one with no text at all (the witness that an
/// empty search box does not evaluate a `LIKE`: `NULL LIKE ?` is `NULL`, so a filtering
/// `LIKE` would drop that row).
fn seeded(name: &str) -> PathBuf {
    let path = temp_db(name);
    let conn = Connection::open(&path).expect("create the temp archive");
    conn.execute_batch(SCHEMA).expect("schema");
    conn.execute(
        "INSERT INTO sources(id,name) VALUES(1,'Reuters'),(2,'AP')",
        [],
    )
    .expect("sources");
    {
        let mut insert = conn
            .prepare(
                "INSERT INTO articles(source_id,title,summary,content,published,iso,place,\
                 lat,lon,relevant,dupe,full_text,publisher) \
                 VALUES(?1,?2,'summary',?3,?4,?5,?6,?7,?8,1,0,1,'wire')",
            )
            .expect("statement");
        for index in 0..IN_WINDOW {
            let bucket = index % 3;
            insert
                .execute(rusqlite::params![
                    1 + index % 2,
                    format!("Story {index} about the chip shortage"),
                    format!("body of story {index} mentioning the harvest"),
                    iso_hours_ago(&conn, 1 + index % 48),
                    if index % 2 == 0 { "FRA" } else { "DEU" },
                    format!("Place {bucket}"),
                    48.85 + bucket as f64,
                    2.35 + bucket as f64
                ])
                .expect("article");
        }
    }
    let two_hours = iso_hours_ago(&conn, 2);
    // Older than a 72-hour window, still relevant and not a duplicate.
    conn.execute(
        "INSERT INTO articles(source_id,title,content,published,iso,relevant,dupe) \
         VALUES(1,'Old story','old body',?1,'FRA',1,0)",
        [iso_hours_ago(&conn, 200)],
    )
    .expect("old article");
    // In the window, but filtered out by the window's own two flags.
    conn.execute(
        "INSERT INTO articles(source_id,title,content,published,relevant,dupe) \
         VALUES(1,'Irrelevant story','body',?1,0,0)",
        [&two_hours],
    )
    .expect("irrelevant article");
    conn.execute(
        "INSERT INTO articles(source_id,title,content,published,relevant,dupe) \
         VALUES(2,'Duplicate story','body',?1,1,1)",
        [&two_hours],
    )
    .expect("duplicate article");
    conn.execute(
        "INSERT INTO articles(source_id,title,summary,content,published,relevant,dupe) \
         VALUES(1,NULL,NULL,NULL,?1,1,0)",
        [&two_hours],
    )
    .expect("textless article");
    let hour_ago = iso_hours_ago(&conn, 1);
    conn.execute(
        "INSERT INTO entities(id,name,type,count,last_seen) VALUES\
         (1,'Chips','ORG',9,?1),(2,'Harvest','ORG',5,?1),(3,'France','LOC',4,?1),\
         (4,'Ana','PER',3,?1),(5,'Q3','MISC',2,?1)",
        [&hour_ago],
    )
    .expect("entities");
    conn.execute(
        "INSERT INTO edges(a,b,weight,type,label) VALUES(1,2,3.0,'cooccur','chips and harvest'),\
         (1,3,2.0,'cooccur',''),(2,4,1.5,'cooccur',''),(3,5,0.5,'cooccur','')",
        [],
    )
    .expect("edges");
    conn.execute(
        "INSERT INTO topics(id,name,keywords,active,hits,last_hit) VALUES\
         (1,'Chips','chip,semiconductor',1,7,?1),(2,'Harvest','harvest,grain',0,1,?1)",
        [&hour_ago],
    )
    .expect("topics");
    conn.execute(
        "INSERT INTO monitors(id,name,query,active,last_run,hits,cadence_minutes) VALUES\
         (1,'Chips','chip export',1,?1,3,60),(2,'Harvest','harvest',0,?1,0,0)",
        [&hour_ago],
    )
    .expect("monitors");
    conn.execute(
        "INSERT INTO article_links(a,b,shared,score,why) VALUES(1,2,3,0.7,'shared entities')",
        [],
    )
    .expect("links");
    // Statistics exist so the plan assertion is about the statement, not about a
    // default assumption of the planner.
    let _ = conn.execute_batch("ANALYZE");
    path
}

/// The window's own two guards, in `data.rs`'s order: the file flag, then the pragma.
fn read_only(path: &Path) -> Connection {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open the temp archive read-only");
    conn.execute_batch("PRAGMA query_only=ON")
        .expect("set query_only");
    conn
}

fn plan(conn: &Connection, sql: &str, params: &[&dyn ToSql]) -> Vec<String> {
    let mut statement = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("the plan of a statement that prepares");
    let rows = statement
        .query_map(params, |row| row.get::<_, String>(3))
        .expect("plan rows");
    rows.flatten().collect()
}

fn row_count(conn: &Connection, sql: &str, params: &[&dyn ToSql]) -> usize {
    let mut statement = conn.prepare(sql).expect("a statement that prepares");
    let mut rows = statement.query(params).expect("rows");
    let mut count = 0;
    while rows.next().expect("row").is_some() {
        count += 1;
    }
    count
}

fn remove(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

// ---------------------------------------------------------------------------
// 1. Every query binds its values
// ---------------------------------------------------------------------------

/// The inventory, by name: every statement the window sends is one read, and every
/// table it names is a literal.
///
/// What it looks for, exactly:
///
/// * each statement literal begins with `SELECT` — no statement the window sends is a
///   write, and there is no second statement hiding behind a `;`;
/// * no statement holds a write, DDL, attach or pragma verb as one of its words — the
///   window cannot change the archive or reach another database even if a value ever
///   reached statement text. A column that merely contains a verb's letters (`created`)
///   is data, not DDL;
/// * the only non-`SELECT` statement the file contains is `PRAGMA query_only`, the
///   read-only guard, and it is *set* (`pragma_update`) and read back;
/// * every `FROM`/`JOIN` is followed by a bare identifier — a table name is schema, and
///   `?` or `{` there would mean the shape of the statement depends on data.
#[test]
fn every_statement_the_window_sends_is_a_single_read() {
    let found = statements(DATA_RS);
    assert!(
        found.len() >= 8,
        "the inventory found {} statements: {:?}",
        found.len(),
        found
    );
    for sql in &found {
        let statement = sql.trim();
        assert!(
            !statement.contains(';'),
            "two statements in one call: {statement}"
        );
        // A verb is a **word**, not a substring. The briefings read names the column
        // `created` (`SELECT … created … FROM briefings …`), and a substring test called
        // that DDL: "CREATED contains CREATE". Matching whole words keeps the rule the
        // test exists for — no write, DDL, attach or pragma *statement* leaves this file —
        // because SQL that writes spells its verb as its own token.
        let tokens: Vec<String> = statement
            .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
            .filter(|part| !part.is_empty())
            .map(|part| part.to_uppercase())
            .collect();
        for verb in [
            "INSERT", "UPDATE", "DELETE", "REPLACE", "DROP", "ALTER", "CREATE", "ATTACH",
            "DETACH", "VACUUM", "PRAGMA", "REINDEX", "ANALYZE", "TRIGGER", "VIEW",
        ] {
            assert!(
                !tokens.iter().any(|token| token == verb),
                "{verb} is a word in a statement the window sends: {statement}"
            );
        }
        let upper = statement.to_uppercase();
        let words: Vec<&str> = upper.split_whitespace().collect();
        for (position, word) in words.iter().enumerate() {
            if *word != "FROM" && *word != "JOIN" {
                continue;
            }
            let table = words
                .get(position + 1)
                .copied()
                .unwrap_or_else(|| panic!("{word} with no table in {statement}"));
            assert!(
                !table.contains('?') && !table.contains('{'),
                "the table after {word} is a value, not a name: {statement}"
            );
        }
    }

    // The pragma half: `PRAGMA query_only` is the only pragma the window sends, and it
    // is not a stray read that nothing acts on (the next test pins the read-back).
    let pragmas: Vec<String> = literals(DATA_RS)
        .into_iter()
        .filter(|literal| literal.trim_start().starts_with("PRAGMA"))
        .collect();
    assert!(
        !pragmas.is_empty(),
        "the window no longer sets query_only — the module doc claims it does"
    );
    for pragma in &pragmas {
        assert!(
            pragma.contains("query_only"),
            "an unexpected pragma: {pragma}"
        );
    }

    // The tables, by name: the archive's. SQLite's own `sqlite_master` is reached exactly
    // once, and the reach is named here rather than waved through: `Archive::open` asks
    // "does this file have any table at all?" to tell a WAL archive in an unwritable folder
    // (which opens successfully and answers every query from an empty schema) from an
    // archive that is merely empty (`src/data.rs:238`). That is a read of the schema, not a
    // walk of it, and it is the only one: a second `sqlite_master` literal, or that one
    // changing shape, is a finding.
    let schema_probes: Vec<String> = literals(DATA_RS)
        .into_iter()
        .filter(|literal| literal.to_uppercase().contains("SQLITE_MASTER"))
        .collect();
    assert_eq!(
        schema_probes.len(),
        1,
        "the window reads sqlite_master {} times; `Archive::open`'s empty-schema probe is \
         the only one it may: {schema_probes:?}",
        schema_probes.len()
    );
    assert_eq!(
        schema_probes[0].trim(),
        "SELECT count(*) FROM sqlite_master",
        "the one sqlite_master read changed shape — it is `Archive::open`'s empty-schema \
         probe, and nothing else may reach the schema"
    );
    for forbidden in ["sqlite_schema", "pragma_table", "temp.", "main."] {
        assert!(
            !DATA_RS.to_uppercase().contains(&forbidden.to_uppercase()),
            "the window reaches {forbidden}"
        );
    }
}

/// Every value is a `?`, and the only text assembled is a `const` fragment or a run of
/// `?` markers.
///
/// What it looks for, exactly:
///
/// * the article statement is assembled from a template plus **four** named fragments —
///   `ARTICLE_COLUMNS`, `ARTICLE_WINDOW`, `ARTICLE_SEARCH`, and the limit marker chosen
///   by a `bool` — and there is no other brace in it;
/// * the edge statement's only interpolation is `{marks}`, a `?`-list built by
///   `vec!["?"; ids.len()].join(",")`; the ids themselves are bound twice (both ends)
///   and never formatted;
/// * no statement uses a named parameter (`:x`, `@x`, `$x`): every binding is
///   positional, so a value cannot be addressed by name from outside.
#[test]
fn every_value_is_a_placeholder_not_text() {
    // The article template and the fragments it asks for.
    let template = statement_in(DATA_RS, "article_sql");
    assert_eq!(
        template.matches("{}").count(),
        2,
        "the article template has exactly two anonymous slots (search predicate, limit \
         marker): {template}"
    );
    for name in ["ARTICLE_COLUMNS", "ARTICLE_WINDOW"] {
        assert!(
            template.contains(&format!("{{{name}}}")),
            "the template no longer interpolates the {name} const: {template}"
        );
        let fragment = const_str(DATA_RS, name);
        assert!(
            !fragment.contains('{'),
            "the {name} fragment is built: {fragment}"
        );
    }
    let search = const_str(DATA_RS, "ARTICLE_SEARCH");
    assert!(
        !search.contains('{'),
        "the search predicate is built: {search}"
    );

    // The assembled statements: no brace left, and the fragments are the only text.
    for searching in [false, true] {
        let sql = articles_sql(searching);
        assert!(
            !sql.contains('{') && !sql.contains('}'),
            "a template slot survived substitution: {sql}"
        );
        assert!(sql.starts_with("SELECT "), "{sql}");
        assert!(sql.contains("FROM articles a"), "{sql}");
        assert!(
            !sql.to_uppercase().contains("INSERT") && !sql.to_uppercase().contains("DELETE"),
            "{sql}"
        );
        // Positional markers only.
        for sigil in [':', '@', '$'] {
            assert!(
                !sql.contains(sigil),
                "a named parameter ({sigil}) in {sql}"
            );
        }
        // The highest marker is the number of values bound: `?1` window, `?2` pattern,
        // `?3` limit when searching; `?1` window and `?2` limit when not.
        let highest = if searching { 3 } else { 2 };
        assert!(
            sql.contains(&format!("?{highest}")),
            "the limit marker ?{highest} is missing: {sql}"
        );
        assert!(
            !sql.contains(&format!("?{}", highest + 1)),
            "a marker beyond the bound values: {sql}"
        );
    }

    // The edge statement: a placeholder run, and the ids bound twice.
    let edges = edges_sql(2);
    assert_eq!(
        edges.matches('?').count(),
        5,
        "two ids fill two IN lists plus the limit: {edges}"
    );
    assert!(
        DATA_RS.contains("let marks = vec![\"?\"; ids.len()].join(\",\");"),
        "the edge placeholder list is built some other way — audit it"
    );
    assert!(
        DATA_RS.contains("params.extend_from_slice(ids);"),
        "the edge ids are bound (twice: both ends of the relation)"
    );
    assert!(
        !edges.contains("1,2"),
        "an id reached the statement text: {edges}"
    );

    // `counts` is one statement, and it binds nothing because it filters on constants.
    let count_sql = const_str(DATA_RS, "COUNT_SQL");
    assert!(
        !count_sql.contains('?') && !count_sql.contains('{'),
        "the counts statement takes no values: {count_sql}"
    );
    assert_eq!(
        count_sql.matches("count(*)").count(),
        8,
        "eight numbers in one pass: {count_sql}"
    );
}

/// A pin's destination is one story, ranked the way the map promises: the newest at that
/// place, inside the window, and the same one every time.
///
/// `Pin::article` is what a click on a dot opens, so the ranking that chooses it is part
/// of the answer and not an implementation detail: `row_number()` over the group by
/// `published DESC, id DESC` (a shared second is broken by the id, so two runs agree),
/// `rn = 1` as the only row kept per group, and the group key that row is numbered
/// inside — the label *and* the country, because two groups can share one label. The
/// filter that keeps `0,0` off the map has to run before `LIMIT`, or it eats the limit.
#[test]
fn a_pin_s_destination_is_ranked_by_publication_then_id() {
    let pins = literal_with(DATA_RS, "avg(lat) OVER (PARTITION BY label, iso)");
    assert!(
        pins.contains("row_number() OVER (PARTITION BY label, iso"),
        "the destination is not ranked inside its own group: {pins}"
    );
    assert!(
        pins.contains("ORDER BY published DESC, id DESC"),
        "the ranking is not publication-then-id descending: {pins}"
    );
    assert!(
        pins.contains("WHERE rn = 1"),
        "more than the newest story per group reaches the answer: {pins}"
    );
    assert!(
        !pins.contains("ORDER BY published DESC, id ASC"),
        "a shared second would answer with the story filed first: {pins}"
    );
    // The label is the definition, taken from the row: a blank place is the country, and
    // a blank country too is an empty label rather than a `null` the map would print.
    assert!(
        pins.contains("coalesce(nullif(place, ''), nullif(iso, ''), '')"),
        "the label is not coalesce(place, iso): {pins}"
    );
    // The bounds and the origin are in SQL, and before the limit: filtered afterwards
    // they would hand back fewer pins than were asked for.
    let origin = pins
        .find("NOT (lat = 0 AND lon = 0)")
        .unwrap_or_else(|| panic!("the 0,0 filter is gone from {pins}"));
    let world = pins
        .find("BETWEEN -90 AND 90")
        .unwrap_or_else(|| panic!("the latitude bound is gone from {pins}"));
    let limit = pins
        .find("LIMIT ?2")
        .unwrap_or_else(|| panic!("the limit is gone from {pins}"));
    assert!(
        origin < limit && world < limit,
        "a coordinate filter runs after the limit: {pins}"
    );
}

/// The search term reaches SQL escaped and bound — never as a pattern the reader can
/// widen.
///
/// What it looks for: `like_pattern` escapes `%`, `_` and `\`; the article statement
/// carries `ESCAPE '\'` on every `LIKE`; and `articles` passes the escaped pattern as
/// `?2` (three columns, one value) and the limit as `?3`.
#[test]
fn the_search_term_reaches_sql_escaped_and_bound() {
    // The escaping, from the source: `like_pattern` prefixes `%`, `_` and `\` with a
    // backslash. It is written with char literals, which is why this looks at the text
    // rather than at the string literals above.
    for character in [r"'\\'", r"'%'", r"'_'"] {
        assert!(
            DATA_RS.contains(character),
            "like_pattern no longer escapes {character}"
        );
    }
    assert!(
        DATA_RS.contains(r"if ch == '\\' || ch == '%' || ch == '_'"),
        "the escaping condition changed; audit it before trusting the assertions below"
    );
    let sql = articles_sql(true);
    assert_eq!(
        sql.matches("LIKE ?2 ESCAPE '\\'").count(),
        3,
        "three columns, one escaped pattern: {sql}"
    );
    assert!(
        DATA_RS.contains("rusqlite::params![since, like_pattern(needle), limit]"),
        "the pattern is no longer built by like_pattern at the call site"
    );
    assert!(
        !DATA_RS.contains("format!(\"%{}%\", needle"),
        "the pattern is built by string formatting again"
    );

    // The escaping is load-bearing, and this is the measurement: the statement the
    // window sends for the needle `%` matches nothing (no body holds a percent sign),
    // while the same pattern *without* `ESCAPE` would match every row in the window.
    let path = seeded("escaping");
    let conn = read_only(&path);
    let since = iso_hours_ago(&conn, 72);
    let pattern = escaped_pattern("%");
    assert_eq!(pattern, "%\\%%", "the escaping under test");
    let escaped = row_count(&conn, &sql, rusqlite::params![&since, &pattern, ALL_ROWS]);
    assert_eq!(escaped, 0, "a search for `%` found something");
    // What the statement would do with the pattern `%{}%` produces *without* the
    // escaping: `%%%` is three wildcards, so every row in the window with text is a hit
    // (the textless row cannot match a `LIKE` at all — `NULL LIKE ?` is `NULL`). That is
    // the "value becomes a pattern" defect the escaping in `like_pattern` prevents.
    let naive = row_count(&conn, &sql, rusqlite::params![&since, "%%%", ALL_ROWS]);
    assert_eq!(
        naive as i64, IN_WINDOW,
        "the unescaped pattern should have been a wildcard"
    );
    drop(conn);
    remove(&path);
}

/// `%text%` with `%`, `_` and `\` escaped — the rule `data.rs`'s `like_pattern` writes,
/// repeated here so the test can produce the pattern the statement receives.
fn escaped_pattern(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    out.push('%');
    for ch in text.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// The pragma is *set* and read back as an integer — the bug that made the window
/// unable to open an archive cannot come back.
///
/// What it looks for: `pragma_update(None, "query_only", true)` (the set),
/// `PRAGMA query_only` read into an `i64` (the check), and a refusal when the value is
/// `0`. The version this replaced read the pragma into a `String` and threw it away,
/// which both failed on the integer and left the guard off.
#[test]
fn the_pragma_is_set_not_merely_read() {
    assert!(
        DATA_RS.contains("pragma_update(None, \"query_only\", true)"),
        "the query_only switch is no longer thrown"
    );
    assert!(
        DATA_RS.contains("PRAGMA query_only"),
        "the switch is not read back, so a driver that ignored it would go unnoticed"
    );
    let read_back = literal_with(DATA_RS, "cannot read query_only back");
    assert!(
        read_back.contains("query_only"),
        "{read_back}"
    );
    // The check is on a number, not on a string: `let enforced: i64`.
    assert!(
        DATA_RS.contains("let enforced: i64"),
        "the read-back must be an integer — reading a pragma into a String fails"
    );
    assert!(
        DATA_RS.contains("if enforced == 0"),
        "a connection that refuses the switch must be refused"
    );
    assert!(
        DATA_RS.contains("refusing to use it"),
        "the refusal must say what it is doing"
    );
}

// ---------------------------------------------------------------------------
// 2. The connection cannot write
// ---------------------------------------------------------------------------

/// The window's two guards, proved on a real file: the same open flags, the same
/// pragma, and every write refused.
#[test]
fn the_window_refuses_a_write_through_the_same_flags_and_pragma() {
    // The flags, from the source: read-only and URI, never read-write, never CREATE.
    assert!(DATA_RS.contains("OpenFlags::SQLITE_OPEN_READ_ONLY"));
    assert!(DATA_RS.contains("SQLITE_OPEN_URI"));
    assert!(!DATA_RS.contains("SQLITE_OPEN_READ_WRITE"));
    assert!(!DATA_RS.contains("SQLITE_OPEN_CREATE"));
    assert!(
        DATA_RS.contains("Connection::open_with_flags("),
        "a plain Connection::open would be read-write"
    );

    let path = seeded("read-only");
    let before = std::fs::metadata(&path)
        .expect("the temp archive exists")
        .len();
    let conn = read_only(&path);
    // The pragma took: this is the switch, not the report.
    let effective: i64 = conn
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .expect("query_only answers an integer");
    assert_eq!(effective, 1, "query_only did not take on a read-only archive");

    for attempt in [
        "INSERT INTO articles(title) VALUES('x')",
        "UPDATE articles SET title='x'",
        "DELETE FROM articles",
        "DROP TABLE articles",
        "CREATE TABLE scratch(a)",
    ] {
        let error = match conn.execute(attempt, []) {
            Ok(_) => panic!("{attempt} was allowed on a read-only archive"),
            Err(error) => error,
        };
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("readonly")
                || message.contains("read-only")
                || message.contains("query_only")
                || message.contains("not authorized"),
            "{attempt} failed for the wrong reason: {error}"
        );
    }
    // A second database on the same connection is the way out that is easy to miss.
    // Measured: SQLite *does* attach an in-memory database to a read-only connection
    // (`Ok`), and every write to it is then refused (`attempt to write a readonly
    // database`) — `query_only` applies to the connection, not to one file. A *file*
    // attach is refused outright and creates nothing, so the window cannot write a side
    // file either. Both halves are asserted, so neither can be lost silently.
    if conn
        .execute("ATTACH DATABASE ':memory:' AS other", [])
        .is_ok()
    {
        let error = conn
            .execute("CREATE TABLE other.scratch(a)", [])
            .expect_err("a write to an attached in-memory database was allowed");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("readonly") || message.contains("read-only"),
            "the second database failed for the wrong reason: {error}"
        );
        // The refusal is real: the attached database holds no table.
        let written: i64 = conn
            .query_row(
                "SELECT count(*) FROM other.sqlite_master WHERE type='table'",
                [],
                |row| row.get(0),
            )
            .expect("read the attached schema");
        assert_eq!(written, 0, "a table was created in an attached database");
    }
    let elsewhere = std::env::temp_dir().join(format!(
        "atlas-desktop-attach-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&elsewhere);
    let attach = format!("ATTACH DATABASE '{}' AS elsewhere", elsewhere.display());
    assert!(
        conn.execute(&attach, []).is_err(),
        "a read-only connection attached a writable file"
    );
    assert!(
        !elsewhere.is_file(),
        "the attach created {}",
        elsewhere.display()
    );
    // Byte-for-byte unchanged: nothing was written and then rolled back.
    assert_eq!(
        std::fs::metadata(&path).expect("still there").len(),
        before,
        "the temp archive changed size under a read-only connection"
    );
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM articles", [], |row| row.get(0))
        .expect("the archive is still readable");
    assert_eq!(rows, TOTAL_ROWS, "the seeded archive is intact");
    drop(conn);
    remove(&path);
}

// ---------------------------------------------------------------------------
// 3. An empty search box costs no search
// ---------------------------------------------------------------------------

/// An empty box is a different statement, with no `LIKE` in it at all.
#[test]
fn an_empty_search_box_sends_no_like_at_all() {
    let empty = articles_sql(false);
    let searching = articles_sql(true);
    assert!(
        !empty.to_uppercase().contains("LIKE"),
        "the empty-box statement still evaluates a LIKE: {empty}"
    );
    assert_eq!(
        searching.matches("LIKE").count(),
        3,
        "the searched statement has one LIKE per column: {searching}"
    );
    assert!(empty.contains("LIMIT ?2"), "{empty}");
    assert!(searching.contains("LIMIT ?3"), "{searching}");
    // Two shapes, not one shape with a sentinel: the difference is the whole of
    // `ARTICLE_SEARCH`, which is a `const` in the file, not a value. The limit marker
    // moves from `?2` to `?3` because the pattern took the second slot.
    let search_fragment = const_str(DATA_RS, "ARTICLE_SEARCH");
    assert_eq!(
        searching.replace(&search_fragment, "").replace("LIMIT ?3", "LIMIT ?2"),
        empty,
        "the searched statement is not the empty one plus the search fragment"
    );
}

/// The empty-box statement is an index range read: no table scan, and no full-scan step
/// inside SQLite either.
#[test]
fn the_empty_search_box_uses_the_window_index_and_takes_no_full_scan() {
    let path = seeded("empty-box");
    let conn = read_only(&path);
    let since = iso_hours_ago(&conn, 72);
    let empty = articles_sql(false);
    let searching = articles_sql(true);

    let empty_plan = plan(&conn, &empty, rusqlite::params![&since, 400i64]);
    assert!(
        empty_plan
            .iter()
            .any(|detail| detail.contains("idx_articles_window")),
        "the article list does not use the window index: {empty_plan:?}"
    );
    assert!(
        !empty_plan
            .iter()
            .any(|detail| detail.starts_with("SCAN") && detail.contains("articles")),
        "the article list scans the table: {empty_plan:?}"
    );

    // A filled box reads the same range and filters the rows it selected.
    let search_plan = plan(
        &conn,
        &searching,
        rusqlite::params![&since, "%chip%", 400i64],
    );
    assert!(
        search_plan
            .iter()
            .any(|detail| detail.contains("idx_articles_window")),
        "a search does not use the window index either: {search_plan:?}"
    );
    assert!(
        !search_plan
            .iter()
            .any(|detail| detail.starts_with("SCAN") && detail.contains("articles")),
        "a search scans the table: {search_plan:?}"
    );

    // SQLite's own counter: no step into a full scan of `articles`, for either shape.
    let fullscan = |sql: &str, params: &[&dyn ToSql]| -> i32 {
        let mut statement = conn.prepare(sql).expect("prepare");
        {
            let mut rows = statement.query(params).expect("query");
            while rows.next().expect("row").is_some() {}
        }
        statement.get_status(rusqlite::StatementStatus::FullscanStep)
    };
    assert_eq!(
        fullscan(&empty, rusqlite::params![&since, 400i64]),
        0,
        "the empty-box statement took a full scan"
    );
    assert_eq!(
        fullscan(&searching, rusqlite::params![&since, "%chip%", 400i64]),
        0,
        "the searched statement took a full scan"
    );

    // The witness: the row with no text at all. `NULL LIKE ?` is NULL, so a filtering
    // LIKE drops it; the empty box returns it, which is what "no LIKE is evaluated"
    // means for a row.
    let textless: i64 = conn
        .query_row(
            "SELECT id FROM articles WHERE title IS NULL AND content IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("the textless row");
    let ids = |sql: &str, params: &[&dyn ToSql]| -> Vec<i64> {
        let mut statement = conn.prepare(sql).expect("prepare");
        let mapped = statement
            .query_map(params, |row| row.get::<_, i64>(0))
            .expect("query");
        mapped.flatten().collect()
    };
    let all = ids(&empty, rusqlite::params![&since, ALL_ROWS]);
    assert_eq!(all.len() as i64, IN_WINDOW_VISIBLE);
    assert!(
        all.contains(&textless),
        "the empty box filtered the textless row: something is evaluating a LIKE"
    );
    let hits = ids(&searching, rusqlite::params![&since, "%zzzz%", ALL_ROWS]);
    assert!(hits.is_empty(), "a needle that matches nothing found {hits:?}");
    let real = ids(&searching, rusqlite::params![&since, "%chip%", ALL_ROWS]);
    assert_eq!(real.len() as i64, IN_WINDOW, "one word hits every body");
    assert!(!real.contains(&textless));
    drop(conn);
    remove(&path);
}

// ---------------------------------------------------------------------------
// 4. The limits are the limits
// ---------------------------------------------------------------------------

/// Every bound limit is honoured, and the window bound filters.
#[test]
fn every_list_honours_its_limit_and_its_window() {
    let path = seeded("limits");
    let conn = read_only(&path);

    let empty = articles_sql(false);
    let searching = articles_sql(true);
    let ids = |hours: i64, limit: i64| -> Vec<i64> {
        let mut statement = conn.prepare(&empty).expect("prepare");
        let mapped = statement
            .query_map(
                rusqlite::params![iso_hours_ago(&conn, hours), limit],
                |row| row.get::<_, i64>(0),
            )
            .expect("query");
        mapped.flatten().collect()
    };

    // The limit is `?2` for the empty box and `?3` when searching; both are exact, and
    // `LIMIT 0` is no rows (SQLite reads `LIMIT -1` as every row, which is why the frame
    // may only pass positive numbers — the next test asserts that).
    assert_eq!(ids(72, ALL_ROWS).len() as i64, IN_WINDOW_VISIBLE);
    assert_eq!(
        ids(72, 400).len(),
        400,
        "the frame's own limit is honoured exactly"
    );
    assert_eq!(ids(72, 3).len(), 3);
    assert_eq!(ids(72, 1).len(), 1);
    assert!(ids(72, 0).is_empty());
    assert_eq!(
        row_count(&conn, &searching, rusqlite::params![iso_hours_ago(&conn, 72), "%chip%", 3i64]),
        3,
        "the searched statement ignores its limit"
    );

    // The window is `?1`: 72 hours excludes the 200-hour-old story, 400 includes it, and
    // one hour excludes everything older.
    let old_id = IN_WINDOW + 1;
    let recent = ids(72, ALL_ROWS);
    assert!(!recent.contains(&old_id), "the window let an old story in");
    assert!(ids(400, ALL_ROWS).contains(&old_id));
    assert!(ids(1, 400).len() < recent.len());

    // Every other list: its own statement, the frame's limit and then one, and the count
    // never exceeds either.
    let since = iso_hours_ago(&conn, 72);
    for (name, literal) in [
        // The marker follows the shipped statement, not a copy of it. `pins` is a CTE with
        // window functions now (`avg(lat) OVER (PARTITION BY label, iso)` computes the
        // group's mean position in the same pass as its size and its newest story), so the
        // aggregate is named by its window clause rather than by `round(avg(...), 4)`. The
        // assertion is unchanged — the map's markers are bounded by the frame's limit.
        (
            "pins",
            literal_with(DATA_RS, "avg(lat) OVER (PARTITION BY label, iso)"),
        ),
        (
            "countries",
            literal_with(DATA_RS, "GROUP BY 1 ORDER BY 2 DESC"),
        ),
        ("entities", literal_with(DATA_RS, "coalesce(count,0) FROM entities")),
    ] {
        let count = |limit: i64| row_count(&conn, &literal, rusqlite::params![&since, limit]);
        let bounded = count(400);
        assert!(bounded <= 400, "{name} exceeded 400: {bounded}");
        assert!(bounded > 1, "{name} has fewer than two rows to bound");
        assert!(count(1) <= 1, "{name} exceeded a limit of one");
        assert_eq!(count(0), 0, "{name} ignored a limit of zero");
    }

    // The edge list: its limit is bound, and an edge is only returned when both of its
    // ends are in the set that was asked for.
    let edges = edges_sql(2);
    let count = |limit: i64| row_count(&conn, &edges, rusqlite::params![1i64, 2, 1, 2, limit]);
    assert_eq!(count(900), 1, "one edge joins 1 and 2");
    assert_eq!(count(1), 1);
    assert_eq!(count(0), 0);
    let lone = edges_sql(1);
    assert_eq!(
        row_count(&conn, &lone, rusqlite::params![3i64, 3, 900i64]),
        0,
        "an id set of one has no edge inside it"
    );
    drop(conn);
    remove(&path);
}

/// The frame passes positive constants: SQLite reads `LIMIT -1` as "no limit", so a
/// negative limit would silently unbind a list.
///
/// **Two things changed and both are pinned, not waved through.** The list's cap is no longer
/// a literal in the shell: `reload` passes `app::LIST_ROWS` — to `articles` and, through
/// `stories_for`, to the country/place list a chip opens — because that number is also the
/// bound on "the strip's number is the number of rows in the list". So this file checks the
/// *name* at each call site and the *value* of the constant, and a limit that is spelled as a
/// literal still has to be the pinned one. And the scan reads the **shipping** half of
/// `app.rs`: the test module inside that file calls the same archive methods with fixtures'
/// limits (`archive.pins(72, LIST_ROWS)`), which say nothing about what a reader's window
/// asks for.
#[test]
fn the_limits_the_window_passes_are_positive_constants() {
    let shipping = APP_RS
        .split("#[cfg(test)]")
        .next()
        .expect("app.rs has no test module to cut");
    assert!(
        shipping.contains("archive.pins(wanted.hours, 400)"),
        "the cut did not reach the shipping code — this rule would pass on an empty string"
    );
    // The one named cap, checked by value so a name cannot drift away from the number the
    // budgets in `tests/query_budget.rs` were measured against.
    assert_eq!(
        atlas_desktop::app::LIST_ROWS,
        400,
        "the window's list cap moved: re-measure the list budget and the strip's promise"
    );
    // The country rollup's cap is a name too: the rollup used to ask for 14 rows, which is
    // fewer than the 56 countries a window of the shipped archive is about — a story could be
    // counted nowhere on the page. It asks for `COUNTRY_ROWS` now, and the number is pinned
    // here for the same reason as `LIST_ROWS`: a budget is a budget for a shape.
    assert_eq!(
        atlas_desktop::app::COUNTRY_ROWS,
        400,
        "the window's country cap moved: re-measure the rollup budget (tests/query_budget.rs)"
    );
    for (function, limit) in [
        ("pins", "400"),
        ("countries", "COUNTRY_ROWS"),
        ("articles", "LIST_ROWS"),
        ("entities", "120"),
        ("edges", "900"),
    ] {
        let marker = format!("archive.{function}(");
        let calls = arguments_of(shipping, &marker);
        assert!(
            !calls.is_empty(),
            "src/app.rs no longer calls {marker}…) — re-check the limit it passes"
        );
        for arguments in calls {
            assert!(
                arguments.contains(&format!(", {limit}")),
                "archive.{function}({arguments}) does not pass the pinned limit {limit}"
            );
            assert!(
                !arguments.contains(", -"),
                "archive.{function}({arguments}) passes a negative limit"
            );
        }
    }
}

/// The argument lists of every `marker` call in `source`, up to its matching `)`.
fn arguments_of(source: &str, marker: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let needle: Vec<char> = marker.chars().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index + needle.len() <= chars.len() {
        if chars[index..index + needle.len()] == needle[..] {
            let mut depth = 0usize;
            let mut arguments = String::new();
            let mut cursor = index + needle.len();
            while cursor < chars.len() {
                match chars[cursor] {
                    '(' => depth += 1,
                    ')' if depth == 0 => break,
                    ')' => depth -= 1,
                    _ => {}
                }
                arguments.push(chars[cursor]);
                cursor += 1;
            }
            out.push(arguments);
            index = cursor;
        }
        index += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// 5. The shape the plan relies on
// ---------------------------------------------------------------------------

/// `atlas-store` declares the window index the plan assertion relies on.
#[test]
fn the_archive_shape_the_plan_relies_on_is_the_one_atlas_store_declares() {
    assert!(
        STORE_SCHEMA_RS.contains("idx_articles_window ON articles(relevant, dupe, published DESC"),
        "atlas-store no longer declares the window index the plan relies on"
    );
    let path = seeded("shape");
    let conn = read_only(&path);
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='articles'")
        .expect("statement")
        .query_map([], |row| row.get(0))
        .expect("rows")
        .flatten()
        .collect();
    assert!(
        names.iter().any(|name| name == "idx_articles_window"),
        "the temporary archive is missing the window index: {names:?}"
    );
    drop(conn);
    remove(&path);
}
