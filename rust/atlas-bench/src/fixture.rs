//! A temporary archive of the size a real ATLAS install reaches, seeded through
//! the real schema.
//!
//! Decisions: the shape of the synthetic archive (5 000 articles, 20 000
//! entities, 60 000 article-to-article links, 60 000 entity-to-article links,
//! 60 000 entity-to-entity edges), and that it is built by the same
//! [`atlas_store::Store`] the app uses — one `init()`, then bulk inserts in one
//! transaction each.
//!
//! What it is **not**: a classifier. The filler words, the city names and the ISO
//! codes below are data for a synthetic archive, the way the reference gazetteer
//! is data. Nothing here decides what counts as news, who publishes it, or which
//! host may be read — the local model and the user's topics do that, and this
//! file never runs in the product.
//!
//! It does not: measure anything, print, or keep the archive after the harness
//! ends ([`Archive::drop`] removes the directory it created).

use std::path::{Path, PathBuf};

use atlas_store::{SqlValue, Store};

/// The archive the numbers in `docs/PERFORMANCE.md` describe.
pub const ARTICLES: usize = 5_000;
pub const ENTITIES: usize = 20_000;
pub const LINKS: usize = 60_000;
pub const ARTICLE_ENTITIES: usize = 60_000;
pub const EDGES: usize = 60_000;
pub const SOURCES: usize = 20;
pub const TOPICS: usize = 10;
pub const CATEGORIES: usize = 8;

/// Filler vocabulary for synthetic article text. Neutral words on purpose: this
/// is prose-shaped data for a database, not a filter over anything.
const WORDS: [&str; 49] = [
    "report", "figure", "market", "budget", "council", "session", "proposal",
    "industry", "region", "meeting", "figure", "deal", "review", "panel",
    "growth", "quarter", "office", "plan", "record", "level", "share",
    "board", "vote", "draft", "measure", "estimate", "output", "trade",
    "supply", "demand", "policy", "study", "survey", "notice", "schedule",
    "sector", "output", "index", "contract", "works", "signals", "briefing",
    "minutes", "figures", "team", "agency", "office", "quarter", "review",
];

/// Geography for synthetic rows: a city, its country, and its centroid.
const PLACES: [(&str, &str, f64, f64); 8] = [
    ("Berlin", "DEU", 52.52, 13.40),
    ("Paris", "FRA", 48.86, 2.35),
    ("Rome", "ITA", 41.90, 12.50),
    ("Madrid", "ESP", 40.42, -3.70),
    ("Warsaw", "POL", 52.23, 21.01),
    ("Tokyo", "JPN", 35.68, 139.69),
    ("London", "GBR", 51.51, -0.13),
    ("Washington", "USA", 38.90, -77.04),
];

/// What the seed wrote, so a report can say the archive really is that size.
#[derive(Clone, Copy, Debug, Default)]
pub struct SeedReport {
    pub articles: usize,
    pub entities: usize,
    pub links: usize,
    pub article_entities: usize,
    pub edges: usize,
    pub threads: usize,
    pub bytes: u64,
}

/// A temp root, the store every path asks, and what the seed wrote. Dropped at
/// the end of a run.
///
/// The shared `atlas_web::AppState` that used to sit here went with the web face:
/// the harness now measures the store and the pipeline, which are the two things
/// the native window and the collector actually run.
pub struct Archive {
    pub root: PathBuf,
    pub store: Store,
    pub seed: SeedReport,
}

impl Archive {
    /// Build the archive at a path the caller keeps (`--keep`), for a person who
    /// wants to run `EXPLAIN QUERY PLAN` against the same rows the table came from.
    pub fn at(root: PathBuf, name: &str, seed_threads: bool) -> Archive {
        let _ = name;
        Archive::build_in(root, seed_threads)
    }

    /// Build the archive under `<temp>/atlas-bench-<name>-<pid>-<nanos>`.
    ///
    /// The directory is this harness's own: it is created here, seeded here and
    /// removed by [`Drop`]. Nothing outside it is written.
    pub fn build(name: &str, seed_threads: bool) -> Archive {
        Archive::build_in(temp_root(name), seed_threads)
    }

    fn build_in(root: PathBuf, seed_threads: bool) -> Archive {
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).expect("data dir");
        std::fs::write(
            root.join("data").join("countries.json"),
            countries_json(),
        )
        .expect("countries.json");
        let store = Store::open(root.join("data").join("atlas.db")).expect("store opens");
        store.init().expect("schema applies");
        // No `public` root and no monitor schema any more: both belonged to the
        // retired HTTP face. The store schema is the window's schema.
        let mut seed = seed_archive(&store);
        if seed_threads {
            // The collector groups stories after a pass; a warm archive has
            // threads, so a thread rebuild is measured against one that does.
            let ctx = atlas_graph::graphdb::Context::from_json(&empty_context());
            let done = atlas_graph::graphlink::recompute_threads(&store, &ctx, 0.20, 40)
                .expect("thread rebuild");
            seed.threads = done.threads;
        }
        seed.bytes = dir_bytes(&root.join("data"));
        Archive { root, store, seed }
    }

    /// The store behind this archive.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Drop one index, so a path can be measured with and without it.
    ///
    /// Used for the A/B a change needs: `--drop-index idx_article_threads_thread`
    /// measures the same statement on the same rows with the index gone, which is
    /// the "before" number — no revert, no second build, no other machine.
    pub fn drop_index(&self, name: &str) {
        self.store()
            .exec(&format!("DROP INDEX IF EXISTS {}", name), &[])
            .expect("drop index");
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// An `atlas-lib` JSON object with no places, countries or categories: the same
/// empty context `graph.py` is called with when the caller has no gazetteer.
pub fn empty_context() -> atlas_lib::json::Json {
    atlas_lib::json::parse(r#"{"places":[],"countries":{},"categories":{},"tags":{}}"#)
        .expect("a literal JSON object")
}

/// A fresh temp root for this process and name.
pub fn temp_root(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // The clock is not enough on its own: two test threads can ask for the same
    // name in the same nanosecond, land in one directory, and seed it twice.
    // A counter in the path makes every archive this process builds its own.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "atlas-bench-{}-{}-{}-{}",
        name,
        std::process::id(),
        nanos,
        seq
    ))
}

/// Bytes under one directory (the database file, its WAL, the log).
pub fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// A deterministic generator: the same archive on every run, on every machine.
pub struct Rng(u64);

/// One generator, seeded: the same bytes on every run and every machine.
pub fn rng(seed: u64) -> Rng {
    Rng::new(seed)
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// One sentence-ish run of filler words.
fn phrase(rng: &mut Rng, words: usize) -> String {
    let mut out = String::with_capacity(words * 8);
    for index in 0..words {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(WORDS[rng.below(WORDS.len())]);
    }
    out
}

/// Roughly `bytes` of paragraph-separated filler, so `content` has the size a
/// stored page has and FTS/LIKE/serialisation are measured on real volume.
pub fn body(rng: &mut Rng, bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 64);
    while out.len() < bytes {
        out.push_str(&phrase(rng, 18));
        out.push_str(". ");
        if out.len() % 400 < 30 {
            out.push_str("\n\n");
        }
    }
    out
}

fn countries_json() -> String {
    let mut rows: Vec<String> = Vec::new();
    for (name, iso, lat, lon) in PLACES {
        rows.push(format!(
            r#"{{"name":"{}","iso":"{}","lat":{},"lon":{}}}"#,
            name, iso, lat, lon
        ));
    }
    format!("[{}]", rows.join(","))
}

/// Insert the whole archive. Every table is written by one prepared statement
/// inside one transaction, which is how the collector writes a sweep.
pub fn seed_archive(store: &Store) -> SeedReport {
    let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
    let now = atlas_store::clock::epoch_secs_now();

    // sources
    let mut sources: Vec<Vec<SqlValue>> = Vec::with_capacity(SOURCES);
    for index in 0..SOURCES {
        sources.push(vec![
            SqlValue::Integer(index as i64 + 1),
            SqlValue::Text(format!("Outlet {}", index + 1)),
            SqlValue::Text(format!("https://outlet{}.example.invalid", index + 1)),
            SqlValue::Text(format!("https://outlet{}.example.invalid/feed", index + 1)),
            SqlValue::Text("en".to_string()),
        ]);
    }
    store
        .exec_many(
            "INSERT INTO sources(id,name,url,rss,lang,active,kind) VALUES(?,?,?,?,?,1,'base')",
            &sources,
        )
        .expect("sources");

    // categories, topics
    let mut categories: Vec<Vec<SqlValue>> = Vec::with_capacity(CATEGORIES);
    for index in 0..CATEGORIES {
        categories.push(vec![
            SqlValue::Integer(index as i64 + 1),
            SqlValue::Text(format!("Category {}", index + 1)),
            SqlValue::Text("kind".to_string()),
            SqlValue::Text(String::new()),
            SqlValue::Text(atlas_store::now_iso()),
        ]);
    }
    store
        .exec_many(
            "INSERT INTO categories(id,name,kind,description,created) VALUES(?,?,?,?,?)",
            &categories,
        )
        .expect("categories");

    let mut topics: Vec<Vec<SqlValue>> = Vec::with_capacity(TOPICS);
    for index in 0..TOPICS {
        topics.push(vec![
            SqlValue::Integer(index as i64 + 1),
            SqlValue::Text(format!("Topic {}", index + 1)),
            SqlValue::Text("report, figure, budget, market".to_string()),
            SqlValue::Text(String::new()),
            SqlValue::Integer(1),
            SqlValue::Integer(1),
            SqlValue::Text(atlas_store::now_iso()),
            SqlValue::Integer(0),
        ]);
    }
    store
        .exec_many(
            "INSERT INTO topics(id,name,keywords,exclude,min_matches,active,created,hits) \
             VALUES(?,?,?,?,?,?,?,?)",
            &topics,
        )
        .expect("topics");

    // articles
    let mut articles: Vec<Vec<SqlValue>> = Vec::with_capacity(ARTICLES);
    let mut countries: Vec<Vec<SqlValue>> = Vec::with_capacity(ARTICLES);
    let mut tags: Vec<Vec<SqlValue>> = Vec::with_capacity(ARTICLES * 3);
    let mut article_topics: Vec<Vec<SqlValue>> = Vec::with_capacity(ARTICLES);
    for index in 0..ARTICLES {
        let id = index as i64 + 1;
        // 70 hours of reporting, a spread of older rows the retention pass can
        // actually delete, and a stable order.
        let hours_ago = if index % 100 == 0 { 40 * 24 } else { index % 70 };
        let published =
            atlas_store::iso_from_epoch(now - hours_ago as i64 * 3_600 - (index as i64 % 3_600));
        let (place, iso, lat, lon) = PLACES[index % PLACES.len()];
        let title = format!(
            "{} {} {}",
            phrase(&mut rng, 6),
            WORDS[rng.below(WORDS.len())],
            id
        );
        let summary = phrase(&mut rng, 22);
        let content = body(&mut rng, 2_400);
        articles.push(vec![
            SqlValue::Integer(id),
            SqlValue::Integer((index % SOURCES) as i64 + 1),
            SqlValue::Text(format!("https://outlet{}.example.invalid/story/{}", index % SOURCES + 1, id)),
            SqlValue::Text(title),
            SqlValue::Text(summary),
            SqlValue::Text(content),
            SqlValue::Text(format!("Author {}", index % 40 + 1)),
            SqlValue::Text(published),
            SqlValue::Text(atlas_store::now_iso()),
            SqlValue::Text("en".to_string()),
            SqlValue::Text(iso.to_string()),
            SqlValue::Real(lat),
            SqlValue::Real(lon),
            SqlValue::Text(format!("hash{:08}", index)),
            SqlValue::Integer(1),
            SqlValue::Integer(0),
            SqlValue::Integer(if index % 3 == 0 { 1 } else { 0 }),
            SqlValue::Text(if index % 4 == 0 {
                format!("Outlet {}", index % SOURCES + 1)
            } else {
                String::new()
            }),
            SqlValue::Text(place.to_string()),
            SqlValue::Text("place".to_string()),
        ]);
        countries.push(vec![
            SqlValue::Integer(id),
            SqlValue::Text(iso.to_string()),
            SqlValue::Integer(1),
        ]);
        for k in 0..3 {
            tags.push(vec![
                SqlValue::Integer(id),
                SqlValue::Text(format!("tag{}", (index + k * 7) % 40)),
                SqlValue::Real(1.0),
                SqlValue::Text("model".to_string()),
                SqlValue::Text(atlas_store::now_iso()),
            ]);
        }
        if index % 10 == 0 {
            article_topics.push(vec![
                SqlValue::Integer(id),
                SqlValue::Integer((index / 10 % TOPICS) as i64 + 1),
                SqlValue::Real(0.75),
                SqlValue::Text("report".to_string()),
            ]);
        }
    }
    store
        .exec_many(
            "INSERT INTO articles(id,source_id,url,title,summary,content,author,published,fetched,\
             lang,iso,lat,lon,hash,relevant,dupe,full_text,publisher,place,geo_src) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            &articles,
        )
        .expect("articles");
    store
        .exec_many(
            "INSERT INTO article_countries(article_id,iso,is_primary) VALUES(?,?,?)",
            &countries,
        )
        .expect("article_countries");
    store
        .exec_many(
            "INSERT INTO article_tags(article_id,tag,weight,source,created) VALUES(?,?,?,?,?)",
            &tags,
        )
        .expect("article_tags");
    store
        .exec_many(
            "INSERT INTO article_topics(article_id,topic_id,score,matched) VALUES(?,?,?,?)",
            &article_topics,
        )
        .expect("article_topics");

    // The full-text index is written by hand, exactly as the collector does it.
    store
        .exec(
            "INSERT INTO article_fts(rowid,title,summary,content) \
             SELECT id,title,summary,content FROM articles",
            &[],
        )
        .expect("fts");

    // entities
    let mut entities: Vec<Vec<SqlValue>> = Vec::with_capacity(ENTITIES);
    for index in 0..ENTITIES {
        entities.push(vec![
            SqlValue::Integer(index as i64 + 1),
            SqlValue::Text(format!("Entity {}", index + 1)),
            SqlValue::Text(if index % 3 == 0 { "org" } else { "person" }.to_string()),
            SqlValue::Integer((index % CATEGORIES) as i64 + 1),
            SqlValue::Integer((index % 400) as i64 + 1),
            SqlValue::Text(atlas_store::iso_from_epoch(now - 90 * 24 * 3_600)),
            SqlValue::Text(atlas_store::now_iso()),
        ]);
    }
    store
        .exec_many(
            "INSERT INTO entities(id,name,type,category_id,count,first_seen,last_seen) \
             VALUES(?,?,?,?,?,?,?)",
            &entities,
        )
        .expect("entities");

    // entity_alias: one alias per entity, so the alias lookup is a real lookup.
    let mut aliases: Vec<Vec<SqlValue>> = Vec::with_capacity(ENTITIES);
    for index in 0..ENTITIES {
        aliases.push(vec![
            SqlValue::Text(format!("E{}", index + 1)),
            SqlValue::Integer(index as i64 + 1),
        ]);
    }
    store
        .exec_many("INSERT INTO entity_alias(alias,entity_id) VALUES(?,?)", &aliases)
        .expect("entity_alias");

    // article ↔ entity links
    let mut article_entities: Vec<Vec<SqlValue>> = Vec::with_capacity(ARTICLE_ENTITIES);
    let per_article = ARTICLE_ENTITIES / ARTICLES; // 12
    for index in 0..ARTICLES {
        for k in 0..per_article {
            let entity = (index * per_article + k) % ENTITIES + 1;
            article_entities.push(vec![
                SqlValue::Integer(index as i64 + 1),
                SqlValue::Integer(entity as i64),
                SqlValue::Real(1.0 + (k % 3) as f64 * 0.5),
            ]);
        }
    }
    store
        .exec_many(
            "INSERT OR IGNORE INTO article_entities(article_id,entity_id,weight) VALUES(?,?,?)",
            &article_entities,
        )
        .expect("article_entities");

    // article ↔ article links (the thread/related layer)
    let mut links: Vec<Vec<SqlValue>> = Vec::with_capacity(LINKS);
    let per_article_links = LINKS / ARTICLES; // 12
    for index in 0..ARTICLES {
        let a = index as i64 + 1;
        for k in 0..per_article_links {
            let mut b = (index * 7 + k * 13) % ARTICLES + 1;
            if b as i64 == a {
                b = (b % ARTICLES) + 1;
            }
            links.push(vec![
                SqlValue::Integer(a),
                SqlValue::Integer(b as i64),
                SqlValue::Integer((k + 2) as i64),
                SqlValue::Real(0.10 + (k as f64) * 0.06 + (index % 7) as f64 * 0.01),
                SqlValue::Text("shared terms".to_string()),
            ]);
        }
    }
    store
        .exec_many(
            "INSERT OR IGNORE INTO article_links(a,b,shared,score,why) VALUES(?,?,?,?,?)",
            &links,
        )
        .expect("article_links");

    // entity ↔ entity edges (the graph view)
    let mut edges: Vec<Vec<SqlValue>> = Vec::with_capacity(EDGES);
    for k in 0..EDGES {
        // `(a, b)` is the primary key, so the second endpoint has to move with
        // the lap as well: `k % ENTITIES` repeats every 20 000 steps, and the
        // lap number folded into `b` keeps every one of the 60 000 edges distinct.
        let a = k % ENTITIES + 1;
        let mut b = (k * 101 + (k / ENTITIES) * 7 + 7) % ENTITIES + 1;
        if b == a {
            b = b % ENTITIES + 1;
        }
        edges.push(vec![
            SqlValue::Integer(a as i64),
            SqlValue::Integer(b as i64),
            SqlValue::Real(1.0 + (k % 50) as f64 / 10.0),
            SqlValue::Text("cooccur".to_string()),
            SqlValue::Text(String::new()),
        ]);
    }
    store
        .exec_many(
            "INSERT OR IGNORE INTO edges(a,b,weight,type,label) VALUES(?,?,?,?,?)",
            &edges,
        )
        .expect("edges");

    let counted = |sql: &str| -> usize {
        store
            .query_one(sql, &[])
            .ok()
            .flatten()
            .and_then(|row| row.int("c"))
            .unwrap_or(0) as usize
    };
    SeedReport {
        articles: counted("SELECT COUNT(*) c FROM articles"),
        entities: counted("SELECT COUNT(*) c FROM entities"),
        links: counted("SELECT COUNT(*) c FROM article_links"),
        article_entities: counted("SELECT COUNT(*) c FROM article_entities"),
        edges: counted("SELECT COUNT(*) c FROM edges"),
        threads: counted("SELECT COUNT(*) c FROM threads"),
        bytes: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_the_same_twice_and_looks_like_prose() {
        let mut first = Rng::new(7);
        let mut second = Rng::new(7);
        let text = body(&mut first, 500);
        assert_eq!(text, body(&mut second, 500));
        assert!(text.len() >= 500);
        assert!(text.contains(' '));
        assert!(!text.contains("  "), "no double spaces");
    }

    #[test]
    fn the_archive_really_is_the_declared_size() {
        // A small shape check, not the 5 000-article run: the counts in
        // `docs/PERFORMANCE.md` are asserted by the harness itself when it prints
        // the archive size next to the numbers.
        let archive = Archive::build("size", true);
        assert_eq!(archive.seed.articles, ARTICLES);
        assert_eq!(archive.seed.entities, ENTITIES);
        assert_eq!(archive.seed.links, LINKS);
        assert_eq!(archive.seed.article_entities, ARTICLE_ENTITIES);
        assert_eq!(archive.seed.edges, EDGES);
        assert!(archive.seed.threads > 0, "a warm archive has threads");
        assert!(archive.seed.bytes > 1_000_000, "the database has pages");
    }
}
