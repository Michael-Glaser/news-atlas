# Cut-over: what was done, in order

The cut-over is complete. This file is the executed runbook, kept as a record: the commands
in the order they were run, what each one proved, and the same sequence as a check a reader
can run today on a fresh clone. `docs/RUST-MIGRATION.md` is the map of what replaced what.

## 0. Preconditions

```bash
cd rust && cargo build --release --offline      # every binary in target/release
cargo test --offline --workspace                # the workspace, nothing fetched
bash rust/build-ui.sh                           # public/ui/: wasm + loader + tiles
```

`cargo build --release --offline` produces `rust/target/release/atlas` (the entry point) and
the engine binaries it needs. `build-ui.sh` compiles `atlas-ui` to `wasm32-unknown-unknown`,
runs `wasm-bindgen` to write `public/ui/`, rasterises the map tiles and copies the shell plus
the three-line `loader.js`. Both steps are build steps: the only network use `build-ui.sh` can
make is the one-time `wasm32` target and the matching `wasm-bindgen-cli`, and it prints the
command instead of fetching silently. `bash rust/build.sh` is the same build plus the eleven
binaries it checks for and `atlas-config audit` as its policy gate.

`cargo test --offline --workspace` is green. It was not, for a while: `atlas-graph` kept a
row-parity test that asserted the deleted `server/graph.py` was on disk, so it panicked on a
machine with a Python interpreter. Those tests are gone (`atlas-graph/src/db_tests.rs` no longer
names Python), which is what makes the workspace-wide run meaningful.

## 1. `atlas doctor` came first

```bash
cargo run --release -p atlas-launcher -- doctor
# or, once built:
./rust/target/release/atlas doctor
```

9 checks, in the order a person would look at them, and **no network call anywhere**:

| Check | What it answers |
| --- | --- |
| `toolchain` | is a Rust toolchain present |
| `native binaries` | are the nine engine binaries next to the app |
| `model file` | is the GGUF where `data/config.json` says, and how big |
| `model server` | is there a `llama-server` to start |
| `loopback invariant` | does the configuration ask for `127.0.0.1` only |
| `sqlite` | can it open and write a database in `data/` (probe file removed afterwards) |
| `browser` | is there a reader browser on this machine |
| `ports` | are the app, model and debug ports usable |
| `data permissions` | is `data/` private to this account |

Exit 0 means `atlas start` will not stop at its second step. A failing check names the path
and the manual step; nothing is downloaded to fix it.

## 2. The archive was carried, not recreated

```bash
cargo run --release -p atlas-migrate -- verify
cargo run --release -p atlas-migrate -- normalize --apply
cargo run --release -p atlas-migrate -- verify
```

`verify` is read-only by construction (`SQLITE_OPEN_READ_ONLY` plus `query_only`) and exits
non-zero when anything disagrees. On this archive it reported **0 broken rows and 4 notes**: the
disagreements it found were informational, not corrupt. (`atlas-migrate`'s own
`report::tests::a_note_never_fails_the_run_and_a_broken_finding_does` is the rule: a note is
coverage, a broken finding is a verdict.)

`normalize` writes nothing without `--apply` — a dry run is the default, not a flag — and it
lists every statement before running it. `--apply` runs the plan in one transaction. It only
touches what the report proved: a full-text index that lagged behind an edit, a row pointing
at an article the retention pass removed, two spellings of one newspaper, a place the text
does not support, a tag that cannot stand as a label. The second `verify` is the proof that
the writes were the ones the report named, and a second `normalize --apply` reports zero
changes.

`carry` was **not** run at cut-over: it re-derives the model's fields, which needs a live
model, and it is a separate decision (`atlas-migrate carry --apply --limit N`).

## 3. The entry point is one binary

```bash
./rust/target/release/atlas start
# equivalent, and what most people run:
./run.sh
```

`atlas start` does the whole boot in one order: a stop handler before anything is spawned, one
instance per data directory (the lock), settings from `atlas-config` (env > `data/config.json`
> default), the loopback invariant checked before a socket exists, the model file checked, the
model server started as a child of this process with the pinned flags, the supervisor attached
before the health wait (one restart with a backoff if it dies while loading), `atlas-web`
serving in this same process through `atlas_web::wiring::real`, and the browser opened at the
address the listener actually bound. Ctrl-C shuts everything down in one order and leaves no
child behind.

The model server's argv, in this order:

```
-m <model>  -c 8192  --host 127.0.0.1  --port <llama_port>  -np 1  --jinja  [--no-webui]  [--spec-type draft-mtp]
```

The first six flags are always passed. `--no-webui` and the MTP flag are asked for only when
the binary's own `--help` names them, so a build that has neither simply gets neither — no
version guessing, no failing start on an unknown option.

## 4. Verified before anything was deleted

```bash
cargo run -p atlas-e2e -- run
```

24 steps in one run, none of them touching the real `data/`:

* a temporary root with `public/`, a freshly seeded `atlas.db` and the binaries copied in;
* a fake model on loopback answering `/v1/models`, `/props` and `/v1/chat/completions`;
* `atlas-web` on an ephemeral loopback port, its address read out of the process's own output;
* the 49 documented routes driven over a real socket, plus the write paths read back;
* one full chat turn through the agent loop, counted model calls included;
* the security invariants: listener is loopback (as reported, as `/api/status` claims, and as
  the socket table sees it), no second socket in the process, a traversal path is a 404, an
  oversized body is a 413, every path the app reports writing is under the temporary root and
  none under the real `data/`, and the boot audit's "nothing is downloaded" verdict is true;
* a real headless-Chrome smoke of the wasm bundle: the shell renders, the seven tabs exist,
  the article list holds this server's newest headline, an article opens, the graph draws, the
  map paints, and the console has no error (a machine with no browser is a `skip` with a
  sentence, never a failure);
* shutdown, then the table: what ran, what it proved, how long it took.

Exit 0 is the verdict. `--no-ui` skips the browser half, `--keep` leaves the temporary root.

## 5. What was removed

Only after step 4 passed:

```bash
git rm -r server/                 # the Python host and its bridges
git rm -r public/app.js public/js/  # the JavaScript frontend
git rm -r tools/*.py              # the Python check suite
git rm -r rust/atlas-net/checks/  # the transport crate's Python harness
```

`run.ps1` and `setup.ps1` were **kept**: they are thin wrappers around the binary, like
`run.sh` and `setup.sh`. What must stay, and does: `public/css/*.css`, `public/ui/`,
`public/index.html` (the shell), `rust/`, `data/`, `docs/`, and `tools/llama.cpp/` (the
vendored upstream build that produces `bin/llama-server`).

After the deletions:

```bash
cargo run --release -p atlas-launcher -- doctor   # still exit 0 with no Python in the tree
cargo run -p atlas-e2e -- run                     # still every step
grep -rn --exclude-dir=target --exclude-dir=llama.cpp "python3" . | grep -v "^./docs/"
```

The last command is the one that finds the stragglers. Today it reports one doc comment in
`atlas-web/src/lib.rs` that still names `python3 server/app.py`, one in `atlas-guard/src/lib.rs`
that still says "Python fallback", `setup.ps1`'s dead step 5 (it looks for the deleted
`tools/doctor.py` and then says "no Python on this machine"), and `README.md`, which still lists
the deleted `tools/*.py` checks. `docs/RUST-MIGRATION.md` §6 covers the code-side ones.

## 6. How to verify the cut-over on a fresh clone

Six commands, in order. On a first build the crate cache and the `wasm32` target may be empty;
both steps say so and retry once with the network, at **build** time only.

```bash
# 1. the workspace builds, nothing fetched at run time
cd rust && cargo build --release --offline && cd ..

# 2. the frontend bundle (wasm + loader + tiles)
bash rust/build-ui.sh

# 3. the machine is ready; 9 checks, no network
./rust/target/release/atlas doctor

# 4. the archive still agrees with itself (read-only)
cargo run --release -p atlas-migrate -- verify

# 5. the whole app, the 49 routes, the security invariants, the UI smoke
cargo run -p atlas-e2e -- run

# 6. start it for real
./run.sh            # or: ./rust/target/release/atlas start
```

`./setup.sh` is steps 1–3 plus the `data/` permissions, in one command, and is idempotent.
`./setup.ps1` is the same on Windows.

What a good run looks like: `atlas doctor` exits 0 with nine `ok` lines (a `warn` is "ATLAS
starts with less than this machine could give it"), `atlas-migrate verify` exits 0, `atlas-e2e
run` exits 0 with every step `ok` or `skip` and no `FAIL`, and `atlas start` prints the loopback
URL it bound and the model it loaded. `curl -s 127.0.0.1:8787/api/health` answers
`{"ok":true,...}`.

## 7. Rollback

The Python host was deleted only after step 4 passed, and the archive is untouched by the
Rust build: `atlas-migrate verify` is read-only, and `normalize` only writes what the data
itself says. If the Rust app ever has to be set aside, the previous commit plus the same
`data/atlas.db` is a working app again.

## 8. Looking back

There is no git history in this working copy (`git log` here answers *not a repository*), so
the order above is the only record of the sequence in this tree. In a checkout that has the
history, `git log --diff-filter=D --name-only -- server/ public/app.js public/js/ tools/`
prints the deletions in the order they happened, and each cut-over commit carries the step it
belongs to.

## 9. What "no slop" means here, concretely

* No Python process, no JavaScript logic, no second HTTP client, no second SQLite layer. The
  only hand-written JavaScript left is `public/ui/loader.js` (three lines, 70 bytes); `public/ui/atlas_ui.js`
  next to it is wasm-bindgen's generated glue, rewritten by every `build-ui.sh` run.
* One listener on `127.0.0.1`. Nothing is downloaded at run time.
* The model decides what it searches, what it reads and what a thing is; the code carries no
  publisher, news-word or entity-type list.
* Every rule that decides something lives in exactly one crate, and that crate has a test that
  fails when the rule changes.
