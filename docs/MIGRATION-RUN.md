# Carrying the archive over — the one-time procedure

Your `data/atlas.db` was written over months by the Python build. It holds the articles,
entities, links, threads, tags, alerts, monitors and topics you collected. The Rust build
opens that same file, so nothing has to be re-collected — but some of what it holds is
wrong, and some of it is a guess rather than a classification.

`atlas-migrate` is the tool that finds each of those, states it, repairs what a migration
broke, and re-derives the fields only the local model may decide. It runs on your machine,
against your file, and it prints what it did.

Nothing here downloads anything, opens a listening socket, or touches a process it did not
start. The only network traffic is the model client talking to `llama.cpp` on
`127.0.0.1`.

---

## Before you start

1. **Stop the app.** `atlas-migrate --apply` writes; two writers on one SQLite file is a
   fight. Close the server (`Ctrl-C` in its terminal) and leave `llama.cpp` running for
   step 4.
2. **Build the workspace and the engines.** The rule engines (`atlas-text`,
   `atlas-places`, `atlas-topics`, `atlas-dates`, `atlas-core`) answer the questions
   `normalize` asks:

   ```
   cargo build --release --offline --manifest-path rust/Cargo.toml
   ```

   If a binary is missing, `verify` says so and names the command. It never pretends the
   check passed.
3. **Keep a copy.** The tool is non-destructive by design — it deletes references to rows
   that are already gone, tags that cannot be a label, and the values `--apply` was asked
   to replace — but a copy is what makes "by design" cheap:

   ```
   sqlite3 "file:data/atlas.db?mode=ro" ".backup data/atlas.db.before-migration"
   ```

   `.backup` reads through the write-ahead log, so the copy is consistent even if the app
   was writing a minute ago — `cp` on the database file alone is not.

Everything below is run from the repository root.

---

## Step 1 — Look, and change nothing

```
cargo run --release --offline --manifest-path rust/Cargo.toml -p atlas-migrate -- verify
```

`verify` opens the archive **read-only** — `SQLITE_OPEN_READ_ONLY` plus `PRAGMA
query_only`, both enforced by SQLite, not by a promise — so this step cannot change a byte.
(`atlas-store` has no read-only open, so the read path uses `rusqlite` directly and the same
`Row`/`SqlValue` boundary `atlas-store::params` defines; every *write* goes through
`atlas-store`.) It prints:

* **row counts** for every table;
* **full-text health** — articles versus indexed documents, whether `ORDER BY rank` and
  `snippet()` answer, how many articles were probed for staleness;
* **findings** — one line each, with the kind (`fts.missing`, `ref.article_tags`,
  `publisher.variant`, `place.unsupported`, `tag.invalid`, `relevant.dupe`, …), the number
  of rows, the first ids, and whether a statement exists that fixes it;
* **checks that did not run**, with the reason (a missing engine is the usual one);
* **20 sampled rows per table**, with every body column shown as `<N chars>` — the report
  never contains article text, so it is safe to keep, paste or paste into a ticket.

Exit code `0` means nothing disagrees. `1` means something does — and step 2 lists the
statements that repair it. `3` means the run could not do its job (no archive, no
engines); that is not a verdict about your data.

Useful flags: `--json` (one object, for a script), `--no-samples` (the findings only),
`--sample N`, `--probe N`, `--place-limit N`.

## Step 2 — Read the statements before they run

```
cargo run --release --offline --manifest-path rust/Cargo.toml -p atlas-migrate -- normalize
```

A dry run is the **default**; `normalize` writes nothing unless you pass `--apply`. It
prints every statement it would execute, in order, with how many rows it touches and up to
eight bound values, then a separate list of findings that have **no** fix in the archive
itself (an article with no URL, a reversed `article_links` pair that only `atlas-graph` can
recompute).

The repairs it can make:

| Finding | Statement | What it means |
| --- | --- | --- |
| `fts.missing`, `fts.orphan`, `fts.stale`, `fts.rank`, `fts.snippet` | `INSERT INTO article_fts(article_fts) VALUES('rebuild')` | the index no longer matches `articles`; FTS5's own rebuild re-derives it |
| `ref.articles`, `ref.entities`, `ref.topics`, `ref.categories`, `ref.threads`, `ref.aliases` | the one `DELETE … WHERE … NOT IN (SELECT id …)` for that table | a row points at something the retention pass removed |
| `publisher.variant` | `UPDATE articles SET publisher=? WHERE id=?` | one outlet stored under two spellings; the spelling the archive uses most wins |
| `place.unsupported` | `UPDATE articles SET place=?, iso=?, lat=?, lon=?, geo_conf=?, geo_src=? WHERE id=?` | the stored place occurs nowhere in the article's own text, and `atlas-places` names another from that text **at least as precisely** — a city is never replaced by a country, and nothing is ever replaced by an outlet anchor |
| `tag.invalid`, `tag.variant`, `tag.publisher` | `DELETE FROM article_tags WHERE article_id=? AND tag=?` | a tag that cannot be a label, repeats another tag, or only repeats the outlet |
| `edge.order`, `edge.self` | `UPDATE edges SET a=b, b=a …` / `DELETE FROM edges WHERE a=b` | an edge stored against the `a < b` invariant |
| `relevant.dupe` | `UPDATE articles SET relevant=0 WHERE dupe=1 AND relevant=1` | a hidden duplicate still shown as relevant |

What it will **not** do, on purpose:

* it does not delete an article, a source, an entity, a topic or a thread;
* it does not decide that a tag is untrue — only that it cannot stand as a label;
* it does not clear or weaken a place it cannot replace (those rows are reported as
  `place.unnamed`: either the engine names nothing, or its answer is less precise than the
  stored one);
* it does not touch anything named in `docs/DATA-MODEL.md` as another crate's decision
  (a similarity score, a thread title, a link explanation).

## Step 3 — Apply

```
cargo run --release --offline --manifest-path rust/Cargo.toml -p atlas-migrate -- normalize --apply
```

One transaction: either every statement lands or none does. Then the tool verifies again
and prints `before: N broken finding(s) · after: M`. Running it a second time prints
`no statement: nothing in the archive disagrees` — every statement is conditional on the
state it repairs.

## Step 4 — Re-derive what a heuristic guessed

This is the long step, and the one that needs the model. `carry` works through the archive
newest-first and asks `llama.cpp` — through `atlas-ingest`, the same pass the app uses when
it reads a new page — to classify each article: entity names and types, relation labels,
aliases, categories, summary. It then re-derives topic membership with `atlas-topics`,
because topic membership is your topics' decision, not the model's.

```
cargo run --release --offline --manifest-path rust/Cargo.toml -p atlas-migrate -- carry                 # what it would do
cargo run --release --offline --manifest-path rust/Cargo.toml -p atlas-migrate -- carry --apply --limit 50
```

* **It never invents a value.** If the model does not answer, the values that pass
  attempted are rolled back and the article is recorded as `unclassified`; the next run
  asks again. An entity the model does not name loses the type a name-shape heuristic
  guessed (`ORG` unless the name is exactly two words) and stays unclassified — which is a
  state the schema already understands.
* **It is resumable.** Every carried article gets a receipt in `meta` under
  `migrate:carry:1:<id>`. A run that stops at `--limit`, at `--max-minutes`, or because
  you closed the laptop continues where it stopped. `--force` re-carries.
* **It is rate-limited.** `--rate-ms` (default 250) paces the calls; the model is local
  and single-threaded, so this is what keeps the machine usable.
* **It is crash-safe.** Before it detaches an article's entity links — which it must, so
  `atlas-graph` sees the model's names as new and can write the model's types — it writes
  the article's links, types, counts and timestamps to `meta`. The next run restores any
  snapshot it finds before doing anything else, so an interrupted article is finished, not
  guessed at.
* **It stops when the model stops.** If `llama.cpp` goes away mid-run the run ends; it does
  not carry on writing. A run that never reached the model prints
  `carry — nothing written (the model was not available; see below)` and exits `3`; the
  word `applied` only appears when something was actually carried.

Suggested cadence: `--limit 50 --max-minutes 20` per sitting, until `carry --apply` reports
`already carried` equal to the candidate count. On a modest machine that is roughly one
model call per article, so plan for a few evenings, not a minute. `--topics-only` runs the
deterministic pass alone and needs no model.

Tags are the one model-owned field this step leaves alone: `article_tags.source` is `ai`
both for tags the agent wrote and for tags the repair pass derived from entity names, so
the archive cannot tell them apart. `verify` reports them as `tag.provenance` — a note, not
a failure — and the model-owned tag pass stays `tag_article`, driven by the agent.

## Step 5 — Prove it

```
cargo run --release --offline --manifest-path rust/Cargo.toml -p atlas-migrate -- verify
```

Exit code `0` is the finish line. The `findings` section then holds only notes: entities
the model has not classified yet (`entity.untyped` — it shrinks as `carry` runs) and the tag
provenance note. `carry` reports its own progress: `classified by the model`,
`left unclassified`, `entity types replaced`, `topic writes`.

Delete the copy from the "Before you start" section once you are satisfied, or keep it —
21 days of retention means the archive turns over on its own.

---

## What the tool guarantees, and how each is measured

| Guarantee | Where it is asserted |
| --- | --- |
| `verify` cannot write | `open::tests::the_read_only_connection_refuses_a_write` |
| every read-only step works on a file it cannot open for writing | `tests::carry::every_read_only_step_works_on_an_archive_that_cannot_be_written` (the fixture is made `0444` first) |
| a dry run changes nothing | `tests::a_dry_run_changes_nothing_and_lists_the_right_statements` (whole-archive fingerprint before and after) |
| the real run fixes exactly what the dry run listed | `tests::the_real_run_fixes_exactly_what_the_dry_run_listed` (same row count, then a clean verify, then a second plan that is empty) |
| a second `--apply` is a no-op | `normalize::tests::applying_twice_is_applying_once` |
| an item the model did not classify stays unclassified | `tests::carry::a_silent_model_leaves_no_value_a_heuristic_guessed` (fingerprint identical after a pass whose model answered nothing) |
| an offline model writes nothing | `tests::carry::an_offline_model_writes_nothing_at_all` |
| a run that could not ask the model never says it was applied | `tests::carry::carry_without_a_model_exits_could_not_run_and_writes_nothing` (exit `3`, header `nothing written`, whole-archive fingerprint unchanged) |
| a carried article is not carried twice | `tests::carry::a_carried_article_is_skipped_by_the_next_run_and_a_resume_is_cheap` |
| the report carries no article text | `tests::the_report_carries_no_article_text`, `verify::tests::a_report_of_the_archive_names_no_body_text` |
| a missing engine is reported, not imitated | `tests::the_cli_reports_a_missing_engine_instead_of_answering_for_it` |

## If something looks wrong

* **`no database at data/atlas.db`** — pass `--db` with the file's real path.
* **`place check: atlas-places unavailable`** — build the workspace (step 2). The rest of
  the checks still ran; the report says which did not.
* **`carry` says `the local model is not answering`** — start `llama.cpp` the way
  `run.sh`/`run.ps1` does, then re-run. Nothing was written.
* **A finding you disagree with** — `--json` prints the same fields as the text report,
  including the statement and the ids. Nothing runs until you pass `--apply`, so a
  disagreement costs a look, not a rollback.
* **You want the archive back** — restore the copy. The tool makes no change it cannot be
  reproduced from, but a copy is simpler than a proof.

---

## What this archive looked like the first time it was measured

Numbers from `atlas-migrate verify` on the repository's own `data/atlas.db`
(2026-11-21), so you can tell a real change from an old report:

| | |
| --- | --- |
| articles / indexed documents | 1 435 / 1 435 — the index was **complete**, `ORDER BY rank` and `snippet()` answered, 20 articles probed, 0 stale |
| entities / entity links / edges | 4 539 / 6 286 / 7 338 |
| aliases pointing at a missing entity | 25 (`Fifa`, `NASDAQ`, `Friday's`, … — fragments the exporter kept) |
| one outlet, two spellings | 4 groups, 14 rows: `Folha de S.Paulo` → `Folha` (11), `Nbcnews` → `NBC News`, `Kyivpost` → `Kyiv Post`, `Bloomberg` → `Bloomberg.com` |
| stored places with no evidence in the text | 110 examined by `atlas-places`: 3 had a better answer, 0 were an improvement to apply, 1 named nothing, 4 were weaker than what was stored |
| tags | 12 rows on 2 articles, provenance not distinguishable (note) |
| entities no article links | 1 011 (note) |
| `normalize --apply` | 15 statements, 39 rows, 5 broken findings → 0; a second run: 0 statements, 0 rows |
| timings | `verify` 0.59–0.63 s over three runs, `normalize --apply` 0.75 s, `carry --apply` with no model 0.00 s (it refuses before writing) |

`carry` had 1 405 candidates (hidden duplicates excluded). The model pass is the only part
whose cost depends on `llama.cpp`, so its numbers are yours to measure: run
`carry --apply --limit 5 --max-minutes 2` once and read the summary line before committing
to a long run.
