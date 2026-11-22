# rust/ — the whole product

Every binary ATLAS runs is built here: one cargo workspace, 23 members, no
interpreted code in the product. `rust/target/release/atlas` is the only entry
point a person needs. The engine binaries beside it are how a decision is asked
from *outside* the app — inside it, the same crates are library calls, so a
stored article costs no child process at all.

```text
cargo build --release --offline --locked     # everything: atlas + the nine engine binaries + the tools
bash rust/build-ui.sh                        # the wasm UI + the offline tile pyramid
cargo test --offline --locked --workspace    # the suite
cargo run --release -p atlas-e2e -- run      # one hermetic end-to-end run
```

The suite on 2026-11-21 (15:57 UTC): 104 test targets, **2 495 passed, 0 failed, 13 ignored**
(the 13 are the deliberate ones). The repository `README.md` carries the same measurement.

## What is what

| member | what it decides |
| --- | --- |
| `atlas` (`atlas-launcher`) | the entry point: `start`, `doctor`, `help` — lock, settings, loopback invariant, the model child, the web listener in this process, the stop order |
| `atlas-web` | the HTTP surface: router, gates, static files, SSE, and the job routes that call the pipeline |
| `atlas-ui` | the face: Rust compiled to wasm, served from `public/ui/` |
| `atlas-ai` | the model client, the prompts, the 23 tool specs and the agent loop |
| `atlas-ingest` | one article's life: find it, read the whole page, classify it, store it |
| `atlas-store` | the SQLite archive: schema, migrations, queries, retention |
| `atlas-graph` | edges, article links, threads — and why two entities are connected |
| `atlas-search` | a search-endpoint client of its own (the model's query in, cards out). Nothing in the workspace links it: the tool the model calls is `atlas-ingest`'s `search`, which parses its own cards and goes through `atlas-net` and `atlas-guard` like every other fetch. Its own `rules::Engines` still spawns `atlas-text`/`atlas-dates`, which is why it is listed here as a member, not as a running part |
| `atlas-topics` | which of the user's topics an article belongs to |
| `atlas-monitor` | the standing questions the model re-checks |
| `atlas-places` | where a story happened |
| `atlas-dates` | timestamps, the freshness window, the age a reader sees |
| `atlas-net` | the HTTP client that reads pages |
| `atlas-browser` | the local browser, over CDP, for pages HTTP cannot read |
| `atlas-guard` | every security primitive: URL and address verdicts, cleaning, redaction |
| `atlas-config` | settings, paths, the download audit |
| `atlas-platform` | the platform answers: paths, permissions, executables, child environments |
| `atlas-migrate` | verify, normalize and carry an existing archive |
| `atlas-e2e` | the one-command proof: a temp install, a real browser, 49 routes |
| `atlas-bench` | the budgets, measured in release |
| `atlas-lib` | the small shared prelude the binaries agree on |
| `atlas-core`, `atlas-text` | HTML → text, entities, similarity; normalisation and titles |

## Before you write Rust here

`docs/RUST-STYLE.md` is the house style: one concern per file, no monoliths, no
`utils` grab-bags, `lib.rs` is a map, `main.rs` is wiring, every public item has a
sentence saying what it decides. `docs/RUST-PROOF.md` is the record that the
product is Rust and what would break that. The architecture and the request path
are in `docs/ARCHITECTURE.md`.

Nothing in this tree downloads anything at run time. A build fetches crates once;
a missing model, browser or engine is a sentence with the manual step, never a
silent fetch.
