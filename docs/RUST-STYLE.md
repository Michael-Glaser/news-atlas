# Rust in ATLAS — how the code is laid out

The app is Rust. That only stays true if the code stays readable, so the layout is
a rule, not a preference.

## One crate, one job

| Crate | Decides |
| --- | --- |
| `atlas-guard` | every security decision: URL/IP gate, text cleaning, escaping, redaction |
| `atlas-config` | settings, paths, the loopback invariant, the spawn environment |
| `atlas-platform` | every answer that depends on the OS: where the data dir and the model are, how a file is made private, where the browser is, how a child is spawned, whether a path is inside the app root |
| `atlas-store` | SQLite: schema, migrations, queries, retention |
| `atlas-text` | normalisation, titles, publishers, URL canonicalisation |
| `atlas-core` | HTML to text, entity extraction, similarity |
| `atlas-places` | where a story happened |
| `atlas-graph` | relations, threads, why two entities are linked |
| `atlas-topics` | whether an article belongs to one of the user's topics |
| `atlas-dates` | timestamps, the freshness window, display ages |
| `atlas-net` | reading a page over HTTP |
| `atlas-browser` | reading a page the HTTP client cannot reach, over CDP |
| `atlas-ai` | the local model, the tool-call protocol, the agent loop |
| `atlas-ingest` | read → classify → store, and the tools the model calls |
| `atlas-web` | the server: routes, static files, SSE, the request gate |
| `atlas-ui` | the frontend, compiled to wasm |

A crate that needs a rule from another crate calls it. A rule is never written twice.

## Inside a crate

```
src/
  lib.rs            crate docs, the module map, the public re-exports — under 150 lines
  main.rs           argv/stdin/stdout only, plus fn main()
  extract.rs        one concern, one verb in the name
  score.rs
  data/             static tables, data only, no logic
    cities.rs
  tests.rs          tests that exercise several modules at once
```

* **One concern per file.** No file over ~600 lines; static data tables are the
  only exception and stay data.
* **Names are nouns** that say what the module decides: `gate`, `hop`, `charset`,
  `undo`, `threads`. Never `utils`, `helpers`, `common`, `misc`, `base`.
* **`main.rs` holds no logic.** It reads arguments, calls the library, prints the
  answer, sets the exit code. Anything else belongs in a module.
* **Tests live next to what they test.** Cross-module behaviour goes to
  `tests.rs`. The suite never shrinks during a refactor.
* **Every module has a doc comment** saying what it decides and what it does not.
  Every public item keeps a doc comment. This is code meant to be read.
* **Public paths are API.** A refactor may move code; it may not make a caller
  change a line.

## Performance is a design property

* Single pass over text; borrow (`&str`, `&[u8]`) before you allocate; no
  `String` clone that a slice would do.
* No `unsafe`. If a crate forces it, one block, justified in a comment.
* The release profile is the only profile that ships: `lto`, `codegen-units=1`,
  `panic=abort`, `strip`.
* Numbers, not adjectives: a performance claim in a comment or a report has a
  measurement behind it (a `#[test]` that prints, or a command that was run).
* The measurement lives in `rust/atlas-bench` (a plain binary, no criterion) and
  its table in `docs/PERFORMANCE.md`. `cargo test --offline -p atlas-bench` fails
  when a path is more than 20 % over its budget, so a regression has to be argued
  with, not shipped.

## What a rule may never be

* No hardcoded news-word, publisher or host lists that decide what counts as news
  or what an entity is. The local model decides that, from the user's topics and
  the article text. Reference geodata (cities, countries, coordinates) and
  content-quality detection (cookie walls, paywalls, navigation dumps) are not
  classification and are allowed.
* No behaviour that depends on a fixed list of "known" people or organisations.

## Security lives in one place

`atlas-guard` answers: may this URL be opened, is this address loopback, what does
this text look like once untrusted characters are gone, is this string a secret.
Every other crate calls it. A second implementation of any of those answers is a
bug, not a convenience.
