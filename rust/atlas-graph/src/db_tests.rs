//! The database half's tests: the write path, idempotent recompute, thread
//! stability, the read side, the parity proof against `server/graph.py`, and the
//! 400-article timing fixture.
//!
//! The 47 engine tests stay in `lib.rs`, untouched. Everything here owns its
//! database: a file under `temp_dir`, named after the test and the process id, so
//! the tests can run in parallel.

use std::path::PathBuf;

use atlas_lib::json::{self, Json};
use atlas_store::{SqlValue, Store};

use crate::graphdb::{self, Caches, Context};
use crate::graphlink;
use crate::graphread;

// ---------------------------------------------------------------- fixtures

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atlas-graph-{}-{}", label, std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A fresh database at `label`, with the atlas-store schema.
fn db(label: &str) -> (PathBuf, Store) {
    let path = temp_dir(label).join("atlas.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
    let store = Store::open(&path).expect("open");
    store.init().expect("init");
    (path, store)
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

/// One clock reading for the whole test run, so two databases seeded by the same
/// test hold byte-identical `published` values.
fn now_epoch() -> i64 {
    atlas_store::parse_epoch(&atlas_store::now_iso())
}

/// An ISO timestamp `hours` before `now_epoch()`.
fn ago(now: i64, hours: i64) -> String {
    atlas_store::iso_from_epoch(now - hours * 3600)
}

fn insert_article(
    store: &Store,
    id: i64,
    title: &str,
    summary: &str,
    content: &str,
    published: &str,
) {
    store
        .exec(
            "INSERT INTO articles(id,title,summary,content,published,relevant,iso) \
             VALUES(?,?,?,?,?,1,'US')",
            &[
                int(id),
                text(title),
                text(summary),
                text(content),
                text(published),
            ],
        )
        .expect("insert article");
}

fn insert_entity(store: &Store, id: i64, name: &str, kind: &str, count: i64) {
    store
        .exec(
            "INSERT INTO entities(id,name,type,count,first_seen,last_seen) \
             VALUES(?,?,?,?,'2026-11-01T00:00:00Z','2026-11-01T00:00:00Z')",
            &[int(id), text(name), text(kind), int(count)],
        )
        .expect("insert entity");
}

fn link(store: &Store, article: i64, entity: i64) {
    store
        .exec(
            "INSERT OR IGNORE INTO article_entities(article_id,entity_id) VALUES(?,?)",
            &[int(article), int(entity)],
        )
        .expect("link");
}

/// The database state both sides must agree on, in a comparable shape: six
/// tables, every row one `|`-joined string, every table sorted — a set
/// comparison for `edges` (SQL leaves that order open), an exact one for the
/// thread ids, which a rebuild has to number the way `lastrowid` would.
fn tables(store: &Store) -> Vec<(&'static str, Vec<String>)> {
    let dump = |sql: &str| -> Vec<String> {
        sorted(
            store
                .query(sql, &[])
                .expect("dump")
                .iter()
                .map(|row| {
                    row.values()
                        .iter()
                        .map(cell)
                        .collect::<Vec<String>>()
                        .join("|")
                })
                .collect(),
        )
    };
    vec![
        ("edges", dump("SELECT a,b,weight,type,label FROM edges")),
        (
            "article_links",
            dump("SELECT a,b,shared,score,why FROM article_links"),
        ),
        (
            "entities",
            dump("SELECT id,name,type,count,category_id FROM entities"),
        ),
        (
            "article_entities",
            dump("SELECT article_id,entity_id FROM article_entities"),
        ),
        ("threads", dump("SELECT id,title,size,updated FROM threads")),
        (
            "article_threads",
            dump("SELECT article_id,thread_id FROM article_threads"),
        ),
    ]
}

/// A number the same way on both sides: `fmt_num(3)` and `fmt_num(3.0)` agree,
/// because SQLite stores an integral `REAL` for one and an `INTEGER` for the
/// other and neither side should care.
fn fmt_num(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{:.6}", value)
    }
}

fn cell(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "-".to_string(),
        SqlValue::Integer(number) => fmt_num(*number as f64),
        SqlValue::Real(number) => fmt_num(*number),
        SqlValue::Text(text) => text.clone(),
        SqlValue::Blob(bytes) => format!("{}b", bytes.len()),
    }
}


fn sorted(mut rows: Vec<String>) -> Vec<String> {
    rows.sort();
    rows
}

/// The one place a `why` line cannot be compared character for character.
///
/// `shared_words()` picks the three most telling words a pair of articles shares.
/// When two words weigh *exactly* the same — which happens as soon as they are
/// equally frequent, as in any uniform fixture — the order between them comes from
/// Python's `set` iteration, i.e. from string hashes: `PYTHONHASHSEED` decides it
/// and no other Python process would answer the same way. This port breaks the tie
/// by the token, so the answer is the same twice in a row.
///
/// Everything else about the row is compared exactly: the pair, the shared count,
/// the score, and the *set* of names in the sentence.
fn normalise_row(table: &str, row: &str) -> String {
    if table != "article_links" {
        return row.to_string();
    }
    let parts: Vec<&str> = row.split('|').collect();
    if parts.len() < 5 {
        return row.to_string();
    }
    let why = parts[4];
    let (kind, names) = match why.find(": ") {
        Some(at) => (&why[..at], &why[at + 2..]),
        None => (why, ""),
    };
    let mut names: Vec<&str> = if names.is_empty() {
        Vec::new()
    } else {
        names.split(", ").collect()
    };
    names.sort_unstable();
    format!(
        "{}|{}|{}|{}|{}{}",
        parts[0],
        parts[1],
        parts[2],
        parts[3],
        kind,
        if names.is_empty() {
            String::new()
        } else {
            format!(": {}", names.join(", "))
        }
    )
}

fn count(store: &Store, sql: &str) -> i64 {
    store
        .query_one(sql, &[])
        .expect("count")
        .and_then(|row| row.get_at(0).and_then(|v| v.as_i64_for_test()))
        .unwrap_or(0)
}

/// Small helper so a test can read a count without naming the column.
trait AsInt {
    fn as_i64_for_test(&self) -> Option<i64>;
}

impl AsInt for SqlValue {
    fn as_i64_for_test(&self) -> Option<i64> {
        match self {
            SqlValue::Integer(number) => Some(*number),
            SqlValue::Real(number) => Some(*number as i64),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- link_article

#[test]
fn link_article_with_no_entities_only_clears_the_article() {
    let (_path, store) = db("link-empty");
    insert_article(&store, 1, "Nothing here", "", "", &ago(now_epoch(), 1));
    insert_entity(&store, 7, "Nvidia", "ORG", 3);
    link(&store, 1, 7);

    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let done = graphdb::link_article(&store, &mut caches, &ctx, 1, &[]).expect("link");

    assert_eq!(done.entities, 0);
    assert_eq!(done.pairs, 0);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM article_entities"),
        0,
        "the previous links of the article are gone"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM entities"),
        1,
        "an entity nobody links to stays"
    );
}

#[test]
fn link_article_writes_one_entity_one_row_and_no_edge() {
    let (_path, store) = db("link-one");
    insert_article(&store, 1, "Nvidia reports", "", "", &ago(now_epoch(), 1));

    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let done = graphdb::link_article(
        &store,
        &mut caches,
        &ctx,
        1,
        &[("Nvidia".to_string(), "ORG".to_string())],
    )
    .expect("link");

    assert_eq!(done.entities, 1);
    assert_eq!(done.pairs, 0, "one entity cannot make a pair");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM entities"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM article_entities"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM edges"), 0);
    let row = store
        .query_one("SELECT name,type,count FROM entities", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row.text("name"), Some("Nvidia"));
    assert_eq!(row.text("type"), Some("ORG"));
    assert_eq!(row.int("count"), Some(1));
}

#[test]
fn link_article_caps_the_edges_of_two_hundred_entities() {
    let (_path, store) = db("link-200");
    insert_article(&store, 1, "Everything at once", "", "", &ago(now_epoch(), 1));
    // 200 people + 20 places: more than the per-article budget of 12 + 3.
    let mut ents: Vec<(String, String)> = Vec::new();
    for n in 1..=200 {
        ents.push((format!("Person Number{}", n), "PERSON".to_string()));
    }
    for n in 1..=20 {
        ents.push((format!("Place Number{}", n), "LOC".to_string()));
    }

    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let done = graphdb::link_article(&store, &mut caches, &ctx, 1, &ents).expect("link");

    // every accepted name is attached to the article (the Python cap is on the
    // *edges*, not on `article_entities`)
    assert_eq!(done.entities, 220);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM article_entities"), 220);
    // The edge budget, as `_keep_ids()` spends it: places are capped at 3 first,
    // which leaves `12 - 3 = 9` slots for people, so 12 entities make the edges.
    assert_eq!(done.pairs, 12 * 11 / 2, "C(12,2) = 66");
    assert_eq!(done.pairs, count(&store, "SELECT COUNT(*) FROM edges") as usize);
    let linked: i64 = count(
        &store,
        "SELECT COUNT(DISTINCT a) + COUNT(DISTINCT b) FROM edges",
    );
    assert!(linked <= 24, "only the kept entities appear in edges");
}

#[test]
fn link_article_folds_duplicate_variants_into_one_row() {
    let (_path, store) = db("link-variants");
    insert_article(&store, 1, "Nvidia, Nvidia, Nvidia", "", "", &ago(now_epoch(), 1));
    let ents: Vec<(String, String)> = [
        "Nvidia",
        "nvidia",
        "NVIDIA",
        "Nvidia Inc",
        "Nvidia Corp.",
        "Nvidia's",
        "Nvidia",
    ]
    .iter()
    .map(|name| (name.to_string(), "ORG".to_string()))
    .collect();

    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let done = graphdb::link_article(&store, &mut caches, &ctx, 1, &ents).expect("link");

    assert_eq!(done.entities, 1, "one canonical entity");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM entities"), 1);
    let row = store
        .query_one("SELECT name,count FROM entities", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row.text("name"), Some("Nvidia"));
    assert_eq!(
        row.int("count"),
        Some(7),
        "every accepted mention increments the counter"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM article_entities"), 1);
}

#[test]
fn link_article_rejects_junk_and_implausible_names() {
    let (_path, store) = db("link-junk");
    insert_article(&store, 1, "Page chrome", "", "", &ago(now_epoch(), 1));
    let ents: Vec<(String, String)> = [
        // Shape and grammar only, since the 1209-word list went. `Tuesday` is still refused
        // here — not because a weekday was on a list of things, but because calendar names
        // are grammar (a name never starts with one). What is *not* refused any more is a
        // page's furniture word: "Continue", "Clique", "Subscribe" now reach this function
        // only if the article itself writes them in lower case, which is
        // `atlas_ingest::entities::extract`'s evidence rule.
        "Tuesday",        // a calendar word
        "ABC",            // an all-caps stub of four characters or fewer
        "Nvidia in",      // ends in a preposition
        "The Nvidia",     // starts with an article
        "Nvidia the SEC", // a determiner inside
        "12:30",          // a timestamp
    ]
    .iter()
    .map(|name| (name.to_string(), "ORG".to_string()))
    .collect();

    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);

    let done = graphdb::link_article(&store, &mut caches, &ctx, 1, &ents).expect("link");

    assert_eq!(done.entities, 0, "nothing survived the filters");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM entities"), 0);
}

/// A publisher is a thing a story names, so the graph stores it.
///
/// This is the behaviour `OUTLETS` (69 publisher names) was preventing: an outlet named in
/// 99 of the live archive's 1435 articles carry, in the `publisher` column, one of those 69
/// names (13 distinct), and 42 articles' title or summary names one; none of them could
/// become an entity, and nothing in the database said a list had decided it. The complementary half — a page's "Read More" — is
/// not offered to this function at all now: `atlas_ingest::entities::extract` refuses a run
/// the article itself writes in lower case, and `enrich::admit` does the same for a name a
/// model offers.
#[test]
fn a_publisher_is_stored_because_the_story_names_it() {
    let (_path, store) = db("link-publisher");
    insert_article(&store, 1, "Chip exports rise", "", "", &ago(now_epoch(), 1));
    let ents: Vec<(String, String)> = ["Reuters", "Bloomberg", "Nvidia"]
        .iter()
        .map(|name| (name.to_string(), "ORG".to_string()))
        .collect();
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let done = graphdb::link_article(&store, &mut caches, &ctx, 1, &ents).expect("link");
    assert_eq!(done.entities, 3, "a publisher is a name: {:?}", done);
    let stored: Vec<String> = store
        .query("SELECT name FROM entities ORDER BY name", &[])
        .expect("rows")
        .iter()
        .filter_map(|row| row.text("name").map(|name| name.to_string()))
        .collect();
    assert_eq!(stored, vec!["Bloomberg", "Nvidia", "Reuters"]);
}

/// A name the graph stores is a name the app's one rule accepts.
///
/// Page text and model output both reach `link_article` unchanged, and what may
/// become an entity name is `atlas_ai::prompts::entity_name`'s decision — the same
/// rule an entity that arrives as a model tool call is held to. Measured before it
/// was applied here: `Nvidia\u{202e}\u{0007}Corp` was stored as an entity name (a
/// right-to-left override and a bell character, in a name the reader sees), and
/// `<img src=x onerror=alert(1)>` was stored as one too.
#[test]
fn a_name_the_graph_stores_is_a_name_a_reader_can_see() {
    let (_path, store) = db("link-invisible");
    insert_article(&store, 1, "Nvidia and the SEC", "", "", &ago(now_epoch(), 1));
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let done = graphdb::link_article(
        &store,
        &mut caches,
        &ctx,
        1,
        &[
            ("Nvidia\u{202e}\u{0007}Corp".to_string(), "ORG".to_string()),
            ("<img src=x onerror=alert(1)>".to_string(), "ORG".to_string()),
            ("\u{202e}\u{0007}".to_string(), "ORG".to_string()),
            ("Bundesbank".to_string(), "ORG".to_string()),
        ],
    )
    .expect("link");

    let names: Vec<String> = store
        .query("SELECT name FROM entities ORDER BY id", &[])
        .expect("names")
        .iter()
        .map(|row| row.text("name").unwrap_or("").to_string())
        .collect();
    assert_eq!(done.entities, 2, "the two names the rule refuses are dropped");
    assert_eq!(
        names,
        vec!["NvidiaCorp".to_string(), "Bundesbank".to_string()],
        "the characters the rule strips are gone, and a name with nothing left is \
         not stored at all"
    );
    for name in &names {
        assert!(
            !name.contains('\u{202e}') && !name.contains('\u{0007}'),
            "{}: an entity name holds nothing a reader cannot see",
            name
        );
        assert!(
            !name.contains('<') && !name.contains('>'),
            "{}: markup is not a name",
            name
        );
    }
}

#[test]
fn link_article_trusts_a_place_name_the_filters_dislike() {
    let (_path, store) = db("link-place");
    insert_article(&store, 1, "New Zealand signs a trade deal", "", "", &ago(now_epoch(), 1));
    let ctx = Context::from_json(&json::parse(r#"{"places":["New Zealand"]}"#).unwrap());
    let mut caches = Caches::new();
    let done = graphdb::link_article(
        &store,
        &mut caches,
        &ctx,
        1,
        &[("New Zealand".to_string(), "LOC".to_string())],
    )
    .expect("link");

    assert_eq!(done.entities, 1);
    let row = store
        .query_one("SELECT name,type FROM entities", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row.text("name"), Some("New Zealand"));
}

// ------------------------------------------------------- the model classifies

/// The type is the model's, verbatim: an entity the model did not classify is
/// stored untyped (`""`), and no suffix, capitalisation or word list may fill it
/// in. "Bundesbank" and "Acme Inc." would be an `ORG` to any heuristic; here they
/// are untyped until the model says otherwise.
#[test]
fn link_article_stores_the_type_the_model_sent_and_nothing_else() {
    let (_path, store) = db("link-types");
    insert_article(&store, 1, "Bundesbank and Aurora Systems", "", "", &ago(now_epoch(), 1));
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let done = graphdb::link_article(
        &store,
        &mut caches,
        &ctx,
        1,
        &[
            ("Bundesbank".to_string(), String::new()),
            ("Aurora Systems".to_string(), "ORG".to_string()),
            ("Acme Inc.".to_string(), String::new()),
            ("Summit Conference".to_string(), String::new()),
        ],
    )
    .expect("link");
    let names: Vec<String> = store
        .query("SELECT name FROM entities ORDER BY id", &[])
        .expect("names")
        .iter()
        .map(|row| row.text("name").unwrap_or("").to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "Bundesbank".to_string(),
            "Aurora Systems".to_string(),
            "Acme".to_string(),
            "Summit Conference".to_string(),
        ],
        "every accepted name, in insertion order"
    );
    assert_eq!(done.entities, 4);

    let types = |name: &str| -> Option<String> {
        store
            .query_one("SELECT type FROM entities WHERE name=?", &[text(name)])
            .unwrap()
            .and_then(|row| row.text("type").map(|value| value.to_string()))
    };
    for untyped in ["Bundesbank", "Acme", "Summit Conference"] {
        assert_eq!(
            types(untyped),
            Some(String::new()),
            "{}: no model type, no type — nothing infers one from the name",
            untyped
        );
    }
    assert_eq!(types("Aurora Systems"), Some("ORG".to_string()));
}

/// The structural gate, named so the trade-off stays visible: graph.py drops any
/// run that contains a newsroom navigation or outlet word, which is page chrome in
/// a scraped teaser ("Sign up for Markets") but also costs real names —
/// "Ministry of Finance" never becomes an entity. It is kept as it is because it
/// is part of the database state `graph.py` produces, and it is not a
/// classification rule: nothing here decides what a name *is*.
#[test]
fn a_name_holding_a_navigation_word_is_still_read_as_page_chrome() {
    let (_path, store) = db("link-chrome");
    insert_article(&store, 1, "Ministry of Finance and Aurora Systems", "", "", &ago(now_epoch(), 1));
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    graphdb::link_article(
        &store,
        &mut caches,
        &ctx,
        1,
        &[
            ("Ministry of Finance".to_string(), "ORG".to_string()),
            ("Aurora Systems".to_string(), "ORG".to_string()),
        ],
    )
    .expect("link");
    let names: Vec<String> = store
        .query("SELECT name FROM entities ORDER BY id", &[])
        .expect("names")
        .iter()
        .map(|row| row.text("name").unwrap_or("").to_string())
        .collect();
    // "ministry" and "finance" are both navigation words, so the run reads as a
    // section label — the cost of keeping the port faithful.
    assert_eq!(names, vec!["Aurora Systems".to_string()]);
}

/// A type, once the model has given one, is never relabelled by a later article —
/// and an entity the model left untyped is filled in when a later article knows.
#[test]
fn link_article_never_relabels_an_entity_it_already_typed() {
    let (_path, store) = db("link-type-stability");
    for id in 1..=3 {
        insert_article(
            &store,
            id,
            "Helio Works and Nimbus",
            "",
            "",
            &ago(now_epoch(), id),
        );
    }
    let ctx = Context::from_json(&Json::Null);
    let mut caches = Caches::new();
    let call = |caches: &mut Caches, id: i64, helio: &str, nimbus: &str| {
        graphdb::link_article(
            &store,
            caches,
            &ctx,
            id,
            &[
                ("Helio Works".to_string(), helio.to_string()),
                ("Nimbus".to_string(), nimbus.to_string()),
            ],
        )
        .expect("link")
    };
    // the first model answer: one organisation, one name nobody classified
    call(&mut caches, 1, "ORG", "");
    call(&mut caches, 2, "PERSON", ""); // a different answer: not applied
    call(&mut caches, 3, "", ""); // a model that knows nothing: not applied either

    let kind = |name: &str| -> Option<String> {
        store
            .query_one("SELECT type FROM entities WHERE name=?", &[text(name)])
            .unwrap()
            .and_then(|row| row.text("type").map(|value| value.to_string()))
    };
    assert_eq!(kind("Helio Works"), Some("ORG".to_string()));

    // an untyped row is completed by the next article that knows, because that is
    // new information and not a replacement
    let ctx = Context::from_json(&Json::Null);
    graphdb::link_article(
        &store,
        &mut Caches::new(),
        &ctx,
        3,
        &[("Nimbus".to_string(), "EVENT".to_string())],
    )
    .expect("link");
    let row = store
        .query_one("SELECT type FROM entities WHERE name='Nimbus'", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row.text("type"), Some("EVENT"));
}

/// The JSON payload of the CLI: a missing type means untyped, and the word
/// "miscellaneous" is a classification, so it is never inserted on the caller's
/// behalf.
#[test]
fn the_cli_keeps_an_untyped_entity_untyped() {
    let (path, store) = db("link-cli-types");
    insert_article(&store, 1, "Nimbus and Nova", "", "", &ago(now_epoch(), 1));
    drop(store);
    let payload = format!(
        r#"{{"db":{},"id":1,"entities":[["Nimbus"],["Nova","EVENT"],{{"name":"Aurora Systems"}}]}}"#,
        json::to_string(&Json::Str(path.to_string_lossy().to_string()))
    );
    let answer = crate::run("link_article", &payload).expect("link_article");
    assert!(answer.contains("\"source\":\"rust\""), "{}", answer);

    let store = Store::open(&path).expect("reopen");
    let untyped = count(
        &store,
        "SELECT COUNT(*) FROM entities WHERE type IS NULL OR type=''",
    );
    assert_eq!(untyped, 2, "Nimbus and Aurora Systems carry no type");
    let row = store
        .query_one("SELECT type FROM entities WHERE name='Nova'", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row.text("type"), Some("EVENT"));
}

// ---------------------------------------------------------------- recompute

/// The fixture the recompute tests and the parity proof share: variants, junk,
/// a place, a topic and two stories that only share their wording.
///
/// `now` is passed in so two databases seeded for the parity proof carry the same
/// `published` values; nothing else in this fixture looks at the clock.
fn seed_recompute_fixture(store: &Store, now: i64) {
    let long = "Nvidia and the SEC traded statements about the accounting review while \
                investors waited for the quarterly numbers to be published.";
    insert_article(
        store,
        1,
        "Nvidia faces SEC accounting investigation",
        long,
        long,
        &ago(now, 2),
    );
    insert_article(
        store,
        2,
        "Nvidia shares slide as regulators review accounting",
        "Nvidia shares fell after the SEC opened a review of its accounting.",
        long,
        &ago(now, 3),
    );
    insert_article(
        store,
        3,
        "Jensen Huang meets regulators in Washington",
        "The Nvidia chief executive met regulators.",
        "Jensen Huang met regulators in Washington and said nothing about the review.",
        &ago(now, 4),
    );
    insert_article(
        store,
        4,
        "Semiconductor prices ease after a long quarter",
        "Prices for memory chips eased.",
        "Memory chip prices eased after a long quarter of shortages.",
        &ago(now, 5),
    );
    insert_article(
        store,
        5,
        "Semiconductor prices ease again in a long quarter",
        "Memory chip prices eased again.",
        "Memory chip prices eased again after a long quarter of shortages.",
        &ago(now, 6),
    );
    insert_article(
        store,
        6,
        "Unrelated story about the weather",
        "It rained.",
        "It rained all week.",
        &ago(now, 7),
    );

    insert_entity(store, 1, "Nvidia", "ORG", 9);
    insert_entity(store, 2, "SEC", "ORG", 5);
    insert_entity(store, 3, "Jensen Huang", "PERSON", 4);
    insert_entity(store, 4, "Washington", "LOC", 6);
    insert_entity(store, 5, "Nvidia Inc", "ORG", 2);   // a variant of 1: merged away
    // A preposition at the end, not a name — refused by grammar. This row used to be
    // "Brasil.", refused by the fragment-punctuation rule; `canonical_name` strips the dot
    // before the gate here, so it was the wrong fixture for "a row the purge must delete".
    insert_entity(store, 6, "Nvidia in", "MISC", 3);    // junk: purged
    // A timestamp, not a name — refused by shape. This row used to be "Tuesday", refused by
    // the weekday half of the 1209-word list; the list is gone, and whether a day word is a
    // page's furniture is now the article's evidence (`atlas_ingest::entities::extract`) to
    // say, so the graph boundary keeps only what a shape can decide.
    insert_entity(store, 7, "12:30", "MISC", 2);       // junk: purged
    insert_entity(store, 8, "Semiconductor", "MISC", 3);

    link(store, 1, 1);
    link(store, 1, 2);
    link(store, 1, 5);
    link(store, 2, 1);
    link(store, 2, 2);
    link(store, 2, 3);
    link(store, 3, 1);
    link(store, 3, 3);
    link(store, 3, 4);
    link(store, 4, 8);
    link(store, 5, 8);
    link(store, 6, 6);
    link(store, 6, 7);

    store
        .exec("INSERT INTO topics(id,name) VALUES(1,'Chips')", &[])
        .expect("topic");
    store
        .exec(
            "INSERT INTO article_topics(article_id,topic_id) VALUES(1,1),(2,1)",
            &[],
        )
        .expect("article_topics");
}

#[test]
fn recompute_rebuilds_edges_and_links_then_changes_nothing_twice() {
    let (path, store) = db("recompute-idempotent");
    let now = now_epoch();
    seed_recompute_fixture(&store, now);
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);

    let first = graphdb::recompute_edges(&store, &mut caches, &ctx, 30).expect("edges");
    assert!(first.merged >= 1, "the Inc variant is folded away");
    // `Brasil.`, `12:30`, and `SEC` — the last one because `ok_name()` never accepts an
    // all-capitals stub of four characters or fewer. (`Tuesday` used to be the third one,
    // refused by the weekday list; that list is gone, so this fixture is shape-refused only.)
    assert_eq!(first.purged, 3, "three entity rows are page chrome");
    assert!(first.edges > 0, "the fixture shares entities");

    let links = graphlink::recompute_links(&store, &mut caches, &ctx, 14, 8, 60_000)
        .expect("links");
    assert!(links.links > 0, "the fixture has connections");
    let threads = graphlink::recompute_threads(&store, &ctx, 0.20, 40).expect("threads");
    assert!(threads.threads >= 1, "at least one thread");

    let once = tables(&store);
    let link_rows = |snapshot: &Vec<(&'static str, Vec<String>)>| -> Vec<String> {
        snapshot
            .iter()
            .find(|(name, _)| *name == "article_links")
            .map(|(_, rows)| rows.clone())
            .unwrap_or_default()
    };
    let thread_rows = |snapshot: &Vec<(&'static str, Vec<String>)>| -> Vec<String> {
        snapshot
            .iter()
            .find(|(name, _)| *name == "article_threads")
            .map(|(_, rows)| rows.clone())
            .unwrap_or_default()
    };
    assert!(
        link_rows(&once).iter().any(|row| row.contains("shared: ")),
        "a link explains itself with a shared name: {:?}",
        link_rows(&once)
    );
    assert!(
        thread_rows(&once).len() >= 2,
        "a thread needs at least two articles"
    );

    // second pass over the same data
    graphdb::recompute_edges(&store, &mut caches, &ctx, 30).expect("edges again");
    graphlink::recompute_links(&store, &mut caches, &ctx, 14, 8, 60_000).expect("links again");
    graphlink::recompute_threads(&store, &ctx, 0.20, 40).expect("threads again");
    let twice = tables(&store);
    assert_eq!(once, twice, "a second recompute changes nothing");

    // and a third, from a cold cache, with the same answer
    let mut cold = Caches::new();
    graphdb::recompute_edges(&store, &mut cold, &ctx, 30).expect("edges cold");
    graphlink::recompute_links(&store, &mut cold, &ctx, 14, 8, 60_000).expect("links cold");
    graphlink::recompute_threads(&store, &ctx, 0.20, 40).expect("threads cold");
    assert_eq!(tables(&store), once, "a cold cache rebuilds the same graph");
    drop(store);
    let _ = path;
}

// ---------------------------------------------------------------- the window

/// The boundary fixture the two window tests share: three articles, each with its
/// own pair of entities, so an edge either exists or it does not.
///
/// * id 1 — 71 hours old: inside a 72-hour window;
/// * id 2 — 90 hours old: outside it, on any comparison;
/// * id 3 — one second before the cutoff: the *same calendar day* as the cutoff
///   whenever the cutoff is not exactly midnight, and strictly earlier than the
///   instant the window names. That is the row the window used to let in.
fn seed_window_fixture(store: &Store, now: i64) {
    let cutoff = atlas_store::parse_epoch(&atlas_store::iso_ago(72.0));
    insert_article(store, 1, "Inside the window", "", "", &ago(now, 71));
    insert_article(store, 2, "Past the window", "", "", &ago(now, 90));
    insert_article(
        store,
        3,
        "The cutoff's own day",
        "",
        "",
        &atlas_store::iso_from_epoch(cutoff - 1),
    );

    insert_entity(store, 1, "Nvidia", "ORG", 2);
    insert_entity(store, 2, "Bundesbank", "ORG", 2);
    insert_entity(store, 3, "Jensen Huang", "PERSON", 2);
    insert_entity(store, 4, "Aurora Systems", "ORG", 2);
    insert_entity(store, 5, "Semiconductor", "MISC", 2);
    insert_entity(store, 6, "Granite Harbor", "ORG", 2);
    link(store, 1, 1);
    link(store, 1, 2);
    link(store, 2, 3);
    link(store, 2, 4);
    link(store, 3, 5);
    link(store, 3, 6);
}

/// The window is the *instant* it names, not the calendar day the cutoff falls on.
///
/// It used to be bound as `datetime('now', ?)`, which SQLite answers as
/// `2026-11-05 10:00:00` — a different shape from the `published` column's
/// `2026-11-05T10:00:00Z`. `T` sorts above the space, so the hour on the cutoff's
/// own day was ignored and every story published that day counted as inside: up to
/// a day wider than the three days asked for here. Without the fix this test sees
/// two edges (the 71-hour pair and the cutoff-day pair) instead of one.
#[test]
fn the_edge_window_is_the_instant_it_asks_for_and_not_the_whole_day() {
    let (_path, store) = db("edge-window");
    let now = now_epoch();
    seed_window_fixture(&store, now);
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);

    // three days = the 72-hour window this crate rebuilds edges for
    let done = graphdb::recompute_edges(&store, &mut caches, &ctx, 3).expect("edges");
    let pairs: Vec<(i64, i64)> = store
        .query("SELECT a,b FROM edges ORDER BY a,b", &[])
        .expect("edges")
        .iter()
        .map(|row| (row.int("a").unwrap_or(0), row.int("b").unwrap_or(0)))
        .collect();

    assert_eq!(
        pairs,
        vec![(1, 2)],
        "only the 71-hour story is inside the window; the 90-hour one and the one a \
         second before the cutoff are not ({} edges reported)",
        done.edges
    );
}

/// The same boundary, on the read side: `top_entities(hours)` counts hours.
///
/// `hours` was bound as `-72 hours` for `datetime('now', ?)`, so the entities of
/// the cutoff's own calendar day came back as "recent" — the four names below
/// instead of two before the fix.
#[test]
fn the_recent_entities_window_counts_hours_and_not_calendar_days() {
    let (_path, store) = db("top-entities-window");
    let now = now_epoch();
    seed_window_fixture(&store, now);

    let names: Vec<String> = graphread::top_entities(&store, 72, 20)
        .expect("recent entities")
        .iter()
        .map(|row| row.text("name").unwrap_or("").to_string())
        .collect();

    assert_eq!(
        names,
        vec!["Nvidia".to_string(), "Bundesbank".to_string()],
        "the last 72 hours are the last 72 hours, whatever day they started on"
    );
}

#[test]
fn threads_are_stable_and_numbered_in_order() {
    let (_path, store) = db("threads-stable");
    let now = now_epoch();
    seed_recompute_fixture(&store, now);
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    graphdb::recompute_edges(&store, &mut caches, &ctx, 30).expect("edges");
    graphlink::recompute_links(&store, &mut caches, &ctx, 14, 8, 60_000).expect("links");

    let first = graphlink::recompute_threads(&store, &ctx, 0.20, 40).expect("threads");
    let rows = store
        .query("SELECT id,title,size FROM threads ORDER BY id", &[])
        .expect("threads");
    assert_eq!(rows.len(), first.threads);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            row.int("id"),
            Some(index as i64 + 1),
            "thread ids are 1..n in creation order"
        );
        assert!(!row.text("title").unwrap_or("").is_empty());
    }
    let ids: Vec<i64> = store
        .query("SELECT article_id,thread_id FROM article_threads ORDER BY article_id", &[])
        .expect("members")
        .iter()
        .map(|row| row.int("thread_id").unwrap_or(0))
        .collect();

    let second = graphlink::recompute_threads(&store, &ctx, 0.20, 40).expect("threads again");
    assert_eq!(first, second);
    let again: Vec<i64> = store
        .query("SELECT article_id,thread_id FROM article_threads ORDER BY article_id", &[])
        .expect("members")
        .iter()
        .map(|row| row.int("thread_id").unwrap_or(0))
        .collect();
    assert_eq!(ids, again, "a rebuild assigns the same thread ids");
}

#[test]
fn an_entity_deleted_while_linked_is_ignored_by_the_next_rebuild() {
    let (_path, store) = db("deleted-entity");
    let now = now_epoch();
    seed_recompute_fixture(&store, now);
    let mut caches = Caches::new();
    let ctx = Context::from_json(&Json::Null);
    let before = graphdb::recompute_edges(&store, &mut caches, &ctx, 30).expect("edges");
    assert!(before.edges > 0);
    assert!(count(&store, "SELECT COUNT(*) FROM edges WHERE a=3 OR b=3") > 0);

    // the row disappears under the graph's feet, the way a prune of old articles
    // or a manual cleanup can leave it
    store
        .exec("DELETE FROM entities WHERE id=3", &[])
        .expect("delete");
    assert!(
        count(
            &store,
            "SELECT COUNT(*) FROM article_entities WHERE entity_id=3"
        ) > 0,
        "the link rows are still there"
    );

    let after = graphdb::recompute_edges(&store, &mut caches, &ctx, 30).expect("edges again");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM edges WHERE a=3 OR b=3"),
        0,
        "a rebuild joins `entities`, so no edge points at the gone row"
    );
    assert!(
        after.edges < before.edges,
        "the graph shrinks: {} -> {}",
        before.edges,
        after.edges
    );
    // `server/graph.py` leaves the `article_entities` rows of a deleted entity
    // behind too — every reader joins `entities`, so they are invisible — and the
    // port does the same rather than inventing a cleanup rule of its own.
    assert!(
        count(
            &store,
            "SELECT COUNT(*) FROM article_entities WHERE entity_id=3"
        ) > 0,
        "orphan links stay, exactly as they do in Python"
    );
    // and the next rebuild is stable again
    let once = tables(&store);
    graphdb::recompute_edges(&store, &mut caches, &ctx, 30).expect("edges a third time");
    assert_eq!(tables(&store), once, "and the next rebuild is stable again");
}

// ---------------------------------------------------------------- read side

#[test]
fn a_subgraph_walks_a_cycle_without_looping() {
    let (_path, store) = db("subgraph-cycle");
    insert_entity(&store, 1, "Alpha", "ORG", 5);
    insert_entity(&store, 2, "Beta", "ORG", 4);
    insert_entity(&store, 3, "Gamma", "ORG", 3);
    for (a, b, weight) in [(1, 2, 3.0), (2, 3, 2.0), (1, 3, 1.0)] {
        store
            .exec(
                "INSERT INTO edges(a,b,weight,type) VALUES(?,?,?,'cooccur')",
                &[int(a), int(b), SqlValue::Real(weight)],
            )
            .expect("edge");
    }

    let answer = graphread::bfs_subgraph(&store, 1, 2, 150).expect("subgraph");
    let root = json::parse(&answer).expect("json");
    let nodes = root.get("nodes").and_then(|v| v.as_arr()).expect("nodes");
    assert_eq!(nodes.len(), 3, "the triangle is complete: {}", answer);
    let edges = root.get("edges").and_then(|v| v.as_arr()).expect("edges");
    // Level 1 finds (1,2) and (1,3); level 2 asks again from {2,3} and sees all
    // three sides. `bfs_subgraph()` does not de-duplicate across levels and
    // neither does this port — the UI draws each edge once itself.
    assert_eq!(edges.len(), 5, "levels 1 and 2, duplicates included: {}", answer);
}

#[test]
fn a_subgraph_skips_page_chrome_but_keeps_the_centre() {
    let (_path, store) = db("subgraph-junk");
    insert_entity(&store, 1, "Nvidia", "ORG", 5);
    insert_entity(&store, 2, "Brasil.", "MISC", 4);
    store
        .exec("INSERT INTO edges(a,b,weight,type) VALUES(1,2,1,'cooccur')", &[])
        .expect("edge");

    let answer = graphread::bfs_subgraph(&store, 1, 1, 150).expect("subgraph");
    let root = json::parse(&answer).expect("json");
    let nodes = root.get("nodes").and_then(|v| v.as_arr()).expect("nodes");
    assert_eq!(nodes.len(), 1, "the junk neighbour is not drawn: {}", answer);
    let edges = root.get("edges").and_then(|v| v.as_arr()).expect("edges");
    assert!(edges.is_empty(), "an edge to a dropped node is dropped too");
}

#[test]
fn a_path_between_disconnected_nodes_is_not_found() {
    let (_path, store) = db("path-none");
    insert_entity(&store, 1, "Alpha", "ORG", 5);
    insert_entity(&store, 2, "Beta", "ORG", 4);
    let found = graphread::graph_path(&store, "Alpha", "Beta").expect("path");
    assert!(!found.found);
    assert!(found.path.is_empty());
}

#[test]
fn a_path_between_connected_nodes_is_shortest() {
    let (_path, store) = db("path-hops");
    insert_entity(&store, 1, "Alpha", "ORG", 5);
    insert_entity(&store, 2, "Beta", "ORG", 4);
    insert_entity(&store, 3, "Gamma", "ORG", 3);
    for (a, b) in [(1, 2), (2, 3)] {
        store
            .exec(
                "INSERT INTO edges(a,b,weight,type) VALUES(?,?,1,'cooccur')",
                &[int(a), int(b)],
            )
            .expect("edge");
    }
    let found = graphread::graph_path(&store, "Alpha", "Gamma").expect("path");
    assert!(found.found);
    assert_eq!(found.path, vec!["Alpha", "Beta", "Gamma"]);

    let same = graphread::graph_path(&store, "Alpha", "Alpha").expect("path");
    assert!(same.found);
    assert_eq!(same.path, vec!["Alpha"], "a node is its own path");
}

#[test]
fn a_profile_lists_the_neighbours_the_ui_opens() {
    let (_path, store) = db("profile");
    insert_entity(&store, 1, "Nvidia", "ORG", 5);
    insert_entity(&store, 2, "SEC", "ORG", 4);
    insert_article(&store, 1, "Nvidia faces the SEC", "", "", &ago(now_epoch(), 1));
    link(&store, 1, 1);
    link(&store, 1, 2);
    store
        .exec(
            "INSERT INTO edges(a,b,weight,type,label) VALUES(1,2,2,'cooccur','Regulators')",
            &[],
        )
        .expect("edge");
    store
        .exec(
            "INSERT INTO entity_alias(alias,entity_id) VALUES('NVIDIA Corp',1)",
            &[],
        )
        .expect("alias");

    let body = graphread::entity_profile(&store, "NVIDIA Corp")
        .expect("profile")
        .expect("found by alias");
    let root = json::parse(&body).expect("json");
    let neighbours = root.get("neighbors").and_then(|v| v.as_arr()).expect("neighbors");
    assert_eq!(neighbours.len(), 1);
    assert_eq!(neighbours[0].get("id").and_then(|v| v.as_f64()), Some(2.0));
    assert_eq!(
        neighbours[0].get("name").and_then(|v| v.as_str()),
        Some("SEC")
    );
    let articles = root.get("articles").and_then(|v| v.as_arr()).expect("articles");
    assert_eq!(articles.len(), 1);
    let aliases = root.get("aliases").and_then(|v| v.as_arr()).expect("aliases");
    assert_eq!(aliases.len(), 1);

    let missing = graphread::entity_profile(&store, "Nobody At All").expect("profile");
    assert!(missing.is_none(), "an unknown name is not found");
}

// ---------------------------------------------------------------- the CLI path

#[test]
fn the_json_cli_answers_every_subcommand_it_advertises() {
    let (path, store) = db("cli");
    let now = now_epoch();
    seed_recompute_fixture(&store, now);
    drop(store);
    let db_path = path.to_string_lossy().to_string();
    for command in crate::SUBCOMMANDS {
        let payload = match *command {
            "link_article" => format!(r#"{{"db":"{}","id":1,"entities":[["Nvidia","ORG"]]}}"#, db_path),
            "recompute" => format!(r#"{{"db":"{}"}}"#, db_path),
            "normalise" => format!(r#"{{"db":"{}","dry_run":true}}"#, db_path),
            "subgraph" => format!(r#"{{"db":"{}","entity_id":1}}"#, db_path),
            "path" => format!(r#"{{"db":"{}","from":"Nvidia","to":"SEC"}}"#, db_path),
            "links_for" => format!(r#"{{"db":"{}","id":1}}"#, db_path),
            "profile" => format!(r#"{{"db":"{}","name":"Nvidia"}}"#, db_path),
            "top_entities" => format!(r#"{{"db":"{}"}}"#, db_path),
            "edges" => r#"{"articles":[]}"#.to_string(),
            "why" => r#"{"a":1,"b":2,"articles":[]}"#.to_string(),
            "threads" => r#"{"links":[],"articles":[]}"#.to_string(),
            "neighbours" => r#"{"id":1,"edges":[],"entities":[]}"#.to_string(),
            other => panic!("no payload for {}", other),
        };
        let answer = crate::run(command, &payload)
            .unwrap_or_else(|error| panic!("{} failed: {}", command, error));
        assert!(
            answer.trim_start().starts_with('{'),
            "{} must answer one JSON object: {}",
            command,
            answer
        );
        json::parse(&answer).unwrap_or_else(|error| panic!("{} -> bad JSON: {}", command, error));
    }
}

#[test]
fn a_missing_database_path_is_an_error_not_a_guess() {
    let error = crate::run("recompute", r#"{"days":30}"#).expect_err("no db");
    assert!(error.contains("'db'"), "{}", error);
}

// ---------------------------------------------------------------- the proof

/// The project root (`rust/atlas-graph` → `rust` → the checkout).
fn project_root() -> PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|rust| rust.parent())
        .expect("project root")
        .to_path_buf()
}

/// Run one subcommand of this crate and return the store it wrote, reopened.
fn run_and_reopen(db_path: &std::path::Path, payload: &str, command: &str) -> Store {
    let answer = crate::run(command, payload)
        .unwrap_or_else(|error| panic!("{} failed: {}", command, error));
    assert!(answer.contains("\"source\":\"rust\""), "{}", answer);
    Store::open(db_path).expect("reopen")
}

/// A wider fixture: 40 articles and 120 entities drawn from a fixed pool, with
/// overlapping names and overlapping wording, so both sides have real pair work
/// to do — several hundred edge candidates, dozens of accepted links and more
/// than one thread.
///
/// Each headline carries exactly two in-range words and each summary one, so a pair
/// of stories shares at most three words. That matters for the proof: with more
/// candidates than slots, `shared_words()` would have to break a tie, and Python
/// breaks it with `set` iteration order (string hashes) — see [`normalise_row`].
fn seed_wide_fixture(store: &Store, now: i64) {
    const WORDS: &[&str] = &[
        "orbital", "turbine", "harbor", "granite", "meadow", "lantern", "pixel", "copper",
        "valley", "beacon", "timber", "violet", "saddle", "marble", "fossil", "cobalt",
        "cinder", "willow", "quartz", "ember",
    ];
    const FIRST: &[&str] = &["Summit", "Harbor", "Larkspur", "Northwind", "Cobalt", "Granite"];
    const SECOND: &[&str] = &["Partners", "Traders", "Bridge", "Works", "Systems", "Holdings"];

    for n in 0..120i64 {
        let name = format!(
            "{} {} {}",
            FIRST[(n as usize) % FIRST.len()],
            SECOND[(n as usize * 5) % SECOND.len()],
            n + 1
        );
        let kind = match n % 5 {
            0 | 1 => "ORG",
            2 => "PERSON",
            3 => "MISC",
            _ => "ORG",
        };
        insert_entity(store, n + 1, &name, kind, 1 + n % 7);
    }
    insert_entity(store, 121, "Brazil", "LOC", 9);
    insert_entity(store, 122, "Germany", "LOC", 8);

    for a in 0..40i64 {
        let w = |seed: i64| WORDS[((a * seed + seed) as usize) % WORDS.len()];
        // two in-range words in the headline, one in the summary, and a body whose
        // vocabulary is used by every story (so it is out of range and adds
        // nothing to the wording vectors)
        let title = format!("{} {} talks resume", w(3), w(7));
        let summary = format!("{} was named.", w(11));
        let content = "Markets moved again and again. Markets moved.";
        insert_article(store, a + 1, &title, &summary, content, &ago(now, a % 30 + 1));
        for step in 0..8i64 {
            let entity = ((a * 7 + step * 13) % 120) + 1;
            link(store, a + 1, entity);
        }
        if a % 6 == 0 {
            link(store, a + 1, 121);
        }
        if a % 7 == 0 {
            link(store, a + 1, 122);
        }
    }
    store
        .exec("INSERT INTO topics(id,name) VALUES(1,'Markets'),(2,'Energy')", &[])
        .expect("topics");
    for a in 1..=20i64 {
        store
            .exec(
                "INSERT INTO article_topics(article_id,topic_id) VALUES(?,1)",
                &[int(a)],
            )
            .expect("article_topics");
    }
    for a in 21..=30i64 {
        store
            .exec(
                "INSERT INTO article_topics(article_id,topic_id) VALUES(?,2)",
                &[int(a)],
            )
            .expect("article_topics");
    }
}

// ---------------------------------------------------------------- the cosine

/// `cosine::compute` is a port of `atlas-core similar`, which ships as a binary
/// subcommand only. This compares the two answers on the same vectors.
#[test]
fn the_cosine_port_answers_exactly_like_atlas_core_similar() {
    let binary = atlas_platform::host::app_binary(
        project_root().join("rust").join("target").join("release"),
        "atlas-core",
    );
    if !binary.is_file() {
        eprintln!(
            "skipped: {} is not built (cargo build --release -p atlas-core)",
            binary.display()
        );
        return;
    }
    // four documents, two of them close, one that shares nothing
    let docs = vec![
        (
            11i64,
            vec![
                ("chip".to_string(), 2.0),
                ("memory".to_string(), 1.0),
                ("price".to_string(), 3.0),
            ],
        ),
        (
            12i64,
            vec![
                ("chip".to_string(), 2.0),
                ("memory".to_string(), 1.0),
                ("demand".to_string(), 1.0),
            ],
        ),
        (
            13i64,
            vec![
                ("price".to_string(), 3.0),
                ("demand".to_string(), 1.0),
                ("chip".to_string(), 0.5),
            ],
        ),
        (
            14i64,
            vec![("weather".to_string(), 4.0), ("rain".to_string(), 2.0)],
        ),
    ];
    let mut payload = String::from("{\"docs\":[");
    for (index, (id, tokens)) in docs.iter().enumerate() {
        if index > 0 {
            payload.push(',');
        }
        payload.push_str(&format!("{{\"id\":{},\"tokens\":{{", id));
        for (n, (token, weight)) in tokens.iter().enumerate() {
            if n > 0 {
                payload.push(',');
            }
            payload.push_str(&format!("{}:{}", json::to_string(&Json::Str(token.clone())), weight));
        }
        payload.push_str("}}");
    }
    payload.push_str("],\"top_per_doc\":12,\"min_score\":0.06}");

    let output = std::process::Command::new(&binary)
        .arg("similar")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(payload.as_bytes())?;
            child.wait_with_output()
        })
        .expect("atlas-core similar");
    assert!(
        output.status.success(),
        "atlas-core similar failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = json::parse(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let mut from_core: Vec<String> = parsed
        .get("pairs")
        .and_then(|v| v.as_arr())
        .expect("pairs")
        .iter()
        .map(|pair| {
            format!(
                "{}-{}-{:.4}",
                pair.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64,
                pair.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64,
                pair.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0)
            )
        })
        .collect();
    from_core.sort();

    let mut port: Vec<String> = graphlink::cosine::compute(&docs, 12, 0.06)
        .into_iter()
        .map(|(a, b, score)| format!("{}-{}-{:.4}", a, b, score))
        .collect();
    port.sort();
    assert_eq!(from_core, port, "the port and the binary disagree");
    assert!(!from_core.is_empty(), "the fixture must produce pairs");
}

// ---------------------------------------------------------------- vocabulary

/// The word sets `ok_name()` asks about are **grammar**, and this test is what keeps a
/// vocabulary of *things* from creeping back in.
///
/// Four lists used to sit here: `JUNK_L` (1209 words), `JUNK_BASE` (83), `UI_JUNK` (77) and
/// `OUTLETS` (69 publishers). They decided which capitalised run could become an entity —
/// the one decision this app reserves for the model — and the archive could not show that
/// a list had made it: 0 of the 4539 stored names are in any of them, while 99 of the 1435
/// articles carry, in the `publisher` column, one of the 69 outlet names (13 publishers) and
/// 42 name one in the title or summary — and none of those could be an entity at all. The
/// four are gone; the numbers below are the grammar that stayed, and the last block is the
/// behaviour that replaced them.
#[test]
fn the_name_vocabulary_is_grammar_and_nothing_else() {
    let lex = crate::Lex::new();
    assert_eq!(lex.stop.len(), 481, "_STOP_L (function words)");
    assert_eq!(lex.text_stop.len(), 574, "TEXT_STOP (headline words)");
    assert_eq!(lex.common.len(), 446, "_COMMON (everyday words)");
    assert_eq!(lex.block.len(), 373, "_BLOCK_L (verbs and filler)");
    assert_eq!(lex.days_months.len(), 103, "_DAYS_MONTHS");
    assert_eq!(lex.edge_grammar.len(), 942, "the grammar union a name may not start or end with");
    assert_eq!(lex.prose.len(), 446, "_PROSE_L (COMMON alone now)");
    assert_eq!(lex.connectors.len(), 24, "CONNECTORS");
    assert_eq!(lex.determiners.len(), 22, "_DETERMINERS");
    assert_eq!(lex.conjunctions.len(), 4, "_CONJUNCTIONS");
}

/// What the gate does now, on the shapes the four deleted lists were pretending to handle.
///
/// A publisher is a thing a story names, so it is a name; "Read more" is furniture because
/// of grammar (`more` is a function word) and, where grammar is not enough, because the
/// article writes the word in lower case (`atlas-ingest`'s `admit`, which has the text).
#[test]
fn a_publisher_is_a_name_and_page_furniture_is_refused_by_shape() {
    let lex = crate::Lex::new();
    for name in ["Reuters", "Bloomberg", "Nvidia", "Jensen Huang", "European Commission"] {
        assert!(
            lex.ok_name(name),
            "{name} was refused — a name may not be refused for the words a build happens to \
             know, which is what OUTLETS and JUNK_L were doing"
        );
    }
    for refused in [
        "Read More",   // a function word at the end
        "Tuesday",     // a calendar word
        "SEC",         // all capitals, four characters or fewer
        "the",         // a function word on its own
        "Nvidia For",  // a name never ends on a preposition
        "Nvidia's",    // a possessive tail is prose
        "Nvidia the SEC", // a determiner inside a run is prose
    ] {
        assert!(
            !lex.ok_name(refused),
            "{refused} was admitted — the structural rules are what is left of the name gate, \
             and they are the half that does not need a dictionary"
        );
    }
}

#[test]
fn canonical_names_fold_the_way_the_python_does() {
    for (raw, want) in [
        ("Nvidia", "Nvidia"),
        ("Nvidia Inc", "Nvidia"),
        ("Nvidia, Inc.", "Nvidia"),
        ("Nvidia Corp. NVDA", "Nvidia"),
        ("Nvidia's", "Nvidia"),
        ("Nvidia’s", "Nvidia"),
        ("  Nvidia  ", "Nvidia"),
        ("Nvidia | Reuters", "Nvidia | Reuters"),
        ("ACME L.L.C.", "ACME"),
        ("Total S.A.", "Total"),
        // an en dash becomes a hyphen and then stops the corporate suffix from
        // matching — `server/graph.py` answers `'Nvidia -'` here too
        ("Nvidia – Inc", "Nvidia -"),
        ("", ""),
    ] {
        assert_eq!(crate::canonical_name(raw), want, "canonical_name({:?})", raw);
    }
    assert_eq!(crate::merge_key("NVIDIA Inc"), "nvidia");
}

// ---------------------------------------------------------------- the clock

/// 400 articles and 4 000 entities, the size the task names.
///
/// The shape is the archive's: every article carries a dozen entities, a band of
/// hubs is shared by a few stories each, and a handful of places add context. The
/// entities are named so they survive `_ok_name()`, and the wording repeats enough
/// for the text vectors to have something to compare.
fn seed_perf_fixture(store: &Store, now: i64) {
    const WORDS: &[&str] = &[
        "orbital", "turbine", "harbor", "granite", "meadow", "lantern", "pixel", "copper",
        "valley", "beacon", "timber", "violet", "saddle", "marble", "fossil", "cobalt",
        "cinder", "willow", "quartz", "ember", "juniper", "hollow", "thicket", "prairie",
    ];
    const FIRST: &[&str] = &["Summit", "Harbor", "Larkspur", "Northwind", "Cobalt", "Granite"];
    const SECOND: &[&str] = &["Partners", "Traders", "Bridge", "Works", "Systems", "Holdings"];

    store
        .in_transaction(|tx| {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO entities(id,name,type,count,first_seen,last_seen) \
                     VALUES(?,?,?,?,'2026-11-01T00:00:00Z','2026-11-01T00:00:00Z')",
                )
                .map_err(atlas_store::StoreError::from)?;
            for n in 0..4_000i64 {
                let name = format!(
                    "{} {} {}",
                    FIRST[(n as usize) % FIRST.len()],
                    SECOND[(n as usize * 5) % SECOND.len()],
                    n + 1
                );
                let kind = match n % 9 {
                    0 | 1 | 2 => "ORG",
                    3 | 4 => "PERSON",
                    5 => "MISC",
                    6 | 7 => "ORG",
                    _ => "PERSON",
                };
                let values = [int(n + 1), text(&name), text(kind), int(1 + n % 5)];
                stmt.execute(rusqlite::params_from_iter(values.iter()))
                    .map_err(atlas_store::StoreError::from)?;
            }
            Ok(())
        })
        .expect("entities");

    for a in 0..400i64 {
        let w = |seed: i64| WORDS[((a * seed + seed) as usize) % WORDS.len()];
        let title = format!("{} {} talks resume", w(3), w(7));
        let summary = format!("{} was named.", w(11));
        let content = format!("{} kept rising while {} fell. {} again.", w(13), w(17), w(19));
        insert_article(store, a + 1, &title, &summary, &content, &ago(now, a % 60 + 1));
        for step in 0..10i64 {
            // 4 000 entities over 400 articles × 10 mentions: the long tail, with a
            // band of hubs each carried by a few stories
            let entity = if step < 3 {
                1 + ((a * 3 + step * 7) % 400)
            } else {
                401 + ((a * 13 + step * 211) % 3_600)
            };
            link(store, a + 1, entity);
        }
        if a % 9 == 0 {
            link(store, a + 1, 3_999);
        }
    }
}

/// The number the report has to name, as a test so it cannot rot.
///
/// `cargo test --release -p atlas-graph` measures the release profile, which is
/// what the app runs. A debug build is ~20× slower and gets its own ceiling, so the
/// suite stays useful without lying about the number.
#[test]
fn recompute_of_400_articles_and_4000_entities_stays_under_the_budget() {
    let now = now_epoch();
    let (path, store) = db("perf");
    seed_perf_fixture(&store, now);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM articles"), 400);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM entities"), 4_000);
    drop(store);

    let payload = format!(
        r#"{{"db":{},"days":30,"link_days":14}}"#,
        json::to_string(&Json::Str(path.to_string_lossy().to_string()))
    );
    // one warm pass (page cache, connection open), then the measured one
    {
        let store = Store::open(&path).expect("open");
        let mut caches = Caches::new();
        let ctx = Context::from_json(&Json::Null);
        let mark = std::time::Instant::now();
        graphdb::recompute_edges(&store, &mut caches, &ctx, 30).expect("warm edges");
        let edges_ms = mark.elapsed().as_secs_f64() * 1000.0;
        let mark = std::time::Instant::now();
        graphlink::recompute_links(&store, &mut caches, &ctx, 14, 8, 60_000).expect("warm links");
        let links_ms = mark.elapsed().as_secs_f64() * 1000.0;
        let mark = std::time::Instant::now();
        graphlink::recompute_threads(&store, &ctx, 0.20, 40).expect("warm threads");
        let threads_ms = mark.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "  phase split [{}]: edges {:.1} ms, links {:.1} ms, threads {:.1} ms",
            if cfg!(debug_assertions) { "debug" } else { "release" },
            edges_ms,
            links_ms,
            threads_ms
        );
    }

    let started = std::time::Instant::now();
    let answer = crate::run("recompute", &payload).expect("recompute");
    let ms = started.elapsed().as_secs_f64() * 1000.0;

    let parsed = json::parse(&answer).expect("json");
    let edges = parsed.get("edges").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let links = parsed.get("links").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let threads = parsed.get("threads").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(edges > 1_000.0, "the fixture must produce a real edge set");
    assert!(links > 100.0, "and a real link set");
    assert!(threads >= 1.0, "and at least one thread");

    let ceiling = if cfg!(debug_assertions) {
        4_000.0
    } else {
        150.0
    };
    eprintln!(
        "recompute(400 articles, 4 000 entities): {:.1} ms [{}] -> {} edges, {} links, \
         {} threads",
        ms,
        if cfg!(debug_assertions) { "debug" } else { "release" },
        edges,
        links,
        threads
    );
    assert!(
        ms < ceiling,
        "the whole recompute took {:.1} ms, over the {:.0} ms budget for this profile",
        ms,
        ceiling
    );
}


// ------------------------------------------------- the model's list, end to end

/// An article whose `iso` and `place` are known, so the country rows have an own place.
fn insert_article_placed(store: &Store, id: i64, title: &str, iso: &str, place: &str) {
    insert_article(store, id, title, "", "", &ago(now_epoch(), 1));
    store
        .exec(
            "UPDATE articles SET iso=?, place=? WHERE id=?",
            &[text(iso), text(place), int(id)],
        )
        .expect("place the article");
}

/// The gazetteer the caller hands in, with each place's own answer: a city resolves to
/// the country it is in, which is what makes Berlin a row of its own next to Ukraine.
fn gazetteer() -> Context {
    let payload = json::parse(
        r#"{"places":{"Berlin":"DEU","Kyiv":"UKR","Moscow":"RUS","Ukraine":"UKR",
                       "Russia":"RUS","Europe":""}}"#,
    )
    .expect("a payload");
    Context::from_json(&payload)
}

/// One row per place the entity list names, and exactly one of them primary.
///
/// The article is filed under Kyiv (`iso=UKR`, `place=Ukraine`) and names Berlin, Kyiv,
/// Moscow, Ukraine, Russia and Europe: three countries follow from it (DEU, UKR, RUS),
/// Ukraine is the primary because it is the article's own place, and Europe — a
/// continent the gazetteer has no ISO for — writes nothing.
#[test]
fn link_article_writes_a_country_row_for_every_place_the_model_names() {
    let (_path, store) = db("link-countries");
    insert_article_placed(&store, 1, "Berlin Summit: Europe ready to talk", "UKR", "Ukraine");
    let ents: Vec<(String, String)> = [
        ("Berlin", "LOC"),
        ("Kyiv", "LOC"),
        ("Moscow", "LOC"),
        ("Europe", "ORG"),
        ("Russia", "LOC"),
        ("Ukraine", "LOC"),
    ]
    .iter()
    .map(|(name, kind)| (name.to_string(), kind.to_string()))
    .collect();

    let mut caches = Caches::new();
    let ctx = gazetteer();
    graphdb::link_article(&store, &mut caches, &ctx, 1, &ents).expect("link");

    let rows: Vec<(String, i64)> = store
        .query("SELECT iso,is_primary FROM article_countries ORDER BY iso", &[])
        .expect("countries")
        .iter()
        .map(|row| {
            (
                row.text("iso").unwrap_or("").to_string(),
                row.int("is_primary").unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("DEU".to_string(), 0),
            ("RUS".to_string(), 0),
            ("UKR".to_string(), 1),
        ],
        "Berlin's Germany, Moscow's and Russia's Russia, and the article's own Ukraine"
    );
    assert_eq!(
        rows.iter().filter(|(_, primary)| *primary == 1).count(),
        1,
        "exactly one primary: two would make the map's chip and the filter disagree"
    );

    // A re-link of the same list writes the same three rows, same primary, no duplicates.
    graphdb::link_article(&store, &mut caches, &ctx, 1, &ents).expect("relink");
    let after: Vec<(String, i64)> = store
        .query("SELECT iso,is_primary FROM article_countries ORDER BY iso", &[])
        .expect("countries")
        .iter()
        .map(|row| {
            (
                row.text("iso").unwrap_or("").to_string(),
                row.int("is_primary").unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(after, rows, "a re-link is the same rows, not more of them");
}

/// An article with no own place still has exactly one primary: the first place it names.
#[test]
fn a_named_place_is_primary_when_the_article_has_no_place_of_its_own() {
    let (_path, store) = db("link-countries-no-own");
    insert_article(&store, 1, "Two countries talk", "", "", &ago(now_epoch(), 1));
    store
        .exec("UPDATE articles SET iso='' WHERE id=1", &[])
        .expect("no own place");
    let ctx = Context::from_json(
        &json::parse(r#"{"places":{"Ukraine":"UKR","Russia":"RUS"}}"#).expect("payload"),
    );
    let ents: Vec<(String, String)> = ["Ukraine", "Russia"]
        .iter()
        .map(|name| (name.to_string(), "LOC".to_string()))
        .collect();
    graphdb::link_article(&store, &mut caches(), &ctx, 1, &ents).expect("link");

    let rows: Vec<(String, i64)> = store
        .query("SELECT iso,is_primary FROM article_countries ORDER BY iso", &[])
        .expect("countries")
        .iter()
        .map(|row| {
            (
                row.text("iso").unwrap_or("").to_string(),
                row.int("is_primary").unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![("RUS".to_string(), 0), ("UKR".to_string(), 1)],
        "the first place named is the primary when nothing else says which it is"
    );
}

/// A guess never outranks an answer, and an answer does outrank a guess.
///
/// Measured on the live archive: the extractor created `European Union` as `PERSON`, the
/// model then saved a story calling it an `Institution`, and the graph kept the guess — so
/// the reader's graph showed an institution as a person. The payload says whose types these
/// are (`types: "model"`), and only then may a stored type be replaced.
#[test]
fn the_models_type_replaces_a_guess_and_a_guess_never_replaces_a_type() {
    let (_path, store) = db("link-authoritative-types");
    insert_article(&store, 1, "First story", "", "", &ago(now_epoch(), 1));
    insert_article(&store, 2, "Second story", "", "", &ago(now_epoch(), 1));

    let guessed: Vec<(String, String)> =
        vec![("European Union".to_string(), "PERSON".to_string())];
    graphdb::link_article(&store, &mut caches(), &gazetteer(), 1, &guessed).expect("link");

    // A later *guess* at a different type does not overwrite the stored one.
    let other_guess: Vec<(String, String)> =
        vec![("European Union".to_string(), "MISC".to_string())];
    graphdb::link_article(&store, &mut caches(), &gazetteer(), 2, &other_guess).expect("link");
    let stored = |store: &atlas_store::Store| -> String {
        store
            .query_one("SELECT type FROM entities WHERE name='European Union'", &[])
            .expect("query")
            .and_then(|row| row.text("type").map(str::to_string))
            .unwrap_or_default()
    };
    assert_eq!(stored(&store), "PERSON", "a guess overwrote a stored type");

    // The model's own answer does: the payload says so, and a third story corrects it.
    insert_article(&store, 3, "Third story", "", "", &ago(now_epoch(), 1));
    let answered: Vec<(String, String)> =
        vec![("European Union".to_string(), "Institution".to_string())];
    let ctx = Context::from_json(&json::parse(r#"{"types":"model"}"#).expect("payload"));
    graphdb::link_article(&store, &mut caches(), &ctx, 3, &answered).expect("link");
    assert_eq!(
        stored(&store),
        "Institution",
        "the model called it an institution and the graph kept the heuristic"
    );
}

fn caches() -> Caches {
    Caches::new()
}

/// A gazetteer name the model typed as a person is a person, not a country: the type
/// decides, and the place table is not allowed to override it.
#[test]
fn a_person_named_like_a_country_is_not_a_country_row() {
    let (_path, store) = db("link-country-namesake");
    insert_article_placed(&store, 1, "Georgia signs", "USA", "Washington");
    let ctx = Context::from_json(
        &json::parse(r#"{"places":{"Georgia":"GEO","Washington":"USA"}}"#).expect("payload"),
    );
    let ents: Vec<(String, String)> = [("Georgia", "Person"), ("Washington", "LOC")]
        .iter()
        .map(|(name, kind)| (name.to_string(), kind.to_string()))
        .collect();
    graphdb::link_article(&store, &mut caches(), &ctx, 1, &ents).expect("link");

    let rows: Vec<(String, i64)> = store
        .query("SELECT iso,is_primary FROM article_countries ORDER BY iso", &[])
        .expect("countries")
        .iter()
        .map(|row| {
            (
                row.text("iso").unwrap_or("").to_string(),
                row.int("is_primary").unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![("USA".to_string(), 1)],
        "the person Georgia adds no country; Washington is the article's own place"
    );
}

/// The model's eight words, stored verbatim, and the edges those eight nodes make.
///
/// `Country` and `City` arrive already mapped to `LOC` by the caller; nothing in this
/// function maps, folds or infers a type — the column holds what the model said, and an
/// `Institution` stays one.
#[test]
fn the_model_s_eight_types_make_the_nodes_and_the_edges() {
    let (_path, store) = db("link-eight-types");
    insert_article(&store, 1, "Eight typed names", "", "", &ago(now_epoch(), 1));
    let ents: Vec<(String, String)> = [
        ("Olaf Scholz", "Person"),
        ("Bundesbank", "Organization"),
        ("European Commission", "Institution"),
        ("Ukraine", "LOC"),
        ("Berlin", "LOC"),
        ("Munich Security Conference", "Event"),
        ("Leopard 2", "Product"),
        ("Kestrel", "Other"),
    ]
    .iter()
    .map(|(name, kind)| (name.to_string(), kind.to_string()))
    .collect();

    let ctx = gazetteer();
    let done = graphdb::link_article(&store, &mut caches(), &ctx, 1, &ents).expect("link");
    assert_eq!(done.entities, 8, "every typed name is an entity: {:?}", done);

    let typed: Vec<(String, String)> = store
        .query("SELECT name,type FROM entities ORDER BY name", &[])
        .expect("entities")
        .iter()
        .map(|row| {
            (
                row.text("name").unwrap_or("").to_string(),
                row.text("type").unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        typed,
        vec![
            ("Berlin".to_string(), "LOC".to_string()),
            ("Bundesbank".to_string(), "Organization".to_string()),
            ("European Commission".to_string(), "Institution".to_string()),
            ("Kestrel".to_string(), "Other".to_string()),
            ("Leopard 2".to_string(), "Product".to_string()),
            ("Munich Security Conference".to_string(), "Event".to_string()),
            ("Olaf Scholz".to_string(), "Person".to_string()),
            ("Ukraine".to_string(), "LOC".to_string()),
        ],
        "the type column is the model's word, unchanged"
    );

    // The edges are those eight nodes and nothing else: every endpoint is one of them,
    // both ends have a name, and eight nodes make C(8,2) = 28 relations.
    let ids: Vec<i64> = store
        .query("SELECT id FROM entities", &[])
        .expect("ids")
        .iter()
        .filter_map(|row| row.int("id"))
        .collect();
    assert_eq!(done.pairs, 28, "C(8,2)");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM edges"), 28);
    let ends: Vec<(i64, i64)> = store
        .query("SELECT a,b FROM edges", &[])
        .expect("edges")
        .iter()
        .map(|row| (row.int("a").unwrap_or(0), row.int("b").unwrap_or(0)))
        .collect();
    for (a, b) in &ends {
        assert!(ids.contains(a) && ids.contains(b), "{a} -> {b}: not the model's list");
        assert_ne!(a, b, "a relation from an entity to itself");
    }
    let mut distinct = ends.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 28, "the same relation twice");
}

/// The structural gate: a four-token run that reads as a sentence is refused, so it makes
/// no node and no edge.
///
/// `To` inside the run is grammar, not a name — the same check that keeps "Nvidia in" out.
/// The refusal is the shape of the string, not a list of words to drop.
#[test]
fn a_four_token_run_that_reads_as_a_sentence_is_not_a_node() {
    let (_path, store) = db("link-sentence");
    insert_article(&store, 1, "a page of furniture", "", "", &ago(now_epoch(), 1));
    let ents: Vec<(String, String)> = [
        ("Berlin Summit To Talk", "Event"),
        ("History Suggests A 5000", "Other"),
        ("Nvidia Stock Is Down", "Product"),
        ("Olaf Scholz", "Person"),
    ]
    .iter()
    .map(|(name, kind)| (name.to_string(), kind.to_string()))
    .collect();
    let done = graphdb::link_article(&store, &mut caches(), &Context::default(), 1, &ents)
        .expect("link");

    assert_eq!(done.entities, 1, "only the person is a name: {:?}", done);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM entities"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM article_entities"), 1);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM edges"),
        0,
        "a refused name makes no node, so it can make no edge"
    );
    let name = store
        .query_one("SELECT name FROM entities", &[])
        .expect("row")
        .and_then(|row| row.text("name").map(|name| name.to_string()));
    assert_eq!(name.as_deref(), Some("Olaf Scholz"));
}

/// The boundary, measured rather than assumed: a four-token run of plain capitals with no
/// grammar word inside passes `_ok_name` — `Berlin Summit Europe Ready` is accepted.
///
/// The graph has no article text at `link_article`, so it cannot ask the one question that
/// separates that run from a real four-word name ("does the story use it twice?"). The
/// archive holds exactly this row (entity 6736, `MISC`), so this is a live boundary and
/// not a hypothetical: it is the caller's list — now the model's — that has to keep it out.
#[test]
fn a_four_token_run_of_plain_capitals_is_a_name_to_the_structural_gate() {
    let (_path, store) = db("link-run-boundary");
    insert_article(&store, 1, "Berlin Summit: Europe ready", "", "", &ago(now_epoch(), 1));
    let done = graphdb::link_article(
        &store,
        &mut caches(),
        &Context::default(),
        1,
        &[("Berlin Summit Europe Ready".to_string(), "Event".to_string())],
    )
    .expect("link");
    assert_eq!(
        done.entities, 1,
        "four capitalised tokens, no grammar word: the shape alone does not refuse it"
    );
}

/// The model's eight-name list cannot reach the pair cap: `keep_ids` keeps all eight, so
/// the extractor's fallback ceiling (14) and the archive's legacy 13–30-name lists are the
/// only lists the cap still trims.
#[test]
fn the_model_s_eight_names_never_reach_the_pair_cap() {
    let refs = |count: u32| -> Vec<crate::EntRef> {
        (1..=count)
            .map(|id| crate::EntRef {
                id,
                name: String::new(),
                kind: "Person".to_string(),
                count: 0.0,
            })
            .collect()
    };
    let keep = crate::keep_ids(&refs(8));
    assert_eq!(keep.len(), 8, "eight names, eight kept: the cap does not bind");
    // The fallback extractor's ceiling is 14 names (`entities::MAX_ENTITIES`), two past
    // the cap: those two, and only those two, are dropped.
    assert_eq!(crate::keep_ids(&refs(14)).len(), 12);
}
