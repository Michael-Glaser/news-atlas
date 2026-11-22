# ATLAS documentation

Everything in this directory, one line each, in alphabetical order. The user-facing overview
is the repository `README.md`; `docs/ARCHITECTURE.md` and `docs/OPERATIONS.md` are the two to
read first when you are working on the app, and `docs/DESIGN.md` the one to read before you
draw anything.

Where a document and the code disagree, the crate headers in `rust/<crate>/src/lib.rs` are
the truth. This index says which files carry history, so that a record of a change that already
happened is not mistaken for a description of the tree today.

| File | What it really contains |
| --- | --- |
| [`API.md`](API.md) | **A record, not a contract: the listener it documents is deleted.** `atlas-web` is not in the workspace (`ls rust`), so `rust/atlas-web/src/routes/mod.rs` — the file this row used to call the source of truth — does not exist either, and `http://127.0.0.1:8787` is a port nothing in this tree can open. What survives as true is the shape a request had: conventions, error shapes, parameter ranges, one example per route. Read it as the history of an interface a reader in an older checkout might still meet, and do not implement against it. |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | How the app is put together now: the native window (`atlas-desktop`) as the product's face, fifteen modules, the reader in the system web view, and no listener, then the `atlas` binary as the collection entry point with its four programs, the 21 member workspace as one table (with the four deleted crates named), the request path (a row in `data/requests.db`), the retired listener and wasm frontend, the browser library that is gone, the model server child, the briefings (who builds the digest, which prompt, which budgets, who asks and who refreshes), the reader (the current URL, Close, the view dropped when the page is left), the data flow and the shutdown order. |
| [`CUTOVER.md`](CUTOVER.md) | **History.** The cut-over as an executed runbook: the commands in the order they ran, what each proved, what was deleted, how to verify it on a fresh clone, and what rollback would have meant. A record, not a task list. |
| [`DATA-MODEL.md`](DATA-MODEL.md) | The archive as it is now, with a §0 on why it is SQLite (and what Postgres would cost) and the `briefings` table at §12: every table and column that matters, the external-content FTS5 table, `article_links.why`, the `meta` keys and the invariants the writers keep. DDL is named as `rust/atlas-store/src/schema.rs`, `monitors.rs` and `briefings.rs`, retention as `prune.rs`; where a sentence names a Python function it says which Rust crate inherited that behaviour. |
| [`DESIGN.md`](DESIGN.md) | The window's visual language, as a rule a contributor can follow: the grey palette and the contrast figures `theme.rs` computes, the five-size type ladder and the two bundled faces, the 4 px space scale, the single 2 px radius, hairlines against the focus ring, what is clickable and what a click does, the map's and the graph's interaction rules, the empty and error states, the mark, and what the document does not allow — each rule cited to the constant or the test that enforces it. |
| [`MIGRATION-RUN.md`](MIGRATION-RUN.md) | The one-time procedure for carrying an archive written by the Python build: look, read the statements, apply, re-derive the model's fields, prove it. The same three steps `atlas-migrate` exposes today: `verify`, `normalize`, `carry`. |
| [`NO-PORTS.md`](NO-PORTS.md) | **Measurements, re-runnable.** What each shipped binary's dependency closure holds, the four programs `atlas start` can spawn, every listener and every dial (the fourth is `atlas-ai`'s), why no shipped binary starts a browser any more (§2.1), the download audit, and what was red on the afternoon of 2026-11-22 — the document `OPERATIONS.md` §9 and `THREAT-MODEL.md` §4.14 point at for "ATLAS opens no port". |
| [`OPERATIONS.md`](OPERATIONS.md) | The operator's view: start and stop (the window alone, then `atlas start`'s four programs, the Windows Ctrl-Close stop, what the window reads and writes, and the six `requests.db` kinds including the one that writes `data/config.json` and the one that asks for a briefing), the six model settings with their ranges, defaults and which flags `llama.cpp` actually sees, what each log says and when it is written, `atlas doctor` check by check, provisioning a fresh machine with `atlas-setup` (the window, the archive, the map outlines, the model file, the model server), a held port, a corrupt archive, backups, moving the install, and §9 — the one port `llama-server` listens on and the numbers nothing binds. |
| [`PERFORMANCE.md`](PERFORMANCE.md) | The measured numbers from `rust/atlas-bench`: the seeded archive, the nine HTTP paths the UI reads, store paths, the per-article pipeline cost, the UI paths, startup, the budget each path must stay under, what was measured and not shipped, and what is still slow. Its two `pipeline.*` rows were measured before the engines moved in-process and say so; the measured before/after for a whole save is quoted from `atlas-ingest/tests/engines.rs`. |
| [`README.md`](README.md) | This index. |
| [`RUST-CRATE-CONTRACT.md`](RUST-CRATE-CONTRACT.md) | **History.** The contract used while porting one Python module to one Rust binary: the stdin/stdout wire format, fidelity rules, tests, the old `server/<name>core.py` bridge and the report-back format. A record of the migration, not a rule for new code — the bridge sections describe files that no longer exist. |
| [`RUST-MIGRATION.md`](RUST-MIGRATION.md) | **History.** The record of the migration: every module that lived in `server/` and what replaced it, what was deleted and when, the crate map, how the cut-over was proven, the rules that survived, and a left-overs list kept current against the tree. |
| [`RUST-PROOF.md`](RUST-PROOF.md) | A re-runnable proof that nothing non-Rust ships: the commands and their real output, every non-Rust file that remains and why, one paragraph for a reviewer, and a section that states what the repository does not prove (the crate cache is machine-local; the one exemption in the no-download audit, and why it is not a hole), plus §1.1 on `wry`: the system web view the reader renders into, macOS and Windows only, with the tree counts that show it. |
| [`RUST-STYLE.md`](RUST-STYLE.md) | How the Rust is laid out: one crate one job, what goes inside a crate, performance as a design property, what a rule may never be, and why security lives in `atlas-guard`. |
| [`SECURITY-PROOF.md`](SECURITY-PROOF.md) | **History with live commands.** The adversarial audit of `rust/`: the claim, the test that proves it, the command that re-runs it — loopback, the gate, no downloads, 10 000 mutated inputs per parser, secrets kept out of logs, CSP, and the section on what the audit does not prove and what it found open. Written before today's changes; `SECURITY-RUST.md` and `THREAT-MODEL.md` are the current statements. |
| [`SECURITY-RUST.md`](SECURITY-RUST.md) | The contract of `rust/atlas-guard`: what decides, where each decision is reached from, the three rules the crate exists to keep, how to prove them, and the limits it states plainly (including where the DNS filter stops covering). |
| [`THREAT-MODEL.md`](THREAT-MODEL.md) | The attack surface as it is now: the native window (`atlas-desktop`) as the face, which opens no socket and executes no markup, the `atlas` process that opens no listener at all (the model server listens; the window dials it), the `llama-server` child, the archive, the window's one writable file (`data/requests.db`) and — retired but still on disk — the wasm frontend; what each attacker can try, what stops it, and where a claim is still open — §4.1's browser-path DNS residual, §4.3's un-fenced graph JSON, §4.9's failed-`--help` model log. Two sentences in it still describe a browser child the pipeline no longer starts (`:28`, `:515`); they are its owner's to retire. |

## Two documents were deleted, and why

`docs/RUST.md` and `docs/SECURITY.md` described the Python host: the optional `atlas-core`
bridge with its breaker, payload floors and Python fallback, and the pre-cut-over security
narrative that named `server/*.py` and `public/js/`. Both files were gone, so almost every
sentence was false, and what was still true (the localhost-only model, the native contract) is
stated with evidence in `SECURITY-RUST.md` and `THREAT-MODEL.md`. They were deleted on
2026-11-21 rather than kept as a stale shelf; `docs/CUTOVER.md` and `docs/RUST-MIGRATION.md` are
the history, and they say they are history.

## Order for a first read

1. `ARCHITECTURE.md`: "The window is the product now" first — the fifteen modules, the read-only
   archive, the one outbound request — then §1–§5 for the collection side: the process, the
   entry point, the crates, the request path.
2. `OPERATIONS.md` §1–§3: the window, the model server, logs, and what `atlas doctor` measures.
3. `DESIGN.md` §1–§5 before you write a view: palette, type, space, form, focus.
4. `DATA-MODEL.md` §1 and the invariants, then the table or endpoint you intend to touch — in
   `API.md`.
5. `SECURITY-PROOF.md` §0 for the shape of the proof, then `THREAT-MODEL.md` §4 for what is
   claimed today.

`ARCHITECTURE.md` §13 lists the file that answers each "where is this decided?" question, so
nothing here repeats it.
