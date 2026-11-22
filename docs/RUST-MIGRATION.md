# Moving ATLAS to Rust — the record

Decision (user, 2026-11): **100 % Rust**, third-party crates allowed, newest releases,
**no Python fallback**. A rule that decides something lives in `rust/` or it does not exist.

The cut-over is done. ATLAS is one Rust binary (`atlas`) plus one wasm bundle. No Python is
executed by the product and none is shipped: the only Python in the tree is inside the
vendored upstream `tools/llama.cpp/`, which builds `bin/llama-server` and is not ours.
`docs/CUTOVER.md` is the runbook that was executed; this file is the record of what moved
where.

## 1. Every module that lived in `server/`, and what replaced it

The table is one row per Python module that existed before the cut-over. Each row names the
crate that now owns the decision; where a file had two halves (a decision and an HTTP route)
both crates are named, because the split is why the module is gone rather than ported whole.

| Deleted Python module | What it decided | Replaced by | Evidence in the crate |
| --- | --- | --- | --- |
| `server/app.py` | the HTTP surface: routes, static files, the request gate, SSE chat | `atlas-web` | `src/routes/mod.rs` (the table), `src/gate.rs`, `src/static_files.rs`, `src/routes/chat.rs` |
| `server/config.py` | settings, `ATLAS_*` env, `data/config.json`, derived paths, the loopback invariant, file modes, the child environment allowlist, the no-download audit | `atlas-config` | `src/spec.rs`, `src/config.rs`, `src/paths.rs`, `src/spawn.rs`, `src/audit.rs`, `src/download.rs` |
| `server/configcore.py` | the JSON-in/JSON-out bridge to the config module | `atlas-config` (`src/cli.rs`, `src/main.rs` — an operator tool, not a bridge) | same |
| `server/db.py` | SQLite: schema, statements, prune, retention, meta | `atlas-store` | `src/lib.rs` and its query modules |
| `server/guard.py` (the bridge) | the security verdicts the app asks for | `atlas-guard`, called **in process** — no subprocess, no bridge | `src/urlsafe.rs` (`url`), `src/cleantext.rs`, `src/sanitize.rs`, `src/token.rs`, `src/redact.rs`, `src/address.rs` |
| `server/rustcore.py` | the bridge to the HTML/entity/similarity core | `atlas-core` (`extract`, `entities`, `similar`), called through `atlas_ingest::NativeRules` and `atlas-web` | `rust/atlas-core/src/` |
| `server/textcore.py` | text normalisation, titles, publishers, URL shaping | `atlas-text` | `src/lib.rs` (`clean_text`, `clean_title`, `publisher`, `norm_url`, `tokenize`) |
| `server/datecore.py` | timestamps and the freshness window | `atlas-dates` | `src/lib.rs` (`parse`, `window`, `format`) |
| `server/collector.py` | fetch, clean, store; the page reader; the full-text backfill; prune | `atlas-ingest` (the pass and its gates), `atlas-net` (the transport), `atlas-dates` (the window), `atlas-places` (where it happened) | `atlas-ingest/src/{read,save,collect,gate}.rs`, `atlas-net/src/{fetch,hop,gate}.rs` |
| `server/ingest.py` | read → classify → store: the model's verdict, dedupe, topics, places, enrichment | `atlas-ingest` | `src/{read,save,handlers,gates,quality,entities}.rs` |
| `server/ingestcore.py` | the bridge to the ingest pipeline | `atlas-ingest` (`src/main.rs` calls the same library the listener uses) | same |
| `server/enrichment.py` | aliases, labels, summaries — once per pair | `atlas-ingest` | `src/enrich.rs` |
| `server/ai.py` | the local model client, the agent loop, the tool schemas, the prompts | `atlas-ai` | `src/chat.rs`, `src/agent/`, `src/specs.rs`, `src/prompts.rs`, `src/sse.rs` |
| `server/aicore.py` | the bridge to the model client | `atlas-ai` (`src/main.rs` is the tool-side CLI, not a bridge) | same |
| `server/monitor.py` | monitors, alerts, one check per monitor | `atlas-monitor` (the check), `atlas-web` (the schema and CRUD that `/api/monitors` and `/api/alerts` serve) | `rust/atlas-monitor/src/`, `atlas-web/src/routes/monitors.rs` |
| `server/monitorcore.py` | the bridge to the monitor check | `atlas-monitor` | same |
| `server/browser.py` | the headless reader browser: spawn, CDP, tab hardening, extraction | `atlas-browser` | `src/{spawn,lifecycle,cdp,commands,open,page,scripts,status}.rs` |
| `server/wsmini.py` | a minimal websocket client for CDP | `atlas-browser` | `src/wire.rs`, `src/cdp.rs` |
| `server/browsercore.py` | the bridge to the browser | `atlas-browser` (`src/main.rs` is the tool-side CLI) | same |
| `server/graph.py` | edges, links, threads, `why`, the graph tables | `atlas-graph` | `src/lib.rs` (engine), `src/{graphdb,graphlink,graphread}.rs` (tables) |
| `server/topics.py` | which of the user's topics an article matches | `atlas-topics` (matching), `atlas-web` (the CRUD at `/api/topics`) | `rust/atlas-topics/src/`, `atlas-web/src/routes/topics.rs` |
| `server/places.py` | where a story happened | `atlas-places` | `src/` (the gazetteer is `src/data/`) |
| `server/geodata.py` | `data/countries.json` from the vendored GeoJSON | `atlas-web::geo` (read-only label lookup) and `atlas-places` (country records) | `atlas-web/src/geo.rs`, `atlas-places/src/data/countries.rs` |
| `server/native.py` | the boot gate: refuse to start without the native binaries | `atlas-web::native` (the report and the required list) and `atlas-launcher` (the start that enforces it) | `atlas-web/src/native.rs`, `atlas-launcher/src/start.rs` |
| `public/app.js`, `public/js/*.js` | the whole frontend | `atlas-ui`, compiled to wasm in `public/ui/` | `rust/atlas-ui/src/` |
| `tools/*.py` (the 14 checks) | start a server, copy the archive, walk the routes, print a table | `atlas-e2e` (one run), `atlas-launcher`'s `doctor` (the pre-flight), `atlas-bench` (the timings), `atlas-migrate` (archive parity) | `rust/atlas-e2e/src/{run,drive,security,routes,ui}`, `rust/atlas-launcher/src/doctor/` |

Two of those rows are worth reading twice, because they were bridges rather than decisions:
`guard.py`, `rustcore.py`, `textcore.py`, `datecore.py`, `ingestcore.py`, `aicore.py`,
`monitorcore.py`, `browsercore.py` and `configcore.py` existed only to start a Rust process
from Python and parse its stdout. Nothing of that shape is left: `atlas-guard`, `atlas-store`,
`atlas-net`, `atlas-ai`, `atlas-graph` and `atlas-browser` are libraries the listener links, and
so are the five rule engines (`atlas-text`, `atlas-core`, `atlas-places`, `atlas-topics`,
`atlas-dates`) — a Rust process asking another Rust process a question was the last transport
left, and it is gone. All five answer in the caller (`atlas-ingest/src/rules/inproc.rs`,
`rules::SPAWNED` empty), one `save_article` asks **18** questions and starts **no** process, and
`atlas-ingest/tests/engines.rs` measures the two wirings side by side (47.6 ms with the process
transport against 0.74–1.44 ms in-process on this machine) and asserts that both stored the same
article. The engine binaries are still built, and `atlas doctor` fails while the listener warns
when one is missing; they are how an engine is asked from *outside* the app, and the process path
survives as the parity witness (`NativeRules::spawn_only`), so an in-process answer can be compared
with the binary's field by field. There is no fallback anywhere in this: nothing re-derives another
engine's answer, and the process transport, where it is used at all, still refuses with a sentence
that names the file and the command that builds it.

## 2. What was deleted, and when

Deleted at cut-over (the order is in `docs/CUTOVER.md`; the tree keeps no `server/` at all):

* `server/` — the Python host: `app.py`, `db.py`, `collector.py`, `ai.py`, `ingest.py`,
  `monitor.py`, `browser.py`, `topics.py`, `graph.py`, `places.py`, `config.py`,
  `enrichment.py`, `geodata.py`, `guard.py`, `native.py`, and the bridge modules
  (`rustcore.py`, `textcore.py`, `datecore.py`, `ingestcore.py`, `aicore.py`,
  `monitorcore.py`, `browsercore.py`, `configcore.py`, `wsmini.py`).
* `public/app.js` and `public/js/` — the JavaScript frontend, replaced by `public/ui/`.
* `tools/*.py` — the Python verification suite, replaced by `atlas-e2e`, `atlas doctor`,
  `atlas-bench` and `atlas-migrate`.
* `rust/atlas-net/checks/` — the Python check harness of the transport crate.

What is left under `tools/` is `README.md` and the vendored `tools/llama.cpp/` (upstream,
untouched, builds `bin/llama-server`). `run.sh`, `setup.sh`, `run.ps1` and `setup.ps1` are
thin wrappers: they check the toolchain, build the workspace, call the binary and pass the
arguments through. Every decision they used to make (ports, the model path, the child
environment, the loopback check, the stop order) is in `atlas-launcher`.

## 3. The crate map now

23 workspace members (`rust/Cargo.toml`). "Tests" counts the test functions declared in the
crate's `src/` and `tests/` — a count of what the crate asserts, not a claim about a run. It is a
snapshot (`grep -rc '#\[test\]\|#\[tokio::test\]' rust/<crate>/src rust/<crate>/tests`); crates
gain tests in between, so treat the column as an order of magnitude and re-count when it matters.

| Crate | Kind | What it owns | Tests |
| --- | --- | --- | --- |
| `atlas-lib` | lib | the minimal JSON parser/writer and the CLI plumbing every binary speaks | 5 |
| `atlas-guard` | lib + bin | every security decision about untrusted input: `url`, `clean_text`, `sanitize_html`, `token`, `redact`, `address`; the `Gated` token; the workspace-wide audits | 97 |
| `atlas-platform` | lib | the one place an OS question is answered: layout, file privacy, browser lookup, child environment, `inside_root`, `exe_suffix` | 47 |
| `atlas-config` | lib + bin | settings, `data/config.json`, derived paths, the loopback invariant, the child env allowlist, the no-download audit | 49 |
| `atlas-core` | lib + bin | `extract` (HTML→text), `entities`, `similar` | 26 |
| `atlas-text` | lib + bin | `clean_text`, `clean_title`, `publisher`, `norm_url`, `tokenize` | 6 |
| `atlas-places` | lib + bin | `locate`, `explain`, `alternatives`, `outlet` — the gazetteer is the crate's own data | 46 |
| `atlas-graph` | lib + bin | edges, `why`, threads, neighbours, and the graph tables it reads and writes | 71 |
| `atlas-topics` | lib + bin | `match`, `rank`, `score` — the user's topics only | 43 |
| `atlas-dates` | lib + bin | `parse`, `window`, `format` — one timestamp rule, one 72 h window | 48 |
| `atlas-store` | lib + bin | SQLite (rusqlite, bundled): schema, FTS5, statements, prune, retention, meta | 81 |
| `atlas-net` | lib + bin | HTTP (ureq 3 + rustls), the gated verdict, redirects through the gate, byte caps | 27 |
| `atlas-search` | lib + bin | `web_search`, `news_for`, `find_news` — one configurable endpoint, ranked by `atlas-core` | 44 |
| `atlas-ai` | lib + bin | the model client, the agent loop, the 23 tool schemas, the prompts, the SSE reader | 103 |
| `atlas-ingest` | lib + bin | read → classify → store: the gates, the model's verdict, dedupe, topics, places, the 23 tool handlers, `research` | 116 |
| `atlas-monitor` | lib + bin | monitors, alerts, one check per monitor, the scheduler's reason | 60 |
| `atlas-migrate` | lib + bin | `verify` (read-only), `normalize`, `carry` — carrying an existing archive over and proving it | 58 |
| `atlas-web` | lib + bin | the one listener (axum): 40 route entries, static files, SSE, the request gate, `wiring::real` → `RealTools` | 156 |
| `atlas-ui` | lib (wasm32) | the whole page: six views, the map, the timeline, the graph, the article dialog, topics, monitors, streaming chat | 286 |
| `atlas-launcher` | lib + bin (`atlas`) | the entry point: `atlas start`, `atlas doctor`, `atlas help`, `--version` | 70 |
| `atlas-bench` | lib + bin | the measurement harness behind `docs/PERFORMANCE.md` | 25 |
| `atlas-e2e` | lib + bin | one command that runs the whole app and reports numbers | 83 |

`atlas-web`'s `RealTools` holds one `atlas-ingest` pipeline, one `atlas-store` connection,
one `atlas-ai` client and tool registry, and the monitor service. Every job route does real
work: `POST /api/collect`, `POST /api/read`, `POST|GET /api/research`,
`POST /api/analyze/<id>`, `POST /api/briefing`, `POST /api/monitors/run`,
`POST /api/topics/<id>/rescan`, `GET /api/tags`. `StubTools` still exists so a test can wire
a fake; the binary never uses it (`atlas_web::wiring::real` returns the real graph or refuses
to start).

## 4. How the cut-over was proven

Four checks, in the order they were run. Each is one command a reader can run today;
`docs/CUTOVER.md` has the full sequence.

1. **`cargo test --offline --workspace`** — the whole workspace, nothing fetched, green.
2. **`cargo run -p atlas-e2e -- run`** — 24 steps in one run: a temporary root (`public/`,
   a fresh seeded `atlas.db`, the binaries copied in), a fake model on loopback, the server
   started on an ephemeral loopback port, the 49 documented routes driven over a real socket,
   the write paths read back, the model's answer through the whole chat path, the security
   invariants (loopback listener, no second socket, traversal refused, oversized body refused,
   hermetic temp root, no downloads), a real headless-Chrome UI smoke, and shutdown with
   timings. Nothing under the real `data/` is opened.
3. **The route table parity** — `atlas-web/tests/routes.rs` drives 49 rows through the router
   without a socket; `atlas-e2e` drives the same 49 rows over a socket, and its
   `routes.parity` step parses the server's own source and fails the run if the two tables
   differ in either direction. A route cannot exist in one and not the other.
4. **`atlas doctor`** — 9 checks, no network call: toolchain, native binaries, model file,
   model server, loopback invariant, SQLite, browser, ports, data permissions. Exit 0 means
   `atlas start` will not stop at its second step.

The archive was carried separately, because it predates the workspace and some of what it
holds is wrong: **`atlas-migrate verify`** (read-only — `SQLITE_OPEN_READ_ONLY` plus
`query_only`) reported **0 broken rows and 4 notes**, and **`atlas-migrate normalize --apply`**
wrote only what the report had proven (a stale FTS row, an orphaned link, a wrong publisher,
a place the text does not support). `verify` again after `normalize` is the proof that the
writes were the ones the report named; a second `normalize --apply` reports zero changes.

## 5. Rules that survived the migration

* **Loopback only.** `atlas-web`'s `boot::bind_address` refuses anything but `127.0.0.1`
  (or `::1`) before a socket exists, and `boot::listen` asks the kernel what it actually bound
  and refuses if the answer is not loopback. The model server is started with
  `--host 127.0.0.1` in its argv, and the reader browser's debug port is proven to be ours
  (see §6 of `docs/SECURITY-RUST.md`).
* **Nothing is downloaded at run time.** No model fetch, no update check, no telemetry, no
  pip, no browser download. Dependencies are fetched once at build time and pinned in
  `rust/Cargo.toml` + `rust/Cargo.lock`; `atlas-config`'s audit and
  `atlas-guard/tests/audit_downloads.rs` are what keep it that way.
* **No hardcoded news-word, publisher or host filters.** The local model decides what is news
  (`atlas-ai`'s news verdict). What `atlas-ingest` refuses is structural — a site root, a
  file, a search-results page (`gate::is_article_url`) — never a publisher.
* **Only current reporting**: 72 h ingest window, 72 h default view, 21 d retention. A
  future-dated item is not current (`atlas-dates` reports `reason: skew`).
* **Nothing automatic for collecting.** The model searches, reads, classifies and tags
  through tool calls. The one background pass is the full-text backfill, and it only opens
  pages the model already stored.
* **Monochrome, English, no gradients.**

## 6. Known left-overs

Each of these is a real line in the tree, checked against the tree today, not a plan:

* **One root-discovery marker still names `server/`.** `atlas-config/src/paths.rs`
  (`Paths::discover`, the `server` half of its `is_project` test) and
  `atlas-platform/src/root.rs` (`looks_like_app_root`) accept a directory that contains
  `rust/Cargo.toml`, or `server/` together with `public/`. The `server/` half can no longer match
  anything this repository ships; it is what recognises a checkout of the *old* layout, and the
  `rust/Cargo.toml` and `data/` markers are what make discovery work today.
* **`atlas-web/src/lib.rs` names the boot command it replaced.** Its header says the boot command
  became `atlas-web` instead of `python3 server/app.py`. That is history written as prose in a
  doc comment; there is no Python here to check.
* **`--log-disable` is passed only when the binary advertises it.** `atlas-launcher/src/llama.rs`
  sends the child's stdout and stderr to `data/llama.log`, and adds `--log-disable` when the
  binary's `--help` output names it (it does for the `bin/llama-server` this tree builds). The
  residual is the build whose `--help` probe failed: `atlas-launcher/src/llama.rs::the_model_server_cannot_be_told_to_log_a_prompt`
  is the test that names it, and it is `#[ignore]`d because closing it is a product trade, not a
  line edit — see `docs/THREAT-MODEL.md` §4.9.

The rest of the old list is now clean, and each was checked rather than assumed:

* `tools/README.md` describes `tools/llama.cpp/` only — not the deleted Python check suite.
* `setup.ps1` has no `tools/doctor.py` step; like `setup.sh` it ends at `atlas doctor`.
* `atlas-guard/src/lib.rs` no longer speaks of a "Python fallback matrix" (there was never a
  fallback, and the phrase is gone).
* `atlas-config`'s no-download audit reads `.rs` as well as `.py/.sh/.ps1/.cmd/.bat/.js`
  (`AUDIT_EXTENSIONS`, seven entries), so the Rust sources are in its walk and not only in
  `atlas-guard/tests/audit_downloads.rs`.
* `atlas-graph`'s Python row-parity tests are gone: `rust/atlas-graph/src/db_tests.rs` spawns no
  interpreter and never named a file that had to exist next to it.
* `rust/build.sh` builds the workspace, checks the ten binaries it produced (`atlas` plus the nine
  engine binaries of `atlas_web::native::REQUIRED`), and runs `atlas-config audit` as its policy
  gate; it no longer calls `tools/sec_audit.py`.

The ` 2.rs` copies are not gone, and pretending they are would be the exact slop this file is
supposed to record: at 16:47 on 2026-11-21 the tree held sixteen space-named copies under `rust/`
and `public/` (`handlers 2.rs`, `routes 2.rs`, `render 2.rs`/` 3.rs`/` 4.rs`, `style 2.css`, …),
2 473 real tiles with thousands of duplicate `N 2.png` files beside them, and 32 space-named
entries inside `data/`. They are created and deleted by whatever saves a file while another writer
holds it, so a count is true only for the minute it was taken; `docs/RUST-PROOF.md` §5 carries the
command, the list and the agreement that each owner deletes their own.
