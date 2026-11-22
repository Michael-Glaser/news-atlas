//! Databases a test can trust: one consistent, one with every inconsistency this crate
//! claims to find, plus the doubles a check needs (the five engines, the model).
//!
//! Decisions: each inconsistency lives in its own article so a test can name the row it
//! expects to move, a fixture is built through `atlas-store` (the same schema and pragmas
//! as an archive), and every fixture lives in its own temporary directory so parallel
//! tests cannot see each other.
//!
//! It does not: ship in a release build (`#[cfg(test)]` on the module), reach the network,
//! or call a real engine — [`doubles`] answers the engines' questions deterministically so
//! a check is tested without a process spawn.

mod doubles;

pub use doubles::{model_answer, pipeline, FakeModel, FakeRules};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use atlas_store::{SqlValue, Store};

static COUNTER: AtomicU64 = AtomicU64::new(0);


/// A fresh directory with an `atlas.db` in it, schema created, no rows.
pub fn empty_database(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "atlas-migrate-{}-{}-{}",
        tag,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("fixture directory");
    let path = root.join("atlas.db");
    let store = Store::open(&path).expect("open fixture");
    store.init().expect("fixture schema");
    path
}

/// Open a fixture for writing.
pub fn open(path: &Path) -> Store {
    Store::open(path).expect("open fixture")
}

fn now() -> String {
    atlas_store::now_iso()
}

/// A database a correct archive looks like: full text complete, references intact, one
/// spelling per outlet, a place the text names, labels that stand.
pub fn clean_database(tag: &str) -> PathBuf {
    let path = empty_database(tag);
    let store = open(&path);
    store
        .exec(
            "INSERT INTO sources(id,name,url) VALUES(1,'Example News','https://example.com')",
            &[],
        )
        .expect("source");
    let articles = [
        (
            1,
            "Berlin summit ends with a pledge",
            "Berlin",
            "https://example.com/a",
            "Example News",
        ),
        (
            2,
            "Chip exports reach a record",
            "",
            "https://example.com/b",
            "Example News",
        ),
    ];
    for (id, title, place, url, publisher) in articles {
        store
            .exec(
                "INSERT INTO articles(id,source_id,url,title,summary,published,publisher,place,\
                 iso,geo_conf,geo_src,relevant,dupe,full_text) VALUES(?,1,?,?,?,?,?,?,'DEU',0.9,\
                 'city',1,0,1)",
                &[
                    SqlValue::Integer(id),
                    SqlValue::Text(url.to_string()),
                    SqlValue::Text(title.to_string()),
                    SqlValue::Text(format!("{} — a summary.", title)),
                    SqlValue::Text(now()),
                    SqlValue::Text(publisher.to_string()),
                    SqlValue::Text(place.to_string()),
                ],
            )
            .expect("article");
        store
            .exec(
                "INSERT INTO article_fts(rowid,title,summary,content) VALUES(?,?,?,'')",
                &[
                    SqlValue::Integer(id),
                    SqlValue::Text(title.to_string()),
                    SqlValue::Text(format!("{} — a summary.", title)),
                ],
            )
            .expect("fts row");
    }
    let entities = [
        (1, "Berlin", "LOC"),
        (2, "Aurora Systems", "ORG"),
    ];
    for (id, name, kind) in entities {
        store
            .exec(
                "INSERT INTO entities(id,name,type,count,category_id,first_seen,last_seen) \
                 VALUES(?,?,?,1,3,?,?)",
                &[
                    SqlValue::Integer(id),
                    SqlValue::Text(name.to_string()),
                    SqlValue::Text(kind.to_string()),
                    SqlValue::Text(now()),
                    SqlValue::Text(now()),
                ],
            )
            .expect("entity");
        store
            .exec(
                "INSERT INTO article_entities(article_id,entity_id,weight) VALUES(?,?,1)",
                &[SqlValue::Integer(1), SqlValue::Integer(id)],
            )
            .expect("link");
    }
    store
        .exec(
            "INSERT INTO edges(a,b,weight,type) VALUES(1,2,1,'cooccur')",
            &[],
        )
        .expect("edge");
    store
        .exec(
            "INSERT INTO topics(id,name,keywords,min_matches,active) \
             VALUES(1,'Semiconductors','chip, exports',1,1)",
            &[],
        )
        .expect("topic");
    path
}

/// A database with one of every inconsistency the crate claims to find.
pub fn database_with_inconsistencies(tag: &str) -> PathBuf {
    let path = empty_database(tag);
    let store = open(&path);
    store
        .exec(
            "INSERT INTO sources(id,name,url) VALUES(1,'Example News','https://example.com')",
            &[],
        )
        .expect("source");
    store
        .exec(
            "INSERT INTO categories(id,name,kind) VALUES(1,'Technology','TECH'),\
             (2,'Organizations','ORG')",
            &[],
        )
        .expect("categories");

    // 1. an article whose headline changed after the index was written (stale index)
    // 2. an article with no index entry at all (missing)
    // 3. a supported place, a tag variant and an outlet tag
    // 4. a stored place its own text never mentions (replaced by the engine)
    // 5. a stored place its own text never mentions and the engine cannot replace
    // 6. a hidden duplicate still marked relevant
    let articles: [(i64, &str, &str, &str, &str, &str, &str); 6] = [
        (
            1,
            "Quiet fishing town reports a mild autumn",
            "Lisbon",
            "https://news.example.com/one",
            "Folha",
            "",
            "",
        ),
        (
            2,
            "Harvest festival draws a crowd",
            "",
            "https://news.example.com/two",
            "Folha de S.Paulo",
            "",
            "",
        ),
        (
            3,
            "Shenzhen factory output rises",
            "Shenzhen",
            "https://news.example.com/three",
            "Reuters",
            "Shenzhen",
            "CHN",
        ),
        (
            4,
            "Aurora Systems opens a Berlin office",
            "Washington",
            "https://news.example.com/four",
            "NBC News",
            "",
            "",
        ),
        (
            5,
            "Nbcnews hires two editors",
            "Lisbon",
            "https://other.example.net/five",
            "Nbcnews",
            "",
            "",
        ),
        (
            6,
            "A short update on the harbour",
            "",
            "https://other.example.net/six",
            "Yahoo Finance",
            "",
            "",
        ),
    ];
    for (id, title, place, url, publisher, _iso, _src) in articles {
        store
            .exec(
                "INSERT INTO articles(id,source_id,url,title,summary,published,publisher,place,\
                 geo_conf,geo_src,relevant,dupe,full_text) \
                 VALUES(?,1,?,?,?,?,?,?,0.9,'city',1,0,0)",
                &[
                    SqlValue::Integer(id),
                    SqlValue::Text(url.to_string()),
                    SqlValue::Text(title.to_string()),
                    SqlValue::Text(format!("{} BODYMARKER body text.", title)),
                    SqlValue::Text(now()),
                    SqlValue::Text(publisher.to_string()),
                    SqlValue::Text(place.to_string()),
                ],
            )
            .expect("article");
    }
    store
        .exec("UPDATE articles SET dupe=1 WHERE id=6", &[])
        .expect("hidden duplicate");
    // 3. bodies that name their place, so the substring test can confirm them
    store
        .exec(
            "UPDATE articles SET content='Shenzhen is where the assembly line stands. \
             BODYMARKER' WHERE id=3",
            &[],
        )
        .expect("supported place body");
    store
        .exec(
            "UPDATE articles SET summary='', content='The office in Berlin opened on Monday. \
             BODYMARKER' WHERE id=4",
            &[],
        )
        .expect("unsupported place body");
    store
        .exec(
            "UPDATE articles SET content='Two editors join the desk this week. BODYMARKER' \
             WHERE id=5",
            &[],
        )
        .expect("unnamed place body");

    // The index: articles 1, 3, 4, 5 have rows; 1 is stale (different words indexed),
    // 2 has none, and 999 is an entry with no article.
    for id in [1i64, 3, 4, 5] {
        let title = if id == 1 {
            "An older headline that no longer exists".to_string()
        } else {
            format!("headline for article {} about Berlin and Shenzhen ports", id)
        };
        store
            .exec(
                "INSERT INTO article_fts(rowid,title,summary,content) VALUES(?,?,?,'')",
                &[
                    SqlValue::Integer(id),
                    SqlValue::Text(title.clone()),
                    SqlValue::Text(format!("{} summary", title)),
                ],
            )
            .expect("fts row");
    }
    store
        .exec(
            "INSERT INTO article_fts(rowid,title,summary,content) VALUES(999,'orphan','','')",
            &[],
        )
        .expect("orphan fts row");

    // Entities: one typed, one untyped, one nothing points at.
    for (id, name, kind, category) in [
        (1i64, "Shenzhen", "LOC", 1i64),
        (2, "Aurora Systems", "ORG", 2),
        (3, "Nvidia", "", 2),
        (4, "Nobody Links This", "ORG", 2),
        (5, "Also Unlinked", "ORG", 2),
    ] {
        store
            .exec(
                "INSERT INTO entities(id,name,type,count,category_id,first_seen,last_seen) \
                 VALUES(?,?,?,1,?,?,?)",
                &[
                    SqlValue::Integer(id),
                    SqlValue::Text(name.to_string()),
                    SqlValue::Text(kind.to_string()),
                    SqlValue::Integer(category),
                    SqlValue::Text(now()),
                    SqlValue::Text(now()),
                ],
            )
            .expect("entity");
    }
    for (article, entity) in [(3i64, 1i64), (3, 2), (3, 4), (4, 2), (4, 3)] {
        store
            .exec(
                "INSERT INTO article_entities(article_id,entity_id,weight) VALUES(?,?,1)",
                &[SqlValue::Integer(article), SqlValue::Integer(entity)],
            )
            .expect("link");
    }
    // 4. references to articles, entities, topics, categories, threads and aliases that
    //    are not there
    store
        .exec("UPDATE entities SET category_id=NULL WHERE id=3", &[])
        .expect("an entity with no category yet");
    for (article, entity) in [(999i64, 1i64), (1, 999)] {
        store
            .exec(
                "INSERT INTO article_entities(article_id,entity_id,weight) VALUES(?,?,1)",
                &[SqlValue::Integer(article), SqlValue::Integer(entity)],
            )
            .expect("dangling link");
    }
    store
        .exec(
            "INSERT INTO article_links(a,b,shared,score,why) VALUES(999,1,1,0.4,'shared: x'),\
             (1,2,1,0.3,'shared: y')",
            &[],
        )
        .expect("links");
    store
        .exec(
            "INSERT INTO threads(id,title,size,updated) VALUES(1,'Thread',1,?)",
            &[SqlValue::Text(now())],
        )
        .expect("thread");
    store
        .exec(
            "INSERT INTO article_threads(article_id,thread_id) VALUES(999,1),(1,999)",
            &[],
        )
        .expect("dangling thread refs");
    store
        .exec(
            "INSERT INTO article_topics(article_id,topic_id,score,matched) VALUES(999,1,1,'x')",
            &[],
        )
        .expect("dangling topic ref");
    store
        .exec(
            "INSERT INTO article_categories(article_id,category_id) VALUES(999,1)",
            &[],
        )
        .expect("dangling category ref");
    store
        .exec(
            "INSERT INTO article_countries(article_id,iso,is_primary) VALUES(999,'USA',0)",
            &[],
        )
        .expect("dangling country ref");
    store
        .exec(
            "INSERT INTO entity_alias(alias,entity_id) VALUES('Ghost',999)",
            &[],
        )
        .expect("dangling alias");
    store
        .exec(
            "INSERT INTO article_tags(article_id,tag,weight,source,created) \
             VALUES(1,'AI CHIPS!',1,'ai',?),(3,'Shenzhen',1,'ai',?),(3,'shenzhen',1,'ai',?),\
             (3,'Reuters',1,'ai',?),(1,'plain tag',1,'ai',?),(999,'stale tag',1,'ai',?)",
            &[
                SqlValue::Text(now()),
                SqlValue::Text(now()),
                SqlValue::Text(now()),
                SqlValue::Text(now()),
                SqlValue::Text(now()),
                SqlValue::Text(now()),
            ],
        )
        .expect("tags");

    // 5. an edge stored the wrong way round, and one from an entity to itself
    store
        .exec(
            "INSERT INTO edges(a,b,weight,type) VALUES(3,2,1,'cooccur'),(4,4,1,'cooccur'),\
             (1,999,1,'cooccur')",
            &[],
        )
        .expect("bad edges");
    path
}

