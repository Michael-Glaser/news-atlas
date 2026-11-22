//! The relation layer's own tables — the Rust port of the DB-bound functions of
//! `server/graph.py`.
//!
//! | module | `server/graph.py` |
//! | --- | --- |
//! | this one | `link_article()`, `_merge_variants()`, `_purge_junk_entities()`, `recompute_edges()`, `_entity_index()`, `idf_map()` |
//! | [`graphlink`](crate::graphlink) | `recompute_links()`, `_article_weights()`, `prune_links()`, `recompute_threads()` |
//! | [`graphread`](crate::graphread) | `bfs_subgraph()`, `top_subgraph()`, `graph_path()`, `links_for()`, `ai.t_entity_profile()`, `ai.t_top_entities()` |
//!
//! # What is deliberately identical
//!
//! * **The SQL.** Every statement is the one `server/graph.py` runs, with the
//!   same bindings in the same order, so a database written by either side is read
//!   by both. Where SQLite leaves a tie open (`ORDER BY c DESC, en.count DESC
//!   LIMIT 24` has no third key), the statement is not re-sorted in Rust: both
//!   sides must see the same rows.
//! * **The windows, except their shape.** `graph.py` binds `datetime('now', ?)`
//!   and compares it to `published`; that cutoff is `2026-11-05 10:00:00` while
//!   every row is `2026-11-05T10:00:00Z`, and `T` sorts above the space, so the
//!   comparison ignored the hour on the cutoff's own calendar day — a window up to
//!   a day wider than asked for. This side binds the cutoff in the archive's own
//!   format (`window_ago`, `atlas_store::clock::iso_ago`), which is the same
//!   instant as `graph.py` meant. The rows are the same rows on any day the two
//!   sides agree about the boundary; `atlas-store`'s `search::index_sql` made the
//!   same change for the same reason.
//! * **The numbers.** `MAX_ENTITIES_PER_ARTICLE`, `MAX_PLACES_PER_ARTICLE`,
//!   `MAX_LINKS_PER_ARTICLE`, `MIN_LINK_SCORE`, the score weights, the
//!   `shared/(shared+1.5)` shrink, `df_cap = max(12, 20 %)`, and the two fallbacks
//!   (`0.5` for an entity's IDF, `1.0` for a norm) are the Python numbers.
//! * **The write shape.** `link_article`, each `recompute` step and the thread
//!   rewrite are each one transaction. `server/graph.py` commits statement by
//!   statement, so a crash between two of them leaves a half-linked article
//!   behind; the end state is the same, the half state is not.
//!
//! # What is passed in instead of looked up
//!
//! Three inputs belong to another module and travel with the request, so the rule
//! stays in one place (the precedent is `graphcore.threads(places=…)`):
//!
//! * `places` — the country/city names `geodata` knows, either as an array of names
//!   (the older payload: they answer `is_place` and nothing more) or as an object
//!   `name → ISO` (the same names *and* the gazetteer's answer, which is what lets a
//!   named city write the country it is in to `article_countries`). `_place_names()`
//!   reads `data/countries.json`, which is `atlas-places`' file, not this crate's.
//! * `countries` — `name_to_iso()`, the same table keyed by country name.
//! * `categories` — `type → category id` from `ai.category_id_for()`, which
//!   creates the category rows. `link_article()` asks for it and falls back to
//!   `NULL`; here a type that is not in the map is `NULL` too.
//!
//! # Never in a log
//!
//! No article text, summary or title is ever printed. Errors are scrubbed through
//! `atlas_store::scrub` and shortened; the JSON answers carry ids, counts and
//! milliseconds.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use atlas_lib::json::{self, Json};
use atlas_store::{placeholders, Row, SqlValue, Store, StoreError};
use rusqlite::{params_from_iter, Connection};

use crate::{canonical_name, keep_ids, merge_key, EntRef, Lex};

// ---------------------------------------------------------------- constants
// (`server/graph.py`, same names, same numbers)

/// `recompute_edges(days=30)` — the window a rebuild reads.
pub const EDGE_DAYS: i64 = 30;
/// `recompute_links(days=14)`.
pub const LINK_DAYS: i64 = 14;
/// `MAX_LINKS_PER_ARTICLE` — a hard per-article cap after the merge.
pub const MAX_LINKS_PER_ARTICLE: usize = 6;
/// `MIN_LINK_SCORE` — a real floor on a connection's score.
pub const MIN_LINK_SCORE: f64 = 0.16;
/// `MIN_TEXT_ONLY` — wording alone must be at least this strong.
pub const MIN_TEXT_ONLY: f64 = 0.40;
/// `MIN_TEXT_WITHOUT_ENTITIES` — no names in common, so the wording decides.
pub const MIN_TEXT_WITHOUT_ENTITIES: f64 = 0.45;
/// `MIN_TEXT_RESCUE` — the weakest wording a lonely story may be rescued on.
pub const MIN_TEXT_RESCUE: f64 = 0.16;
/// `_IDF_TTL` and the entity-index TTL: `time.time() - at < 120`.
pub(crate) const CACHE_TTL_S: u64 = 120;
/// `m.get(e, 0.5)` — an entity with no IDF row weighs as much as a rare one.
pub(crate) const IDF_FALLBACK: f64 = 0.5;
/// `payload`'s `max(12, top_per_article * 2)` is the gate `similar` gets.
pub(crate) const TEXT_PAIR_FLOOR: usize = 12;
/// The smallest `df` a wording token needs to enter a vector (`df < 2`).
pub(crate) const TEXT_DF_MIN: i64 = 2;

// ---------------------------------------------------------------- values

pub(crate) fn err<T>(message: impl Into<String>) -> Result<T, StoreError> {
    Err(StoreError::new(message))
}

pub(crate) fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

pub(crate) fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

pub(crate) fn real(value: f64) -> SqlValue {
    SqlValue::Real(value)
}

/// One statement on the caller's connection (the transaction's, or the store's).
pub(crate) fn query(
    conn: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> Result<Vec<Row>, StoreError> {
    let mut stmt = conn.prepare(sql).map_err(StoreError::from)?;
    let cols: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(|c| c.to_string())
        .collect();
    let mut rows = stmt
        .query(params_from_iter(params.iter()))
        .map_err(StoreError::from)?;
    let mut out: Vec<Row> = Vec::new();
    while let Some(row) = rows.next().map_err(StoreError::from)? {
        let mut vals = Vec::with_capacity(cols.len());
        for index in 0..cols.len() {
            let raw = row.get_ref(index).map_err(StoreError::from)?;
            vals.push(SqlValue::try_from(raw).map_err(StoreError::from)?);
        }
        out.push(Row::new(cols.clone(), vals));
    }
    Ok(out)
}

pub(crate) fn query_one(
    conn: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> Result<Option<Row>, StoreError> {
    Ok(query(conn, sql, params)?.into_iter().next())
}

pub(crate) fn exec(
    conn: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> Result<usize, StoreError> {
    let mut stmt = conn.prepare(sql).map_err(StoreError::from)?;
    run(&mut stmt, params)
}

/// [`exec`] on a statement the caller prepared once and runs in a loop.
///
/// The same binding, in the same order: a row-at-a-time caller with a fixed
/// statement compiles it once instead of once per row.
pub(crate) fn run(
    stmt: &mut rusqlite::Statement<'_>,
    params: &[SqlValue],
) -> Result<usize, StoreError> {
    stmt.execute(params_from_iter(params.iter()))
        .map_err(StoreError::from)
}

/// `max(1, value)` — the `or 1` of `db.query_one(...)["c"] or 1`.
pub(crate) fn one(value: i64) -> i64 {
    if value < 1 {
        1
    } else {
        value
    }
}

/// `round(x, 4)` — Python rounds the exact value half-to-even, and so does
/// Rust's fixed-point formatter, so both sides store the same double.
pub(crate) fn round4(x: f64) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    format!("{:.4}", x).parse::<f64>().unwrap_or(0.0)
}

/// `int(share * count)`, floored — `int(0.2 * len(arts))` and friends.
pub(crate) fn fraction(count: usize, share: f64) -> i64 {
    (share * count as f64) as i64
}

/// A `REAL`/`INTEGER`/`TEXT` value re-bound unchanged (a `published` column
/// travels as whatever SQLite stored).
pub(crate) fn same_value(value: Option<&SqlValue>) -> SqlValue {
    match value {
        Some(SqlValue::Null) | None => SqlValue::Null,
        Some(other) => other.clone(),
    }
}

// ------------------------------------------------------- what a label may be

/// Characters one relation label may hold. The same 60 as `atlas-ingest`'s
/// `enrich::RELATION_LABEL_CHARS`, which is where the rule came from: `edges` is
/// this crate's table, so the rule for the text stored in it belongs here.
pub const LABEL_CHARS: usize = 60;

/// A relation label the graph may hold: one short line of text, or `""`.
///
/// `edges.label` is model output over page text — "governs", "member of" — drawn
/// on an edge in the UI and read back by the graph tools ("why are these two
/// connected"). It used to be stored as the model spelled it: measured, a hostile
/// answer put `<img src=x onerror=alert(1)>` on a real edge.
///
/// The rule is the one a short display term gets, and nothing more: control and
/// bidi characters out ([`atlas_guard::cleantext::clean`], the app's decision
/// about invisible characters), one line, no markup or JSON punctuation, at most
/// [`LABEL_CHARS`] characters, and it has to contain a letter. *Which* words make
/// a good label stays the model's decision — there is no word list here, and there
/// could not be one.
///
/// `atlas-ingest`'s enrichment pass applies the same rule to a label it reads out
/// of an answer (`enrich::relation_label`) and should call this one instead: it
/// already depends on this crate, and two copies of a rule are two answers to the
/// same question a release apart. Until it does, a change to the rule is a change
/// to both — never to a third copy.
pub fn relation_label(raw: &str) -> String {
    let cleaned = atlas_guard::cleantext::clean(raw, Some(LABEL_CHARS)).text;
    let flat = cleaned.split_whitespace().collect::<Vec<&str>>().join(" ");
    if flat.is_empty() || !flat.chars().any(|ch| ch.is_alphabetic()) {
        return String::new();
    }
    // Punctuation a tag or a JSON object is built from. A label is a phrase; a
    // phrase that still holds these is a fragment of something else.
    if flat
        .chars()
        .any(|ch| matches!(ch, '<' | '>' | '{' | '}' | '[' | ']' | '|' | '\\' | '"' | '`' | '='))
    {
        return String::new();
    }
    flat.chars().take(LABEL_CHARS).collect()
}

// ---------------------------------------------------------------- JSON out
// The engine writes its own JSON; these keep the assembler readable without a
// second serializer in the crate.

pub(crate) fn jstr(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    json::escape_str(&mut out, value);
    out
}

pub(crate) fn jnum(value: f64) -> String {
    json::num_str(value)
}

/// A number with a fixed number of decimals (`12.4` ms), never an integral `12`.
pub(crate) fn jfixed(value: f64, places: usize) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    format!("{:.*}", places, value)
}

/// `{"a":1,"b":[1,2]}` from values that are already JSON text.
pub(crate) fn jobj(pairs: &[(&str, String)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&jstr(key));
        out.push(':');
        out.push_str(value);
    }
    out.push('}');
    out
}

pub(crate) fn jarr(items: &[String]) -> String {
    let mut out = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(item);
    }
    out.push(']');
    out
}

/// A SQLite value as JSON text.
pub(crate) fn value_text(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "null".to_string(),
        SqlValue::Integer(number) => jnum(*number as f64),
        SqlValue::Real(number) => jnum(*number),
        SqlValue::Text(text) => jstr(text),
        // JSON has no bytes and no column of this schema stores one; the length
        // keeps the shape valid and leaks nothing.
        SqlValue::Blob(bytes) => jstr(&format!("{} bytes", bytes.len())),
    }
}

// ---------------------------------------------------------------- the payload

/// The tables another module owns, handed in on the payload.
#[derive(Default)]
pub struct Context {
    /// `_place_names()` — lower-cased country and city names.
    places: HashSet<String>,
    /// The place gazetteer with its answer: name (exact case) → ISO code, for every
    /// country *and city* the caller knows. A city maps to the country it is in, so an
    /// article that names Berlin, Kyiv and Moscow writes Germany and Ukraine and Russia
    /// to `article_countries`, not only the one place the story is filed under.
    ///
    /// Filled from `places` when it is an object instead of an array (the array form is
    /// the older payload and still answers `is_place`), and from `place_iso` when that
    /// is sent on its own.
    place_iso: HashMap<String, String>,
    /// `name_to_iso()` — country name (exact case) → ISO code.
    countries: HashMap<String, String>,
    /// `ai.category_id_for()` — entity type → category id.
    categories: HashMap<String, i64>,
    /// Whether the types in this payload are the model's own answer (`"types": "model"`)
    /// rather than a shape rule's guess.
    ///
    /// A stored type normally wins: the first reader of a name keeps its kind, and nothing
    /// re-classifies behind the reader's back. That rule is right for a guess and wrong for
    /// an answer — measured, the model called the European Union an `Institution` while a row
    /// created by the capitalised-run extractor said `PERSON`, and the graph kept the guess.
    /// When the caller says the types came from the model, the model's word replaces the
    /// stored one (and the category with it); when it does not, nothing changes.
    authoritative: bool,
}

impl Context {
    /// Read `places`, `countries` and `categories` from the payload. Anything
    /// missing is simply unknown: no place is trusted, no country is mapped, no
    /// category is assigned — which is what the Python fallbacks do when the
    /// geodata file or the AI helper is missing.
    pub fn from_json(input: &Json) -> Context {
        let mut ctx = Context::default();
        if let Some(items) = input.get("places").and_then(|v| v.as_arr()) {
            for item in items {
                if let Some(name) = item.as_str() {
                    let low = name.trim().to_lowercase();
                    if !low.is_empty() {
                        ctx.places.insert(low);
                    }
                }
            }
        }
        // The same key as an object carries the gazetteer's answer with each name.
        if let Some(Json::Obj(items)) = input.get("places") {
            ctx.absorb_places(items);
        }
        if let Some(Json::Obj(items)) = input.get("place_iso") {
            ctx.absorb_places(items);
        }
        if let Some(Json::Obj(items)) = input.get("countries") {
            for (name, iso) in items {
                if let Some(iso) = iso.as_str() {
                    ctx.countries.insert(name.clone(), iso.to_string());
                }
            }
        }
        ctx.authoritative = input
            .get("types")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("model"));
        if let Some(Json::Obj(items)) = input.get("categories") {
            for (kind, id) in items {
                if let Some(id) = id.as_f64() {
                    ctx.categories.insert(kind.clone(), id as i64);
                }
            }
        }
        ctx
    }

    /// One `name → ISO` table into [`Self::place_iso`] and the name into
    /// [`Self::places`], so a gazetteer sent as an object answers both questions.
    fn absorb_places(&mut self, items: &[(String, Json)]) {
        for (name, iso) in items {
            let name = name.trim();
            let Some(iso) = iso.as_str() else {
                continue;
            };
            let iso = iso.trim();
            if name.is_empty() || iso.is_empty() {
                continue;
            }
            self.places.insert(name.to_lowercase());
            self.place_iso
                .insert(name.to_string(), iso.to_uppercase());
        }
    }

    /// `_is_place()` — a geodata name, whatever shape the English filters dislike.
    pub fn is_place(&self, name: &str) -> bool {
        !name.is_empty() && self.places.contains(name.to_lowercase().as_str())
    }

    /// The ISO code a place name stands for: the gazetteer's own answer first (a city
    /// maps to its country), then `name_to_iso` for a country name.
    pub fn iso_of(&self, name: &str) -> Option<&str> {
        match self.place_iso.get(name) {
            Some(iso) => Some(iso.as_str()),
            None => self.countries.get(name).map(String::as_str),
        }
    }
}

pub(crate) fn need_text(input: &Json, key: &str) -> Result<String, StoreError> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => err(format!("'{}' must be a non-empty string", key)),
    }
}

pub(crate) fn opt_usize(input: &Json, key: &str, fallback: usize) -> usize {
    input.get(key).and_then(|v| v.as_usize()).unwrap_or(fallback)
}

pub(crate) fn opt_i64(input: &Json, key: &str, fallback: i64) -> i64 {
    match input.get(key).and_then(|v| v.as_f64()) {
        Some(number) if number.is_finite() => number as i64,
        _ => fallback,
    }
}

pub(crate) fn opt_f64(input: &Json, key: &str, fallback: f64) -> f64 {
    match input.get(key).and_then(|v| v.as_f64()) {
        Some(number) if number.is_finite() => number,
        _ => fallback,
    }
}

pub(crate) fn need_id(input: &Json, key: &str) -> Result<i64, StoreError> {
    match input.get(key).and_then(|v| v.as_f64()) {
        Some(number) if number.is_finite() && number.fract() == 0.0 => Ok(number as i64),
        _ => err(format!("'{}' must be a whole number", key)),
    }
}

/// Open the one file this crate is allowed to open.
pub(crate) fn open(input: &Json) -> Result<Store, StoreError> {
    let path = need_text(input, "db")?;
    Store::open(&path)
}

/// Scrub a failure reason before it reaches a caller.
pub(crate) fn fail(error: StoreError) -> String {
    atlas_store::scrub(error.message())
}

// ---------------------------------------------------------------- caches
// `_entity_index()` and `idf_map()`: one map per process, 120 s, and the same
// invalidation trigger — a merge or a purge throws the name index away.

/// One row of `entities` in the shape `_entity_index()` keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct NameRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub count: i64,
}

/// `idf_map()`'s answer: entity → weight, plus the article count behind it.
#[derive(Clone, Debug, Default)]
pub struct Idf {
    pub map: HashMap<i64, f64>,
    pub n: i64,
}

/// The two in-memory maps `server/graph.py` keeps between calls.
///
/// A CLI invocation builds one, uses it and exits; a long-lived caller (the
/// bridge) keeps one and gets what Python gets: 120 seconds of reuse, and a
/// rebuild whenever a write invalidates it. The maps are *moved* out while a
/// write uses them — `db.py` mutates its cached row dicts in place — so a
/// 4 000-name index is never deep-copied.
#[derive(Default)]
pub struct Caches {
    entity_index: Option<(Instant, HashMap<String, NameRow>)>,
    idf: HashMap<i64, (Instant, Idf)>,
}

impl Caches {
    pub fn new() -> Caches {
        Caches::default()
    }

    /// The name index if it is still inside its 120 seconds, moved out.
    pub fn take_fresh_entity_index(&mut self) -> Option<HashMap<String, NameRow>> {
        match self.entity_index.take() {
            Some((at, map)) if at.elapsed().as_secs() < CACHE_TTL_S => Some(map),
            _ => None,
        }
    }

    /// Put the (possibly updated) index back and restart the clock.
    pub fn store_entity_index(&mut self, map: HashMap<String, NameRow>) {
        self.entity_index = Some((Instant::now(), map));
    }

    /// `_entity_index(force=True)` — the invalidation trigger of
    /// `_merge_variants()` and `_purge_junk_entities()`.
    pub fn invalidate_names(&mut self) {
        self.entity_index = None;
    }

    /// `idf_map(days)` if still fresh, moved out.
    pub fn take_fresh_idf(&mut self, days: i64) -> Option<Idf> {
        match self.idf.remove(&days) {
            Some((at, idf)) if at.elapsed().as_secs() < CACHE_TTL_S => Some(idf),
            _ => None,
        }
    }

    pub fn store_idf(&mut self, days: i64, idf: Idf) {
        self.idf.insert(days, (Instant::now(), idf));
    }

    /// How many windows are cached — `status` reports it instead of guessing.
    pub fn idf_windows(&self) -> usize {
        self.idf.len()
    }
}

/// `_entity_index()`'s body: canonical key → the row with the highest count.
pub(crate) fn load_entity_index(
    conn: &Connection,
) -> Result<HashMap<String, NameRow>, StoreError> {
    let rows = query(conn, "SELECT id,name,type,count FROM entities", &[])?;
    let mut by_id: HashMap<i64, NameRow> = HashMap::new();
    let mut by_key: HashMap<String, NameRow> = HashMap::new();
    for row in &rows {
        let entry = NameRow {
            id: row.int("id").unwrap_or(0),
            name: row.text("name").unwrap_or("").to_string(),
            kind: row.text("type").unwrap_or("").to_string(),
            count: row.int("count").unwrap_or(0),
        };
        by_id.insert(entry.id, entry.clone());
        let key = merge_key(&entry.name);
        if key.is_empty() {
            continue;
        }
        match by_key.get(&key) {
            Some(current) if current.count >= entry.count => {}
            _ => {
                by_key.insert(key, entry);
            }
        }
    }
    // `by_key.setdefault(...)`: an alias never overrides a name already there.
    for row in &query(conn, "SELECT alias, entity_id FROM entity_alias", &[])? {
        let alias = row.text("alias").unwrap_or("");
        let id = row.int("entity_id").unwrap_or(0);
        if let Some(entry) = by_id.get(&id) {
            let key = merge_key(alias);
            by_key.entry(key).or_insert_with(|| entry.clone());
        }
    }
    Ok(by_key)
}

/// The archive's own timestamp for a window that starts `days` before now.
///
/// A window is a **value in the format the rows are stored in**
/// (`2026-11-05T10:00:00Z`, [`atlas_store::iso_ago`]) and never
/// `datetime('now', ?)`: SQLite hands that back as `2026-11-05 10:00:00`, and
/// `published >= ?` across two shapes is a string comparison in which `T` sorts
/// above the space — so every article published on the cutoff's own calendar day
/// passed the window no matter what hour it was published, up to a day wider than
/// asked for. `atlas-store`'s `search::index_sql` carries the same note and the
/// same fix; `db_tests::the_window_is_the_archive_timestamp_and_not_a_day_wider`
/// is the proof.
pub(crate) fn window_ago(days: i64) -> String {
    atlas_store::clock::iso_ago(days as f64 * 24.0)
}

/// `idf_map(days)` — rare entities carry more meaning than "United States".
pub(crate) fn idf_map(conn: &Connection, days: i64) -> Result<Idf, StoreError> {
    let window = window_ago(days);
    let n = query_one(
        conn,
        "SELECT COUNT(*) c FROM articles WHERE relevant=1 AND published>=?",
        &[text(&window)],
    )?
    .and_then(|row| row.int("c"))
    .unwrap_or(1);
    let n = one(n);
    let rows = query(
        conn,
        "SELECT ae.entity_id e, COUNT(DISTINCT ae.article_id) df FROM article_entities ae \
         JOIN articles a ON a.id=ae.article_id WHERE a.relevant=1 AND a.published>=? \
         GROUP BY ae.entity_id",
        &[text(&window)],
    )?;
    let mut map: HashMap<i64, f64> = HashMap::with_capacity(rows.len());
    for row in rows {
        let entity = row.int("e").unwrap_or(0);
        let df = one(row.int("df").unwrap_or(0));
        map.insert(entity, (1.0 + n as f64 / df as f64).ln());
    }
    Ok(Idf { map, n })
}

// ---------------------------------------------------------------- link_article

/// What [`link_article`] did, so a caller can count instead of guess.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Linked {
    /// Entity ids now attached to the article (`len(uniq)` in Python).
    pub entities: usize,
    /// Co-occurrence pairs written to `edges`.
    pub pairs: usize,
}

/// `link_article(article_id, ents)` — attach entities to an article and add the
/// co-occurrence edges.
///
/// Everything — the entity rows, the `article_entities` rows, the `edges`
/// increments and the `article_countries` rows — happens in one transaction, so a
/// reader never sees an article that is half linked.
pub fn link_article(
    store: &Store,
    caches: &mut Caches,
    ctx: &Context,
    article_id: i64,
    ents: &[(String, String)],
) -> Result<Linked, StoreError> {
    let lex = Lex::new();
    let now = atlas_store::now_iso();
    // The index is moved out of the cache and back, so the write never copies it
    // and a second call costs nothing.
    let cached = caches.take_fresh_entity_index();
    let (result, index) = store.in_transaction(|tx| {
        let mut index = match cached {
            Some(map) => map,
            None => load_entity_index(tx)?,
        };
        let mut ids: Vec<(i64, String)> = Vec::new();
        // The statements the loops below run, compiled once: an article brings up
        // to `MAX_ENTITIES_PER_ARTICLE` names and `keep_ids()` caps them at 12
        // pairs, so a `prepare` per row compiled the same four statements a
        // hundred times for the same rows.
        let mut bump = tx
            .prepare("UPDATE entities SET count=count+1, last_seen=?, type=? WHERE id=?")
            .map_err(StoreError::from)?;
        let mut create = tx
            .prepare(
                "INSERT INTO entities(name,type,count,category_id,first_seen,last_seen) \
                 VALUES(?,?,1,?,?,?)",
            )
            .map_err(StoreError::from)?;
        let mut attach = tx
            .prepare("INSERT OR IGNORE INTO article_entities(article_id,entity_id) VALUES(?,?)")
            .map_err(StoreError::from)?;
        let mut country = tx
            .prepare(
                "INSERT OR IGNORE INTO article_countries(article_id,iso,is_primary) VALUES(?,?,?)",
            )
            .map_err(StoreError::from)?;
        for (raw, typ) in ents {
            // What may become an entity name is `atlas_ai::prompts::entity_name`'s
            // decision — the app's one rule for a name the graph holds, and the
            // same rule an entity that arrives as a model *tool call* is held to
            // (`atlas_ai::wire`). Page text reaches this function unchanged (a
            // scraped title, a model's answer over that title); measured before
            // this line existed, a name holding U+202E and a bell character was
            // stored and drawn, and `<img src=x onerror=alert(1)>` was stored as a
            // name. A name the rule refuses is dropped, never replaced by a guess;
            // one it accepts is stored with the characters it strips already gone,
            // and then folded as before.
            let named = atlas_ai::prompts::entity_name(raw);
            if named.is_empty() {
                continue;
            }
            let name = canonical_name(&named);
            if name.is_empty() || name.chars().count() < crate::MIN_NAME_CHARS {
                continue;
            }
            // country names come from geodata and are trusted even when they
            // contain a word the English filters dislike ("New Zealand")
            if !(ctx.is_place(&name) || lex.ok_name(&name)) {
                continue;
            }
            let key = merge_key(&name);
            if let Some(row) = index.get(&key).cloned() {
                // The model decides what a name *is*. Python rewrote a stored
                // `MISC` with the type of the article being linked
                // (`row["type"] if row["type"] and row["type"] != "MISC" else typ`);
                // this layer never infers a kind from a name. Two rules, and which one
                // applies is the payload's own word (`ctx.authoritative`):
                // * a **guess** ("MISC" or "") is filled in when a later article knows the
                //   name — it never overwrites a type somebody already stated;
                // * an **answer** (the model's own list) replaces a stored type, because a
                //   heuristic that called the European Union a `PERSON` must not outrank the
                //   model saying `Institution` about the same story.
                let kind = if row.kind.is_empty() || (ctx.authoritative && !typ.is_empty()) {
                    typ.clone()
                } else {
                    row.kind.clone()
                };
                run(&mut bump, &[text(&now), text(&kind), int(row.id)])?;
                if let Some(slot) = index.get_mut(&key) {
                    slot.kind = kind.clone();
                    slot.count += 1;
                }
                ids.push((row.id, kind));
                continue;
            }
            let category = match ctx.categories.get(typ) {
                Some(id) => int(*id),
                None => SqlValue::Null,
            };
            let created = run(
                &mut create,
                &[text(&name), text(typ), category, text(&now), text(&now)],
            );
            let (eid, kind) = match created {
                Ok(_) => (tx.last_insert_rowid(), typ.clone()),
                // concurrent insert of the same name
                Err(_rejected) => match query_one(
                    tx,
                    "SELECT id,type FROM entities WHERE name=?",
                    &[text(&name)],
                )? {
                    Some(row) => (
                        row.int("id").unwrap_or(0),
                        row.text("type").unwrap_or(typ).to_string(),
                    ),
                    None => continue,
                },
            };
            index.insert(
                key,
                NameRow {
                    id: eid,
                    name: name.clone(),
                    kind: kind.clone(),
                    count: 1,
                },
            );
            ids.push((eid, kind));
        }

        exec(
            tx,
            "DELETE FROM article_entities WHERE article_id=?",
            &[int(article_id)],
        )?;
        // `dict.setdefault`: the first type seen for an id wins, order kept.
        let mut uniq: Vec<(i64, String)> = Vec::new();
        let mut seen: HashSet<i64> = HashSet::new();
        for (eid, typ) in ids {
            if !seen.insert(eid) {
                continue;
            }
            uniq.push((eid, typ));
        }
        for (eid, _) in &uniq {
            run(&mut attach, &[int(article_id), int(*eid)])?;
        }
        // Graph edges: cap the entity list per article so hubs do not swallow
        // everything (`_keep_ids()`).
        let refs: Vec<EntRef> = uniq
            .iter()
            .map(|(eid, kind)| EntRef {
                id: *eid as u32,
                name: String::new(),
                kind: kind.clone(),
                count: 0.0,
            })
            .collect();
        let keep: Vec<i64> = keep_ids(&refs).into_iter().map(|id| id as i64).collect();
        let mut pairs = 0usize;
        // The same plan for all of them, and `keep_ids()` caps the list at 12
        // entities, so this loop writes at most 66 rows.
        let mut edge = tx
            .prepare(
                "INSERT INTO edges(a,b,weight,type) VALUES(?,?,?,?) \
                 ON CONFLICT(a,b,type) DO UPDATE SET weight=weight+1",
            )
            .map_err(StoreError::from)?;
        for i in 0..keep.len() {
            for j in (i + 1)..keep.len() {
                run(
                    &mut edge,
                    &[int(keep[i]), int(keep[j]), int(1), text("cooccur")],
                )?;
                pairs += 1;
            }
        }

        // Countries of the article: one row per place the entity list names, plus the
        // article's own place.
        //
        // The name is only a *key* here — what is stored is the ISO code — so it is not
        // coerced: a filter that dropped a name would drop a country the article really
        // is about. A city resolves to the country it is in through the gazetteer's own
        // answer ([`Context::iso_of`]), which is why an article naming Berlin, Kyiv and
        // Moscow writes DEU, UKR and RUS instead of only the place it is filed under.
        //
        // Exactly one row is primary: the article's own place when the list names it (it
        // is appended below when the list does not), else the first place named. A table
        // with either zero or two primaries makes the map's "primary" chip and the
        // country filter disagree about which country the story belongs to.
        //
        // `INSERT OR IGNORE` on `(article_id, iso)` and the `DELETE` above make a re-link
        // of the same list write the same rows: same set, same primary, no duplicates.
        exec(
            tx,
            "DELETE FROM article_countries WHERE article_id=?",
            &[int(article_id)],
        )?;
        let own = query_one(tx, "SELECT iso FROM articles WHERE id=?", &[int(article_id)])?
            .and_then(|row| row.text("iso").map(|iso| iso.to_string()))
            .filter(|iso| !iso.is_empty());
        let mut written: Vec<String> = Vec::new();
        for (raw, typ) in ents {
            let name = canonical_name(raw);
            let mut candidates: Vec<&str> = vec![name.as_str()];
            if raw.as_str() != name.as_str() {
                candidates.push(raw.as_str());
            }
            for candidate in candidates {
                // A place the gazetteer knows is a country row unless the model said the
                // name is something a country cannot be: `Georgia` typed `Person` is a
                // person, and a person is not the country they are named after. The kinds
                // that let the gazetteer answer are the ones that do not name a different
                // thing — the place kind itself, and the ones that say nothing.
                if !(place_kind(typ) && ctx.is_place(candidate)) {
                    continue;
                }
                let Some(iso) = ctx.iso_of(candidate) else {
                    continue;
                };
                let iso = iso.to_uppercase();
                if !written.iter().any(|seen| *seen == iso) {
                    written.push(iso);
                }
                break;
            }
        }
        if let Some(iso) = own.as_ref() {
            if !written.iter().any(|seen| seen == iso) {
                written.push(iso.clone());
            }
        }
        let primary = own.or_else(|| written.first().cloned());
        for iso in &written {
            let is_primary = if Some(iso) == primary.as_ref() { 1 } else { 0 };
            run(
                &mut country,
                &[int(article_id), text(iso), int(is_primary)],
            )?;
        }
        Ok((
            Linked {
                entities: uniq.len(),
                pairs,
            },
            index,
        ))
    })?;
    caches.store_entity_index(index);
    Ok(result)
}

/// May a gazetteer name become a country row under this type?
///
/// `LOC` is the graph's own place kind (`Country` and `City` arrive mapped to it), and
/// an empty type, `MISC` or `Other` is the model saying it does not know — the gazetteer
/// is then the only evidence there is. Every other kind names something a country cannot
/// be: a person, an organisation, an institution, an event, a product.
fn place_kind(kind: &str) -> bool {
    matches!(
        kind.trim().to_ascii_uppercase().as_str(),
        "LOC" | "" | "MISC" | "OTHER" | "PLACE" | "COUNTRY" | "CITY"
    )
}

// ---------------------------------------------------------------- variants

/// `_merge_variants()` — fold spelling variants (case, possessive,
/// `Inc`/`Corp`/`Ltd`) into one entity row, then normalise the survivors.
pub(crate) fn merge_variants(conn: &Connection) -> Result<usize, StoreError> {
    let rows = query(conn, "SELECT id,name,count FROM entities", &[])?;
    let mut groups: HashMap<String, Vec<(i64, String, i64)>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        let key = merge_key(row.text("name").unwrap_or(""));
        if key.is_empty() {
            continue;
        }
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push((
            row.int("id").unwrap_or(0),
            row.text("name").unwrap_or("").to_string(),
            row.int("count").unwrap_or(0),
        ));
    }
    let mut merged = 0usize;
    // One plan per statement for the whole merge. This loop folds thousands of
    // variants on a real archive, and a `prepare` per row compiled the same six
    // statements once per folded row; the rows written are the same rows.
    let mut move_links = conn
        .prepare("UPDATE OR IGNORE article_entities SET entity_id=? WHERE entity_id=?")
        .map_err(StoreError::from)?;
    let mut drop_links = conn
        .prepare("DELETE FROM article_entities WHERE entity_id=?")
        .map_err(StoreError::from)?;
    let mut keep_alias = conn
        .prepare("INSERT OR IGNORE INTO entity_alias(alias,entity_id) VALUES(?,?)")
        .map_err(StoreError::from)?;
    let mut move_aliases = conn
        .prepare("UPDATE OR IGNORE entity_alias SET entity_id=? WHERE entity_id=?")
        .map_err(StoreError::from)?;
    let mut drop_aliases = conn
        .prepare("DELETE FROM entity_alias WHERE entity_id=?")
        .map_err(StoreError::from)?;
    let mut add_count = conn
        .prepare("UPDATE entities SET count=count+? WHERE id=?")
        .map_err(StoreError::from)?;
    let mut drop_entity = conn
        .prepare("DELETE FROM entities WHERE id=?")
        .map_err(StoreError::from)?;
    for key in order {
        let mut list = match groups.remove(&key) {
            Some(list) => list,
            None => continue,
        };
        if list.len() < 2 {
            continue;
        }
        // `lst.sort(key=lambda r: (-(r["count"] or 0), r["id"]))`
        list.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        let keep = list[0].0;
        for row in &list[1..] {
            let gone = row.0;
            run(
                &mut move_links,
                &[int(keep), int(gone)],
            )?;
            run(&mut drop_links, &[int(gone)])?;
            // An alias is a name the graph answers a lookup with, so it is held to
            // the same rule an entity name is (`atlas_ai::prompts::entity_name`),
            // and not written at all when the rule refuses it. This loop writes
            // names out of rows a build without that rule may have stored — the
            // alias would otherwise carry a right-to-left override into every
            // later name lookup.
            let alias = atlas_ai::prompts::entity_name(&row.1);
            if !alias.is_empty() {
                run(&mut keep_alias, &[text(&alias), int(keep)])?;
            }
            run(&mut move_aliases, &[int(keep), int(gone)])?;
            run(&mut drop_aliases, &[int(gone)])?;
            run(&mut add_count, &[int(row.2), int(keep)])?;
            run(&mut drop_entity, &[int(gone)])?;
            merged += 1;
        }
    }
    // normalise the survivors too: "CoreWeave Inc" -> "CoreWeave", "Nvidia's" -> "Nvidia"
    let mut rename = conn
        .prepare("UPDATE entities SET name=? WHERE id=?")
        .map_err(StoreError::from)?;
    for row in query(conn, "SELECT id,name FROM entities", &[])? {
        let current = row.text("name").unwrap_or("").to_string();
        let target = canonical_name(&current);
        if !target.is_empty() && target != current {
            if run(
                &mut rename,
                &[text(&target), int(row.int("id").unwrap_or(0))],
            )
            .is_ok()
            {
                merged += 1;
            }
        }
    }
    Ok(merged)
}

/// `_purge_junk_entities()` — delete entity rows that are navigation text,
/// outlets, weekdays or fragments.
pub(crate) fn purge_junk_entities(
    conn: &Connection,
    lex: &Lex,
    ctx: &Context,
) -> Result<usize, StoreError> {
    let mut junk: Vec<i64> = Vec::new();
    for row in query(conn, "SELECT id,name FROM entities", &[])? {
        let name = canonical_name(row.text("name").unwrap_or(""));
        if name.is_empty() || lex.is_junk_name(&name) {
            junk.push(row.int("id").unwrap_or(0));
            continue;
        }
        if ctx.is_place(&name) {
            // a real place keeps its row, whatever its shape
            continue;
        }
        if !lex.ok_name(&name) {
            junk.push(row.int("id").unwrap_or(0));
        }
    }
    for part in junk.chunks(400) {
        let marks = placeholders(part.len());
        let ids: Vec<SqlValue> = part.iter().map(|id| int(*id)).collect();
        exec(
            conn,
            &format!(
                "DELETE FROM article_entities WHERE entity_id IN ({})",
                marks
            ),
            &ids,
        )?;
        let mut both: Vec<SqlValue> = part.iter().map(|id| int(*id)).collect();
        both.extend(part.iter().map(|id| int(*id)));
        exec(
            conn,
            &format!("DELETE FROM edges WHERE a IN ({}) OR b IN ({})", marks, marks),
            &both,
        )?;
        // An alias is `alias TEXT UNIQUE` -> entity id: left behind, it points at
        // a row that no longer exists, so `_entity_index()` (`by_id.get`) drops it
        // on the floor and the alias is lost for good — and it would block a later
        // merge from storing that spelling (the UNIQUE key is taken).
        exec(
            conn,
            &format!("DELETE FROM entity_alias WHERE entity_id IN ({})", marks),
            &ids,
        )?;
        exec(
            conn,
            &format!("DELETE FROM entities WHERE id IN ({})", marks),
            &ids,
        )?;
    }
    Ok(junk.len())
}

// ---------------------------------------------------------------- edges

/// The co-occurrence edge set of one mention dump: `(article, entity)` rows in,
/// `(a, b) → weight` out, with `_keep_ids()`' per-article cap applied.
///
/// The cap keeps the *first* names of the list it is handed, so the order of an
/// article's mentions is part of the answer — and the caller has no `ORDER BY` to
/// offer (`article_entities` has none, and the query plan decides what order the
/// rows arrive in). Sorting the ids first makes the kept names a property of the
/// row set instead of the plan: the same archive produces the same edges however
/// SQLite chooses to read it.
fn pairs_from_mentions(
    mentions: &[(i64, i64)],
    kind: &HashMap<i64, String>,
) -> BTreeMap<(i64, i64), i64> {
    let mut per: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (article, entity) in mentions {
        per.entry(*article).or_default().push(*entity);
    }
    let mut pairs: BTreeMap<(i64, i64), i64> = BTreeMap::new();
    for entities in per.values_mut() {
        // sorted before the cap: `keep_ids()` slices the head of this list
        entities.sort_unstable();
        entities.dedup();
        let refs: Vec<EntRef> = entities
            .iter()
            .map(|entity| EntRef {
                id: *entity as u32,
                name: String::new(),
                kind: kind.get(entity).cloned().unwrap_or_default(),
                count: 0.0,
            })
            .collect();
        let keep: Vec<i64> = keep_ids(&refs).into_iter().map(|id| id as i64).collect();
        for i in 0..keep.len() {
            for j in (i + 1)..keep.len() {
                *pairs.entry((keep[i], keep[j])).or_insert(0) += 1;
            }
        }
    }
    pairs
}

/// What a rebuild did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgesDone {
    pub merged: usize,
    pub purged: usize,
    pub edges: usize,
    /// Edges whose stored weight disagreed with the articles that mention both ends.
    /// A rebuild that reports a number here is a rebuild that repaired the archive.
    pub reweighed: usize,
    /// Reasons carried over the rebuild. A label is the model's one sentence about a
    /// pair; the weight changes on every rebuild and the sentence does not, so it is
    /// taken off before the edges are rewritten and put back after.
    pub labelled: usize,
}

/// `recompute_edges(days=30)` — rebuild the co-occurrence graph from scratch
/// (relevant articles only), in one transaction.
///
/// The pair loop is driven by the posting lists *inside* an article: only
/// entities that share an article can become a pair, so the work is
/// `Σ |kept entities per article|²` (≤ 66 per article) and never `n²` over the
/// 4 000 entities the archive holds.
pub fn recompute_edges(
    store: &Store,
    caches: &mut Caches,
    ctx: &Context,
    days: i64,
) -> Result<EdgesDone, StoreError> {
    let lex = Lex::new();
    let window = window_ago(days);
    let out = store.in_transaction(|tx| {
        let merged = merge_variants(tx)?;
        let purged = purge_junk_entities(tx, &lex, ctx)?;
        let rebuild = rebuild_edges(tx, &window)?;
        Ok(EdgesDone {
            merged,
            purged,
            edges: rebuild.edges,
            reweighed: rebuild.reweighed,
            labelled: rebuild.labelled,
        })
    })?;
    if out.merged > 0 || out.purged > 0 {
        // `_entity_index(force=True)`: the name index now points at rows that
        // have been merged away or deleted.
        caches.invalidate_names();
    }
    Ok(out)
}

/// What rewriting the edge table produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rebuilt {
    pub edges: usize,
    pub reweighed: usize,
    pub labelled: usize,
}

/// Rewrite every edge from `article_entities` inside the caller's transaction.
///
/// The weight of a relation *is* the number of articles that mention both ends, so
/// this is the definition, not an approximation: the rows that disagree with it
/// afterwards are rows written by [`link_article`] incrementally and never repaired.
///
/// Two things survive the rewrite because they are not derived: the reason a relation
/// carries (`edges.label`, the model's one sentence) and its `type`. The old body ran
/// `DELETE FROM edges` and re-inserted `(a,b,weight,'cooccur')`, which silently threw
/// every label away — measured on `data/atlas.db`: all 7 338 edges blank. The labels
/// are lifted into memory first and written back after the insert, so a rebuild can no
/// longer destroy the only human-readable thing the table holds.
pub(crate) fn rebuild_edges(conn: &Connection, window: &str) -> Result<Rebuilt, StoreError> {
    let before = query(
        conn,
        "SELECT a,b,weight,label,type FROM edges",
        &[],
    )?;
    let mut kept_labels: Vec<((i64, i64), String)> = Vec::new();
    let mut stored: HashMap<(i64, i64), i64> = HashMap::with_capacity(before.len());
    for row in &before {
        let a = row.int("a").unwrap_or(0);
        let b = row.int("b").unwrap_or(0);
        if let Some(label) = row.text("label") {
            if !label.trim().is_empty() {
                kept_labels.push(((a, b), label.to_string()));
            }
        }
        stored.insert((a, b), row.int("weight").unwrap_or(0));
    }
    drop(before);

    let rows = query(
        conn,
        "SELECT ae.article_id a, ae.entity_id e, en.type t FROM article_entities ae \
         JOIN entities en ON en.id=ae.entity_id JOIN articles ar ON ar.id=ae.article_id \
         WHERE ar.relevant=1 AND ar.published>=?",
        &[text(window)],
    )?;
    let mut mentions: Vec<(i64, i64)> = Vec::with_capacity(rows.len());
    let mut kind: HashMap<i64, String> = HashMap::new();
    for row in &rows {
        let article = row.int("a").unwrap_or(0);
        let entity = row.int("e").unwrap_or(0);
        mentions.push((article, entity));
        kind.insert(entity, row.text("t").unwrap_or("").to_string());
    }
    drop(rows);
    let pairs = pairs_from_mentions(&mentions, &kind);
    // A stored row that the evidence no longer gives — reweighted, or dropped because
    // the pair stopped sharing an article — is one repair. Counting it this way is what
    // lets the dry run's number and the pass's number be the same number.
    let reweighed = stored
        .iter()
        .filter(|(pair, weight)| pairs.get(pair) != Some(weight))
        .count();

    exec(conn, "DELETE FROM edges", &[])?;
    // One prepared statement for the whole edge set: the pairs come out of a
    // `BTreeMap`, so the insert order is the deterministic one, and 12 000
    // rows cost one plan instead of 12 000.
    if !pairs.is_empty() {
        let mut stmt = conn
            .prepare("INSERT INTO edges(a,b,weight,type) VALUES(?,?,?, 'cooccur')")
            .map_err(StoreError::from)?;
        for ((a, b), weight) in &pairs {
            let values = [int(*a), int(*b), int(*weight)];
            stmt.execute(params_from_iter(values.iter()))
                .map_err(StoreError::from)?;
        }
    }
    // The reason goes back on the pair it was written for, and only while the pair is
    // still drawn: a label for a relation the evidence no longer supports is dropped
    // with the edge rather than left pointing at nothing.
    let mut labelled = 0usize;
    let mut restore = conn
        .prepare("UPDATE edges SET label=? WHERE a=? AND b=?")
        .map_err(StoreError::from)?;
    for ((a, b), label) in &kept_labels {
        run(&mut restore, &[text(label), int(*a), int(*b)])?;
        labelled += 1;
    }
    Ok(Rebuilt {
        edges: pairs.len(),
        reweighed,
        labelled,
    })
}

// ---------------------------------------------------------------- commands

/// `link_article` — `{"db":…,"id":…,"entities":[[name,type],…]}`, plus the
/// `places` / `countries` / `categories` tables described on [`Context`].
pub fn cmd_link_article(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let ctx = Context::from_json(input);
    let article_id = need_id(input, "id").map_err(fail)?;
    let mut ents: Vec<(String, String)> = Vec::new();
    if let Some(items) = input.get("entities").and_then(|v| v.as_arr()) {
        for item in items {
            match item {
                Json::Arr(pair) => {
                    let name = pair.first().and_then(|v| v.as_str()).unwrap_or("");
                    // no type in the payload means the model had none: untyped,
                    // not "miscellaneous"
                    let kind = pair.get(1).and_then(|v| v.as_str()).unwrap_or("");
                    if !name.is_empty() {
                        ents.push((name.to_string(), kind.to_string()));
                    }
                }
                Json::Obj(_) => {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let kind = item
                        .get("type")
                        .or_else(|| item.get("kind"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !name.is_empty() {
                        ents.push((name.to_string(), kind.to_string()));
                    }
                }
                _ => continue,
            }
        }
    }
    let mut caches = Caches::new();
    let done = link_article(&store, &mut caches, &ctx, article_id, &ents).map_err(fail)?;
    Ok(jobj(&[
        ("entities", jnum(done.entities as f64)),
        ("pairs", jnum(done.pairs as f64)),
        ("source", jstr("rust")),
    ]))
}

/// `recompute` — `{"db":…,"parts":["edges","links","threads"]}`. The default runs
/// all three, in the order `collector.py` runs them.
pub fn cmd_recompute(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let ctx = Context::from_json(input);
    let edge_days = opt_i64(input, "days", EDGE_DAYS);
    let link_days = opt_i64(input, "link_days", LINK_DAYS);
    let top_per_article = opt_usize(input, "top_per_article", 8);
    let max_pairs = opt_usize(input, "max_pairs", 60_000);
    let min_score = opt_f64(input, "min_score", crate::DEFAULT_MIN_SCORE);
    let max_size = opt_usize(input, "max_size", crate::DEFAULT_MAX_SIZE);
    let mut parts: Vec<String> = Vec::new();
    match input.get("parts") {
        None | Some(Json::Null) => {
            for name in ["edges", "links", "threads"] {
                parts.push(name.to_string());
            }
        }
        Some(Json::Arr(items)) => {
            for item in items {
                match item.as_str() {
                    Some(name) => parts.push(name.to_string()),
                    None => return Err("'parts' must be an array of names".to_string()),
                }
            }
        }
        Some(_) => return Err("'parts' must be an array of names".to_string()),
    }
    let started = Instant::now();
    let mut caches = Caches::new();
    let mut edges = EdgesDone::default();
    let mut links = crate::graphlink::LinksDone::default();
    let mut threads = crate::graphlink::ThreadsDone::default();
    let mut pruned = 0usize;
    for part in &parts {
        match part.as_str() {
            "edges" => edges = recompute_edges(&store, &mut caches, &ctx, edge_days).map_err(fail)?,
            "links" => {
                links = crate::graphlink::recompute_links(
                    &store,
                    &mut caches,
                    &ctx,
                    link_days,
                    top_per_article,
                    max_pairs,
                )
                .map_err(fail)?
            }
            "threads" => {
                threads = crate::graphlink::recompute_threads(&store, &ctx, min_score, max_size)
                    .map_err(fail)?
            }
            "prune" => pruned = crate::graphlink::prune_links(&store).map_err(fail)?,
            other => return Err(format!("unknown recompute part: {}", other)),
        }
    }
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(jobj(&[
        ("edges", jnum(edges.edges as f64)),
        ("merged", jnum(edges.merged as f64)),
        ("purged", jnum(edges.purged as f64)),
        ("reweighed", jnum(edges.reweighed as f64)),
        ("labelled", jnum(edges.labelled as f64)),
        ("links", jnum(links.links as f64)),
        ("candidates", jnum(links.candidates as f64)),
        ("threads", jnum(threads.threads as f64)),
        ("members", jnum(threads.members as f64)),
        ("pruned", jnum(pruned as f64)),
        ("ms", jfixed(ms, 1)),
        ("source", jstr("rust")),
    ]))
}

#[cfg(test)]
mod tests {
    //! The decisions in this file a test can hold still: the edge set is a
    //! property of the rows and not of the order they arrive in, a purge takes
    //! everything that pointed at the row it deleted, and a relation label is a
    //! short display term or nothing at all.

    use super::*;

    fn scratch_db(label: &str) -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!(
            "atlas-graph-db-{}-{}",
            label,
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("atlas.db");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
        let store = Store::open(&path).expect("open");
        store.init().expect("init");
        (path, store)
    }

    #[test]
    fn the_edge_set_does_not_depend_on_the_order_the_mentions_arrive_in() {
        // One article with more names than the cap allows, so `_keep_ids()` has to
        // choose: if the choice follows the arrival order, this article's edges
        // move whenever the plan does.
        let mut kind: HashMap<i64, String> = HashMap::new();
        let mut mentions: Vec<(i64, i64)> = Vec::new();
        for entity in 1..=20i64 {
            kind.insert(entity, "PERSON".to_string());
            mentions.push((7, entity));
        }
        let forward = pairs_from_mentions(&mentions, &kind);

        let mut reversed = mentions.clone();
        reversed.reverse();
        let backward = pairs_from_mentions(&reversed, &kind);
        assert_eq!(
            forward, backward,
            "the cap must keep the same names whatever order the rows arrive in"
        );

        // interleaved with a second article, so the grouping is exercised too
        // a second article that shares no name with the first, interleaved with it
        let mut mixed: Vec<(i64, i64)> = Vec::new();
        for id in 101..=103i64 {
            kind.insert(id, "ORG".to_string());
        }
        mixed.push((9, 103));
        for (index, (article, entity)) in mentions.iter().enumerate() {
            if index % 2 == 0 {
                mixed.push((9, 101 + (index as i64 / 2) % 3));
            }
            mixed.push((*article, *entity));
        }
        let shuffled = pairs_from_mentions(&mixed, &kind);
        let of_the_first: BTreeMap<(i64, i64), i64> = shuffled
            .iter()
            .filter(|((a, b), _)| *a <= 20 && *b <= 20)
            .map(|(key, weight)| (*key, *weight))
            .collect();
        assert_eq!(
            of_the_first, forward,
            "an article s edges depend on its own rows, not on the interleaving"
        );
        assert_eq!(
            shuffled.len(),
            forward.len() + 3,
            "the second article contributes its own three pairs"
        );

        // the cap really did bite: 20 names, 12 kept, so 66 pairs, each weighing 1
        assert_eq!(forward.len(), 66);
        assert!(forward.values().all(|weight| *weight == 1));
        assert!(forward.keys().all(|(a, b)| a < b));
    }

    #[test]
    fn a_purged_entity_takes_its_alias_with_it() {
        let (_path, store) = scratch_db("purge-alias");
        let lex = Lex::new();
        let ctx = Context::from_json(&atlas_json_null());
        store
            .exec(
                "INSERT INTO entities(id,name,type,count) VALUES(1,'Read More','MISC',3)",
                &[],
            )
            .expect("junk entity");
        store
            .exec(
                "INSERT INTO entity_alias(alias,entity_id) VALUES('Read More',1)",
                &[],
            )
            .expect("alias");
        let purged = store
            .in_transaction(|tx| super::purge_junk_entities(tx, &lex, &ctx))
            .expect("purge");
        assert_eq!(purged, 1, "page chrome is purged");
        let left = store
            .query_one("SELECT COUNT(*) c FROM entity_alias WHERE entity_id=1", &[])
            .expect("count")
            .and_then(|row| row.int("c"))
            .unwrap_or(-1);
        assert_eq!(
            left, 0,
            "an alias pointing at a deleted row is dead weight that blocks the \
             spelling from ever being stored again"
        );
    }

    fn atlas_json_null() -> Json {
        Json::Null
    }

    #[test]
    fn a_relation_label_is_a_short_display_term_or_nothing() {
        // The rule as `edges.label` was filled before this function existed:
        // whitespace collapsed and cut at 60, nothing else. The "before" half of the
        // comparison, kept here so the hostile strings below are measured against
        // something instead of asserted from memory.
        fn label_the_old_way(raw: &str) -> String {
            raw.split_whitespace()
                .collect::<Vec<&str>>()
                .join(" ")
                .chars()
                .take(LABEL_CHARS)
                .collect()
        }

        // `edges.label` is model output over page text. The rule is `atlas-ingest`'s
        // `enrich::relation_label`, spelled here because `edges` is this crate's
        // table.
        assert_eq!(relation_label("governs"), "governs");
        assert_eq!(relation_label("  member of  "), "member of");
        assert_eq!(relation_label("governs\u{202e}"), "governs");
        assert_eq!(relation_label("governs\u{0007} now"), "governs now");
        assert_eq!(relation_label("two\nlines"), "two lines");
        assert_eq!(relation_label("1234"), "", "no letter, no label");
        assert_eq!(relation_label(""), "");

        // What a hostile answer could get onto a real edge before the rule: markup,
        // a JSON object and a pipe were stored verbatim, and a bidi override came
        // along inside a word that was otherwise fine.
        for hostile in [
            "<img src=x onerror=alert(1)>",
            "{\"a\":1}",
            "partners | rivals",
        ] {
            assert_eq!(
                label_the_old_way(hostile),
                hostile,
                "the old rule stored {} as it was",
                hostile
            );
            assert_eq!(relation_label(hostile), "", "and the rule refuses {}", hostile);
        }
        assert_eq!(
            label_the_old_way("governs\u{202e}"),
            "governs\u{202e}",
            "an override inside a word used to be stored too"
        );

        assert_eq!(
            relation_label(&"x".repeat(200)).chars().count(),
            LABEL_CHARS,
            "a label is a label, not a paragraph"
        );
    }

    /// An alias is a name the graph answers a lookup with, so it is held to the same
    /// rule an entity name is. This is the reachable case: two rows whose
    /// `merge_key` folds them together, one of them carrying a character a reader
    /// cannot see (U+2028, which `canonical_name` reads as whitespace). Without the
    /// coercion the alias row holds the invisible character, and every later name
    /// lookup inherits it.
    #[test]
    fn a_folded_variant_does_not_carry_an_invisible_character_into_its_alias() {
        let (_path, store) = scratch_db("merge-alias");
        for (id, name, count) in [
            (1, "Nvidia", 9),
            (2, "Nvidia\u{2028}Inc", 2),
        ] {
            store
                .exec(
                    "INSERT INTO entities(id,name,type,count) VALUES(?,?,'ORG',?)",
                    &[int(id), text(name), int(count)],
                )
                .expect("entity");
        }
        let merged = store
            .in_transaction(super::merge_variants)
            .expect("merge");
        assert_eq!(merged, 1, "the variant is folded into the survivor");
        let alias = store
            .query_one("SELECT alias FROM entity_alias", &[])
            .expect("alias")
            .and_then(|row| row.text("alias").map(|value| value.to_string()))
            .unwrap_or_default();
        assert_eq!(
            alias, "Nvidia Inc",
            "the alias is a name a reader can see, spelled as the app spells names"
        );
        assert!(
            !alias.contains('\u{2028}'),
            "no invisible character in a name the graph answers with"
        );
    }
}
