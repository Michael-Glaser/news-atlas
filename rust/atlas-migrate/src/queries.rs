//! The documented statements, written once.
//!
//! Every query this crate runs lives here, named after the invariant it reads, with
//! the table list of `docs/DATA-MODEL.md` §"Tables" as the allowlist. A table name is
//! schema, so it is the only text ever assembled into a statement; every *value* is
//! bound as a `?`.
//!
//! It does not: decide what a disagreement means ([`crate::findings`]), fix anything
//! ([`crate::normalize`]), or touch a row.

/// The user tables of the archive, in dependency order. `article_fts` is listed
/// because its row count is part of the report; its shadow tables are SQLite's and
/// are not counted.
pub const TABLES: &[&str] = &[
    "sources",
    "articles",
    "article_fts",
    "entities",
    "article_entities",
    "edges",
    "categories",
    "article_categories",
    "entity_alias",
    "article_links",
    "threads",
    "article_threads",
    "article_countries",
    "topics",
    "article_topics",
    "article_tags",
    "meta",
    "alerts",
    "monitors",
    "monitor_seen",
    "enrich_checked",
];

/// Is one table there at all? A database without `articles` is not an archive.
pub const TABLE_EXISTS: &str = "SELECT name FROM sqlite_master WHERE type='table' AND name=?";

/// How many rows one table holds.
pub fn count(table: &str) -> String {
    debug_assert!(TABLES.contains(&table));
    format!("SELECT COUNT(*) AS n FROM \"{}\"", table)
}

/// A deterministic sample: the first rows by rowid, never a random draw, so two
/// runs of `verify` on the same archive print the same report.
pub fn sample(table: &str) -> String {
    debug_assert!(TABLES.contains(&table));
    format!("SELECT * FROM \"{}\" ORDER BY rowid LIMIT ?", table)
}

// -- full-text health -------------------------------------------------------

/// Documents the index actually holds (`article_fts_docsize` has one row per
/// indexed document; a scan of `article_fts` reads the content table instead).
pub const FTS_DOCUMENTS: &str = "SELECT COUNT(*) AS n FROM article_fts_docsize";

/// Articles with no index entry: search cannot find them however they are written.
pub const FTS_MISSING: &str = "SELECT a.id AS id FROM articles a \
     LEFT JOIN article_fts_docsize d ON d.id=a.id WHERE d.id IS NULL ORDER BY a.id";

/// Index entries whose article is gone: a delete that skipped the index.
pub const FTS_ORPHANS: &str = "SELECT d.id AS id FROM article_fts_docsize d \
     LEFT JOIN articles a ON a.id=d.id WHERE a.id IS NULL ORDER BY d.id";

/// `ORDER BY rank` — the query every search path uses. `query_only` allows this:
/// it reads the index, it does not write it.
pub const FTS_RANK: &str = "SELECT rowid AS id, bm25(article_fts) AS score FROM article_fts \
     WHERE article_fts MATCH ? ORDER BY rank LIMIT ?";

/// `snippet()` — the second thing the search UI needs from the index.
pub const FTS_SNIPPET: &str = "SELECT rowid AS id, snippet(article_fts, 0, '[', ']', '…', 8) AS s \
     FROM article_fts WHERE article_fts MATCH ? ORDER BY rank LIMIT ?";

/// Is one term in the index for one row? The staleness probe.
pub const FTS_ROW_FOR_TERM: &str = "SELECT 1 AS x FROM article_fts \
     WHERE article_fts MATCH ? AND rowid=? LIMIT 1";

/// FTS5's own verification of the index against its content table. This one writes
/// (it walks the index and reports the first inconsistency), so it only runs on the
/// write connection, before a rebuild.
pub const FTS_INTEGRITY: &str = "INSERT INTO article_fts(article_fts) VALUES('integrity-check')";

/// Rebuild the index from `articles` — FTS5's documented command for an
/// external-content table, and the only correct way to repair it.
pub const FTS_REBUILD: &str = "INSERT INTO article_fts(article_fts) VALUES('rebuild')";

// -- references -------------------------------------------------------------

/// A child table pointing at an article that no longer exists, one statement per
/// table so the report names the table that is wrong.
pub fn dangling_articles(table: &str) -> Option<&'static str> {
    match table {
        "article_entities" => Some(
            "SELECT DISTINCT article_id AS id FROM article_entities \
             WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_tags" => Some(
            "SELECT DISTINCT article_id AS id FROM article_tags \
             WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_countries" => Some(
            "SELECT DISTINCT article_id AS id FROM article_countries \
             WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_topics" => Some(
            "SELECT DISTINCT article_id AS id FROM article_topics \
             WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_categories" => Some(
            "SELECT DISTINCT article_id AS id FROM article_categories \
             WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_threads" => Some(
            "SELECT DISTINCT article_id AS id FROM article_threads \
             WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_links" => Some(
            "SELECT a AS id FROM article_links WHERE a NOT IN (SELECT id FROM articles) \
             UNION SELECT b FROM article_links WHERE b NOT IN (SELECT id FROM articles)",
        ),
        _ => None,
    }
}

/// The `DELETE` that clears one child table of references to missing articles.
/// `article_links` is the only one with two ends.
pub fn clear_dangling_articles(table: &str) -> Option<&'static str> {
    match table {
        "article_entities" => Some(
            "DELETE FROM article_entities WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_tags" => Some(
            "DELETE FROM article_tags WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_countries" => Some(
            "DELETE FROM article_countries WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_topics" => Some(
            "DELETE FROM article_topics WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_categories" => Some(
            "DELETE FROM article_categories WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_threads" => Some(
            "DELETE FROM article_threads WHERE article_id NOT IN (SELECT id FROM articles)",
        ),
        "article_links" => Some(
            "DELETE FROM article_links WHERE a NOT IN (SELECT id FROM articles) \
             OR b NOT IN (SELECT id FROM articles)",
        ),
        _ => None,
    }
}

/// `article_entities.entity_id` pointing at a missing entity.
pub const DANGLING_ENTITIES: &str = "SELECT DISTINCT entity_id AS id FROM article_entities \
     WHERE entity_id NOT IN (SELECT id FROM entities)";
pub const CLEAR_DANGLING_ENTITIES: &str =
    "DELETE FROM article_entities WHERE entity_id NOT IN (SELECT id FROM entities)";

/// `article_topics.topic_id` pointing at a missing topic.
pub const DANGLING_TOPICS: &str = "SELECT DISTINCT topic_id AS id FROM article_topics \
     WHERE topic_id NOT IN (SELECT id FROM topics)";
pub const CLEAR_DANGLING_TOPICS: &str =
    "DELETE FROM article_topics WHERE topic_id NOT IN (SELECT id FROM topics)";

/// `article_categories.category_id` pointing at a missing category.
pub const DANGLING_CATEGORIES: &str = "SELECT DISTINCT category_id AS id FROM article_categories \
     WHERE category_id NOT IN (SELECT id FROM categories)";
pub const CLEAR_DANGLING_CATEGORIES: &str =
    "DELETE FROM article_categories WHERE category_id NOT IN (SELECT id FROM categories)";

/// `article_threads.thread_id` pointing at a missing thread.
pub const DANGLING_THREADS: &str = "SELECT DISTINCT thread_id AS id FROM article_threads \
     WHERE thread_id NOT IN (SELECT id FROM threads)";
pub const CLEAR_DANGLING_THREADS: &str =
    "DELETE FROM article_threads WHERE thread_id NOT IN (SELECT id FROM threads)";

/// `edges` ends pointing at missing entities.
pub const DANGLING_EDGES: &str = "SELECT a AS id FROM edges WHERE a NOT IN (SELECT id FROM entities) \
     UNION SELECT b FROM edges WHERE b NOT IN (SELECT id FROM entities)";
pub const CLEAR_DANGLING_EDGES: &str = "DELETE FROM edges WHERE a NOT IN (SELECT id FROM entities) \
     OR b NOT IN (SELECT id FROM entities)";

/// `entity_alias` rows whose canonical entity is gone.
pub const DANGLING_ALIASES: &str = "SELECT alias AS id FROM entity_alias \
     WHERE entity_id NOT IN (SELECT id FROM entities)";
pub const CLEAR_DANGLING_ALIASES: &str =
    "DELETE FROM entity_alias WHERE entity_id NOT IN (SELECT id FROM entities)";

// -- stored values that contradict the row ---------------------------------

/// Every article with the stored values the checks compare: no text body is read
/// here, only the columns a contradiction is decided from.
pub const ARTICLES_FOR_FIELDS: &str = "SELECT id, url, title, summary, publisher, place, iso, \
     lat, lon, geo_conf, geo_src, relevant, dupe FROM articles ORDER BY id";

/// Articles whose body is non-empty and whose stored place does not occur in the
/// title, summary or body — the place has no evidence in the text it came from.
///
/// Case-insensitive substring, no word list: a stored place that appears in the text
/// is supported and is left alone.
pub const PLACE_UNSUPPORTED: &str = "SELECT id, place, COALESCE(geo_src,'') AS geo_src FROM articles \
     WHERE COALESCE(TRIM(place),'')<>'' AND instr(lower(COALESCE(title,'')||' '||\
     COALESCE(summary,'')||' '||COALESCE(content,'')), lower(TRIM(place)))=0 \
     ORDER BY id";

/// The body text one article has, capped by the caller: the only place a fix reads
/// article text, and it never leaves memory.
pub const ARTICLE_TEXT: &str = "SELECT id, title, summary, substr(COALESCE(content,''),1,3000) AS body, \
     url, publisher, place, iso, geo_conf, geo_src FROM articles WHERE id=?";

/// Publisher spellings, with how many articles carry each, and the delivery host
/// each spelling was stored under.
///
/// The host is the authority of `articles.url`, extracted for grouping only: the
/// canonical form is `atlas-text norm_url`, and nothing here re-derives it.
pub const PUBLISHER_SPELLINGS: &str = "SELECT publisher, \
     CASE WHEN instr(url,'://')>0 THEN substr(url, instr(url,'://')+3) ELSE url END AS rest, \
     COUNT(*) AS n FROM articles WHERE COALESCE(TRIM(publisher),'')<>'' GROUP BY publisher, url";

/// Every article's stored publisher, with the URL it was stored under.
pub const ARTICLE_PUBLISHERS: &str = "SELECT id, COALESCE(url,'') AS url, \
     COALESCE(publisher,'') AS publisher FROM articles ORDER BY id";

/// One publisher value, by article id: the fix binds the modal spelling here — but only
/// while the row still holds the spelling the check saw as a variant.
///
/// The last `AND TRIM(publisher)=?` is what makes `--apply` conditional *per statement*: a
/// plan kept from an earlier run, replayed after the user or the app has since written a
/// third spelling, changes zero rows instead of overwriting a value this run never
/// measured. `TRIM` is there because the spelling groups are built from trimmed values, so
/// a padded row must still match its own group.
pub const SET_PUBLISHER: &str =
    "UPDATE articles SET publisher=? WHERE id=? AND TRIM(publisher)=?";

/// The same, for an article with no publisher at all: a fill may only write into the gap
/// it was planned from, never over a value that arrived in between.
pub const SET_PUBLISHER_IF_EMPTY: &str =
    "UPDATE articles SET publisher=? WHERE id=? AND COALESCE(TRIM(publisher),'')=''";

/// The articles with no publisher stored, up to a bound (each costs one `atlas-text`
/// process), so `verify` cannot spend an afternoon on a column a migration emptied.
pub const ARTICLES_WITHOUT_PUBLISHER: &str = "SELECT id, COALESCE(url,'') AS url FROM articles \
     WHERE COALESCE(TRIM(publisher),'')='' ORDER BY id LIMIT ?";

/// How many articles have no publisher at all, so a bounded fill can say how much of the
/// gap it left: a silent bound reads like a complete pass.
pub const COUNT_WITHOUT_PUBLISHER: &str = "SELECT COUNT(*) AS n FROM articles \
     WHERE COALESCE(TRIM(publisher),'')=''";

/// One article's place, by id — conditional on the place the check found unsupported, so a
/// replayed plan cannot overwrite a place that was fixed (or re-derived) in between.
///
/// The bound values are, in order: the six new columns, the article id, and the stored
/// place the check rejected.
pub const SET_PLACE: &str = "UPDATE articles SET place=?, iso=?, lat=?, lon=?, geo_conf=?, geo_src=? \
     WHERE id=? AND TRIM(place)=?";

/// The documented default when no place is found (`docs/DATA-MODEL.md` §1), conditional on
/// the place it clears so a second run is a no-op.
pub const CLEAR_PLACE: &str = "UPDATE articles SET place='', geo_src='', geo_conf=NULL \
     WHERE id=? AND COALESCE(TRIM(place),'')<>''";

/// Tag rows, with the article's publisher and the other tags on the same article.
pub const TAGS: &str = "SELECT t.article_id AS article_id, t.tag AS tag, t.source AS source, \
     COALESCE(a.publisher,'') AS publisher FROM article_tags t \
     LEFT JOIN articles a ON a.id=t.article_id ORDER BY t.article_id, t.tag";

/// Drop one tag row.
pub const DELETE_TAG: &str = "DELETE FROM article_tags WHERE article_id=? AND tag=?";

/// The articles that carry tags at all: the ids the provenance note names.
pub const TAG_ARTICLE_IDS: &str = "SELECT DISTINCT article_id AS id FROM article_tags \
     ORDER BY article_id LIMIT ?";

/// The rows the full-text staleness probe reads: a term comes from the row's own text, so
/// nothing here is a word list.
pub const PROBE_SAMPLE: &str = "SELECT id, COALESCE(title,'') AS title, \
     COALESCE(summary,'') AS summary FROM articles ORDER BY id LIMIT ?";

// -- invariants the schema documents --------------------------------------

/// "Always set together with `relevant=0`" (`docs/DATA-MODEL.md` §1).
pub const RELEVANT_DUPE: &str = "SELECT id FROM articles WHERE dupe=1 AND relevant=1";
pub const FIX_RELEVANT_DUPE: &str = "UPDATE articles SET relevant=0 WHERE dupe=1 AND relevant=1";

/// `edges.a < edges.b` (`docs/DATA-MODEL.md` §3).
///
/// A reversed edge has two cases, and they need different statements: when the correct
/// row is already there, flipping would collide with the primary key, so the reversed one
/// is removed; when it is not, the two ends are swapped.
///
/// The id these select is the table's own `rowid`, not a number folded out of the two
/// ends: a reader can look a `rowid` up (`SELECT * FROM edges WHERE rowid=…`), and a
/// folded `a*1000000+b` both escapes the row it came from and collides across types.
pub const EDGE_ORDER: &str = "SELECT rowid AS id FROM edges WHERE a>b";
pub const EDGE_ORDER_MIRRORED: &str = "SELECT e.rowid AS id FROM edges e WHERE e.a>e.b \
     AND EXISTS (SELECT 1 FROM edges m WHERE m.a=e.b AND m.b=e.a AND m.type=e.type)";
pub const EDGE_ORDER_PLAIN: &str = "SELECT e.rowid AS id FROM edges e WHERE e.a>e.b \
     AND NOT EXISTS (SELECT 1 FROM edges m WHERE m.a=e.b AND m.b=e.a AND m.type=e.type)";
pub const CLEAR_MIRRORED_EDGES: &str = "DELETE FROM edges WHERE a>b AND EXISTS \
     (SELECT 1 FROM edges m WHERE m.a=edges.b AND m.b=edges.a AND m.type=edges.type)";
pub const FLIP_EDGES: &str = "UPDATE edges SET a=b, b=a WHERE a>b AND NOT EXISTS \
     (SELECT 1 FROM edges m WHERE m.a=edges.b AND m.b=edges.a AND m.type=edges.type)";
pub const EDGE_SELF: &str = "SELECT rowid AS id FROM edges WHERE a=b";
pub const CLEAR_SELF_EDGES: &str = "DELETE FROM edges WHERE a=b";

/// `article_links.a < article_links.b` (`docs/DATA-MODEL.md` §4), by `rowid` for the same
/// reason as [`EDGE_ORDER`].
pub const LINK_ORDER: &str = "SELECT rowid AS id FROM article_links WHERE a>=b";

/// An article with no URL cannot be deduplicated, cached or read: reported, and
/// not fixable here — the URL is not in the archive.
pub const ARTICLES_WITHOUT_URL: &str =
    "SELECT id FROM articles WHERE COALESCE(TRIM(url),'')='' ORDER BY id";

/// An article with no headline at all.
pub const ARTICLES_WITHOUT_TITLE: &str =
    "SELECT id FROM articles WHERE COALESCE(TRIM(title),'')='' ORDER BY id";

/// An entity nothing points at: harmless, but it means a link pass was cut short.
pub const ORPHAN_ENTITIES: &str = "SELECT e.id AS id FROM entities e WHERE NOT EXISTS \
     (SELECT 1 FROM article_entities ae WHERE ae.entity_id=e.id) ORDER BY e.id";

/// An entity whose stored type is empty: the documented "unclassified" state, and
/// the state the carry-over leaves a name the model did not classify.
pub const UNTYPED_ENTITIES: &str = "SELECT COUNT(*) AS n FROM entities \
     WHERE COALESCE(TRIM(type),'')=''";

/// The first unclassified entities, so the note names rows a reader can act on instead of
/// only a count.
pub const UNTYPED_ENTITY_IDS: &str = "SELECT id FROM entities WHERE COALESCE(TRIM(type),'')='' \
     ORDER BY id LIMIT ?";

/// One article, if it is there at all: an id in `--ids` that names nothing is reported,
/// not classified.
pub const ARTICLE_EXISTS: &str = "SELECT 1 AS x FROM articles WHERE id=?";

/// Entities carrying a type and no model receipt: what the carry-over re-derives.
pub const TYPED_ENTITIES: &str = "SELECT COUNT(*) AS n FROM entities WHERE COALESCE(TRIM(type),'')<>''";

/// Articles with no tag at all, and the model receipts the carry-over has written.
pub const ARTICLES: &str = "SELECT COUNT(*) AS n FROM articles";

/// The carry-over's own ledger, read back for a resume.
pub const LEDGER_ROWS: &str = "SELECT k, v FROM meta WHERE k LIKE 'migrate:%' ORDER BY k";

/// Articles in the order the carry-over works through them: newest first, so a
/// run that runs out of time has spent it on the part of the archive a reader sees.
pub const CARRY_CANDIDATES: &str = "SELECT id FROM articles WHERE dupe=0 \
     ORDER BY published DESC, id DESC";

/// All articles, duplicates included.
pub const CARRY_CANDIDATES_ALL: &str =
    "SELECT id FROM articles ORDER BY published DESC, id DESC";

/// One article, as the topic pass and the model pass need it.
pub const CARRY_ARTICLE: &str = "SELECT id, title, summary, substr(COALESCE(content,''),1,3000) AS body, \
     url, publisher, COALESCE(relevant,0) AS relevant FROM articles WHERE id=?";

/// The active topics the topic pass matches against (`atlas-topics` reads them).
pub const ACTIVE_TOPICS: &str =
    "SELECT id,name,keywords,exclude,min_matches,active FROM topics WHERE active=1 ORDER BY id";

/// `topics.assign`'s write, kept identical to `atlas-ingest::save`: the assignment,
/// its score and the terms that matched.
pub const UPSERT_ARTICLE_TOPIC: &str = "INSERT INTO article_topics(article_id,topic_id,score,matched) \
     VALUES(?,?,?,?) ON CONFLICT(article_id,topic_id) DO UPDATE SET score=excluded.score, \
     matched=excluded.matched";

/// An article that matches one of the reader's topics is `relevant=1`, and this
/// never turns the flag off: the carry-over adds reach, it does not hide a story.
///
/// `AND dupe=0` is not decoration: `docs/DATA-MODEL.md` says `dupe=1` is always stored with
/// `relevant=0`, so raising the flag on a hidden duplicate would make the topic pass create
/// the exact `relevant.dupe` contradiction `normalize` repairs. A repair tool may not
/// manufacture the fault it fixes.
pub const MARK_RELEVANT: &str =
    "UPDATE articles SET relevant=1 WHERE id=? AND relevant=0 AND dupe=0";

/// The entities one article links, with the type each carries.
pub const ARTICLE_ENTITIES: &str = "SELECT e.id AS id, e.name AS name, COALESCE(e.type,'') AS type, \
     COALESCE(e.count,0) AS count, e.first_seen AS first_seen, e.last_seen AS last_seen, \
     e.category_id AS category_id, COALESCE(ae.weight,1) AS weight FROM article_entities ae \
     JOIN entities e ON e.id=ae.entity_id WHERE ae.article_id=? ORDER BY e.id";

/// Detach one article's entity links, so the model's answer is what
/// `atlas-graph::link_article` sees as new.
pub const DETACH_ARTICLE_ENTITIES: &str = "DELETE FROM article_entities WHERE article_id=?";

/// Put one link back, exactly as it was.
pub const RESTORE_ARTICLE_ENTITY: &str =
    "INSERT OR IGNORE INTO article_entities(article_id,entity_id,weight) VALUES(?,?,?)";

/// Clear a stored type so `link_article` may write the model's answer for it. Only
/// applied to an entity with no model receipt of its own, and only where there is a type
/// to clear, so re-running a pass touches nothing.
pub const CLEAR_ENTITY_TYPE: &str =
    "UPDATE entities SET type='' WHERE id=? AND COALESCE(TRIM(type),'')<>''";

/// Restore the bookkeeping `link_article` increments, so the carry-over leaves no
/// drift in a count it did not intend to change.
pub const RESTORE_ENTITY_BOOKKEEPING: &str =
    "UPDATE entities SET count=?, first_seen=?, last_seen=? WHERE id=?";

/// The type a model pass ended up with, for the ledger.
pub const ENTITY_TYPE: &str = "SELECT COALESCE(type,'') AS type FROM entities WHERE id=?";

/// One entity's category, set from its type by the same map `categorise_entities`
/// uses (`atlas_ai::prompts::category_by_type`).
pub const SET_ENTITY_CATEGORY: &str = "UPDATE entities SET category_id=? WHERE id=?";

/// The category id behind a label, or nothing.
pub const CATEGORY_ID: &str = "SELECT id FROM categories WHERE name=? COLLATE NOCASE";

/// One article's categories, tags, summary and edge labels: the pre-state the
/// heuristic guard restores from.
pub const ARTICLE_CATEGORY_IDS: &str =
    "SELECT category_id AS id FROM article_categories WHERE article_id=?";
pub const DELETE_ARTICLE_CATEGORY: &str =
    "DELETE FROM article_categories WHERE article_id=? AND category_id=?";
pub const ARTICLE_TAG_NAMES: &str = "SELECT tag FROM article_tags WHERE article_id=?";
pub const ARTICLE_SUMMARY: &str = "SELECT COALESCE(summary,'') AS s FROM articles WHERE id=?";
pub const SET_SUMMARY: &str = "UPDATE articles SET summary=? WHERE id=?";
pub const ARTICLE_EDGE_LABELS: &str = "SELECT a, b, COALESCE(label,'') AS label FROM edges \
     WHERE a IN (SELECT entity_id FROM article_entities WHERE article_id=?) \
     AND b IN (SELECT entity_id FROM article_entities WHERE article_id=?)";

/// One edge's label, asked for the pair in one order.
///
/// The heuristic provenance guard needs it: a `reason:<a>:<b>` row may only exist for an
/// edge that carries a label, and an edge is stored in whichever direction the writer
/// chose, so the guard asks both.
pub const EDGE_LABEL_FOR_PAIR: &str =
    "SELECT COALESCE(label,'') AS label FROM edges WHERE a=? AND b=? LIMIT 1";
pub const CLEAR_EDGE_LABEL: &str = "UPDATE edges SET label='' WHERE a=? AND b=?";
pub const DELETE_META_KEY: &str = "DELETE FROM meta WHERE k=?";
pub const META_KEYS_LIKE: &str = "SELECT k, v FROM meta WHERE k LIKE ? ORDER BY k";

/// The one credential never to print: a meta key that looks like a secret is
/// reported as redacted even in the ledger dump.
pub const META_KEYS: &str = "SELECT k FROM meta ORDER BY k";

/// The topic assignments one article already has, so a refresh can tell a change
/// from a no-op.
pub const ARTICLE_TOPIC_ROWS: &str = "SELECT topic_id AS id, COALESCE(score,1) AS score, \
     COALESCE(matched,'') AS matched FROM article_topics WHERE article_id=?";

/// The entity ids one article links right now (after the model pass).
pub const ARTICLE_ENTITY_IDS: &str =
    "SELECT entity_id AS id FROM article_entities WHERE article_id=?";

/// The ledger entries the carry-over owns: its own keys only, never the app's.
pub const LEDGER_ENTRIES: &str = "SELECT k, v FROM meta WHERE k LIKE 'migrate:carry:%' ORDER BY k";

/// Drop a category that came from a type the model has since left empty: the pair
/// (type, category) must not disagree.
pub const CLEAR_ENTITY_CATEGORY: &str = "UPDATE entities SET category_id=NULL WHERE id=?";

/// Every category id, so the guard can tell a category a heuristic invented from one the
/// archive already had.
pub const CATEGORY_IDS: &str = "SELECT id FROM categories ORDER BY id";

/// Remove a category nothing points at any more: a label a heuristic pass created for a
/// type the model never gave.
pub const DELETE_UNUSED_CATEGORY: &str = "DELETE FROM categories WHERE id=? \
     AND NOT EXISTS (SELECT 1 FROM article_categories WHERE category_id=categories.id) \
     AND NOT EXISTS (SELECT 1 FROM entities WHERE category_id=categories.id)";

/// One entity's category, for the heuristic guard.
pub const ENTITY_CATEGORY_ID: &str = "SELECT category_id AS id FROM entities WHERE id=?";

/// One `meta` key, for the checks that read the ledger without a write connection.
pub const META_KEY: &str = "SELECT v FROM meta WHERE k=?";
