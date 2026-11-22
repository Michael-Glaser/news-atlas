# ATLAS performance — measured, not claimed

Every table in this document names the harness that produced it. Most of them come
from `rust/atlas-bench`, a plain binary (no criterion) that
seeds a temporary SQLite archive **through the real schema** and times the paths a
user actually hits, in a release build:

```
cargo build --release --offline --manifest-path rust/Cargo.toml
./rust/target/release/atlas-bench                 # every path, best of 2 passes
./rust/target/release/atlas-bench --repeat 3      # the numbers in this file
./rust/target/release/atlas-bench --json          # the same table as JSON
```

The two `ui.` timings and the frontend sizes need the frontend crate on the host:

```
cargo build --release --offline -p atlas-bench --features ui-measure
```

Without the feature the table says so in its notes; the sizes are read from disk
either way.

```
archive: 5 000 articles, 20 000 entities, 60 000 article-to-article links,
         60 000 article-to-entity links, 60 000 entity edges, 136 threads,
         61 MB on disk (2.4 kB of text per article, 70 h of reporting)
```

A path is measured 30 times after 5 warm-up runs and reported as a percentile; the
whole suite is run `--repeat` times and the **fastest p95 per path** is the number,
because this machine is shared with other builds and a single pass can carry
somebody else's compiler. A path is fast when it is fast repeatedly. The two
process-spawning measurements are the exception: a boot is expensive, so startup runs
20 boots plus one warm-up, and the pipeline 30 articles.

Even so, consecutive `--repeat 3` runs of the same binary differ: the paths under
1 ms move by ±0.05 ms, and the ones above it move with whatever else the machine is
doing. Three full runs of the shipped binary gave `server.map` p95 6.53 / 6.53 /
9.02 ms and `store.threads` p95 88.7 / 88.7 / 155.2 ms — the same code, the
difference being a compiler from another agent starting mid-run. The table quotes a
quiet pass; the budgets have that spread built in, which is why they sit about 50 %
above the numbers instead of on them, and why a 3× repeat with the fastest pass kept
is the default.

A p95 also needs enough samples to mean anything. Startup used to be measured five
times, where the 95th percentile *is* the slowest run; it is measured twenty times
now, and the honest number is larger than the three-digit figure a five-run p95 gives
(note the asterisk below).

Nothing was downloaded to produce this file, nothing is bound except loopback, and
no process was started that this harness did not spawn itself (the startup
measurement spawns `atlas-web` in a throwaway root and kills it).

## The table

`p95` = 95th percentile of one path, milliseconds, release build, 5 000-article
archive.

Two route arguments are clamped by the server itself
(`input::clamp_int`, `limit` 1..200), so `/api/articles?limit=400` and
`/api/graph?limit=400` are not reachable from the wire: the harness measures 200
(the clamp) instead of a number the route refuses to serve, and the frontend's
"400 rows" case is measured where it exists — the wasm list build.

"Before" is the same harness on the same archive before this work. A bold row is a
change that is bigger than the run-to-run spread and is named under the table; a row
that moved by less than 0.15 ms (`store.list`, `store.fts`, `server.search`) moved
inside that spread, so it is neither a win nor a loss off this table.

| path | p50 | p95 before | p95 now | budget | why the budget |
|---|---:|---:|---:|---:|---|
| `server.status` | 3.22 | 11.76 | **3.53** | 8.0 | the status page polls it |
| `server.articles100` | 0.43 | 0.87 | **0.50** | 2.0 | 100 rows on a click |
| `server.articles200` | 0.72 | 1.17 | **0.79** | 3.0 | widest list the route serves |
| `server.article` | 0.14 | 0.14 | 0.14 | 1.0 | the reader panel opens |
| `server.search` | 0.34 | 0.33 | 0.35 | 2.0 | typed, so between keystrokes |
| `server.graph120` | 0.36 | 0.27 | **0.37** | 2.0 | graph first paint |
| `server.graph200` | 0.37 | 0.31 | **0.47** | 2.0 | largest graph served |
| `server.map` | 6.17 | 10.48 | **6.53** | 12.0 | the overview tab's first request |
| `server.timeline` | 0.89 | 5.72 | **0.94** | 3.0 | redraws on every window change |
| `server.threads` | 3.63 | 23.09 | **4.02** | 8.0 | 40 threads, two aggregates each |
| `server.sse_first_byte` | 0.07 | 0.10 | 0.07 | 1.0 | the chat panel shows its first frame |
| `store.list (200 rows)` | 0.26 | 0.26 | 0.30 | 1.0 | the list statement alone |
| `store.fts` | 1.71 | 1.82 | **1.93** | 5.0 | one FTS5 match over the archive |
| `store.entity` | <0.01 | <0.01 | <0.01 | 0.5 | an exact entity lookup |
| `store.threads (rebuild)` | 84.9 | 90.36 | **88.7** | 200 | runs after a collection pass |
| `store.prune` | 2.92 | 3.33 | **3.09** | 8.0 | scheduled retention, 400-row batches |
| `pipeline.engines (20 kB article)` | 16.02 | 19.18 | **16.14** | 40 | one article through the shipped rules — measured while `atlas-text` and `atlas-core` were still child processes, see the note below |
| `pipeline.inprocess (3 stages)` | 0.38 | 0.54 | **0.46** | 2.0 | the same work, with no transport at all |
| `ui.list_build (400 rows)` | 0.17 | 0.18 | **0.19** | 1.0 | one list refresh in wasm |
| `ui.graph_tick120` | 0.03 | 0.03 | 0.03 | 0.5 | one layout frame, 120 nodes |
| `ui.graph_tick400` | 0.38 | 0.39 | 0.40 | 1.5 | one layout frame, 400 nodes |
| `startup.listen` | 73.7 | 58.88 \* | **79.2** \* | 160 | exec → bound socket |
| `startup.first_byte` | 74.4 | 0.93 \* | **79.6** \* | 160 | exec → first JSON answer |

\* `startup` is the row whose clock changed: the earlier harness started it after
`spawn()` returned, so it could not see the process start at all. Measured from
`exec`, as the row is defined, `atlas-web` is listening 79.2 ms (p95, 20 boots) after
the shell hands it to the kernel — of which about 20 ms is loading a 12 MB binary and
starting the runtime, and about 50 ms is config → store → audit → bind. Its `p50` is
73.7 ms and its slowest of 20 boots 96.3 ms; on a loaded machine the same 20-boot p95
reads 128.3 ms. The clock now starts where the row says it does, and the sample count
matches the percentile: the budget (160 ms) was written from the honest clock.

Two rows **grew**: `server.graph120` 0.27 → 0.37 ms and `server.graph200` 0.31 →
0.47 ms. That is the graph endpoint's answer getting bigger (934 → 5 230 bytes: node
categories arrived in another change), not the handler getting slower — 0.37 ms of
work answers 5.7× more bytes. They are listed here because a table that only shows
improvements is a brochure.

### The pipeline rows are older than the wiring

`pipeline.engines` above (16.14 ms) was measured while two of the five rule engines were still
child processes — `atlas-text` and `atlas-core` — which is what the row's name has always meant.
All five answer in the caller's process now (`atlas-ingest/src/rules/inproc.rs`, `rules::SPAWNED`
empty), so the shipped row is the in-process cost and the 16 ms figure no longer describes it.
**The harness has not been re-run for this table**: `atlas-bench` is not started in this session,
and its own `pipeline.engines` documentation (`rust/atlas-bench/src/pipeline.rs`) still says
"spawns two binaries for the rest", so the number, the comment and the 40 ms budget for that path
are all pending the bench owner's re-measurement. What *has* been measured, in a test this
document can point at, is a whole save:

    cargo test --release -p atlas-ingest --test engines -- --nocapture

| wiring | questions as processes | `save_article` |
| --- | --- | --- |
| before (`atlas-text` + `atlas-core` as children) | 15 of 18 | 47.6 / 49.8 / 64.5 ms |
| after (every engine a library call) | **0** | 0.74 / 0.81 / 1.44 ms |
| control: every question a process | 18 | 57.8 / 59.1 / 59.5 ms |

The numbers are that test's own (its header carries the runs; the body prints the two wirings
side by side and asserts they stored the same article). Per question it is 4.3–4.6 ms as a
process against 0.16–0.25 ms in this process.

**The retired frontend's size, to the byte.** This table used to describe the face of
the app. It now describes what the *web* path cost, and the three numbers below were
stale: every one of them was re-read from disk while writing the section that follows
(`wc -c`, nothing built), and every one had moved.

| artefact | bytes | note |
|---|---:|---|
| `rust/target/wasm32-unknown-unknown/release/atlas_ui.wasm` | 2 592 466 | the build output; was 2 571 807 |
| `public/ui/atlas_ui_bg.wasm` (what the browser downloaded) | 1 460 259 | was 1 438 427 |
| `public/ui/atlas_ui.js` (the glue) | 49 106 | was 49 138 |
| `public/ui/loader.js` | 70 | |
| `public/ui/index.html` (the shell) | 1 718 | |
| **first navigation, before the app drew anything** | **1 511 153** | wasm + glue + loader + shell |
| `public/ui/tiles/` — the map, as 2 473 PNGs | 1 614 765 | raster pyramid, fetched per view; 9 936 KiB of blocks (`du -sk`), which is 4 KB per file and not image data |

Served as-is: `atlas-web`'s request gate accepts `identity` transfer encoding and
there is no compression layer (`atlas-web/src/gate.rs`), so those bytes are the bytes
that crossed the socket. `public/vendor/*.geojson` was **not** part of that path — the
retired map drew the PNG pyramid and never parsed an outline.

## The native window: what it costs to open

The face of the app is `rust/atlas-desktop`: one process, one window, a read-only
SQLite connection, no listener, no HTML, no wasm. "Open" is an ordered list of work,
and the order matters, because the largest cost is not in `new`:

1. `main.rs` calls `eframe::run_native` — the window, the GPU surface and the font
   atlas. **Not measured here**: this pass never opens a window (see the note at the
   end of this section for what that leaves out).
2. `app::Atlas::new` installs the style, opens the archive
   (`data.rs::Archive::open`: `SQLITE_OPEN_READ_ONLY` plus `PRAGMA query_only`, so a
   view cannot write by mistake) and runs `reload()`: `counts()`, `pins(72, 400)`,
   `countries(72, 14)`, `articles(72, 400, "")`, `entities(72, 120)`,
   `edges(&ids, 900)`, `topics()`, `monitors()`, then `GraphView::fill`.
3. The first frame paints the rail, the status row and the Overview page. The Overview
   builds the map *lazily* (`views/overview.rs`, `get_or_insert_with(MapView::new)`),
   and `MapView::new` parses the vendored outlines (`views/map.rs::World::load`) — so
   **the 3 MB GeoJSON parse happens inside the first frame**, not in `new`.
4. Every frame after that paints from memory: no query, no file, and no list is copied
   (the page reads the caller's `Snapshot` in place).

`atlas-desktop` has no `atlas-web`, `atlas-net` or `atlas-config` in its dependency
list, so opening the window runs **no boot audit and binds no socket**: the two things
the retired web path did before its first answer are simply not in this path.

### How to reproduce

The crate measures its own costs in tests that print what they measured — run them
with `--nocapture` (the budgets beside each number are the crate's own, and the
`tests/perf.rs` header states them for a debug build):

```bash
cd rust
cargo test --offline -p atlas-desktop -- --nocapture                 # every measurement the crate prints
cargo test --offline -p atlas-desktop --test perf -- --nocapture      # only the data path + the outlines
```

Both are the crate's own tests, so they print the tree's current numbers rather than
this document's; `tests/perf.rs` measures the eight archive paths and the outline
parse, `tests/map_zoom_pan.rs` and `tests/graph_layout.rs` the frames per zoom and the
graph tick, and `tests/query_budget.rs` the statements' plans. While this section was
being written the crate was mid-change and its test targets failed to compile for long
stretches (`cargo build -p atlas-desktop` did not); a command that does not build is a
fact about the tree that day, and the numbers below were taken with the harness
described at the end of this section while it did.

Two numbers the crate does not print — **the first frame** and **the search path at a
larger window** — were measured for this section by a harness kept outside the tree
(its core is at the end of this section): a measurement test belongs to the crate's
owner, and this document may add no file to the crate.

```bash
# today's archive, copied so nothing can write the live one
mkdir -p /tmp/atlas-perf && cp data/atlas.db data/atlas.db-wal /tmp/atlas-perf/
# the same 72 h window, twelve times bigger (one statement, printed in the table below)
```

and the sizes, which need no build:

```bash
wc -c public/ui/atlas_ui_bg.wasm public/ui/atlas_ui.js public/ui/loader.js \
      public/ui/index.html public/vendor/ne_50m_countries.geojson
find public/ui/tiles -name '*.png' | wc -l; find public/ui/tiles -name '*.png' -exec wc -c {} + | tail -1
```

### What it costs, measured

Release build (`--release`), one process, 20–50 timed runs after a warm-up, with the
values kept — so a row is the call the window makes, not the SQL alone (see the note
under the table). The machine was **not quiet**: load average 129 from other builds on
the same box, which is why the p95 column is not quoted here — individual statements
reached 51–101 ms p95 on that noise, the `edges` statement and the `articles` search
among them. The medians below are the quietest of three passes; the minima are the same
passes' best.

Today's archive — `data/atlas.db`, 1 435 articles, **269 inside the 72 h window**,
4 539 entities, 7 338 edges, 212 article links:

| step | median | min |
|---|---:|---:|
| `Archive::open` (read-only, `query_only`) | 0.03 ms | 0.02 ms |
| `counts()` (eight counts, one statement) | 0.011 ms | 0.011 ms |
| `pins(72, 400)` | 0.30 ms | 0.16 ms |
| `countries(72, 14)` | 0.08 ms | 0.05 ms |
| `articles(72, 400)` — empty search box | 0.44 ms | 0.43 ms |
| `articles(72, 400)` — one word, `berlin` | 0.69 ms | 0.65 ms |
| `entities(72, 120)` | 0.04 ms | 0.04 ms |
| `edges(120 ids, 900)` | 2.7 ms | 1.1 ms |
| `topics()` | 0.008 ms | 0.005 ms |
| `monitors()` | 0.005 ms | 0.004 ms |
| **`reload()` — all eight, plus the graph fill** | **4.1 ms** | 2.2 ms |
| `World::load` — 3 083 490 bytes of GeoJSON | 129 ms | 12 ms |
| **frame 1** (Overview, first paint) | **126 ms** | — |
| … of which the outline parse, as the card itself reports it | 122 ms | — |
| steady frame, Overview: `run` + `tessellate` (169 shapes) | 0.45 + 0.54 ms = 0.99 ms | 0.31 ms |
| frame, Articles page (254 stories in 400 rows) | 0.26 ms | 0.25 ms |

"Paint" is `egui::Context::run` and `Context::tessellate`: layout, text shaping and
vertex building — the CPU half of a frame. The GPU half is a handful of draw calls for
169 shapes and is not measured here.

Each row is the call the window makes, so a row includes what the call returns: the
`articles` rows copy 254 rows of `String` fields (about 1 MB of body text) into
`Article`s, which is why they are higher than the two figures the crate's own
`data.rs` header quotes for the same statements — 0.20 ms for the window read with the
row *output discarded*, and 0.85 ms for a search that hits 98 rows. 0.20 ms is the SQL;
0.44 ms here is the SQL plus the structs the views draw. Neither number contradicts the
other — same archive, same statement shapes, a different finish line — and this document
quotes both rather than picking the flattering one.

The same window with a bigger archive. One statement copies the archive's own 72 h
rows twelve times into a scratch copy (same schema, same indexes, and the 985 592 characters of body text the
crate's own header measured for the original 1 435 rows, once per copy):

```sql
INSERT INTO articles (source_id, url, title, summary, content, published, iso, lat, lon, relevant, dupe, full_text)
SELECT a.source_id, 'https://example.org/synth/'||a.id||'/'||k.n, a.title, a.summary, a.content,
       strftime('%Y-%m-%dT%H:%M:%SZ','now'), a.iso, a.lat, a.lon, 1, 0, a.full_text
FROM articles a, (SELECT 1 n UNION ALL SELECT 2 n UNION ALL SELECT 3 n UNION ALL SELECT 4 n
                  UNION ALL SELECT 5 n UNION ALL SELECT 6 n UNION ALL SELECT 7 n UNION ALL SELECT 8 n
                  UNION ALL SELECT 9 n UNION ALL SELECT 10 n UNION ALL SELECT 11 n UNION ALL SELECT 12 n) k
WHERE a.relevant = 1;
```

4 663 articles, **3 497 inside the window**, 22 994 944 bytes on disk:

| step | 269 in the window | 3 497 in the window | |
|---|---:|---:|---|
| `pins(72, 400)` | 0.30 ms | 4.9 ms | ×17 |
| `countries(72, 14)` | 0.08 ms | 0.74 ms | ×9 |
| `articles(72, 400)` — empty box | 0.44 ms | 0.25 ms | flat: `LIMIT 400` stops an index range read |
| `articles(72, 400)` — `berlin` | 0.69 ms | **100 ms** | ×145 |
| `edges(120 ids, 900)` | 2.7 ms | 1.2 ms | flat: 120 ids are 120 ids |
| **`reload()`** | **4.1 ms** | **53 ms** | ×13 |
| frame 1 | 126 ms | 130 ms | the parse dominates both |

Three things to read out of that table. The list with an empty search box does not grow
with the archive, because it is an index range read that stops at 400 rows. A **search
needle** does: the `LIKE` is evaluated over every body in the window (the crate's own
`data.rs` header says so and this is the number — 0.7 ms at 269 stories, 100 ms at
3 497). And the first frame barely moves, because 122 of its 126 ms are the outline
parse, which has nothing to do with the archive.

### How that compares with the retired web path

| what a reader waits for | retired web path | native window |
|---|---|---|
| a server that is ready | `startup.listen` p95 **79.2 ms** (20 boots), first JSON answer **79.6 ms** — this document's own numbers for `atlas-web`, of which ~50 ms was config → store open + migration check → **boot audit** → bind | none: no listener, no bind, no boot audit (`atlas-desktop` does not depend on `atlas-web`/`atlas-config`) |
| the code that draws | 1 511 153 bytes shipped to the browser and **compiled** there every navigation: 1 460 259 of wasm + glue + shell | 5 514 064 bytes of binary (`wc -c rust/target/release/atlas-desktop`), read from the page cache once at exec, then a window and a GPU surface. This document measured `atlas-web` at ~20 ms for 12 MB of process load; 5.5 MB scales to ~9 ms — **reasoned**, not measured |
| the map | 2 473 PNGs, 1 614 765 bytes of image data (`find … -name '*.png' -exec wc -c`), fetched per view over loopback and decoded tile by tile | 3 083 490 bytes of GeoJSON read from disk once, parsed into 1 632 rings / 99 613 points, drawn as lines from then on |
| the data | one HTTP request per list (`/api/articles`, `/api/map`, …), 0.5–6.5 ms each on the 5 000-article fixture, answered by the server and parsed as JSON by the wasm | `reload()`: eight statements, **4.1 ms** today and **53 ms** at 3 497 stories in the window, read straight into the structs the views draw |
| the first thing a reader sees | first byte 79.6 ms + wasm compile + first paint | **126 ms**, of which 122 ms is the outline parse |

What is **measured** in that table: the retired path's byte counts (re-read today, and
the three stale ones corrected above), the retired path's startup and route numbers
(this document's earlier passes, on the harness named beside them), the boot audit's
~50 ms share (quoted from the same paragraph — the audit itself has never been
measured on its own), and every native-window number in the two tables above.

What is **reasoned, not measured**: the browser's wasm compile and first-paint times
(no browser is started in this pass, and the retired bundle cannot be compiled
without one), tile decode times, and the window/GL-context creation cost (a window is
never opened here — the frame numbers are egui's CPU pass only). Those omissions all
push the retired path's total *up* and the native window's *down*, so the comparison
above is conservative for the native side except in one place: a warm wasm module in
a long-lived tab pays the compile once, exactly as the native window pays the outline
parse once. Both are "first open" costs; neither is a per-frame cost.

### The measurement harness for the two numbers the crate does not print

`rust/atlas-desktop` is a binary crate, so an integration test can only reach its code
by `#[path]`-including the sources — which is what the crate's own `tests/perf.rs`
does. The first frame needs one thing more: an `egui::Context` driven by hand. Thirty
lines, no window, no new dependency:

```rust
// A scratch crate whose only dependencies are the window's own:
// egui = "=0.31.1", rusqlite = "=0.43.0" (bundled), serde_json = "=1.0.154".
// The `#[path]` list is the crate's `main.rs` minus `app` (which needs eframe).
#[path = "/…/rust/atlas-desktop/src/assets.rs"]  mod assets;
#[path = "/…/rust/atlas-desktop/src/data.rs"]    mod data;
#[path = "/…/rust/atlas-desktop/src/geo.rs"]     mod geo;
#[path = "/…/rust/atlas-desktop/src/gesture.rs"] mod gesture;
#[path = "/…/rust/atlas-desktop/src/http.rs"]    mod http;
#[path = "/…/rust/atlas-desktop/src/theme.rs"]   mod theme;
#[path = "/…/rust/atlas-desktop/src/views/mod.rs"] mod views;

use std::time::Instant;

fn main() {
    // A copy of the archive, never the live file.
    let archive = data::Archive::open("/tmp/atlas-perf/atlas.db").unwrap();

    // app::Atlas::reload, in its order and with its limits.
    let started = Instant::now();
    let counts = archive.counts();
    let pins = archive.pins(72, 400);
    let countries = archive.countries(72, 14);
    let articles = archive.articles(72, 400, "");
    let entities = archive.entities(72, 120);
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let edges = archive.edges(&ids, 900);
    let _topics = archive.topics();
    let _monitors = archive.monitors();
    let mut graph = views::graph::GraphView::default();
    graph.fill(entities, edges);
    println!("reload {:?}", started.elapsed());

    let snapshot = views::overview::Snapshot { pins, countries, articles, counts, hours: 72 };
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut overview = views::overview::Overview::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 900.0))),
        ..Default::default()
    };
    let frame = |overview: &mut views::overview::Overview| {
        ctx.run(input.clone(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let _ = views::overview::show(ui, overview, &snapshot);
                });
            })
        })
    };

    // Frame 1: the map is built here, so the outline parse is inside this number.
    let started = Instant::now();
    let first = frame(&mut overview);
    let painted = started.elapsed();
    let started = Instant::now();
    let _ = ctx.tessellate(first.shapes, first.pixels_per_point);
    println!("frame 1 {painted:?} + tessellate {:?}", started.elapsed());
    // What the card itself says it paid for the outlines.
    println!("card: {}", overview.map.as_ref().map(|map| map.outlines()).unwrap_or_default());
}
```

The numbers above were produced by this program, run from the repository root (so
`assets::root()` finds `public/vendor/`) — with the module list and the frame closure
of the crate as it stood at the time. Prose is not a compiler: **this harness breaks
whenever the crate's module list changes**, and it did, twice, while this section was
being written (`views/chat.rs` added a `crate::http` dependency). That fragility is the
argument for moving these two numbers into `atlas-desktop` itself, as
`tests/frame_cost.rs` beside `tests/perf.rs`: same `#[path]` includes, same
`egui::Context::run` — but the crate's own test, which the crate's own change breaks
loudly instead of this document's prose going quietly stale. That is a cross-file
change (this document owns no Rust), so it is written down here rather than made.

## What changed, and what it bought

Four statements got the index or the shape they needed, each in the file that owns
the decision. Every number below is the same harness, the same archive, before and
after.

### 1. `atlas-store/src/schema.rs` — two indexes the archive never had

`atlas-store/src/schema.rs`, `POST_MIGRATION_INDEXES`. `db.py`'s schema is kept, and
two indexes are added to the list `Store::init` applies to an existing archive:

* `idx_article_threads_thread ON article_threads(thread_id)` — `article_threads` is
  keyed on `article_id`, so *every* "which articles are in thread N" scanned the
  whole table: `/api/threads` runs two such aggregates per thread, 40 threads.
* `idx_articles_window ON articles(relevant, dupe, published DESC, full_text, iso)` —
  every count on the status page and the map filters `relevant=1 AND dupe=0 AND
  published>=?`, and `db.py`'s index only carried `(relevant, published)`, so each
  statement read the row of every match.

`atlas-store/src/schema.rs::the_thread_lookups_use_an_index_instead_of_scanning`
fails if a thread lookup ever scans again (`EXPLAIN QUERY PLAN` must name the index).

| statement | before | after | speed-up |
|---|---:|---:|---:|
| `/api/threads` (route) | 23.09 ms | 4.02 ms | **5.7× faster** |
| status page's relevant count | 5.37 ms | 0.09 ms | **60× faster** |
| status page's full-text sum | 4.00 ms | 0.13 ms | **31× faster** |
| map's per-country count | 4.26 ms | 0.57 ms | **7.5× faster** |
| `/api/status` (route) | 11.76 ms | 3.53 ms | **3.3× faster** |
| `/api/map` (route) | 10.48 ms | 6.53 ms | **1.6× faster** |

Measured with the harness itself, so the "before" is the same binary without the
index: `./target/release/atlas-bench --drop-index idx_article_threads_thread`
reports 21.98 ms for the route on the same rows (3.4 ms with it).

### 2. `atlas-web/src/routes/articles.rs` — two queries that read what they throw away

* **The list count joined a table no column of it used.** The count behind
  `/api/articles` was `SELECT COUNT(*) … LEFT JOIN article_threads`; without a
  thread filter that join is a primary-key probe per matching row. The join is now
  only there when a thread filter needs it. Measured on 4 950 rows in `sqlite3`:
  **1.68 ms → 0.10 ms**.
* **The timeline read 4 000 rows to show 400.** `/api/timeline` thins the window to
  400 markers, but it selected every column of all 4 000 rows first. It now reads
  the window's ids (an index-only scan), thins *those*, and reads the details of the
  400 survivors (`thin_evenly`, `details`). Measured: the wide select **7.97 ms**,
  the id scan **0.90 ms**; route p95 **5.72 ms → 0.94 ms**.

### 3. Result: what a screen costs now

| what the user does | before | after |
|---|---:|---:|
| opens the app (exec → first answer) | 58.9 ms (from after spawn) | 79.6 ms (from exec) |
| the list screen (100 rows) | 0.87 ms | 0.50 ms |
| the map tab | 10.48 ms | 6.53 ms |
| the threads panel | 23.09 ms | 4.02 ms |
| the timeline, every window change | 5.72 ms | 0.94 ms |
| the status page | 11.76 ms | 3.53 ms |

## What was measured and **not** shipped

A candidate that does not move the number is not a change; it is churn with a
regression risk. These were built, measured, and left out (or removed again):

* **Per-row JSON → one buffer.** The list routes were changed to serialise rows
  straight into one `Vec<u8>` (`atlas_store::rows_to_json_text`) instead of building
  a `Value` per row and walking it again. On 200 rows the two encodings measure
  `store.encode_value` **0.16 ms** and `store.encode_text` **0.18 ms** — the
  `Value` tree is not the bottleneck, serde_json is fast at both. Reverted; the
  harness keeps both paths as measurements so the claim stays checkable.
* **A `place`-carrying covering index for the map's group-by.** 6.6 ms with it,
  6.6 ms without: SQLite's temp B-tree for `GROUP BY COALESCE(NULLIF(place,''), iso)`
  is the cost, not the row reads. Not added.
* **An index on `article_links(score)`** for the thread rebuild: 33.6 ms → 26.2 ms
  for that statement, and it puts a write cost on the most-written table in the
  archive. Not added; the rebuild stays at ~89 ms in a scheduled job.
* **Capping the graph layout per frame.** The suspicion was that the O(n²) layout
  tick dominated a frame. It is 0.03 ms at 120 nodes and 0.40 ms at 400 (three ticks
  per frame, measured through `atlas_ui::views::graph::sim::step`), against a 16 ms
  frame. Nothing to cap, so nothing was changed in the layout.

## What is still slow, and who owns it

Numbers, not adjectives — these are the paths where a real cost remains:

| path | p95 | where the time goes | the change that would take it |
|---|---:|---|---|
| `store.threads` | 88.7 ms | `SELECT a,b,score FROM article_links WHERE score>=? ORDER BY score DESC,a,b` — a scan plus a sort of 60 000 rows (`atlas-graph/src/graphlink.rs::recompute_threads`) | read the links once into an id-indexed array instead of a sorted SQL result, or accept the 26 ms an index buys |
| `startup` | 79.6 ms | 20 ms process load, ~50 ms boot: config, store open + migration check, boot audit, bind (`atlas-web/src/main.rs`, `boot.rs`) | measure the boot audit; it is the largest block that runs before the listener |
| `server.map` | 6.53 ms | the grouped statement's temp B-tree over the window (6.0 ms of 6.5 in `sqlite3`) | pre-aggregate place counts at collection time, or a query that groups on an indexed expression |
| `window.first_frame` | **126 ms** (median) | 122 ms of it is `views/map.rs::World::load` parsing 3 083 490 bytes of GeoJSON into a `serde_json::Value` tree, then copying out 99 613 points — on the first frame, because the Overview builds the map lazily (`views/overview.rs`) | deserialize into `Vec<Ring>` directly (`#[derive(Deserialize)]`, no `Value` in between), or build the map in a worker and paint the plate without coastlines for the first frames. Either is a change in `atlas-desktop`, not here |
| `window.search` | **100 ms** at 3 497 stories in the window (0.7 ms at 269) | `articles(72, 400, "berlin")`: the `LIKE` is evaluated over the body of every row in the window, not over the matches (`data.rs::articles`, and its own header says the cost grows with the window) | the `article_fts` FTS5 table the schema already maintains: `MATCH` the needle and intersect with the window's ids, instead of scanning bodies. Measured nowhere yet — it is the next measurement, not a claim |
| `window.pins` | **4.9 ms** at 3 497 stories in the window (0.30 ms at 269) | `GROUP BY round(lat,2), round(lon,2)` over the window: a temp B-tree, the same shape as `server.map` below, and the covering index was measured to buy nothing there | pre-aggregate place counts when an article is stored, or store the rounded coordinate as a column and index it |
| `window.reload` (a click, not a frame) | **53 ms** at 3 497 stories (4.1 ms today) | the rail's 24 h / 3 d / 7 d buttons call `reload()` synchronously in the frame: eight statements, then a repaint | read the snapshot on a worker and swap it in when it arrives; the frame loop already has no other reason to block |

The last four rows are the native window's; the first three are the retired server's,
kept because the collectors they name still run. `atlas-desktop`'s own harness
(`tests/perf.rs`) carries a p95 budget for every archive path it measures — counts 2.0,
pins 6.0, countries 4.0, articles 20.0, entities 6.0, edges 6.0, topics 2.0, monitors
2.0 ms, and 900 ms for the outline parse — so the first two of the four new rows are
already inside a ceiling somebody will have to argue with, and the search path is not:
`window.search` is measured by no test in the tree.

The pipeline row that used to sit here — 16.1 ms for a per-article cost that was almost entirely
`Command::spawn` — is gone for the reason the section above gives: no engine question starts a
process any more, and a whole save measures 0.74–1.44 ms in
`atlas-ingest/tests/engines.rs`. `atlas-bench`'s `pipeline.engines` comment and its 40 ms budget
still describe the old wiring; lowering them belongs to `atlas-bench`'s owner, and this document
will not quote a number the harness in this tree did not produce.

## The regression rule

`docs/PERFORMANCE.md` is only a document; `atlas-bench` is the rule. Each path has a
budget (`rust/atlas-bench/src/budgets.rs`), set from the measured p95 plus about
50 % room for a slower machine. A path that exceeds its budget by more than **20 %**
is a regression:

* the binary prints `REGRESSION <path>: p95 … is more than 120% over the … budget`
  and exits 1;
* `rust/atlas-bench/tests/budgets.rs` fails the build:

```
cargo test --offline -p atlas-bench            # measures, enforces against a debug build
```

A debug build is several times slower than a release build for reasons that have
nothing to do with the code, so the test scales its ceiling in a debug build and
says so; the exact 20 % verdict is the binary's, in release.

One thing the test does that the binary does not: it holds a process-wide lock, so
the three tests in `tests/budgets.rs` never measure at the same time. Three harnesses
running on three threads measured each other and produced a false regression the
first time this was wired up — `store.list` read 1.35 ms against a 0.28 ms cost, on
the same rows, because two sibling tests were paging the same archive in. A benchmark
that competes with itself is a random-number generator; the lock and `repeat: 3` are
what make its verdict mean something.

If a path is legitimately slower because it now does more, change its budget **in
the same commit** and say why in the row — a budget edited without a reason is how a
regression gets to stay.
