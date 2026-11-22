//! The search engine template is one `meta` row (`meta.search_engine`, key
//! `atlas_ingest::search::META_KEY`). Three questions, asked here because they are
//! storage questions before they are search questions:
//!
//! 1. **Can a hostile template escape SQL?** It cannot, and the reason is structural:
//!    the row is written by [`atlas_store::Store::set_meta`] and read by
//!    [`atlas_store::Store::get_meta`], both of which bind the key *and* the value
//!    (`INSERT INTO meta(k,v) VALUES(?,?)`, `SELECT v FROM meta WHERE k=?`), and the
//!    one place the value is *used* — the engine's URL template — is a URL, not a
//!    statement: `atlas_ingest::search::Engine::url` percent-encodes the query into
//!    the string and the gate judges the result. `atlas-ingest/src/search.rs` contains
//!    no `format!("SELECT…")` at all. So the corpus below is stored, read back
//!    byte-for-byte, and leaves `meta`, `articles` and `sqlite_master` exactly as they
//!    were — the test proves it instead of asserting it in prose.
//! 2. **Can the window write it?** No. `atlas-desktop/src/data.rs` opens the archive
//!    with `SQLITE_OPEN_READ_ONLY` *and* `PRAGMA query_only = ON` (and reads the
//!    switch back), and the file contains no `INSERT`/`UPDATE`/`DELETE`/`DROP`/
//!    `set_meta` at all; the window's one writable file is `data/requests.db`, whose
//!    statement is `INSERT INTO requests`. Both are asserted from the sources next
//!    door, because a test here cannot open a `rusqlite` connection with the window's
//!    flags (integration tests see this crate's API only) — a source assertion is the
//!    honest reach, and it fails the moment somebody adds a write to the read path.
//! 3. **Is the meta row bounded?** Yes, in Rust, and this file pins the boundary.
//!    `atlas-store/src/meta.rs` caps the key at [`atlas_store::meta::MAX_KEY_BYTES`]
//!    (256) and the value at [`atlas_store::meta::MAX_VALUE_BYTES`] (8 192), counted
//!    with `str::len()` before the redaction probe and refused with a sentence naming
//!    the cap. The `meta` DDL still has no `CHECK`, on purpose: SQLite stops reading a
//!    string at its first NUL, so a value of `"\0"` plus a megabyte measures 0 in SQL —
//!    the last test writes exactly that row through raw SQL to show why the count has to
//!    be Rust's. (The request queue's `MAX_PAYLOAD_BYTES` = 65 536 is the other cap on
//!    this path, for the window's route to a settings write.)
//!
//! Both earlier findings are now the store's contract, and the tests were rewritten to
//! the new one rather than deleted: a key or value over its cap is refused with a
//! sentence naming the number; a template that carries a credential is refused **and
//! nothing is written** (replacing it with `[redacted]` would keep the row and lose the
//! address, so `atlas_ingest::search` would quietly fall back to its built-in engine);
//! and a bare token that is not a location is still stored as `[redacted]`, because
//! nothing is lost by that and a secret never lands in the archive.
//!
//! No network, no ports: every database is a temporary file under
//! `std::env::temp_dir()`, created and removed by the test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use atlas_store::search::{match_query, match_terms};
use atlas_store::{now_iso, Requests, SqlValue, Store};

// ------------------------------------------------------------------------ fixtures

/// A temporary root, removed when the test ends (the guard is the path).
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Root {
    fn archive(&self) -> PathBuf {
        self.0.join("data").join("atlas.db")
    }

    fn queue(&self) -> PathBuf {
        self.0.join("data").join("requests.db")
    }
}

/// A directory no other test uses: the process id plus a counter, because two fixtures
/// alive at once must not share a database.
fn temp_root(label: &str) -> Root {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-store-engine-{}-{}-{}",
        label,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    Root(root)
}

/// An archive with the real schema, the way the app creates it.
fn archive(root: &Root) -> Store {
    let store = Store::open(root.archive()).expect("open");
    store.init().expect("schema");
    store
}

/// A sibling crate's source, for the assertions that have to be made where the decision
/// is. `path` is relative to this crate: `../atlas-desktop/src/data.rs`.
///
/// Read at run time and not with `include_str!`: these files belong to other agents, and
/// a moved file should fail one test with a sentence, not the whole build.
fn source(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|error| {
        panic!(
            "cannot read {} ({}): the assertion below is about that file, so a missing \
             file is a failure and not a skip",
            path,
            error
        )
    })
}

/// The source with its comments removed and everything else kept.
///
/// A rule about what a file *does* must read the code, not the prose around it. The
/// window's queue file carries a doc comment explaining that a *path* in a pasted URL is
/// a citation and not a credential — "measured over the 1439 URLs in `data/atlas.db`,
/// treating a path as a value position redacted 231 of them" (`atlas-desktop/src/
/// requests.rs`, on `carries_a_token`) — which is a sentence about redaction and says
/// nothing about which file the window opens. A scan for text cannot tell that sentence
/// from a statement, so it strips comments first. (Measured before this: the assertion
/// below failed on that sentence while the code opened `data/requests.db`.)
///
/// One walk over the file keeps every byte outside a comment — line (`//`) and block
/// (`/* */`, nested as Rust allows) — and **keeps string literals intact**, including the
/// `//` inside `"https://…"`, so nothing that is data is mistaken for a comment and
/// nothing after it is swallowed.
fn code_only(source: &str) -> String {
    enum State {
        Code,
        Line,
        Block(usize),
        Text,
        Raw(usize),
    }

    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut state = State::Code;
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        let next = chars.get(index + 1).copied();
        match state {
            State::Code => {
                if ch == '/' && next == Some('/') {
                    state = State::Line;
                    index += 2;
                    continue;
                }
                if ch == '/' && next == Some('*') {
                    state = State::Block(1);
                    index += 2;
                    continue;
                }
                if ch == '"' {
                    state = State::Text;
                    out.push(ch);
                    index += 1;
                    continue;
                }
                if ch == 'r' {
                    // `r"…"` or `r#"…"#`: a raw string is code, and a `//` inside it is a
                    // URL, not a comment.
                    let mut scan = index + 1;
                    let mut hashes = 0usize;
                    while chars.get(scan) == Some(&'#') {
                        hashes += 1;
                        scan += 1;
                    }
                    if chars.get(scan) == Some(&'"') {
                        for kept in index..=scan {
                            out.push(chars[kept]);
                        }
                        state = State::Raw(hashes);
                        index = scan + 1;
                        continue;
                    }
                }
                out.push(ch);
                index += 1;
            }
            State::Line => {
                if ch == '\n' {
                    out.push('\n');
                    state = State::Code;
                }
                index += 1;
            }
            State::Block(depth) => {
                if ch == '*' && next == Some('/') {
                    state = if depth > 1 {
                        State::Block(depth - 1)
                    } else {
                        State::Code
                    };
                    index += 2;
                    continue;
                }
                if ch == '/' && next == Some('*') {
                    state = State::Block(depth + 1);
                    index += 2;
                    continue;
                }
                index += 1;
            }
            State::Text => {
                out.push(ch);
                if ch == '\\' {
                    if let Some(escaped) = next {
                        out.push(escaped);
                        index += 2;
                        continue;
                    }
                }
                if ch == '"' {
                    state = State::Code;
                }
                index += 1;
            }
            State::Raw(hashes) => {
                if ch == '"' && (0..hashes).all(|offset| chars.get(index + 1 + offset) == Some(&'#')) {
                    out.push('"');
                    for _ in 0..hashes {
                        out.push('#');
                    }
                    index += 1 + hashes;
                    state = State::Code;
                    continue;
                }
                out.push(ch);
                index += 1;
            }
        }
    }
    out
}

/// Every value that has ever been an injection's first attempt, plus the four shapes a
/// *template* invites: printf, positional and numbered placeholders, and one with the
/// FTS5 operators spelled out.
const HOSTILE: [&str; 13] = [
    "'); DROP TABLE meta; --",
    "\" OR 1=1 --",
    "x'; DELETE FROM articles; --",
    "/* */ ; --",
    "1; SELECT load_extension('evil');",
    "' UNION SELECT k, v FROM meta;",
    "https://search.example.com/?q={query}&format=RSS'; ATTACH DATABASE 'x' AS y; --",
    "https://search.example.com/?q={query}&printf=%s%n",
    "https://search.example.com/?q={query}&positional=$1",
    "https://search.example.com/?q={query}&question=?",
    "\u{202e}https://search.example.com/?q={query}&format=RSS\u{202c}",
    "https://search.example.com/?q={query}&format=RSS\nDROP TABLE meta;",
    "'; PRAGMA writable_schema=ON; --",
];

// ------------------------------------------------------------------------ 1. SQL surface

/// The corpus goes through `set_meta`/`get_meta` — the only two statements that touch
/// `meta` — and comes back byte for byte, with the schema untouched.
///
/// What this pins: both statements bind (a `format!` would have made the first value
/// close the string and the `DROP` would have run), the value is *data* on the way in
/// and on the way out, and nothing about the archive's shape changes — 26 tables before
/// and after, the `meta` DDL character for character what the schema wrote, and
/// `PRAGMA integrity_check` still `ok`.
#[test]
fn a_hostile_template_is_stored_as_data_and_never_as_sql() {
    let root = temp_root("corpus");
    let store = archive(&root);
    let tables_before = tables(&store);
    let ddl_before = meta_ddl(&store);

    for (index, value) in HOSTILE.iter().enumerate() {
        let key = format!("search_engine:{index}");
        let redacted = store
            .set_meta(&key, value)
            .unwrap_or_else(|error| panic!("set_meta({value:?}): {}", error.message()));
        assert!(
            !redacted,
            "{value:?} is a template, not a credential — guard must not replace it"
        );
        assert_eq!(
            store.get_meta(&key).expect("get").as_deref(),
            Some(*value),
            "{value:?} did not come back as the data it is"
        );
    }

    // The tables the corpus tried to drop, delete from and rewrite are all still there,
    // with the same definitions.
    assert_eq!(tables(&store), tables_before, "a table was created or dropped");
    assert!(
        tables_before.contains(&"meta".to_string()),
        "meta must survive the corpus: {tables_before:?}"
    );
    assert_eq!(
        meta_ddl(&store),
        ddl_before,
        "the meta table's definition changed"
    );
    assert_eq!(
        store
            .query_one("SELECT count(*) c FROM meta", &[])
            .expect("count")
            .and_then(|row| row.int("c")),
        Some(HOSTILE.len() as i64),
        "every value is one row, and no statement added another"
    );
    let integrity = store
        .query("PRAGMA integrity_check", &[])
        .expect("integrity")
        .first()
        .and_then(|row| row.get_at(0))
        .and_then(|value| match value {
            SqlValue::Text(text) => Some(text.clone()),
            _ => None,
        });
    assert_eq!(integrity.as_deref(), Some("ok"), "the file is still sound");
}

/// The **key** is bound too. A key goes to the same two statements, and a key is what a
/// caller is most likely to build from a request payload (`search_engine` today, a
/// per-monitor key tomorrow).
#[test]
fn a_hostile_key_under_the_cap_is_data_and_not_sql_either() {
    let root = temp_root("keys");
    let store = archive(&root);
    let tables_before = tables(&store);

    let keys: [&str; 4] = [
        "'); DROP TABLE meta; --",
        "search_engine'; DELETE FROM meta; --",
        "key with a \u{0} nul",
        "\u{202e}dangerous",
    ];
    for (index, key) in keys.iter().enumerate() {
        let value = format!("template-{index}");
        store
            .set_meta(key, &value)
            .unwrap_or_else(|error| panic!("set_meta({key:?}): {}", error.message()));
        assert_eq!(
            store.get_meta(key).expect("get").as_deref(),
            Some(value.as_str()),
            "{key:?} did not round-trip"
        );
    }
    // The longest key the contract allows still round-trips, so the cap is a boundary and
    // not a shrinking budget.
    let longest = "k".repeat(atlas_store::meta::MAX_KEY_BYTES);
    store.set_meta(&longest, "edge").expect("a key at the cap");
    assert_eq!(
        store.get_meta(&longest).expect("get").as_deref(),
        Some("edge")
    );

    assert_eq!(tables(&store), tables_before, "the schema is untouched");
    assert_eq!(
        store
            .query_one("SELECT count(*) c FROM meta", &[])
            .expect("count")
            .and_then(|row| row.int("c")),
        Some(keys.len() as i64 + 1)
    );
}

/// The key cap, in Rust (`atlas-store/src/meta.rs:51`) and refused before anything is
/// written: a key over [`atlas_store::meta::MAX_KEY_BYTES`] is an error whose sentence
/// names the cap and the length it was handed — never a silent truncation, and never a
/// row.
///
/// Measured: 257 bytes and 4 096 bytes both refused with *"a meta key is capped at 256
/// bytes; this one is N."*, and `get_meta` afterwards finds nothing.
#[test]
fn a_key_over_the_cap_is_refused_with_a_sentence_naming_it() {
    let root = temp_root("key-cap");
    let store = archive(&root);
    let over = [
        "k".repeat(atlas_store::meta::MAX_KEY_BYTES + 1),
        "k".repeat(4096),
        format!("{}'); DROP TABLE meta; --", "k".repeat(400)),
    ];
    for key in &over {
        let refused = store.set_meta(key, "value");
        let message = match refused {
            Ok(replaced) => panic!("a {} byte key was stored (replaced={replaced})", key.len()),
            Err(error) => error.message().to_string(),
        };
        assert!(
            message.contains(&atlas_store::meta::MAX_KEY_BYTES.to_string()),
            "the sentence names the cap: {message}"
        );
        assert!(
            message.contains(&key.len().to_string()),
            "and the length it was handed ({}): {message}",
            key.len()
        );
        assert_eq!(
            store.get_meta(key).expect("get"),
            None,
            "a refused key leaves no row"
        );
    }
    assert_eq!(
        store
            .query_one("SELECT count(*) c FROM meta", &[])
            .expect("count")
            .and_then(|row| row.int("c")),
        Some(0),
        "nothing at all was written"
    );
    assert!(
        tables(&store).contains(&"meta".to_string()),
        "the schema is untouched by the hostile tail"
    );
}

/// The value arrives whole: a NUL byte, a right-to-left override, and the placeholder
/// characters that *look* like parameters in other languages are all just text here.
///
/// A NUL is the interesting one — SQLite's own `length()` counts it as a character and
/// rusqlite hands the string back with the byte intact (measured: 3 characters for
/// `"a\0b"`), so a template cannot be silently cut at its first NUL.
#[test]
fn the_value_arrives_whole_including_a_nul_and_a_bidi_mark() {
    let root = temp_root("bytes");
    let store = archive(&root);

    let values: [(&str, String); 3] = [
        ("nul", format!("a{0}b", '\u{0}')),
        (
            "bidi",
            "https://search.example.com/?q={query}\u{202e}gnp.exe\u{202c}".to_string(),
        ),
        (
            "placeholders",
            "https://search.example.com/?q=%s&p=$1&n=?&q={query}".to_string(),
        ),
    ];
    for (key, value) in &values {
        store.set_meta(key, value).expect("set");
        let back = store.get_meta(key).expect("get").expect("a value");
        assert_eq!(&back, value, "{key} did not round-trip");
        assert_eq!(
            back.chars().count(),
            value.chars().count(),
            "{key} changed length"
        );
    }
    // The NUL is kept by the reader — and SQLite's own `length()` stops at it, which is
    // why the cap below must be counted in Rust and not in SQL. Measured: `length(v)` = 1
    // for a three-character value, while `get_meta` hands all three characters back.
    assert_eq!(
        store
            .query_one("SELECT length(v) n FROM meta WHERE k='nul'", &[])
            .expect("length")
            .and_then(|row| row.int("n")),
        Some(1),
        "SQLite's text functions stop at the NUL; the stored value does not"
    );
}

/// Why the two statements above matter, measured on the same store: take one key that is
/// **not** stored and ask for it twice — once the way this crate asks (bound parameter)
/// and once the way a `format!` would.
///
/// * bound, `get_meta("' OR '1'='1")` finds nothing, because the key *is* that string;
/// * interpolated into `SELECT v FROM meta WHERE k='{key}'`, the same key closes the
///   quote and the statement returns a row the caller never asked for. Measured: one row,
///   which is why a template is never interpolated.
///
/// And the second classic — a key carrying `; DROP TABLE meta; --` — is refused before it
/// runs (SQLite's prepare takes one statement), with the table intact afterwards. This is
/// the test that would fail first if somebody "simplified" `set_meta`/`get_meta` into a
/// `format!`: the value the interpolation returns is the value the bound path does not.
#[test]
fn a_bound_getter_and_an_interpolated_one_disagree_about_the_same_key() {
    let root = temp_root("interpolated");
    let store = archive(&root);
    store.set_meta("search_engine", "https://a.test/?q={query}").expect("set");

    // The classic: not a stored key, but it closes the quote nicely.
    let key = "' OR '1'='1";
    assert_eq!(
        store.get_meta(key).expect("bound getter"),
        None,
        "bound, this key is a key and nothing is stored under it"
    );
    let interpolated = format!("SELECT v FROM meta WHERE k='{key}'");
    let rows = store
        .query(&interpolated, &[])
        .expect("the interpolated statement is valid SQL — that is the point");
    assert!(
        !rows.is_empty(),
        "the interpolation returned {} rows that the bound getter did not",
        rows.len()
    );

    // The other classic: a statement, then a second statement.
    let key = "'); DROP TABLE meta; --";
    let interpolated = format!("SELECT v FROM meta WHERE k='{key}'");
    let refused = store.query(&interpolated, &[]);
    assert!(
        refused.is_err(),
        "an interpolated key with a second statement must not run"
    );
    assert!(
        tables(&store).contains(&"meta".to_string()),
        "meta survived the attempt"
    );
    assert_eq!(
        store.get_meta("search_engine").expect("get").as_deref(),
        Some("https://a.test/?q={query}"),
        "and the stored template is exactly what was written"
    );
}

/// The template that carries a credential is **refused**, and nothing is written.
///
/// This replaces the earlier contract (the value was replaced with `[redacted]`, which
/// kept the row and lost the address: `[redacted]` has no `{query}`, so
/// `atlas_ingest::search::Engine::new` refused it, `resolve` fell through to
/// `DEFAULT_TEMPLATE`, and every search quietly ran on somebody else's endpoint while
/// the settings screen said the reader's engine was saved). `atlas-store/src/meta.rs`
/// now separates a *secret* from a *location*: an http(s) value that the guard reads as
/// credential material is an error naming the fix, and the row is left exactly as it
/// was.
///
/// Measured here: all three templates are refused, the sentence says "location with a
/// credential" and "nothing is stored", the template stored before the attempt is still
/// there byte for byte, a key that was never written is still `None`, and no secret
/// appears in any `meta` row. The redaction itself is unchanged where it is right — a
/// bare token that is not a location is still replaced by `[redacted]` — and a template
/// with no credential in it is still untouched, so the refusal is about the material
/// and not about the key being `search_engine`.
#[test]
fn a_template_that_carries_a_key_is_refused_and_nothing_is_written() {
    let root = temp_root("credential");
    let store = archive(&root);

    // Something real is already stored, so "unchanged" has a value to compare against.
    let plain = "https://search.example.com/?q={query}&format=RSS";
    assert!(!store.set_meta("search_engine", plain).expect("set"));
    assert!(
        !store.set_meta("monitor_engine", plain).expect("set"),
        "a second key, never written again, stands for the untouched case"
    );

    let carrying = [
        "https://search.example.com/?q={query}&api_key=sk-live-abcdefghijklmnop",
        "https://search.example.com/?q={query}&token=abc",
        "https://search.example.com/?q={query}&password=hunter22",
    ];
    for value in carrying {
        let refused = store.set_meta("search_engine", value);
        let message = match refused {
            Ok(replaced) => panic!(
                "{value:?} carries a credential and was stored (replaced={replaced}) \
                 instead of refused"
            ),
            Err(error) => error.message().to_string(),
        };
        assert!(
            message.contains("location with a credential"),
            "the sentence says what the value is: {message}"
        );
        assert!(
            message.contains("nothing is stored"),
            "and that nothing was written: {message}"
        );
        assert!(
            message.contains("search_engine"),
            "and which key it refused: {message}"
        );
        // Unchanged, not `[redacted]`: the marker would be the loss this refusal exists
        // to prevent.
        assert_eq!(
            store.get_meta("search_engine").expect("get").as_deref(),
            Some(plain),
            "the stored template is exactly what it was before the refusal"
        );
        assert_eq!(
            store.get_meta("monitor_engine").expect("get").as_deref(),
            Some(plain),
            "and no other key was touched either"
        );
    }

    // A key that never had a value still has none: a refused write is not a redacted one.
    let refused = store.set_meta("never_written", "https://x.test/?q={query}&token=abc");
    assert!(refused.is_err(), "still a location carrying a credential");
    assert_eq!(
        store.get_meta("never_written").expect("get"),
        None,
        "no row, and no `[redacted]` row either"
    );

    // The secret is nowhere in the file's meta rows.
    let dump = store
        .query("SELECT v FROM meta", &[])
        .expect("dump")
        .iter()
        .filter_map(|row| row.text("v").map(|text| text.to_string()))
        .collect::<Vec<String>>()
        .join("\n");
    assert!(
        !dump.contains("sk-live") && !dump.contains("hunter22"),
        "a credential reached the archive: {dump}"
    );

    // The redaction is unchanged where it is right: a bare token is not a location, so
    // nothing is lost by replacing it with the marker, and the caller is told (`true`).
    assert!(
        store
            .set_meta("openai_api_key", "sk-live-abcdefghijklmnop")
            .expect("set"),
        "a bare token is still redacted, not refused"
    );
    assert_eq!(
        store.get_meta("openai_api_key").expect("get").as_deref(),
        Some("[redacted]")
    );

    // And a template with no credential in it is stored as written.
    let plain_again = "https://search.example.com/?q={query}&format=Atom";
    assert!(!store.set_meta("search_engine", plain_again).expect("set"));
    assert_eq!(
        store.get_meta("search_engine").expect("get").as_deref(),
        Some(plain_again),
        "the plain template round-trips"
    );
}

// ------------------------------------------------------------------------ 2. the read path

/// The only thing the rest of the app does with this row is turn it into a search, and a
/// search is a *word list*, not syntax: `match_terms` keeps letters and digits and drops
/// everything else, `match_query` quotes each term and marks it a prefix.
///
/// Both halves are checked here: the query the builder produces never carries syntax, and
/// the public path ([`atlas_store::Store::search`]) answers every hostile value with
/// `Ok` — no `fts5: syntax error`. The punctuation-only value is the interesting one: it
/// builds **no** query at all, and `article_fts MATCH ''` *is* a syntax error (measured:
/// `fts5: syntax error near ""`). `Store::search` never sends it — the empty query goes
/// to the `LIKE` path instead (measured: 0 rows for `/* */ ; --`, 1 row for `chips`), so
/// a template made of punctuation degrades to "nothing matches" instead of an error in a
/// view.
#[test]
fn a_hostile_template_is_only_ever_a_search_word() {
    let root = temp_root("fts");
    let store = archive(&root);
    store
        .exec(
            "INSERT INTO articles(url,title,summary,content,published,relevant,dupe) \
             VALUES('https://example.com/chips','Chip export rules','Summary','Body about \
             chips and exports.','2026-11-22T10:00:00Z',1,0)",
            &[],
        )
        .expect("article");
    store
        .exec("INSERT INTO article_fts(article_fts) VALUES('rebuild')", &[])
        .expect("index");

    for value in HOSTILE {
        let query = match_query(value);
        // The built query is made of the value's own letters and digits, quoted: no `"`
        // from the value, no `*`, no `(`, `)`, `:`, `-`, `AND`, `OR`, `NOT`, `NEAR`.
        for term in match_terms(value) {
            assert!(
                term.chars().all(|ch| ch.is_alphanumeric()),
                "{value:?} produced a term with syntax in it: {term:?}"
            );
        }
        assert!(
            !query.contains('(') && !query.contains(':') && !query.contains('\u{202e}'),
            "{value:?} produced {query:?}"
        );
        // The statement runs with the built query as a bound parameter when there is one,
        // and the public path answers `Ok` for the value whether there is one or not.
        if !query.is_empty() {
            store
                .query(
                    "SELECT rowid FROM article_fts WHERE article_fts MATCH ?",
                    &[SqlValue::Text(query.clone())],
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "MATCH ? refused {query:?} built from {value:?}: {}",
                        error.message()
                    )
                });
        }
        store
            .search(value, 10)
            .unwrap_or_else(|error| panic!("search({value:?}): {}", error.message()));
    }

    // The path really answers: a word from the article finds it, and a punctuation-only
    // value finds nothing rather than failing.
    assert_eq!(
        store.search("chips", 10).expect("search").len(),
        1,
        "a plain query still finds the stored article"
    );
    assert_eq!(
        store.search("/* */ ; --", 10).expect("search").len(),
        0,
        "punctuation matches nothing and is not an error"
    );

    // A value that is nothing but punctuation is no query at all, and the builder says so
    // by handing back an empty string rather than a statement fragment.
    assert_eq!(match_query("/* */ ; --"), "");
    assert!(match_terms("/* */ ; --").is_empty());
}

// ------------------------------------------------------------------------ 3. the window

/// The window reads the archive and cannot write it; the one file it writes is the
/// request queue.
///
/// Asserted from the sources, because an integration test of this crate cannot open a
/// connection with the window's flags or call the window's code — and because the
/// property is a property *of those files*: a `set_meta` added to `data.rs` tomorrow
/// would be a write path in the window whether or not a runtime test noticed.
///
/// What is checked, and why each one matters:
///
/// * `SQLITE_OPEN_READ_ONLY` is among `data.rs`'s flags — the flag is the promise, not a
///   check the file could get wrong;
/// * `pragma_update` + `query_only` are both there — SQLite's own switch, set on the
///   connection, so a statement cannot pass even if the flag were dropped;
/// * no `INSERT `, `UPDATE `, `DELETE FROM`, `DROP `, `set_meta` in `data.rs` or in any
///   production file under `src/data/` (`tests.rs` is the fixture that seeds a
///   temporary database, and it is exempted by name, once);
/// * `src/requests.rs` writes `data/requests.db` (the quoted literal) and its code never
///   names `atlas.db`.
///
/// Every scan below runs over [`code_only`], not over the file's text: a comment that
/// *explains* a rule must not be able to fail the rule, and must not be able to satisfy
/// it either.
#[test]
fn the_window_can_only_read_the_archive_and_writes_only_the_queue() {
    let data = code_only(&source("../atlas-desktop/src/data.rs"));
    for needle in ["SQLITE_OPEN_READ_ONLY", "pragma_update", "query_only"] {
        assert!(
            data.contains(needle),
            "atlas-desktop/src/data.rs must keep {needle}: it is the read-only promise"
        );
    }
    for forbidden in ["INSERT ", "UPDATE ", "DELETE FROM", "DROP ", "set_meta"] {
        assert!(
            !data.contains(forbidden),
            "atlas-desktop/src/data.rs contains {forbidden:?} in code: the window's read \
             path must not write the archive"
        );
    }
    assert!(
        !data.contains("SQLITE_OPEN_READ_WRITE"),
        "the window must never ask for a writable handle"
    );

    // Production files under `src/data/` — everything except the fixture, which is
    // `#[cfg(test)]` and seeds a temporary database through its own writable handle.
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../atlas-desktop/src/data");
    if let Ok(entries) = std::fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
                continue;
            }
            let text = code_only(&std::fs::read_to_string(&path).unwrap_or_default());
            for forbidden in ["INSERT ", "UPDATE ", "DELETE FROM", "DROP "] {
                assert!(
                    !text.contains(forbidden),
                    "{} contains {forbidden:?} in code: the window's read path must not write",
                    path.display()
                );
            }
        }
    }

    let queue = code_only(&source("../atlas-desktop/src/requests.rs"));
    assert!(
        queue.contains("INSERT INTO requests"),
        "the window's one writable statement is the queue push"
    );
    // The file name is a quoted literal in the code (`…join("data").join("requests.db")`),
    // so it is the literal that is asked for — not the word, which a sentence about
    // redaction can contain.
    assert!(
        queue.contains("\"requests.db\""),
        "the queue lives in the literal \"data/requests.db\""
    );
    assert!(
        !queue.contains("\"atlas.db\""),
        "the window's writable file must not be the archive: the literal \"atlas.db\" \
         appears in code"
    );

    // And the engine key is read with a bound parameter, in the crate that reads it.
    let search = code_only(&source("../atlas-ingest/src/search.rs"));
    assert!(
        search.contains("get_meta(META_KEY)"),
        "the template is read through the store's bound getter"
    );
    for forbidden in [
        "format!(\"SELECT",
        "format!(\"INSERT",
        "format!(\"UPDATE",
        "format!(\"DELETE",
        "format!(\"MATCH",
    ] {
        assert!(
            !search.contains(forbidden),
            "atlas-ingest/src/search.rs contains {forbidden:?}: the template must never \
             be interpolated into SQL"
        );
    }
}

/// The mechanism the rule above rests on, pinned on its own: [`code_only`] drops prose and
/// keeps code — in **both** directions, because a scan that lets a comment fail the rule is
/// annoying and one that lets a comment hide the code is dangerous.
///
/// Measured cases, and why each one is here: a doc sentence naming `data/atlas.db` must
/// not trip the rule (that is the failure this fix answers); a string literal naming
/// `"atlas.db"` must trip it (or the rule is asleep); a `//` inside `"https://…"` must not
/// start a comment (or every URL would swallow the rest of its line and the rule would
/// miss real code); a block comment nests; a raw string keeps its content.
#[test]
fn the_scan_reads_code_and_not_comments() {
    let prose = "// measured over the 1439 URLs in `data/atlas.db`\n\
                 pub fn same_directory() { let path = root().join(\"requests.db\"); }";
    assert!(
        !code_only(prose).contains("atlas.db"),
        "a sentence about the archive is not a write to it"
    );
    assert!(code_only(prose).contains("join(\"requests.db\")"));

    let code = "pub fn other() { let path = root().join(\"atlas.db\"); }";
    assert!(
        code_only(code).contains("\"atlas.db\""),
        "code that opens the archive is still caught"
    );

    let url = "pub const U: &str = \"https://example.com/?q=atlas.db\"; pub fn f() {}";
    let stripped = code_only(url);
    assert!(
        stripped.contains("https://example.com/?q=atlas.db"),
        "`//` inside a string is data, not a comment: {stripped}"
    );
    assert!(
        stripped.contains("pub fn f()"),
        "and nothing after it was swallowed: {stripped}"
    );

    let block = "/* outer /* inner */ still prose: atlas.db */ let x = 1;";
    let stripped = code_only(block);
    assert!(!stripped.contains("atlas.db"), "{stripped}");
    assert!(stripped.contains("let x = 1;"), "{stripped}");

    let raw = "let s = r#\"atlas.db /* not a comment */\"#; let y = 2;";
    let stripped = code_only(raw);
    assert!(stripped.contains("atlas.db"), "{stripped}");
    assert!(stripped.contains("let y = 2;"), "{stripped}");
}

// ------------------------------------------------------------------------ 4. the cap
/// The value cap, in Rust (`atlas-store/src/meta.rs:58`): a marker is a timestamp, a path
/// or a URL template, and [`atlas_store::meta::MAX_VALUE_BYTES`] is room around the
/// biggest of them. Above it, the write is an error naming the cap and the length it was
/// handed — not a truncation, and not a row.
///
/// The table's own DDL still has no `CHECK`, and that is the point of this test: a
/// `CHECK(length(v) <= n)` would not be a cap. SQLite stops reading a string at its first
/// NUL, so a value of `"\0"` plus a megabyte measures **0** in SQL while being 1 048 577
/// characters to Rust — measured below by writing that row through raw SQL, which is the
/// only way past `set_meta` and exactly the shape a `CHECK` would have let through.
///
/// Also pinned: the boundary (8 192 bytes stored whole, 8 193 refused), and a 4 KiB
/// template — the size a real engine template has — still round-trips, so the cap cannot
/// be tightened later into something that breaks a configured engine.
#[test]
fn the_meta_cap_is_counted_in_rust_and_a_megabyte_is_refused() {
    let root = temp_root("cap");
    let store = archive(&root);
    let cap = atlas_store::meta::MAX_VALUE_BYTES;

    // The definition itself: no CHECK, so the cap has to be counted in Rust.
    let ddl = meta_ddl(&store).expect("the meta table exists");
    assert_eq!(
        ddl, "CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT)",
        "the DDL decides whether a length can be refused at all"
    );
    assert!(
        !ddl.to_uppercase().contains("CHECK"),
        "there is no SQL cap, which is why the one above exists: {ddl}"
    );

    // At the cap: stored whole. A marker key, because `search_engine` carries the gate's
    // smaller cap (below) — this is the generic one.
    let at_cap = "A".repeat(cap);
    store.set_meta("enrich:1", &at_cap).expect("a value at the cap");
    assert_eq!(
        store.get_meta("enrich:1").expect("get").as_deref(),
        Some(at_cap.as_str()),
        "a value exactly at the cap is a marker, not a document"
    );

    // One byte over: refused, with the two numbers.
    let over = "A".repeat(cap + 1);
    let message = match store.set_meta("enrich:1", &over) {
        Ok(_) => panic!("a {}-byte value was accepted over a {cap}-byte cap", over.len()),
        Err(error) => error.message().to_string(),
    };
    assert!(
        message.contains(&cap.to_string()),
        "the sentence names the cap: {message}"
    );
    assert!(
        message.contains(&over.len().to_string()),
        "and the length it was handed: {message}"
    );
    assert_eq!(
        store.get_meta("enrich:1").expect("get").as_deref(),
        Some(at_cap.as_str()),
        "a refused write leaves the stored value exactly as it was"
    );

    // The engine template carries the *gate's* cap: a template longer than the URL the gate
    // can judge is one no search could ever build, and storing it would make Settings say
    // "yours" while the agent fell back to the built-in engine (measured once at 5 000
    // characters). 2 048 is accepted, 2 049 is refused naming 2 048.
    let engine_cap = atlas_store::meta::MAX_ENGINE_TEMPLATE_BYTES;
    assert_eq!(engine_cap, 2_048, "the gate's number, spelled once");
    let mut template = String::from("https://www.bing.com/news/search?q={query}&pad=");
    while template.len() < engine_cap {
        template.push('a');
    }
    assert_eq!(template.len(), engine_cap);
    store
        .set_meta(atlas_store::meta::ENGINE_KEY, &template)
        .expect("a template the gate can build is stored");
    let too_long = format!("{template}a");
    let refused = match store.set_meta(atlas_store::meta::ENGINE_KEY, &too_long) {
        Ok(_) => panic!("a {}-character template was accepted", too_long.len()),
        Err(error) => error.message().to_string(),
    };
    assert!(
        refused.contains("2048") && refused.contains("gate"),
        "the sentence names the gate's cap and why: {refused}"
    );
    assert_eq!(
        store
            .get_meta(atlas_store::meta::ENGINE_KEY)
            .expect("get")
            .as_deref(),
        Some(template.as_str()),
        "the refused template left the stored one exactly as it was"
    );

    // A mebibyte: refused, and no row anywhere.
    let big = "A".repeat(1024 * 1024);
    let message = match store.set_meta("megabyte", &big) {
        Ok(_) => panic!("a 1 MiB value was accepted over a {cap}-byte cap"),
        Err(error) => error.message().to_string(),
    };
    assert!(
        message.contains("8192"),
        "the sentence names the cap in bytes: {message}"
    );
    assert!(
        message.contains("1048576"),
        "and the megabyte it was handed: {message}"
    );
    assert_eq!(store.get_meta("megabyte").expect("get"), None, "no row");

    // The reason the count is Rust's and not SQL's: a leading NUL, then a megabyte.
    let mut smuggled = String::from("\u{0}");
    smuggled.push_str(&"A".repeat(1024 * 1024));
    let message = match store.set_meta("nul-smuggled", &smuggled) {
        Ok(_) => panic!("a NUL-smuggled megabyte was accepted"),
        Err(error) => error.message().to_string(),
    };
    assert!(
        message.contains(&smuggled.len().to_string()),
        "Rust counted all {} bytes: {message}",
        smuggled.len()
    );
    assert_eq!(store.get_meta("nul-smuggled").expect("get"), None);

    // The counter-example, through raw SQL (never the app's path): the same value stored
    // without the cap measures 0 in SQL, so a `CHECK(length(v))` would have allowed it.
    store
        .exec(
            "INSERT INTO meta(k,v) VALUES('bypass',?)",
            &[SqlValue::Text(smuggled)],
        )
        .expect("raw insert");
    assert_eq!(
        store
            .query_one("SELECT length(v) n FROM meta WHERE k='bypass'", &[])
            .expect("length")
            .and_then(|row| row.int("n")),
        Some(0),
        "SQL sees an empty string; the row holds a megabyte — a CHECK would miss it"
    );

    // A long template that still fits the gate's cap — what a configured engine really
    // looks like — round-trips whole.
    let engine_cap = atlas_store::meta::MAX_ENGINE_TEMPLATE_BYTES;
    let mut template = String::from("https://search.example.com/?q={query}&pad=");
    while template.len() + 100 < engine_cap {
        template.push('b');
    }
    assert!(
        template.len() <= engine_cap,
        "the fixture must fit under the engine's cap"
    );
    store.set_meta("search_engine", &template).expect("set");
    assert_eq!(
        store.get_meta("search_engine").expect("get").as_deref(),
        Some(template.as_str()),
        "a long but legitimate template still stores whole"
    );
}

/// The cap that *does* exist on this path: the window's route to a settings write is the
/// request queue, and the queue refuses a payload past `MAX_PAYLOAD_BYTES` (65 536) with
/// a sentence naming both numbers — measured: `"a request payload is capped at 65536
/// bytes; this one is 71696"`.
///
/// Also pinned: the payload is data (a template inside it is stored and claimed back
/// byte for byte, quotes and all), and pushing a request creates **no** `atlas.db` — the
/// window's writable file is the queue, and the archive is not touched by it.
#[test]
fn the_only_cap_on_this_path_is_the_queue_payload() {
    let root = temp_root("queue");
    let queue = Requests::open(root.queue()).expect("queue");

    // A template inside a request is one JSON string, and the queue does not read it.
    let payload = format!("{{\"template\": \"{}\"}}", HOSTILE[0]);
    let id = queue.push("topic_add", &payload).expect("push");
    assert!(id > 0);
    let claimed = queue.claim_due(&now_iso(), 60, 5).expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].payload, payload, "the payload is data, verbatim");
    assert_eq!(claimed[0].kind, "topic_add");

    // 70 KiB of payload is past the cap, and the refusal names the cap.
    let oversized = format!("{{\"template\": \"{}\"}}", "A".repeat(70 * 1024));
    let refused = queue.push("topic_add", &oversized);
    let message = match refused {
        Ok(id) => panic!("a 71 KiB payload was accepted as request {id}"),
        Err(error) => error.message().to_string(),
    };
    assert!(
        message.contains("65536"),
        "the refusal names the cap: {message}"
    );
    assert!(
        message.contains(&oversized.len().to_string()),
        "and the size it was handed: {message}"
    );

    // The queue's file is its own: no archive was created beside it.
    assert!(
        !root.archive().exists(),
        "pushing a request must not create data/atlas.db"
    );
    assert!(root.queue().exists(), "the queue is the file that was created");
    // `depth` counts *unfinished* rows, and a claimed row is unfinished until its claimer
    // finishes it — the lease is what makes a crashed claimer's request come back.
    assert_eq!(
        queue.depth().expect("depth"),
        1,
        "a leased row is still an open request"
    );
    queue.finish(claimed[0].id, None).expect("finish");
    assert_eq!(queue.depth().expect("depth"), 0, "a finished row is done");
}

// ------------------------------------------------------------------------ helpers

/// Every table in the file, sorted — the shape the corpus must not change.
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

/// The `meta` table's own DDL, as SQLite stored it.
fn meta_ddl(store: &Store) -> Option<String> {
    store
        .query_one(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='meta'",
            &[],
        )
        .expect("ddl")
        .and_then(|row| row.text("sql").map(|text| text.to_string()))
}
