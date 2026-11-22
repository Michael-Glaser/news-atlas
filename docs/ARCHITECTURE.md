# ATLAS architecture

How the app is put together now that it is 100% Rust: a native window that reads the archive,
one collection process that starts up to four programs (provisioning when a piece is missing,
then the model server, the agent and the window), and the SQLite files between them.
`docs/DATA-MODEL.md` covers the tables, `docs/SECURITY-PROOF.md` the security claims with the
test that proves each one. `docs/API.md` is the **record of a listener that no longer exists**:
`atlas-web` was deleted with the web frontend, so nothing in this tree serves its routes.

## The window is the product now

The face of ATLAS is `rust/atlas-desktop` → `rust/target/release/atlas-desktop`: an `egui`
window that draws the archive and serves nothing — there is no `atlas-web` left to be a client
of. It holds
one read-only SQLite connection to `data/atlas.db`, paints what the queries return, and asks the
local model questions with tools in the loop (`src/agent.rs`, `src/tools.rs`).

The crate is a **library with a thin binary**: `src/lib.rs` declares the fifteen modules and
`src/main.rs` is one function — a `ViewportBuilder` (title, size, the dock icon drawn by
`logo::icon(256)`) and `eframe::run_native`. The split is not cosmetic: before it, every
integration test re-included `src/*.rs` with `#[path]`, so a test could not see a `pub(crate)`
item, a duplicate module lived under `tests/support/`, and one file's `mod tests;` resolved
against the *test's* directory. Now the tests say `use atlas_desktop::data;` and there is one
copy of every module (`rust/atlas-desktop/Cargo.toml`, `[lib]` + `[[bin]]`).

It is built on **eframe/egui 0.37.0**, and three of that version's interfaces are load-bearing
here: a page's panels are shown inside the root `Ui` that `Context::run_ui` hands the callback
(`src/views/graph.rs:1040-1048`, `src/views/monitors.rs:241-250`); the tests drop the returned
output with `Output::drop_without_applying_deltas()`, so a drawn page cannot change the next
frame; and text is measured through `ui.ctx().fonts_mut(...)`, which is how the overview's
headline and the graph's label fit are computed (`src/views/overview.rs:167`, `:263`,
`src/views/graph.rs:533`). A reader upgrading the pin should expect those four call sites to
move first.

| Module | What it decides |
| --- | --- |
| `app` | the frame: a rail, a page, a status row, and *when* the archive is read |
| `views` | what a page looks like, one module per page |
| `data` | every query, over one read-only SQLite connection |
| `assets` | where the project root and the map's outlines are |
| `geo` | the map's projection, zoom and level of detail, with no UI types in it |
| `gesture` | the pointer arithmetic behind a smooth pinch and drag |
| `theme` | the palette, the type ladder, the one corner radius |
| `logo` | the mark, drawn with the painter and rasterised for the dock |
| `link` | the one place a stored address may become an action |
| `http` | the one socket: a streamed POST to the local model on a `127.0.0.1` literal |
| `tools` | the archive as seven read-only tools the model may call, plus the prompt that teaches the workflow |
| `agent` | the loop: offer the tools, run the calls, ask again, stop at six steps |
| `requests` | the window's one writable file: an intent row in `data/requests.db` |
| `webview` | the system web engine as a child view: `engine()`, `allowed()`, `open()` — WKWebView/WebView2 on macOS/Windows, absent on Linux |
| `setup` | reads `data/setup.json` — the installer's progress report — and answers only "running, done, failed, or I do not know"; it never guesses a percentage |

```text
   atlas-desktop  (the window — the product's face)
    │
    ├─ egui/eframe 0.37.0  ──── the GPU surface, the rail and its pages, the status row
    ├─ rusqlite 0.43.0, bundled ── data/atlas.db read-only; data/requests.db read-write
    ├─ serde_json 1.0.154  ──── the vendored country outlines (GeoJSON), parsed once
    ├─ wry 0.57.3 (macOS, Windows) ── the system web view as a child rectangle: WKWebView / WebView2
    ├─ src/link.rs (std only) ── http/https + printable ASCII, or the address is shown as text
    └─ src/http.rs (std only) ── one outbound, streamed POST to 127.0.0.1:<llama_port>, chat only

   (no listener, no child process, no HTML of ours, no CSS, no wasm; one file it may write,
    and it is the intent queue, not the archive)
```

| Fact | Where it lives | How it was read |
| --- | --- | --- |
| five direct dependencies on macOS and Windows, four of them portable | `rust/atlas-desktop/Cargo.toml` | `cargo tree -p atlas-desktop --depth 1 -e normal` prints five lines at 18:03Z on this Mac: `eframe v0.37.0`, `egui v0.37.0`, `rusqlite v0.43.0`, `serde_json v1.0.154`, `wry v0.57.3` (all exact pins, `=x.y.z`); with `--target x86_64-unknown-linux-gnu` the same command prints three — `wry` and `serde_json` sit in the manifest's `[target.'cfg(any(target_os = "macos", target_os = "windows"))'.dependencies]` table, so a Linux build has no engine **and no `serde_json`**, which is a manifest fix its owner has to make before the text reader can build there (not built here: no Linux target installed); the resolved graph holds no `hyper`, `tokio`, `ureq`, `rustls` or `axum` |
| the archive is opened read-only, twice over | `src/data.rs:214` `open` | `OpenFlags::SQLITE_OPEN_READ_ONLY` (:226), then `pragma_update(None, "query_only", true)` (:232), then the value is read back (:235) and a `0` is a refusal |
| no listener, no process | `src/` | `rust/atlas-desktop/tests/no_spawn_no_socket.rs` scans the source (comments and strings blanked first) and fails on a `TcpListener` bind or a `std::process`, with one reviewed entry each for the two exceptions below |
| one outbound socket, chat only | `src/http.rs:452`, `:525` | the address is `SocketAddr::from(([127, 0, 0, 1], port))` — built from the parsed URL, never resolved; `TcpStream::connect_timeout` is the only connection in the crate; `endpoint` refuses anything that is not `http://127.0.0.1:<port>` |
| a stored address is gated before it becomes an action | `src/link.rs` `openable` | `http`/`https` in lower case, host of 1–253 characters with no `@` and no backslash, printable ASCII only, ≤ `MAX_LENGTH = 512`; the article pane offers two: **read the page in the window** (`views::reader::source_link`, `views/articles.rs:516`, which only *remembers* the address — the shell opens it) and **copy it** (`link::label(url)`, `ctx.copy_text`, no `Hyperlink`); a refused address is shown as text and neither door appears |
| the window's **one** writable file is the intent queue, never the archive | `src/requests.rs` `push`, `accept` | `grep -rn "SQLITE_OPEN_READ_WRITE" rust/atlas-desktop/src` finds one code line (`requests.rs:159`), and the file it opens is `data/requests.db` (`queue_path`, :73); `KINDS` is the closed set `research`, `monitor_run`, `topic_add`, `enrich`, `settings_set`, `brief` (`src/requests.rs:61`); the same file's `acceptance` table takes one row per licence version (`src/views/license.rs:49`); `tests/no_spawn_no_socket.rs` pins that one target |
| the first launch is a licence screen, not the archive | `src/views/license.rs`, `src/app.rs:284` `gate` | `app.rs` asks `requests::accepted_at(&queue, license::VERSION)` at construction; when it is false the frame draws `draw_gate` and returns before the rail; the text is `include_str!("../../assets/APACHE-2.0.txt")`, so nothing is opened to draw it |
| the chat is an agent loop, not one request | `src/tools.rs`, `src/agent.rs` | `tools::names()` is the seven tools (`search_news`, `get_article`, `entity_profile`, `graph_neighbors`, `graph_path`, `timeline`, `stats`); `tools::MAX_STEPS = 6`; every handler opens the archive `READ_ONLY` with `MAX_ROWS = 12` rows and `MAX_FIELD = 400` characters |
| one search engine, built in, and it says whose | `atlas-ingest/src/search.rs` `DEFAULT_TEMPLATE`, `resolve` | the template is `https://www.bing.com/news/search?q={query}&format=RSS&setmkt=en-US&setlang=en` (:63); `resolve(configured, meta, env)` falls back to `Engine::built_in()`, and the tool result carries `engine.host` + `engine.source` and never the substituted URL |
| a read happens when what the window wants changes, never per frame | `src/app.rs:121` `Demand`, `:285` `reload` | the status row prints how many reads there have been and what the last cost, so a per-frame read could not hide |
| the viewport: 1440×900, minimum 860×560, title `ATLAS`, app id `atlas` | `src/main.rs:28`–:35 | read from the `ViewportBuilder` |
| monochrome by construction | `src/theme.rs` | every palette colour is `Color32::from_gray(n)`, `window_shadow` and `popup_shadow` are `Shadow::NONE`, and `every_colour_in_the_palette_is_greyscale` / `the_installed_visuals_carry_no_hue` assert both |
| the project root is found from the executable | `src/assets.rs` `root` | walk up to the nearest ancestor holding `rust/Cargo.toml`, exactly as the launcher does |
| the map is vector, not tiles | `src/assets.rs` `geojson`, `src/views/map.rs` | `public/vendor/ne_50m_countries.geojson` (3 083 490 bytes) is parsed into coastline rings once, when the Overview page is first drawn |
| the mark is drawn, not shipped | `src/logo.rs`, `src/main.rs:34` | `Geometry` in fractions of the box, painted in the rail and rasterised at 256 px for the window manager; `every_pixel_of_the_icon_is_grey` fails on one tinted pixel |

The line numbers in that table were re-read on 2026-11-22, after the library/binary split and the
new mark; the file and symbol names are the durable part and a line number is a hint.

What it deliberately does not do, and the code says why:

- **It cannot collect.** The connection is read-only, so no view can write the archive, and
  the window starts no child. A click that means *go and look* becomes **one row in
  `data/requests.db`** (`src/requests.rs`: `research`, `monitor_run`, `topic_add` or `enrich`)
  and nothing else — the queue is an intent, and the agent that claims it is `atlas start`'s
  child, not the window's. The archive stays read-only and the window's write surface is that
  one file, whose other table holds one row per accepted licence version (`acceptance`,
  `apache-2.0+notice-2`) so the first screen does not come back. Those are the only two
  shapes it can write: an intent, or a record of a tick.
- **It reads the model as a thinking model.** `atlas-ai`'s `ChatRequest::new` sets
  `enable_thinking: true` and `DEFAULT_MAX_TOKENS = 8192` (`atlas-ai/src/chat.rs:37`, `:150`),
  the agent's `AgentConfig::default()` does the same, and the shipped model server runs
  `-c` at `model_context`'s default of 32768 (`atlas-config/src/spec.rs:99`, written at
  `rust/atlas-launcher/src/llama.rs:183-187`), the same number as `atlas-ai`'s
  `context::CONTEXT_TOKENS` (`atlas-ai/src/context.rs:59`, the fallback when no window is
  configured). The
  measured cost is in the source because it is the reason the budgets are what they are: one
  classification with thinking on took **239 s** and **4 633** completion tokens
  (`atlas-ai/src/agent/state.rs:41`, `:72`).
- **It cannot start the model, and it cannot reach anything but the model.** Chat is a streamed
  POST to `http://127.0.0.1:<llama_port>/v1/chat/completions` (`stream: true`, SSE parsed in
  `src/http.rs`), no TLS, no redirect, no retry. The address comes from `ATLAS_MODEL_URL` or
  the `8080` default (`src/app.rs:116`); a name, even `localhost`, is refused rather than
  resolved, so no request can leave the machine. With nothing listening, the reply is a
  sentence that names the start script — `./run.sh` on macOS and Linux, `run.ps1` on Windows
  (`src/http.rs:66`).
- **It takes no lock and writes no log.** `data/atlas.lock` belongs to collection, and the
  window has no log file of its own: what it has to say, it says in its status row.
- **It does not draw every view the archive supports.** The rail has a page each for the
  overview (map and stories), the articles, the graph, the topics, the monitors and the chat;
  threads and the timeline are in the archive and in the JSON API but not on the rail yet.

**One question is a loop.** `src/agent.rs` offers the seven archive tools to the model, runs
what it asks for against the archive (read-only, `MAX_ROWS = 12`, `MAX_FIELD = 400`,
`MAX_ANSWER = 6 000` characters, `src/tools.rs`), and asks again with the results until the model
answers or `tools::MAX_STEPS = 6` steps are used. Everything the model says that is not the
answer — the thinking, the tool it is running — is drawn as working, never as the reply: the
window's `http::Reply` keeps `thinking` and `content` in separate fields, and
`atlas-ai/src/chat.rs:241` says plainly that the thinking "is **not** the answer and is never
substituted for one". A run that ends on its step budget still gets an answer, because the loop
reads the tag form a model writes when it has no tool turn left —
`<function=name>` blocks parsed as `CallSource::MarkupInText` (`atlas-ai/src/tool.rs:315-325`).

One consequence to keep in mind: the window does **not** depend on `atlas-store`. Its queries
are its own (`src/data.rs`), which is what keeps a writer out of the binary's dependency graph,
and its one writable file (`src/requests.rs`) opens `data/requests.db` with its own copy of the
schema, not the store's — `tests/no_spawn_no_socket.rs` checks the copy against the shape the module writes, and the store's own test owns the other half. It also means a column
renamed in `atlas-store` has to be renamed here. The window's crate is the place to look when the
two disagree.

## 1. The shape of the app

What that leaves is the collection side: `atlas start`, one process that starts up to four
programs and then waits for the window. It serves nothing — no JSON API, no listener of any
kind. The agent is the only part whose dependency closure holds the fetcher; it reads pages over
plain HTTP and starts no browser. `atlas start --dry-run` prints exactly that plan — root,
provisioning, window, agent, model file, model server, "no listener", stop order — and starts
nothing (`rust/atlas-launcher/src/plan.rs::plan_lines`; the run that produced those lines is
quoted in `docs/RUST-PROOF.md` §2.5, and `docs/OPERATIONS.md` §1 is the table).

```text
  atlas start  (rust/target/release/atlas)  ── the collection process, waits for the window
   │
   ├─ atlas-setup --install, a child, ONLY on a machine that is not provisioned
   │    once, supervised, before anything below; it exits before the model server starts,
   │    and a provisioned machine spawns nothing here (setup.rs:368, start.rs:157)
   │
   ├─ llama-server, a child of this process ──► 127.0.0.1:<llama_port> (-c 32768, -np 1)
   │    started with --host 127.0.0.1 in its own argv; the port is `llama_port`, 8080 by
   │    default (rust/atlas-config/src/spec.rs) — the product's only listening socket
   │
   ├─ atlas-monitor serve, a child ───────────── the always-on agent: claims rows from
   │    data/requests.db and runs them; prints one JSON line per action on inherited stdout
   │    reads pages over plain HTTP (NullBrowser: no Chrome, no CDP, no listener)
   │
   └─ atlas-desktop, a child ─────────────────── the window: reads data/atlas.db read-only,
        binds nothing, writes one intent row in data/requests.db
```

The stop order is fixed in one constant: `rust/atlas-launcher/src/shutdown.rs:51` is
`[Step::Window, Step::Agent, Step::Model]` — the window first (it is what a reader closed), then
the agent, so it cannot be left claiming work while the model server goes away, then the model
server.

The three things the launcher will not delegate: which ports exist, which model file is
loaded, and the order things stop in. Each of them lives in one module of
`rust/atlas-launcher` with a test beside it (`ports.rs`, `model.rs`, `shutdown.rs`), so a
script cannot drift away from the binary.

Useful consequence: the collection process is `atlas`, the product's face is `atlas-desktop`,
and the two share nothing but the database file and the request queue. The listener
(`atlas-web`), the wasm frontend (`atlas-ui`), the browser library (`atlas-browser`) and the
harness (`atlas-e2e`) are not in the tree any more: `ls rust` lists twenty-one crates, and
`cargo metadata --offline --locked --no-deps | grep -o '"name":"atlas-[a-z-]*"' | sort -u`
prints twenty-one names, none of them a listener.

## 2. The collection entry point: `atlas start`

This is the process that starts up to four programs — the provisioning child when a piece is
missing, then the model server, the agent and the window — and writes nothing itself — the agent
and the pipeline do the writing. The window does not run it and does not need it to read; see the
section at the top.

`atlas` has three commands — `start`, `doctor`, `help` — and exit codes 0 (it worked),
1 (ATLAS cannot do it, the sentence above says why) and 2 (the command line was not
understood). An unknown word is refused rather than guessed at: `atlas strat` prints
`Try "atlas help".` and exits 2.

`atlas start` runs its steps in the only order that is safe
(`rust/atlas-launcher/src/start.rs`, whose header is the same list):

1. **The stop handler, first.** Every wait in the boot watches one `SIGTERM`/Ctrl-C
   future, so a signal during model loading stops the process with its children instead of
   leaving them behind.
2. **The lock and the modes.** An OS advisory lock on `data/atlas.lock` — one ATLAS per data
   directory. The kernel releases it when the holder dies, so "is the previous instance
   alive?" is answered by the lock, not guessed from a pid. A lock file nobody holds is
   reported (`took over a lock left by pid 57108, started …`) and taken over; then `data/`
   and the files in it are made owner-only, and the repaired count is part of what a start
   reports.
3. **The settings** from `atlas-config`: environment, then `data/config.json`, then the
   default. A value that does not validate stops the start and names the key and where it
   came from — `data/config.json:<key>` when the file set it, `env:ATLAS_<KEY>` when an
   environment variable did (`rust/atlas-config/src/spec.rs:413`, `:477`).
4. **The loopback invariant**, before any child exists: the one address this boot is
   responsible for is the model server's, it is a `127.0.0.1` literal judged loopback by
   `atlas-guard`, and it is passed to `llama-server` as `--host 127.0.0.1`. Nothing in this
   process listens.
5. **Provisioning**, if this machine has never been provisioned: when the model file or
   `bin/llama-server` is missing, `atlas-setup --install` runs as a supervised child — once,
   with its output inherited and its progress in `data/setup.json` — and the boot waits for it.
   It is the only step in the product that can reach the network, it is skipped entirely when
   both pieces are present, and `--no-install` turns it off (`setup.rs:368`, `start.rs:157`).
6. **The model file, the window binary and the agent binary.** `models/<model_local>`, then the
   derived name, then the newest other `.gguf` in `models/`; the window and the agent are
   resolved before the model starts, so a machine that never built one stops in a sentence
   instead of after four minutes of loading weights. Nothing is fetched here.
7. **The model server** as a child, with the flags in §7. The supervisor attaches before the
   health wait, so a server that dies while loading is restarted once with a backoff instead
   of being waited on for four minutes.
8. **The agent**, `atlas-monitor serve`, as a child: the always-on half that claims rows from
   `data/requests.db` and keeps a monitor's cadence while the window is open or closed.
9. **The window**, as a child; this process waits for it, and closing it is how `atlas start`
   ends.

The stop order covers the three that are still running when a stop arrives — window, agent, model
server — because the provisioning child of step 5 is gone before step 7 starts. On Windows the
stop arrives through three events rather than one (Ctrl-C, Ctrl-Break, and the console's X, which
is `CTRL_CLOSE_EVENT`); `docs/OPERATIONS.md` §1.3 has the detail and the one thing about it that
cannot be verified on this machine.

## 3. The listener and the web frontend: deleted

There is no HTTP surface, no static file tree and no app log any more: `atlas-web`, `atlas-ui`
and `atlas-browser` were removed from the workspace and `public/ui/` with them. Measured:
`ls rust` lists twenty-one crates, `ls public` prints `vendor`, and
`grep -rn atlas-web rust/*/Cargo.toml` answers two comments (in `atlas-bench` and
`atlas-launcher`, both saying the crate is not a dependency) and no dependency line.

What replaced it, one row each:

- **the window is the reader** — it paints the archive from a read-only SQLite connection and
  opens a story's page in the system web view rather than serving a page of its own
  (`docs/NO-PORTS.md` §1–§2 is the measurement);
- **a request is a row** — the window writes one of six kinds into `data/requests.db` and the
  agent (`atlas-monitor serve`, a child of `atlas start`) claims it, instead of a JSON route
  being called (see §5);
- **the map is vector** — the outlines in `public/vendor/ne_50m_countries.geojson` are parsed
  once and painted as lines, so there is no tile server, no cached pyramid and no handler that
  could serve one.

`docs/API.md` survives as the record of what those routes answered, for a reader looking at an
older checkout. Nothing in this tree implements them.

## 4. The crate map

The workspace is `rust/Cargo.toml`; each row's sentence is that crate's own `//!` header.

| Crate | What it is |
| --- | --- |
| `atlas-lib` | shared, `std`-only foundations for the native binaries: JSON, the stdin/stdout CLI shape |
| `atlas-guard` | every security decision about untrusted input: the URL/IP gate, text cleaning, escaping, redaction |
| `atlas-config` | the settings table, `ATLAS_*` environment names, `data/config.json`, derived paths, the loopback invariant, file permissions, the child environment allowlist, the "nothing is downloaded" audit |
| `atlas-platform` | the one place that answers an OS-dependent question: where the data dir and model are, how a file is made private, how a child is spawned — and, unused by anything shipped since the browser path went, where a Chromium is installed (`browser.rs`) |
| `atlas-core` | dependency-free JSON-in/JSON-out engine: `extract` (HTML → readable text), entities, similarity, HTML scanning |
| `atlas-text` | the string decisions: cleaning, titles, publishers, URL canonical form |
| `atlas-places` | where a story happened, from a curated gazetteer, weighted by where the name appears |
| `atlas-graph` | relations: entities, edges, IDF-weighted connections with their `why`, threads — engine and tables |
| `atlas-topics` | the topic gate: which of your topics an article belongs to |
| `atlas-dates` | the one date/time implementation: parsing, the 72-hour window, future-date skew, display ages |
| `atlas-store` | persistence: schema, migrations, every statement, FTS5 search, retention, monitor and alert tables, and the request queue (`requests.rs`: `data/requests.db`, `KINDS`, the one-statement claim) |
| `atlas-net` | the one place a request leaves this machine: gate the URL, follow redirects through the gate, byte cap, decode |
| `atlas-search` | a search client of its own (one endpoint, cards parsed structurally, no publisher or host list) that **no other crate links**: the tool the model calls is `atlas-ingest/src/search.rs`, a second implementation of the same job inside the crate that already has the fetcher and the gate. This crate is still a workspace member with a binary, and its `rules::Engines` still spawns `atlas-text` and `atlas-dates` — a transport the running app does not use |
| `atlas-ai` | the local model: loopback HTTP/1.1 client, SSE, tool-call protocol, the agent loop, context budget, prompts, tool schemas, health |
| `atlas-ingest` | the read/classify/store pipeline: the 23 tool handlers, the page cache, the quality predicate, save gates, enrichment |
| `atlas-monitor` | monitors (a saved query, one check at a time, the model's verdict per candidate, alerts, the check log) **and the always-on agent**: `atlas-monitor serve` claims rows from `data/requests.db` and runs them through the same pipeline; `atlas start` spawns it (`rust/atlas-launcher/src/agent.rs`) and stops it before the model |
| `atlas-migrate` | carrying an existing archive over (`verify`, `normalize`, `carry`) and proving it was carried |
| `atlas-bench` | the measurement harness behind `docs/PERFORMANCE.md`: seeded archive, the paths a user hits, timed in release |
| `atlas-desktop` | **the product's face**: an `egui`/`eframe` window that draws the archive from a read-only `rusqlite` connection, chats with seven read-only archive tools, writes exactly one file (`data/requests.db`, an intent row) and opens no listener and no child (see the section at the top) |
| `atlas-launcher` | `atlas`: the boot sequence, lock, model resolution, the model server's flags, supervision of up to four programs (the provisioning child when a piece is missing, then `llama-server`, `atlas-monitor serve`, the window), the stop order `[Window, Agent, Model]`, `--dry-run`'s plan, `doctor` |
| `atlas-setup` | **install side, not the app**: `check` (the ten rows a fresh machine is measured against — the window binary, the page view, the archive, the map outlines, the settings, the model file, which model is served, the `llama.cpp` binary, the model server's port and the start command; `rust/atlas-setup/src/check.rs:44-58`), `model` (fetch the pinned GGUF, verify size and sha256, install), `llama` (build `bin/llama-server` from the vendored tree through CMake), `all` (model, then llama, then check). No `ui` subcommand: the wasm bundle went with the frontend. No crate links it; `atlas start` runs it once, as a child, only when the model file or `bin/llama-server` is missing, and never on a provisioned machine; every action of its own needs `--yes` after printing the URL or the argv |

Four crates that used to be in this table are gone: `atlas-web` (the listener), `atlas-ui`
(the wasm frontend), `atlas-browser` (the DevTools client) and `atlas-e2e` (the harness that
drove the first of them). `ls rust` is the measurement: twenty-one directories, none of those
four. What they documented is now §3, §6 and §8 of this file.

`atlas-setup` is the exception in that table: it is a member so `cargo build --release`
produces it, and it is deliberately *not* reachable from the app
(`atlas-guard/tests/audit_manifest.rs::the_install_tool_is_not_linked_into_the_app`).

Nineteen of these crates also produce a binary (`cargo metadata --offline --locked --no-deps`
counts nineteen targets of kind `bin`: `atlas`, `atlas-ai`, `atlas-bench`, `atlas-config`,
`atlas-core`, `atlas-dates`, `atlas-desktop`, `atlas-graph`, `atlas-guard`, `atlas-ingest`,
`atlas-migrate`, `atlas-monitor`, `atlas-net`, `atlas-places`, `atlas-search`, `atlas-setup`,
`atlas-store`, `atlas-text`, `atlas-topics`; `atlas-lib` and `atlas-platform` are libraries
only). `atlas doctor` checks the toolchain and the data files the boot needs
(`rust/atlas-launcher/src/doctor/checks.rs`), not a set of engine binaries. What the engine
binaries are **not**, any more, is a
transport: the app asks every one of them as
a library call in its own process (`atlas-ingest/src/rules/inproc.rs`, `rules::SPAWNED` empty), one
`save_article` asks 18 rule questions and starts no child, and `atlas-ingest/tests/engines.rs`
measures the two wirings against each other and asserts they store the same article. The binary
remains as the way to ask an engine from outside the app, and as the parity witness
(`NativeRules::spawn_only`) that keeps an in-process answer comparable with the binary's.
`atlas-ingest`, `atlas-ai` and `atlas-monitor` are libraries linked into the programs that run.
The only
children the app starts are `llama-server`, `atlas-monitor serve` and the window — plus, on a
machine that has never been provisioned, `atlas-setup --install` once, before them
(`rust/atlas-launcher/src/plan.rs`). No process this app starts launches a browser.
(`atlas-search` is linked into nothing — see its row above — so its spawn transport is a
workspace member's own, not a process this app starts.)

## 5. The request path

A request is a row now. The window writes one of six kinds into `data/requests.db` —
`research`, `monitor_run`, `topic_add`, `enrich`, `settings_set`, `brief`
(`rust/atlas-desktop/src/requests.rs:61-68`; the store's `KINDS` is the same closed set and a
test fails if the two drift, `:732`) — and the agent claims one row at a time with one
`UPDATE … RETURNING` statement (`atlas-store`'s `requests` module), runs it through
`atlas-ingest`, and prints one JSON line per action on stdout.

There is no router, no gate, no rate limiter and no static handler, because there is no way in:
the file is the only input. Each kind's payload is validated against a closed schema before a
model call is paid for — `brief` names its six scopes and its byte caps in
`rust/atlas-monitor/src/brief.rs:28-58`, and `atlas-desktop` refuses to build a payload it knows
is wrong (`requests.rs`, `MAX_PAYLOAD`).

The window never writes the archive. Its connection is `SQLITE_OPEN_READ_ONLY` with
`query_only` on (`src/data.rs`), and the queue is the one file it opens for writing.


## 6. The retired frontend: deleted, not kept

`atlas-ui`, the `atlas-ui-tiles` host binary, `public/index.html`, `public/style.css`,
`public/ui/` and `rust/build-ui.sh` are gone (`ls rust` → no `atlas-ui*`; `ls public` →
`vendor`; `ls rust/*.sh` → `rust/build.sh`). The window draws the archive itself, and the only
markup it ever renders is the third-party page a reader asks the Reader to open (§10).

This section used to describe the wasm bundle — the mount point, the loader, the tile pyramid
and the commands that built them. There is nothing left to describe, and its numbers (tile
counts, byte sizes) were numbers about a tree that is not here; the tests that pinned them went
with the crates.


## 7. The model server as a child

`llama-server` is started by `atlas-launcher/src/llama.rs` — the only place in the app that
starts it — with these flags:

```text
-m <model>  -c 32768  -np 1  --host 127.0.0.1  --port <llama_port>  --jinja
```

- `--host 127.0.0.1` is written out rather than relied on, because this app starts a listener
  and a default can change.
- `-c 32768 -np 1` is one context with one slot on purpose. `32768` is `model_context`'s
  default (`atlas-config/src/spec.rs:99`, range 1 024–262 144, `ATLAS_MODEL_CONTEXT`), written
  into the argv at `rust/atlas-launcher/src/llama.rs:183-187`; `-np` is `SLOTS = "1"`
  (`llama.rs:79`). The same number is `atlas-ai`'s `context::CONTEXT_TOKENS`
  (`atlas-ai/src/context.rs:59`) — the window that the agent's step, token and deadline budgets
  are built around, and the fallback a run uses when no window is configured. llama.cpp divides
  `-c` by `-np`, and a slot small enough to split would make the first tool turn answer "out of
  context". The default went from 8 192 to 32 768 when the model started thinking: the thought is
  spent before the answer begins (`atlas-ai/src/context.rs:50-52`).
- `--jinja` is what makes tool calling work with this model's chat template.

`--no-webui` and `--spec-type draft-mtp` are build dependent, so they are asked for first:
`llama-server --help` runs once and its text is searched for the flag. A build that has
neither gets neither — no version guessing, no failing start on an unknown option.

The child is expected at `bin/llama-server` and then on `PATH`; its stdout and stderr go to
`data/llama.log`. The app waits on `/health` and keeps running with the model down: the window
still opens, chat says the model is not answering, and the agent's research passes report the
same thing instead of pretending.

## 8. The reader browser: gone, and why it stays gone

Some pages cannot be read over HTTP: JavaScript-only bodies, consent walls, wrapped links. ATLAS
used to read them in a headless Chrome that the agent spawned itself. It does not any more, and
the reason is a port rather than a preference: a browser can only be driven over the DevTools
protocol, which means a socket the browser listens on, and the product's rule is that no process
it starts opens a listening socket.

`atlas-browser` was deleted with the frontend (`ls rust` → no `atlas-browser`;
`grep -rn atlas-browser rust/*/Cargo.toml` → one comment in `atlas-launcher`, no dependency).
`atlas-ingest` keeps the seam — `NullBrowser` (`rust/atlas-ingest/src/lib.rs:369`, and the
module's table at `:22` says "JS-only pages: nobody") — so a JavaScript-only page produces a
sentence the model can act on ("the page needs JavaScript and this build reads over plain HTTP
only - no browser runs here, so the text cannot be rendered. Try another source.",
`atlas-ingest/src/read.rs:138`) and the model picks another link.

`atlas-guard/tests/reachability.rs:858`
(`the_browser_the_flag_and_the_listener_are_unreachable_from_every_binary_the_app_starts`) still
asserts the absence three ways: no manifest edge to a browser crate, no remote-debugging flag or
socket in the agent's closure source, and a non-vacuity check that the scanner still finds
capabilities elsewhere.

What the retired library got right is recorded here so a future decision starts from it rather
than rediscovering it: Chrome was started with `--headless=new`, a profile at
`<root>/data/chrome-profile` (a child of the `0700` data tree) and `--remote-debugging-port=0`,
so Chrome picked an ephemeral loopback port and wrote it — with the websocket path — into
`DevToolsActivePort` inside that profile, the only control channel the crate read; the endpoint
was proven ours before the first request (the profile, the child's own listener read back as
loopback, a `ws://` loopback literal); every navigation was gated again and the tab was hardened
(downloads denied, no file chooser, no `window.open`); and extraction was `atlas-core`'s, the
same reader the HTTP path uses. It never had the HTTP path's resolver — Chrome made its own DNS
lookup — which is why the DNS residual in `docs/THREAT-MODEL.md` §4.1 is recorded as closed only
for the path that remains (the window's engine, which this file's §10 covers).


## 9. Briefings

A briefing is the model's sentence about one piece of the archive — a country, a place, a topic,
a monitor, one entity, or the whole window — plus up to eight facts under it, written **once**,
stored in `briefings`, and drawn by the window. Asking the model on every frame would be a call
per repaint; a template would be a summary of nothing. Three crates, three jobs.

**Who builds it.** `atlas-ingest::Pipeline::brief(scope, key, hours)`
(`rust/atlas-ingest/src/brief.rs:89`). The scope must be one of `SCOPES` (`window`, `country`,
`place`, `topic`, `monitor`, `entity`); the stories are one SQL statement per scope over the
columns and join tables the rest of the app already reads (`brief_stories`, `:215`), newest
first, at most `BRIEF_STORIES = 40` (`:41`); the digest is capped at `BRIEF_CHARS = 24 000`
(`:49`) with per-field cuts — title 120, summary 220, outlet 60, date 25 (`:53-63`). Every field
goes through `atlas_ai::prompts::field_line` and the list is fenced by
`atlas_ai::prompts::data_block`, so a story that prints a fence marker or an instruction cannot
speak to the model outside the data block.

**Which prompt.** `atlas_ai::brief::BRIEF_PROMPT` (`rust/atlas-ai/src/brief.rs:51`), then
`digest_instruction(BULLETS)` — the app asks for **5** bullets (`atlas-ingest/src/brief.rs:64`),
clamped by the prompt to `BULLET_RANGE = 3..=6` — then the fenced `STORIES` block. The answer is
**one json object and nothing else**: `{line, bullets}`. `atlas_ai::brief::parse` reads exactly
that object and ignores a sentence outside it; when the first answer is not the object, `brief`
asks **once** more with `ONLY_JSON` appended (`brief.rs:70`), and a second miss is a refusal with
one sentence and no row. There is no retry loop, no timer and no background job.

**Which budgets.** `self.budgets()` (`atlas-ingest/src/research.rs:310`) is
`atlas_ai::Budgets::for_root(root)`: the call's completion budget is
`model_reasoning + model_answer`, and `model_context` is the window the prompt is trimmed to
(`docs/OPERATIONS.md`, "The model settings"). A reader who lowers the window gets shorter
digests, not silent truncation at the model's own limit.

**What it writes.** One row per `(scope, key, hours)` with `watermark = max(articles.id)` read
*before* the digest is built, so freshness is "a newer story exists", not a clock
(`docs/DATA-MODEL.md` §12). A model that is not answering is a refusal and no row — never a
briefing about nothing.

**Who refreshes it, and how often.** Two doors, one plan:

- The reader's press on the card writes a `brief` row in `data/requests.db` (the sixth kind,
  `rust/atlas-desktop/src/views/brief.rs:41`) and the agent answers it
  (`rust/atlas-monitor/src/serve.rs:580`).
- After a run that stored at least one story, `refresh_after`
  (`rust/atlas-monitor/src/brief.rs:386`) refreshes in a fixed order — the window, then the
  countries of the new stories busiest first (ties by ISO code, so two runs over one archive
  refresh the same page), then the run's own topic or monitor — spending at most
  `BRIEF_REFRESH = 3` model calls (`:43`). A scope whose stored `watermark` is already at the
  archive's newest id is skipped for free, so a second run over unchanged stories costs nothing.

**Measured state, this pass.** The chain is complete end to end on the write side and
green on each link: the table (`Store::briefings_init`, called by `init`) with `-p atlas-store
--lib briefings` **9 passed**; the prompt and the strict parse (`-p atlas-ai --lib brief`) **13
passed**; the digest and the row (`-p atlas-ingest --lib brief`) **20 passed**; the queue arm, the
refresh plan and the seam (`-p atlas-monitor --lib brief`) **18 passed** — wired in
`rust/atlas-monitor/src/wiring.rs:103-109` as `PipelineBriefs::new(store, pipeline)`, whose
`ready()` is `true`, its `watermark()` is `Store::briefing_get` and its `brief()` is
`Pipeline::brief` (`rust/atlas-monitor/src/brief.rs:227-259`); and the card, `-p atlas-desktop
--lib views::brief` **19 passed**.

**The window asks for one.** The last wire is in, and it is the window's, not the briefing
code's:

- `maybe_ask_for_briefing` (`rust/atlas-desktop/src/app.rs:1623`, called once per frame from
  `:1213`) refuses unless the shipped switch is on (`Atlas::asking_for_briefings`, `:614`), the
  page has at least one story and no briefing; it dedupes on `(scope, key, hours)` in
  `brief_asked` (`:462`), so a page read twice, or a chip visited again, does not queue a second
  request. The payload is `views::brief::payload(scope, key, hours, why)` and the row it writes
  is a `brief` request (`requests.rs`, `KINDS`).
- Both cards draw it: `brief::Ask::new` at `app.rs:1275` (Articles) and `:1705` (Overview), with
  `brief::show` (`rust/atlas-desktop/src/views/brief.rs:274`).
- Every read re-reads the row: `reload` calls `archive.briefing(brief_scope, brief_key, hours)`
  and, separately, `archive.briefing("window", "", hours)` (`app.rs:759-764`), so a briefing the
  agent writes while the page is open appears on the next read without a press.

Measured state: `grep -n "maybe_ask_for_briefing" rust/atlas-desktop/src/app.rs` →
`:1213` (the call) and `:1623` (the function); `grep -c "brief::Ask" rust/atlas-desktop/src/app.rs`
→ `3` (two `Ask::new` sites and the doc comment at `:280`). What is **not** measured here:
whether a shipped start writes the row end to end — that needs the agent, the model and a
running window, none of which this pass may start.

## 10. The reader

One page, read in the window: the story's own address, rendered by the operating system's web
engine where there is one, and as the archive's stored text where there is not. Nothing listens
and nothing is downloaded to make it work.

**The engine** — `rust/atlas-desktop/src/webview.rs`:

- `engine()` (`:21`) is `cfg!(any(target_os = "macos", target_os = "windows"))`. On macOS the
  engine is WKWebView, on Windows WebView2; both ship with the system. On Linux `wry` is not
  compiled in — it needs WebKitGTK installed before the app would even build — so `engine()` is
  `false` and the text reader is the page.
- `allowed(url)` (`:50`) is the one rule about what may load: `http`/`https` (case-insensitive),
  a non-empty host, no `user:pass@`, no control character or space, no bracketed IPv6 literal.
  `file:`, `data:`, `javascript:`, `about:` and every other scheme are refused, each for the
  reason written beside it. Loopback is allowed on purpose — the model server is there.
- `open(url, rect) -> Option<Page>` (`:107`) builds the view with
  `WebViewBuilder::new().with_url(..).with_bounds(..).with_devtools(false)`, a navigation handler
  that is `allowed` again for every link, redirect, meta refresh or `location.href` the page
  attempts, and `with_new_window_req_handler(.. Deny)` so `window.open` cannot make a second
  surface nobody governs; then `build_as_child(&parent)` with the window's raw handle (`:237`).
  It returns `None` when the address is refused or the engine cannot be created — and the caller
  draws the text page instead of an empty rectangle.
- `Page::set_bounds` (`:160`), `go` (`:175`), `back` (`:195`), `forward` (`:200`), `url`
  (`:205`). The address is kept in Rust and not read back from the engine: the engine answers
  asynchronously, and a half-navigated view would report the previous page.
- The engine uses the platform's default data store; this build sets no data directory and no
  incognito mode (`grep -c "with_data_directory\|with_incognito" src/webview.rs` → 0), so a
  page's cookies live where that platform keeps its web view's cookies.

**The page** — `rust/atlas-desktop/src/views/reader.rs`: `Story` (`:74`, the seven fields the
page draws, built from an archive row by `Story::of`), `Action` (`:108`: `None`, `Back`,
`Refresh`, `Close`, `Ask`, `Tag`) and `show(ui, story, view, stale)` (`:130`). With a view, the
page draws a toolbar and hands the engine a rectangle. The toolbar holds, left to right:
**`Close`** (`CLOSE`, `:128` — first, because it is the control a reader needs when the page
covers the window; it leaves the page and returns to wherever the reader came from), the history
arrow (only when there is a view to go back in), **the host of the address the page is on** as a
label — `Page::url()`, not the story's address, so a page that follows a link or a redirect is
named by where it went (`:144-150`; the label is not clickable, `:206-209`) — then `Reload`,
`Ask about this` and `Tag it`, and one hairline where the page begins. Without a view, the stored
text: title, `source · when`, the freshness line (`STALE_LINE`, `:62`), `Teaser only — the full
text was not read.` when `full_text` is false, the paragraphs, the address, and
`ENGINE_SENTENCE` (`:53`) saying this is the archive's copy of the page's words, not the page
itself.

**The shell** — `app.rs`: `Page::Reader` (`PAGES`, `:107`), `open_reader` (`:1434`, which gates
the address through `link::openable` *before* the engine is asked and keeps the story),
`reader_page` (`:1554`, which draws the way back, then `reader::show`), and the origin stack the
rest of the window uses. `Action::Close` (`:1570-1581`) is `go_back()` when there is an origin
and `set_page(Page::Articles)` when the address was opened without one; `Ask` drafts a question
on Chat; `Tag` queues a `research` row — the window still writes only its queue.

**The engine view lives exactly as long as the page does.** `close_reader_unless_here`
(`app.rs:645`) sets `reader_page = None` whenever the page is no longer `Page::Reader`, and it is
called from `set_page` (`:641`) and from `go_back` (`:1503`), so every door out — the rail, Close,
Back, Esc, Alt+Left — drops the view and the story with it. The view is **dropped, not hidden**.
Two reasons, both measured in the shipped window (`:3589-3597`): an egui panel cannot clip a
child web view, because the engine paints **over** the window rather than inside a layout — a
reader pressed "Map" with a source page open and the page stayed on top of the map, with nothing
in the window able to close it — and a hidden view would still hold a live document (cookies, a
running script) for a page the reader has walked away from. The rule is pinned by
`leaving_the_reader_closes_the_engine_view_and_the_story_with_it` (`app.rs:3596`), which walks
all four doors and fails if any leaves the view alive.

**What is measured.** `cargo build --offline --locked -p atlas-desktop` → `Finished \`dev\`
profile [unoptimized + debuginfo] target(s) in 12.61s` with the engine compiled in for this
platform, and the reader's own units and the drop rule pass (`--lib views::reader`,
`app::tests::leaving_the_reader_closes_the_engine_view_and_the_story_with_it`). What is **not**
verifiable without a screen: the engine actually rendering a page. `webview::open` needs a window
and a GPU session, so the code path is compiled and its refusals are tested, not run.

**Why this is not a browser.** One rectangle of one window, no tabs, no address bar, no
`window.open`, no devtools, no downloads — and the page is fetched by the system engine, not by
`atlas-net`, so the gate that protects the *agent* is not what protects the reader. The only
socket in the crate's own code is still `src/http.rs:525` (`docs/NO-PORTS.md` §3).

## 11. The data flow

One article, from a query to a thread:

1. **Search.** The model writes the query; `atlas-ingest::search` asks **one** endpoint — the
   reader's template (`meta.search_engine`, or `ATLAS_SEARCH_ENGINE`) and otherwise the built-in
   `search::DEFAULT_TEMPLATE`, Bing News' RSS feed (`rust/atlas-ingest/src/search.rs:62-63`);
   `resolve` tries config, then `meta`, then the environment, and a configured template that does
   not validate is skipped rather than turning every search into a refusal. It gates every link
   through `atlas-guard`, fetches the result page through `atlas-net`, unwraps the engine's
   click-through link so the model is handed the publisher's address, and returns title, link,
   publisher, date and snippet plus `"engine": {"host": …, "source": "configured"|"default"}`. The
   model decides what is relevant and what to open. No allowlist and no blocklist of publishers,
   hosts or words is consulted anywhere in the path.
2. **Read the whole page.** `read_article` goes to `atlas-ingest`: the URL is gated, fetched
   over `atlas-net` (redirects through the gate, byte cap, `identity` encoding, charset
   decode), and turned into text by `atlas-core extract`. A wall, a consent screen or a
   navigation dump is refused by one predicate (`quality::content_junk`), which is also the
   predicate used when storing and when repairing. When HTTP cannot reach the text there is no second
   transport: the call refuses with the JS-only sentence and the model chooses another link
   (`NullBrowser`, §8). The text is cached, so a second tool call about
   the same article does not pay for the page again.
3. **Classify.** The model reads that text and answers with a summary, tags, categories and a
   verdict on whether the page is reporting. That verdict is the only relevance decision;
   the model's output is cleaned and capped like any other untrusted text, and
   instruction-injection markers found in a page are flagged and kept, because the text is
   evidence.
4. **Store.** `save_article` applies the gates in one place: the 72-hour window and
   future-date skew (`atlas-dates`), the topic gate (`atlas-topics`), and duplicates by URL
   and by normalised title (`atlas-store`). Only a row that passes becomes an article, and
   it becomes one together with its FTS row, its tags, its categories and its topic links.
5. **Derive.** `atlas-places` decides the place from title, summary and body, weighted by
   zone, with a city beating its own country. `atlas-graph` writes entities, edges and
   article connections: IDF-weighted cosine over shared entities plus a topic signal, each
   connection stored with the sentence that explains it, and every article keeping its
   strongest partners so no story sits alone. Threads are built from those connections and
   never have a single member.
6. **Monitor.** A monitor check calls the same research pass for its own query, selects what
   is new since its last check, asks the model to judge each candidate, raises one alert
   above the threshold, records the below-threshold verdicts so the next check does not pay
   for them again, and writes a check-log row with the reason it ran.

Retention runs at the end of a collection pass, in that order: rows older than 21 days are
deleted in batches of 400 (children before the article), then the graph is rebuilt once, then
the reader fills in body text for what the model saved as a teaser. Nothing prunes on a timer,
and the listener's boot does not prune — a pass does, because a pass is what adds rows.

## 12. Shutdown order

The order is one constant, `shutdown::ORDER = [Window, Agent, Model]`
(`rust/atlas-launcher/src/shutdown.rs:51`), not the order a caller happened to register steps in.
The window reaps first — it is the only thing that draws and the only thing that talks to the
model, so a model stopped under it would leave a Chat page asking a port nothing answers. Then
the agent (`atlas-monitor serve`), because it *works*: it claims a request and drives a research
pass through the model, so stopping the model first would turn the action it is on into a
connection error; it is asked with `SIGTERM`, finishes the line it is printing and exits 0, and
a 20 s grace bounds the wait (`atlas-launcher/src/agent.rs`, `STOP_GRACE`). The model server goes
last. Every child is reaped before the process exits, and a process this launcher did not start
is never signalled. Before this the list was `Web, Browser, Model`; two of those are gone from
the product and the agent was added.

## 13. Where the truth lives

- What the window shows and what it reads: `rust/atlas-desktop/src/app.rs` (the rail, the
  pages, the status row) and `src/data.rs` (one query per list, read-only).
- What the model may do, and what it sees first: `rust/atlas-desktop/src/tools.rs`
  (`tools::names()` is the seven tools, `MAX_STEPS = 6`, `SYSTEM` is the prompt).
- How one question becomes a loop of steps: `rust/atlas-desktop/src/agent.rs`.
- The window's one write, and the kinds it may write: `rust/atlas-desktop/src/requests.rs`.
- Which module of the window owns what: `rust/atlas-desktop/src/lib.rs` (the fifteen modules, one line each).
- Whether a stored address may be opened, and what the link says: `rust/atlas-desktop/src/link.rs`.
- What the window's palette and type scale are: `rust/atlas-desktop/src/theme.rs`.
- The mark: `rust/atlas-desktop/src/logo.rs`.
- What the reader may load, and the window's engine: `rust/atlas-desktop/src/webview.rs`.
- How a briefing is written and read: `rust/atlas-ingest/src/brief.rs`, `rust/atlas-ai/src/brief.rs`, `rust/atlas-store/src/briefings.rs`.
- Where the window looks for the archive and the outlines: `rust/atlas-desktop/src/assets.rs`.
- What a request may say: the six kinds in `rust/atlas-desktop/src/requests.rs:61-68` and the
  per-kind payload checks beside them (`views/brief.rs` for `brief`).
- Which tools the model has and their argument bounds: `rust/atlas-ai/src/specs.rs` (the
  window's own read-only tools are `rust/atlas-desktop/src/tools.rs`).
- Which tools the model has and their argument bounds: `rust/atlas-ai/src/specs.rs`.
- The schema, migrations and retention: `rust/atlas-store/src/schema.rs`, `prune.rs`.
- The settings, their ranges and their environment names: `rust/atlas-config/src/spec.rs`.
- What a save refuses: `rust/atlas-ingest/src/save.rs` (window, topic gate, duplicate rules).
- Why two articles are connected, and why a story is a thread: `rust/atlas-graph`.
- Why a story sits where it does on the map: `rust/atlas-places`.
- Which ports exist and what stops in which order: `rust/atlas-launcher/src/ports.rs` and
  `shutdown.rs`.
