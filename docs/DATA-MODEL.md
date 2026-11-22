# ATLAS data model

SQLite, one file: `data/atlas.db` (WAL, `synchronous=NORMAL`, `foreign_keys=ON`, and
`trusted_schema=OFF` + defensive mode — see `docs/THREAT-MODEL.md` §4.8).

## 0. Why SQLite, and what Postgres would cost

The archive is one file and nothing about it is a service. `Store::open` opens a path
(`rust/atlas-store/src/lib.rs:166`); there is no daemon to start, no port to hold, no user to
create, no password to keep, and a backup is a copy of `data/atlas.db` while nothing writes
(`docs/OPERATIONS.md` §7). The window opens the same file `READ_ONLY` + `query_only`, so the
reader cannot write the archive even by accident, and the launcher's instance lock is the only
coordination the writers need. It is also what makes the security claims short: there is no
listening socket anywhere in ATLAS (`docs/NO-PORTS.md`), and there would be one if the store
were a server.

Measured on this Mac at this size (2026-11-22, release build, the store's own timing test
`cargo test --release --offline -p atlas-store --lib two_thousand_articles -- --nocapture`,
`rust/atlas-store/src/tests.rs:56` — a 2 000-article archive built in a temp directory, not the
shipped one):

```text
atlas-store: 2000 articles in one transaction = 43.3 ms; 2000 FTS rows in one transaction = 5.4 ms;
list 50 recent = 0.12 ms; FTS search (30 of 2000) = 6.42 ms; count = 0.13 ms;
single committed insert = 0.12 ms; the same 2000 rows with one commit each = 211.3 ms (5x slower)
```

The shipped archive, from `stat` (metadata only — this pass did not open it):

```text
data/atlas.db     7 733 248 bytes   mtime 2026-11-22T18:05Z
data/atlas.db-wal   206 032 bytes
```

So a page read is tenths of a millisecond and a full-text search over thousands of rows is a few
milliseconds: at this size the file is faster than a network call would be, and the one write
that costs anything is a commit per row — which is why the ingest path batches
(`insert_many`, one transaction).

**Postgres is the next step if, and only if, one archive ever needs concurrent writers on
different machines** — several collectors storing at once, or a reader on a laptop reading an
archive a server is still filling. What it would cost, concretely: a daemon to install, start and
supervise; a listening socket and therefore a port, an address, TLS or a tunnel, and credentials
— the three things this app currently does not have; a schema migration story that no longer fits
in `Store::open`'s `init`; a backup that is `pg_dump` plus a version to restore into, instead of a
file copy; the window's read path becomes a network client (with a timeout, a retry and a failure
mode beside every page); and `docs/NO-PORTS.md`'s central sentence would have to be rewritten.
None of that buys anything at 7.7 MB and one writer. The move is worth making when a second
machine has to write, and not before.

The DDL lives in the store: `rust/atlas-store/src/schema.rs` (`SCHEMA`, `MIGRATIONS`,
`POST_MIGRATION_INDEXES`), `rust/atlas-store/src/monitors.rs` (`monitors`, `alerts`,
`monitor_seen`, `monitor_runs`, `enrich_checked`) and `rust/atlas-store/src/briefings.rs`
(`briefings`, §12). Nothing declares SQL foreign keys:
integrity is maintained by the writers and by `Store::prune_old`
(`rust/atlas-store/src/prune.rs`), which deletes children before the
article.

This document was written against the Python host (`server/db.py`, `server/monitor.py`,
`db.prune_old`) and carried over to the Rust crates that own the same decisions: the tables,
columns and invariants below are the current ones, and a name like `collector.store_article`,
`db.prune_old` or `enrichment.analyze_batch` in a "written by" column is the **Python original
kept as provenance**, not a file in this tree. `docs/RUST-MIGRATION.md` §1 maps each `server/*.py`
module to the crate that replaced it — `db.py` → `atlas-store`, `collector.py`/`enrichment.py` →
`atlas-ingest`, `monitor.py` → `atlas-monitor` with `atlas-store/src/monitors.rs`, `graph.py` →
`atlas-graph`, `places.py` → `atlas-places`, `dates`/`guard`/`text` likewise — so a reader
following a `collector.*` name should open `rust/atlas-ingest/src/` and look for the same word.

Conventions used by every table:

- **Timestamps** are ISO-8601 UTC text, second precision, `%Y-%m-%dT%H:%M:%SZ`
  (`atlas_store::clock::now_iso`, and the same shape `atlas-dates` parses). Comparisons are string
  comparisons, which is why every writer must keep that exact shape.
- **Booleans** are `INTEGER` 0/1. `relevant`, `dupe`, `full_text`, `active`, `seen`,
  `is_primary` are never `NULL` by intent.
- **Pair tables** (`article_entities`, `edges`, `article_links`) are ordered pairs:
  `a < b`, one row per pair. `article_links` additionally has `PRIMARY KEY(a, b)`.
- **Ids in text** are never stored in SQL; `why`, `matched`, `analysis` carry display strings.

## 1. `articles`

One row per stored story. Written by `collector.store_article` (feed path) and
`ingest.save_article` (AI path); read by everything.

| Column | Type | Written by | Meaning |
| --- | --- | --- | --- |
| `id` | INTEGER PK | SQLite | Rowid, the number humans cite as `[Article #ID]` |
| `source_id` | INTEGER | both inserts | → `sources.id`. Feed: the feed. AI: the publisher row `_source_id()` created or found |
| `url` | TEXT **UNIQUE** | both inserts | Original article URL, comments/tracking intact. The single dedupe key that cannot be bypassed |
| `guid` | TEXT | feed only | Feed item id, ≤300 chars. Empty for AI ingests |
| `title` | TEXT | both | Headline, ≤500. Passed through `collector.clean_title`, so an `" - Publisher"` tail is removed |
| `summary` | TEXT | both | Telegraphic summary: feed description (≤800) or the model's 2–3 sentences (≤600) |
| `content` | TEXT | both | Article body, ≤20000. Feed path starts with the feed's own text; `full_text` says whether a real read has replaced it |
| `author` | TEXT | both | Byline from the feed (≤200); the literal `"AI"` for AI ingests, i.e. the server stored it, not a human |
| `published` | TEXT | both | Publication time. Feed: `collector.parse_date`; AI: `ingest._parse_dt`, which falls back to now when the page has no usable date |
| `fetched` | TEXT | both | Insert time. Second fallback for any age query (`COALESCE(NULLIF(published,''),NULLIF(fetched,''),'')`) |
| `lang` | TEXT | feed only | Feed language (`de`, `en`, …). Empty for AI ingests |
| `iso` | TEXT | both | 3-letter country code from `places.locate`. `''` when nothing was found |
| `lat`, `lon` | REAL | both | Point used by the map. City coordinates, country anchor, or outlet anchor, depending on `geo_src` |
| `hash` | TEXT | both | `sha1(url)`. Feed path: `sha1` of the raw URL; AI path: `sha1` of `ingest.norm_url(url)`. Both variants are checked by `_dupe_by_url` |
| `relevant` | INTEGER | `topics.assign` / `topics.retag` / `db.dedupe_titles` | 1 = matched at least one active topic at the last evaluation. Only `relevant=1` rows appear in lists, map, graph, search and monitors |
| `dupe` | INTEGER | `db.dedupe_titles` | 1 = another row tells the same story. Flag only, nothing is deleted. Always set together with `relevant=0` |
| `full_text` | INTEGER | both | 1 = `content` is a validated full read (see §12). The `/api/read` backfill works on `full_text=0` |
| `fetch_tries` | INTEGER | `collector.read_one` | Failed read attempts. The backfill skips rows at `>= 3` |
| `fetched_url` | TEXT | `collector._store_content` | URL that actually served the text (after redirects, Google News resolution) |
| `read_at` | TEXT | both | Time of the last read attempt (also set on failure) — the row's freshness stamp for text, not for the story |
| `publisher` | TEXT | both | Outlet name as displayed. `collector.publisher_of(url, title)` resolves it from the final host or the headline suffix; the model may pass it explicitly. The UI falls back to `sources.name` |
| `place` | TEXT | both | Human place label: city name ("Beirut") or short country ("Lebanon"). Precision is in `geo_src` |
| `geo_conf` | REAL | both | 0–1 confidence of the place pick. Outlet fallback is fixed at `0.2` |
| `geo_src` | TEXT | both | **Precision** of the pick: `city`, `country`, `outlet`, `none` (feed path) or `''` (AI path, where `places.locate` returned nothing) |

Indexes: `idx_articles_published(published DESC)`, `idx_articles_iso(iso)`,
`idx_articles_source(source_id)`, plus `idx_articles_relevant(relevant, published DESC)` and
`idx_articles_fulltext(full_text, published DESC)` from `POST_MIGRATION_INDEXES`.

`place`/`geo_src` split matters for the map: `/api/map` groups by
`COALESCE(NULLIF(place,''), iso)` and forwards `geo_src` as `prec`, so a story whose place is
only known from its outlet can be drawn differently from a located one. When nothing is found,
`place` is stored as `''` and `geo_src` as `''` (AI ingest, where `places.locate` returned
nothing) or `none` (`collector.store_article`, `collector.read_one`, `collector.relocate_all`).
Rows written by older versions can still hold `NULL` in `place`, `geo_src` and `geo_conf`, so
every reader uses `COALESCE`/`NULLIF`.

## 2. `article_fts` (FTS5)

```sql
CREATE VIRTUAL TABLE article_fts USING fts5(title, summary, content,
        content='articles', content_rowid='id');
```

An **external-content** FTS5 table: it stores the index only and reads columns from `articles`
by rowid. There are no triggers, so every writer of `title`/`summary`/`content` must maintain it
by hand — inserts write
`INSERT INTO article_fts(rowid,title,summary,content) VALUES(?,?,?,?)`, and re-reads do
`DELETE FROM article_fts WHERE rowid=?` followed by a fresh insert (the ported
`collector._store_content`: `atlas-ingest/src/save.rs` on the first store,
`atlas-ingest/src/enrich.rs` on the full-text backfill). `Store::prune_old`
(`rust/atlas-store/src/prune.rs`) deletes the FTS row before the article row. A direct
`UPDATE articles SET content=…` that skips FTS leaves the search index stale — that is the one
invariant FTS cannot enforce itself. `prune_removes_articles_and_every_reference_to_them` is the
test that keeps that order honest.

Used by `db.fts_query` (chat `search_news`, `/api/search`), `monitor._candidates` and
`ingest.tag_stats`, always with `a.relevant=1` and a `hours` bound.

## 3. `entities`, `article_entities`, `entity_alias`, `edges`

| Table | Columns | Notes |
| --- | --- | --- |
| `entities` | `id` PK, `name` **UNIQUE**, `type`, `category_id`, `count`, `first_seen`, `last_seen` | `type` ∈ `PERSON`, `ORG`, `LOC`, `EVENT`, `TOPIC`, `MISC`, `TECH`. `count` goes up by one per entity on every `graph.link_article` call — normally once per article, but a re-read or a monitor alert that calls it again adds another, so treat it as a popularity weight, not an exact article count. `first_seen`/`last_seen` are the timestamps of the same operations |
| `article_entities` | `article_id`, `entity_id`, `weight` (default 1), PK`(article_id, entity_id)` | Membership. Rewritten per article by `graph.link_article` (delete-then-insert); `weight` is not used by the link score — IDF covers that |
| `entity_alias` | `alias` **UNIQUE**, `entity_id` | Short form → canonical entity ("Merkel" → "Angela Merkel"), written by `enrichment._merge_alias` |
| `edges` | `a`, `b`, `weight`, `type` (default `'cooccur'`), `label`, PK`(a, b, type)` | Entity co-occurrence with `a < b`. `weight` = number of articles whose capped entity set contains both. `label` is an English relation written by `enrichment` ("governs", "member of") and only ever filled when empty |

`entities.category_id` is set from the entity type at insert (`ai.category_id_for`, deterministic
default per type) and can be corrected by enrichment. Entities are the vocabulary of everything
downstream: `article_links` similarity, thread titles, monitor candidate selection, the graph view.

Junk control: `graph.is_junk_name`, `_ok_name` and `_purge_junk_entities` reject navigation
text, outlet names, weekdays, fragments and pure stopword runs; `_merge_variants` folds
case/possessive/corporate-suffix variants into one row before `recompute_edges`. Per-article caps:
`MAX_ENTITIES_PER_ARTICLE = 12`, `MAX_EXTRACTED = 24`, `MAX_PLACES_PER_ARTICLE = 3`.

## 4. `article_links` — the "related articles" edges

| Column | Type | Meaning |
| --- | --- | --- |
| `a`, `b` | INTEGER | Article ids, always `a < b`; PK`(a, b)` |
| `shared` | INTEGER | Number of shared entities that contributed to the score (0 for wording-only and topic-filler links) |
| `score` | REAL | 0–1. `0.6 · entity cosine (IDF-weighted, shrunk for tiny overlap) + 0.4 · text cosine`, floored at `MIN_LINK_SCORE = 0.16`; wording-only links are `0.55 · text_cos`; topic fillers are fixed at `0.16` |
| `why` | TEXT | The sentence the UI and the chat show. Generated in a fixed order: `"shared: Jensen Huang, Nvidia"`, else `"same wording: chips, export"`, else `"same topic: Semiconductors"` |

Rules enforced by `graph.recompute_links(days=14, top_per_article=8, max_pairs=60000)`:

- Places are removed from the similarity vectors; a shared country is not a story.
- A pair is kept only when it is *strong* (`shared >= 2 and escore >= 0.12`, or
  `shared == 1 and ecos >= 0.45`, or `tcos >= 0.32`) and clears `MIN_LINK_SCORE`.
- Wording-only pairs need `tcos >= MIN_TEXT_ONLY = 0.40`; a pair whose only explanation would be
  a shared place needs `tcos >= MIN_TEXT_WITHOUT_ENTITIES = 0.45` plus shared tokens, otherwise
  it has no `why` and is dropped.
- Each article keeps at most `MAX_LINKS_PER_ARTICLE = 6` partners (`top_per_article` is capped by
  it), so no story sits alone while the graph stays readable.
- A topic filler link (`why = "same topic: <name>"`) is only added for an article with fewer
  than 2 links of its own, to its next same-topic neighbour.
- Rows are rewritten wholesale for the window (`DELETE … WHERE a IN (…) OR b IN (…)`), then
  `prune_links()` removes every link whose end is no longer `relevant=1`.

`graph.links_for(article_id, limit)` reads both directions and joins `articles` and `sources`, so
a caller does not need to know the canonical order.

## 5. `threads`, `article_threads`

| Table | Columns | Notes |
| --- | --- | --- |
| `threads` | `id` PK, `title`, `size`, `updated` | `size` = member count (≥2, ≤`max_size=40`); `updated` = `published` of the newest member |
| `article_threads` | `article_id` **PK**, `thread_id` | At most one thread per article. `recompute_threads` truncates both tables and rebuilds |

`graph.recompute_threads(min_score=0.20, max_size=40)`: union-find over `article_links` with
`score >= 0.20`, unions that would exceed `max_size` are refused (so one strong story cannot
swallow the archive), clusters smaller than 2 are dropped. Titles come from `_thread_title`:
entities present in ≥`max(2, 30 % of members)` articles, top 3, joined with ` · `; else shared
headline words; else the oldest member's title. A thread with no usable label is not created.

## 6. `article_tags` — what the AI wrote

```sql
CREATE TABLE article_tags(article_id INTEGER NOT NULL, tag TEXT NOT NULL,
    weight REAL DEFAULT 1, source TEXT, created TEXT, PRIMARY KEY(article_id, tag));
```

| Column | Meaning |
| --- | --- |
| `article_id`, `tag` | PK. `tag` is normalised by `ingest._norm_tags`: lowercase, `#` stripped, ≤4 words, ≤40 chars, punctuation trimmed, stopword-only and duplicates rejected, at most `MAX_TAGS = 12` per article |
| `weight` | Always `1` today; kept so a ranking can be introduced without a migration |
| `source` | `'ai'` for everything `save_article`/`tag_article` writes. A non-AI writer must set its own value |
| `created` | Insert time |

Tags are the model's own vocabulary, deliberately separate from `categories` (fixed taxonomy) and
from `entities` (extracted from text). Reads: `ingest.tags_for(article_id)`,
`ingest.tag_stats(limit)`, `/api/tags`, `/api/articles?tag=`, `/api/article/{id}`. Writes are
append-only per call (`ON CONFLICT DO NOTHING`); `tag_article` does not delete tags it did not
send, it adds.

## 7. `categories`, `article_categories`

| Table | Columns | Notes |
| --- | --- | --- |
| `categories` | `id` PK, `name` **UNIQUE**, `kind`, `description`, `created` | `kind` is the entity type the category defaults to (`PERSON`, `ORG`, `LOC`, `EVENT`, `TOPIC`, `TECH`, `ECON`, `POL`, `SCI`, `SPORT`, `CULT`, `LEGACY`). Seeded by `ai.seed_categories()` from `SEED_CATEGORIES` (People, Organizations, Places & Countries, Events, Topics, Technology, Economy, Politics, Science, Sports, Culture); legacy German names are renamed in place |
| `article_categories` | `article_id`, `category_id`, PK`(article_id, category_id)` | Many-to-many. Written by `ingest._apply_tags` (`_ensure_category` creates a missing category with description `"added by the AI"`) and by `enrichment._apply_categories` |

An article's categories therefore have two independent sources: the six-ish names the model put
in `save_article`'s `categories` argument, and enrichment's per-article categorisation (max 3).
Neither removes the other. `/api/categories` reports counts, `/api/articles?category=` matches by
name (exact or substring).

## 8. `article_topics`, `topics`, `article_countries`

| Table | Columns | Notes |
| --- | --- | --- |
| `topics` | `id` PK, `name` **UNIQUE**, `keywords`, `exclude`, `min_matches` (default 1), `active` (default 1), `created`, `hits`, `last_hit` | `keywords` and `exclude` are comma/semicolon/newline separated strings, compiled by `topics._compile` into word-boundary regexes (multi-word terms tolerate ` ` and `-`). A topic matches when at least `min_matches` distinct keywords hit and no `exclude` term hits. `hits` counts assignments, `last_hit` is the last one |
| `article_topics` | `article_id`, `topic_id`, `score`, `matched`, PK`(article_id, topic_id)` | The gate between an article and the UI. `score` = 3.0 per keyword in the title + 1.5 per keyword in the summary + 0.5 in the body, plus 0.25 per matched keyword. `matched` is the display string ("AI model (title), OpenAI (summary)"), ≤300 chars |
| `article_countries` | `article_id`, `iso`, `is_primary` (default 0), PK`(article_id, iso)` | All countries an article touches. Written by `graph.link_article` from the entities that resolve to a country, plus the article's own `iso` with `is_primary=1`. Used by `/api/articles?iso=` alongside `articles.iso` |

Matching is cached (`topics._CACHE` keyed by id+keywords+exclude+min_matches, `_TOPICS` for 10 s)
and invalidated by `topics.invalidate()` after CRUD. `topics.assign` writes/updates the rows,
removes assignments that no longer match, sets `articles.relevant` and bumps `topics.hits`/
`last_hit`. `topics.retag(days)` re-evaluates the archive and writes only what changed.

## 9. `sources`

| Column | Meaning |
| --- | --- |
| `id` PK, `name` | Label shown as the article's source when `articles.publisher` is empty |
| `url` | Homepage |
| `rss` **UNIQUE** | Feed URL — or a pseudo feed for AI publishers: `"ai:<publisher>"` |
| `country`, `iso`, `lat`, `lon`, `lang` | Declared origin of the outlet. Not used for the article's place (that is `places.locate`), only as a fallback and for display |
| `active` | 1 = eligible for collection |
| `last_ok`, `last_error`, `n` | Last success time, last failure text (surfaced when one feed breaks), cumulative new articles |
| `kind` | `'base'` = bundled world feed from `feeds/sources.json`; `'topic'` = per-topic search feed from `topics.sync_sources()`; `'ai'` = publisher row created on first AI ingest (`ingest._source_id`) |
| `topic_id` | Owner topic for `kind='topic'`, else `NULL` |

Collection is gated twice: `topics.has_topics()` must be true, and `active_sources()` only
returns `kind='topic'` rows unless `meta.collect_base = "1"` (the world-feed toggle). Pseudo feeds
(`ai:`) are never fetched.

## 10. `monitors`, `alerts`, `monitor_seen`, `enrich_checked`

| Table | Columns | Notes |
| --- | --- | --- |
| `monitors` | `id` PK, `name`, `query`, `threshold` (default 5), `active`, `created`, `last_run`, `hits` | A saved search, not a subscription: it is evaluated only by `monitor.check/check_all` (chat tool, `/api/monitors/run`, Monitors page). `last_run` is seeded to `now - 24 h` on creation; `hits` accumulates raised alerts |
| `alerts` | `id` PK, `monitor_id`, `kind` (default `'news'`), `article_id`, `url`, `title`, `relevance` (0–10), `summary`, `analysis`, `entities`, `seen`, `created` | `kind` ∈ `news` (from the archive, has `article_id`) / `web` (from the browser pass, `article_id` `NULL`). `analysis` holds the scorer's one-line reason, `entities` a JSON array of names (≤8). One alert per `(monitor_id, url)` and per `(monitor_id, article_id)` — enforced by `_alert_exists`, not by a constraint |
| `monitor_seen` | `monitor_id`, `key`, PK`(monitor_id, key)` | Dedupe memory: `a<article_id>` once the model judged an article below threshold, `w<sha1(url)[:16]>` once a web result was considered. Prevents re-scoring and re-alerting |
| `enrich_checked` | `a`, `b`, PK`(a, b)` | Created by `monitor.init()` but not read or written by any code path today — reserved pair memory. Enrichment's real receipt is `meta['enrich:<id>']` |

`monitor._candidates` looks back `db.window_hours()` (72 h by default), newest first: FTS match
on the query terms, else a LIKE pass, plus articles sharing a matching entity; bounded by
`MAX_ARTICLES`. Scoring is `ai.analyze_for_monitor` when the model is reachable and time is left,
else `_keyword_score`. `alerts.entities` is also written back into `article_entities`
(`_link_entities`) so an alert's names stay connected in the graph.

## 11. `meta`

Key/value text table, `k` PK. Used as counters, receipts and job state — not as configuration
(config is `data/config.json` plus the environment).

| Key | Written by | Meaning |
| --- | --- | --- |
| `last_collect`, `last_collect_note` | `collector.collect_all` | Last feed run and its note (e.g. `no active topics`) |
| `last_ai_ingest` | `ingest.save_article` | Last successful AI ingest |
| `last_enrich` | `collector.enrich_articles`, `enrichment.analyze_batch` | Last full-text backfill / enrichment pass |
| `last_prune`, `last_dedupe` | `collector.prune`, `db.dedupe_titles` | Rate limiter (prune at most every 12 h unless forced) and last dupe flagging |
| `last_retag` | `topics.retag` | Last topic re-evaluation |
| `last_repair`, `last_relocate`, `last_publisher_backfill` | `collector.repair_content`, `relocate_all`, `backfill_publishers` | Maintenance stamps |
| `connections_version` | `app.migrate_connections` | Schema/behaviour version of the graph rebuilds; `2` means the current model |
| `collect_base` | `/api/settings/collect-base` | `"1"` = bundled world feeds are collected too |
| `research_state`, `research_started`, `research_result` | `ingest.research_async` | Research job: `running`/`idle`, start time, the JSON result (≤400 000 chars) |
| `briefing_state`, `briefing_json` | `atlas-web`'s retired route only (`rust/atlas-web/src/briefing.rs:32-33`) | **Not written by the product any more.** The cached digest these two held (`{at, hours, articles, text}`, 30 min) was replaced by the `briefings` table (§12): nothing in `atlas-ingest`, `atlas-monitor` or the window reads or writes these keys, and `atlas-web` is the harness-only listener |
| `enrich:<id>` | `enrichment.analyze_article` | Per-article receipt: `{at, source, …}`, where `source` is `model` or `heuristic`. Also the "already analysed" marker |

`meta` is read on hot paths by the retired listener's `/api/status`, so its keys are a de-facto
public surface of that harness: `last_collect`, `collect_note`, `collect_base` and `briefing_at`
there come from `last_collect`, `last_collect_note`, `collect_base` and `briefing_json.at` here.
The window reads none of them — it reads `articles`, the rollups and `briefings` — so a key that
disappears blanks a field in `atlas-web`'s JSON, not in the product.

## 12. `briefings` — the prepared summary

One row per `(scope, key, hours)`: the model's sentence about one piece of the archive and the
few facts under it, written **before** the reader looks and drawn by the window instead of
re-reading thirty stories. DDL and every rule below are `rust/atlas-store/src/briefings.rs`
(`SCHEMA` at `:41`, `INDEX` at `:59`, `SCOPES` at `:70`); `Store::init` calls `briefings_init`,
so every archive has the table whether or not a briefing was ever written.

| Column | Type | Meaning |
| --- | --- | --- |
| `scope` | TEXT | one of `window`, `country`, `place`, `topic`, `monitor`, `entity` — a closed set, refused by name at write time, because a page reads it by name |
| `key` | TEXT | the scope's own address: an ISO code, a place label, a topic or monitor id as text, an entity id, and `""` for `window` (`briefings.rs:14-16`) |
| `hours` | INTEGER | the window the text was built over (`72` today); `MIN_HOURS` is 1 and there is no upper cap in the store |
| `watermark` | INTEGER | `max(articles.id)` at the moment the text was written |
| `created` | TEXT | ISO stamp of the write |
| `line` | TEXT | the sentence above the bullets, at most 400 characters (`MAX_LINE_CHARS`) |
| `bullets` | TEXT | JSON array of strings in the model's order, at most 8 (`MAX_BULLETS`), 240 characters each (`MAX_BULLET_CHARS`) |
| `articles` | INTEGER | how many stories the model was given |
| `model` | TEXT | the model's own name, as the caller asked it |
| `ms` | INTEGER | how long the model took, in milliseconds |

`PRIMARY KEY(scope, key, hours)` is the whole design: **one row per scope and window**, so a
refresh replaces the text in place instead of growing a history nobody reads, and a page's read
is a single row by key rather than "the newest of many". `idx_briefings_scope` on
`(scope, hours, created)` is the other read — the newest text for a scope at a window.

**`watermark` is the freshness rule, and it is not a clock.** A briefing is stale exactly when a
newer *article* exists (`max(articles.id)` has moved past the stored value), which is why an
archive that stopped collecting does not churn the model, and why a burst of twelve stories
refreshes one line once. An id is monotonic in arrival; a publisher's timestamp can be
backdated, so it is not used for this.

The caps are **refusals, never truncations** (`briefings.rs:20-27`): a briefing cut in half reads
like a model that stopped mid-sentence. A `bullets` value that is not JSON answers
`Ok(None)` — a hand-edited row loses its bullets, never the page it would have appeared on.

**Who writes it, who reads it.** The agent writes it: `atlas-ingest`'s `Pipeline::brief` builds
the digest and stores one row (§ `docs/ARCHITECTURE.md`, "Briefings"). The window reads it and
never writes: `rust/atlas-desktop/src/data.rs:975` is a `SELECT` on the read-only connection, and
the card that draws it is `rust/atlas-desktop/src/views/brief.rs`.

One spelling, three places: the store reserves `""` for the `window` scope's key
(`briefings.rs:14-16`), the agent's refresh plan names it the same way (`WINDOW_KEY = ""`,
`rust/atlas-monitor/src/brief.rs:34`), and the window reads `("window", "", hours)` with the page's own window as `hours`
(`rust/atlas-desktop/src/data.rs:975`; the card's tests read it at `72`). A row written under any other key for `window` would be
invisible to all three, which is why the constant exists rather than a literal at each call.

## 13. Invariants

Load-bearing rules. Break one and a UI page, a tool or a check is wrong — not just less pretty.

1. **`relevant=1` requires a topic match.** `articles.relevant` is only ever set by
   `topics.assign` / `topics.retag` / `db.dedupe_titles`, and always from an actual
   `article_topics` row (or to `0` when there is none). Consequences: an article with
   `relevant=1` and no `article_topics` row is corrupt; `dupe=1` implies `relevant=0`; every list
   endpoint, the map, the graph, FTS search and monitor candidates filter on `relevant=1`, so
   dropping the flag hides a row without deleting it. If no topic exists, nothing can become
   relevant — the ingest paths refuse the article and say so.
2. **`full_text=1` requires validated text.** Only two writers set it: `collector.read_one` →
   `_store_content(ok=True)` after `_valid_text(title, text)` (≥`MIN_FULLTEXT = 700` chars and
   `content_junk()` clean: no paywall/consent markers, not a navigation dump, ≥2 sentences,
   ≥34 % of the headline's long words present), and `ingest.save_article`, which inserts
   `full_text=1` because `read_article` only caches text that passed `_looks_like_article`
   (≥700 chars, no wall markers, ≥4 sentences or ≥2500 chars) and `save_article` demands
   ≥`MIN_CHARS = 700`. `full_text=0` means "teaser or unread", never "unvalidated body".
   `repair_content` demotes rows whose stored text is junk back to `full_text=0`.
3. **`place` is a label, `geo_src` is the precision.** Precision is exactly `city`, `country`,
   `outlet` (feed path, `conf=0.2`) or nothing — `none`/`''`. `place` is never a country name
   when precision is `city`, and never empty when precision is `outlet`. Coordinates match the
   precision: city centroid for `city`, country anchor for `country`/`outlet`.
4. **Article identity.** `articles.url` is UNIQUE. The same story from several outlets is stored
   once: normalised headline (`db.norm_title`) plus a 6 h window, or the same publisher at any
   time inside the age window. Duplicates are flagged (`dupe=1`), never deleted, and a copy with
   full text displaces a copy without.
5. **Pair tables are canonical.** `article_links.a < b`, `edges.a < b`, one row per pair. A
   reader must still query both directions (`WHERE l.a=? OR l.b=?`) because it cannot know which
   side it is on.
6. **Every article that is shown has a `why` on its links.** `recompute_links` refuses a pair it
   cannot explain, so `article_links.why` is non-empty for every stored row. Empty `why` means a
   writer bypassed `graph.recompute_links`.
7. **Derived tables are rebuildable, sources of truth are not.** `edges`, `article_links`,
   `threads`, `article_threads`, `article_entities` and `article_countries` can be dropped and
   rebuilt from `articles` + `entities` (`recompute_edges(30)`, `recompute_links(14)`,
   `recompute_threads()`). `articles`, `article_tags`, `article_categories`, `article_topics`,
   `topics` and `monitors`/`alerts` carry information that exists nowhere else.
8. **FTS stays in sync by hand** (§2). Any write to `title`/`summary`/`content` must write or
   re-insert the `article_fts` row for that id; deletes must delete it first.
9. **Freshness is a filter, not a cleanup.** Age is enforced at ingest (72 h) and by every query
   (`hours`, default `window_hours`). `retention_days` (21) is only applied by
   `collector.prune()`, which runs inside `collect_all()` — no timer, no per-ingest sweep. Rows
   older than the retention window may exist and are simply outside every default query.
10. **Timestamps are UTC `%Y-%m-%dT%H:%M:%SZ`.** Every age comparison is a string comparison
    against `datetime('now', '-N hours')` or `db.iso_ago(hours)`. A row with another timestamp
    shape sorts as "very old" and disappears from the UI.
11. **Length caps are part of the contract.** `title` 500, `summary` 600/800, `content` 20000,
    tag 40, topic name 80, monitor query 500, `why`/`matched`/`analysis` ~300. Writers truncate;
    HTTP input is rejected instead (400) when over its cap.
12. **`sources.rss` is UNIQUE, including the pseudo feeds.** A new AI publisher must use
    `"ai:<name>"`; inserting a second row with an empty `rss` fails.

## 14. Where to verify

The Python check scripts this section used to name (`tools/freshness_check.py` and its five
siblings) were deleted with `server/`. Each invariant is now asserted by the Rust test that owns
the writer:

| Claim | Command |
| --- | --- |
| Tables and columns as documented | `sqlite3 -readonly data/atlas.db ".schema articles"`, or `cargo test -p atlas-store` (`one_init_is_the_whole_archive_the_routes_read`) |
| Invariants 1, 2, 4, 6 (dates, windows, de-duplication) | `cargo test -p atlas-dates`, `cargo test -p atlas-ingest` (the crate's own `tests` module) |
| Topic gate and re-tagging | `cargo test -p atlas-topics`, `cargo test -p atlas-ingest` (`tags::tests`) |
| Entity graph, links, threads | `cargo test -p atlas-graph` |
| Ingest tools and read cache | `cargo test -p atlas-ingest` (`handlers::tests`, `cache::tests`) |
| Retention and the child-before-parent deletes | `cargo test -p atlas-store` (`prune::tests`) |
| Monitor/alerts behaviour | `cargo test -p atlas-monitor`, `cargo test -p atlas-store` (`alerts`) |
| Endpoint shapes against all of the above | `cargo test -p atlas-web --test routes`, then `cargo run -p atlas-e2e -- run` |
| An archive carried from the Python build | `cargo run -p atlas-migrate -- verify` (the three steps are `docs/MIGRATION-RUN.md`) |
