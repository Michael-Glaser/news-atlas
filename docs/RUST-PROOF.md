# RUST-PROOF

A sceptic should be able to re-run every command below and get the same output. They were
run from the repository root on 2026-11-21, on macOS arm64, with `python3` present at
`/usr/bin/python3`; the audit in §4 and the window's counts in §2.5 were re-run on 2026-11-22
and say so where they land in the text. Nothing here is a claim: where the repository does not
yet meet the guarantee, §5 says so.

---

## 1. What "100% Rust" means here — and what it does not

**It means:** every line of code that *runs as part of ATLAS* is Rust compiled by cargo.
The product's face is `rust/target/release/atlas-desktop`, a native window drawn by
`egui`/`eframe` from a read-only `rusqlite` connection — Rust, with no HTML, CSS or JavaScript
written by this project anywhere in it (§1.1 is the one place a page's own markup is rendered,
by the operating system's engine). The collection process (`rust/target/release/atlas`) is Rust, and so
are the programs it starts: the window, the agent (`atlas-monitor serve`),
`bin/llama-server`'s supervisor, and — on a machine that is not provisioned — the install child
`atlas-setup --install`, which is Rust too. No program of ours launches a browser any more: the
pipeline's browser seam is `NullBrowser` (`atlas-ingest/src/lib.rs:368-410`), so the only
listening socket in the product is `llama-server`'s loopback port. The web listener still exists
as `atlas-web` — a Rust crate and a binary — but **no shipped binary links it any more**: the
product serves nothing, and the harness (`atlas-e2e`) is what starts it.
Every helper the app talks to — `atlas-core`, `atlas-guard`, `atlas-text`, `atlas-graph`,
`atlas-config` — is a Rust binary or library in the same workspace, built by the same
`cargo build`.

**No HTML, CSS or JavaScript of ours is needed to see the archive.** That is the change the
window made, and it is checkable in one place: `rust/atlas-desktop/Cargo.toml` names five direct
dependencies on macOS and Windows, and `cargo tree -p atlas-desktop --depth 1` prints five lines
at 18:03Z on this Mac — `eframe v0.37.0`, `egui v0.37.0`, `rusqlite v0.43.0` (bundled),
`serde_json v1.0.154`, `wry v0.57.3`, each pinned `=x.y.z`. The crate contains no HTML string, no
template and no listener, and no JavaScript is written by it. Its `include_str!`s are
all test code reading the crate's own `.rs` files
(`src/theme.rs:990`, `src/app.rs:574`, and the integration tests that scan `data.rs`, `app.rs`
and `atlas-store/src/schema.rs` for SQL), so no asset is baked into the binary. The wasm frontend
in `public/` still builds and is kept as history — `atlas-e2e` still loads it — but nothing the
product shows a user goes through it.

Its one socket is stated rather than implied, because it is the single exception to "the window
opens nothing": `src/http.rs` makes one outbound HTTP/1.1 POST to
`http://127.0.0.1:<llama_port>/v1/chat/completions` when you ask the chat a question. The
address is built as `SocketAddr::from(([127, 0, 0, 1], port))` (`src/http.rs:452`) from a URL
the window itself re-parses, so no name is ever resolved; there is no TLS, no redirect, no retry
and a 30 s timeout. That port is the only number the window takes from `data/config.json`
(`llama_port`, `src/assets.rs:101`), and the window listens on nothing — so it cannot be the
reason a port is held, and a held port cannot keep it from opening (`docs/OPERATIONS.md` §9).
`rust/atlas-desktop/tests/no_spawn_no_socket.rs` scans `src/` for a listener
bind and for `std::process` and fails on either, with one reviewed exception each for the client
and for the chat page's worker thread — so the claim is pinned, not just written down.

### 1.1 `wry`, and why the reader does not break the claim

The reader shows a story's own page in the window. On macOS and Windows that page is rendered by
the **operating system's** web engine, reached through `wry` — a Rust crate (Rust FFI bindings,
no C++ of ours) that hands the platform a rectangle of our window. Measured 2026-11-22:

```text
$ cargo tree --offline -p atlas-desktop --depth 1 --edges normal
├── eframe v0.37.0 ├── egui v0.37.0 ├── rusqlite v0.43.0 ├── serde_json v1.0.154 └── wry v0.57.3

$ cargo tree --offline -p atlas-desktop --edges normal | grep -c wry                    # this Mac
1
$ cargo tree --offline -p atlas-desktop --target x86_64-unknown-linux-gnu --edges normal | grep -c wry
0
$ cargo tree --offline -i wry
wry v0.57.3
└── atlas-desktop v0.1.0 (…/atlas-desktop)
```

`wry` is an exact pin (`=0.57.3`, `Cargo.lock:3828`) and one package in the window's graph: the
normal tree resolves 159 packages, of which `wry`'s own subtree is 61 (`cargo tree -p wry
--edges normal --prefix none --no-dedupe | sort -u | wc -l`). It has exactly one parent, the
window. On Linux it is not compiled at all — the same `cargo tree --target` above answers `0`,
because the dependency sits in the manifest's `[target.'cfg(any(target_os = "macos",
target_os = "windows"))'.dependencies]` table — and the reader draws the archive's text instead.

**Why this is still 100% Rust.** No web engine is downloaded and none is vendored: WKWebView and
WebView2 ship with the operating system, and `wry` is the binding that asks for one. There is no
HTML, CSS or JavaScript *of ours* anywhere in the product — the markup the engine renders is the
third-party page the reader asked to read, in a view with no tabs, no address bar, no devtools
and no `window.open` (`src/webview.rs`, `with_devtools(false)` and
`with_new_window_req_handler(.. Deny)`), and only `http`/`https` addresses with no credentials
reach it (`webview::allowed`). What it does mean, plainly: **a page's own scripts run**, in the
system engine, inside this window — the reader is a reader of the web, not a sandbox around it.
That is the one place the product executes code it did not compile, and `docs/THREAT-MODEL.md`
is where the consequences belong.

**It does not mean "no other language exists on disk":**

- **The model server is upstream C++.** `bin/llama-server` and the 25 dylibs beside it are
  compiled from the vendored `tools/llama.cpp` tree. It is a child process on
  `127.0.0.1`; Rust starts it, supervises it and stops it, but does not implement it.
  Re-implementing GGML/llama.cpp is a different project.
- **The retired frontend is Rust, but its instantiation was JavaScript.** `atlas-ui` compiled
  to `public/ui/atlas_ui_bg.wasm`. Reaching it from a page needed a module loader and
  wasm-bindgen's generated glue. That glue (`public/ui/atlas_ui.js`, 49 KB) is **generated,
  never hand-edited**, and the only hand-written JavaScript in the tree is
  `public/ui/loader.js`: 3 lines, 70 bytes.

      import init, { start } from "/ui/atlas_ui.js";
      await init();
      start();

  It exists as a file, not an inline `<script>`, because the server sent
  `script-src 'self'`. No window, no product binary and no documentation path needs it now.
- **Data and styling are not code.** CSS, GeoJSON coastlines, PNG map tiles, the GGUF
  weights and `data/` at run time have no Rust form and should not be pretended into one.
  §3 itemises every one of them.

The honest one-sentence version: *the app is Rust — the window most of all, with no browser in
its path; the model it talks to is C++ it did not write; and the web frontend that needed a
loader to boot Rust is retired.*

One more crate sits beside the app and is Rust too: `atlas-setup` (§2.7), the install-side
binary that builds the model server and fetches the pinned model file. It is not linked into
the app; `atlas start` runs it as a child only on a machine where the model file or
`bin/llama-server` is missing, and never on a provisioned one — which is why the audit names it
instead of leaving it to be discovered.

---

## 2. The commands that prove it

Each block is the command as run, then its real output.

### 2.1 There is no Python

    $ find . -name "*.py" -not -path "./tools/llama.cpp/*" -not -path "./rust/target/*" -not -path "./data/*"
    $ echo "exit=$?"
    exit=0

Nothing printed. (`tools/llama.cpp/` is excluded because it is upstream and carries 268
`.py` files of its own — see §3.)

### 2.2 There is no JavaScript outside the generated UI

    $ find . -name "*.js" -not -path "./tools/llama.cpp/*" -not -path "./rust/target/*" -not -path "./public/ui/*"
    $ echo "exit=$?"
    exit=0

Nothing printed. Without the `public/ui/*` exclusion there are exactly two `.js` files, both
generated: `public/ui/loader.js` (3 lines) and `public/ui/atlas_ui.js` (wasm-bindgen glue).

### 2.3 No TypeScript, Ruby, Go, Java, PHP, C

    $ for ext in py js ts tsx rb go java php c cc cpp h hpp; do \
        n=$(find . -name "*.$ext" -not -path "./tools/llama.cpp/*" \
              -not -path "./rust/target/*" -not -path "./data/*" \
              -not -path "./public/ui/*" | wc -l | tr -d ' '); \
        printf "%-6s %s\n" ".$ext" "$n"; done
    .py    0
    .js    0
    .ts    0
    .tsx   0
    .rb    0
    .go    0
    .java  0
    .php   0
    .c     0
    .cc    0
    .cpp   1
    .h     0
    .hpp   0

The single `.cpp` is a false positive worth stating plainly: it is the **directory**
`./tools/llama.cpp` itself — `-name "*.cpp"` matches the directory entry, and
`-not -path "./tools/llama.cpp/*"` excludes its contents but not its name. There is no C++
source file outside the vendored tree.

Outside the vendored tree there are exactly two `.ts` files, both in `public/ui/` and both
generated by wasm-bindgen: `atlas_ui.d.ts` and `atlas_ui_bg.wasm.d.ts`. They are type
declarations with no runtime. (`tools/llama.cpp/` holds 806 more of upstream's own.)

### 2.4 The shell scripts that remain — and why they are not the app

    $ find . -name "*.sh" -not -path "./tools/llama.cpp/*" -not -path "./rust/target/*" -not -path "./data/*" | sort
    ./run.sh
    ./rust/build-ui.sh
    ./rust/build.sh
    ./setup.sh

    $ find . -name "*.ps1" -not -path "./tools/llama.cpp/*" -not -path "./rust/target/*" -not -path "./data/*" | sort
    ./run.ps1
    ./setup.ps1

**The four wrappers a user touches** are `run.sh`, `setup.sh`, `run.ps1`, `setup.ps1`. They
do only what a shell does better than a binary: make sure a build exists, then hand off. The
proof that they are not the app is the last line of `run.sh`:

    exec "$ATLAS" "${ARGS[@]}"

`run.sh` replaces itself with `rust/target/release/atlas`; after that the shell is gone. All
behaviour — ports, the model file, the loopback check, the child's environment, the stop
order — lives in `atlas-launcher`, where it is tested. Same for `setup.sh` (it invokes
`cargo build`, then `atlas-setup check`, then `atlas doctor`), and the `.ps1` twins. The one
thing those scripts hand to a binary is provisioning: `atlas-setup`, which is install-side
only (§2.7).

`rust/build.sh` and `rust/build-ui.sh` are **build-time** drivers, also not the app: they
call `cargo build` / `cargo install wasm-bindgen-cli` and copy the outputs. Nothing in them
runs while ATLAS serves a page.

A `.sh` is therefore "code that runs the app" only in the sense that a shell can `exec` a
binary. Remove all four and the binary still starts; give the binary to a user and they
never need them.

### 2.5 Every binary is built by cargo

    $ cargo metadata --offline --no-deps --format-version 1 --manifest-path rust/Cargo.toml \
        | jq -r '.packages[] | .name as $p | .targets[] | select(.kind[]=="bin") | "\($p) -> \(.name)"' | sort
    atlas-ai -> atlas-ai
    atlas-bench -> atlas-bench
    atlas-browser -> atlas-browser
    atlas-config -> atlas-config
    atlas-core -> atlas-core
    atlas-dates -> atlas-dates
    atlas-desktop -> atlas-desktop
    atlas-e2e -> atlas-e2e
    atlas-graph -> atlas-graph
    atlas-guard -> atlas-guard
    atlas-ingest -> atlas-ingest
    atlas-launcher -> atlas
    atlas-migrate -> atlas-migrate
    atlas-monitor -> atlas-monitor
    atlas-net -> atlas-net
    atlas-places -> atlas-places
    atlas-search -> atlas-search
    atlas-setup -> atlas-setup
    atlas-store -> atlas-store
    atlas-text -> atlas-text
    atlas-topics -> atlas-topics
    atlas-ui -> atlas-ui-tiles
    atlas-web -> atlas-web

25 workspace members (`members` in `rust/Cargo.toml`, confirmed by `workspace_members` in the
metadata), 23 binary targets — both counted from `.packages`, not from the manifest by eye. Two
of them are the ones a person runs: **`atlas-desktop`, the window**, and **`atlas`**, the
collection entry point, which is `atlas-launcher`:

    $ ./rust/target/release/atlas --version
    ATLAS/1.0 — atlas-launcher 0.1.0

    $ ./rust/target/release/atlas help        # quoted from the HELP constant, args.rs:29-72
    ATLAS — fully local news intelligence. One process starts the app.

      atlas start [--root DIR] [--dry-run] [--no-install]
            Load data/config.json, check that the model server would listen on 127.0.0.1
            only, then provision if this machine has never run ATLAS: when the model file
            or bin/llama-server is missing, atlas-setup --install runs as a child once —
            a multi-gigabyte download, the only time this app touches the network — and
            writes its progress to data/setup.json for the window to draw. Then the model
            file and the window binary are checked, llama-server is started as a child on
            127.0.0.1, waited for, then ATLAS itself opens: the native window
            (rust/target/release/atlas-desktop), which reads data/atlas.db directly.
            ATLAS opens no port of its own and starts no browser. Closing the window —
            or Ctrl-C — stops the model server this command started.
            --dry-run prints that sequence and exits without starting anything.
            --no-install skips the provisioning step, for a machine whose owner places
            the model file and bin/llama-server by hand.

      atlas doctor [--root DIR]
            Print, in plain English, what this machine has and what it is missing:
            toolchain, the window binary, the model file and its size, the loopback
            invariant, a writable SQLite, the model server's port, data permissions.
            Exit 0 when ATLAS can start. No network call is made.

      atlas help            this text
      atlas --version       the version

    The help text is a little behind the code and the difference matters. It describes the
    provisioning child and the model server, never names the agent (`atlas-monitor serve`),
    and its closing sentence stops at the model server, while `shutdown.rs:51` stops
    `[Window, Agent, Model]`. Read the `--dry-run` lines below as the current account; the
    help string lives in a crate this pass does not own.

    $ cargo test --offline -p atlas-launcher --test no_listener -- --nocapture \
        the_dry_run_prints_a_plan          # 2026-11-22 13:12Z, in the test's own temp root
    ATLAS: root <tmp>/atlas-dry-run-10683-…
    ATLAS: provisioning <tmp>/rust/target/release/atlas-setup --install --root <tmp> (the model
           file (<tmp>/models/Qwen3.5-9B-…IQ4_XS.gguf) and bin/llama-server (<tmp>/bin/llama-server)
           missing: the one time this start touches the network)
    ATLAS: window <tmp>/rust/target/release/atlas-desktop (found)
    ATLAS: agent <tmp>/rust/target/release/atlas-monitor serve (found; one JSON line per action on stdout)
    ATLAS: model file <tmp>/models/Qwen3.5-9B-…IQ4_XS.gguf does not exist yet: the provisioning step above fetches it
    ATLAS: model server <tmp>/bin/llama-server does not exist yet: the provisioning step above builds it from the vendored llama.cpp tree
    ATLAS: no listener: no HTTP server, no browser, no port opened by ATLAS — the window reads data/atlas.db itself
    ATLAS: stop order: window, then agent, then model server
    Nothing was spawned, nothing was bound, no window was opened.

    (That is the real text: it comes from `plan_lines`
    (`rust/atlas-launcher/src/plan.rs`) through the test above, run in a temp root where the
    model and `llama-server` are absent, so the two "does not exist yet" lines are the honest
    ones for a fresh machine. `<tmp>` and the `…` in the model name are my shortenings of the
    test's own path and the shipped file name. `atlas start --dry-run` itself — with a real
    root — is not something this pass may execute; §5 says what that costs the document.)

The programs it starts are the window, the agent (`atlas-monitor serve`, the only one whose
dependency closure holds the fetcher: `cargo tree -p atlas-monitor --edges normal` shows
`atlas-ingest` → `atlas-net`, and no `atlas-browser` — measured 14:13Z, `grep -c atlas-browser` →
`0`), `llama-server`, and — only on a machine that is not provisioned — `atlas-setup --install`,
the fourth program and the only one that ever fetches. What is **not** in the launcher's closure
is the listener: `cargo tree -p atlas-launcher --edges normal` is five workspace crates —
`atlas-launcher`, `atlas-config`, `atlas-guard`, `atlas-platform`, `atlas-store` — so `atlas
start` cannot serve a page even if it wanted to (`docs/NO-PORTS.md` §1–§2).

No binary in the product is produced by anything except `cargo build`. There is no Makefile,
no npm script, no `setup.py` that emits a component.

**The window is one crate with two targets.** `atlas-desktop` carries `[lib] name =
"atlas_desktop"` and `[[bin]] name = "atlas-desktop"` in the same manifest: the library is the
fifteen modules the window is made of (`src/lib.rs` declares all fifteen), and the binary is one function
that opens it (`src/main.rs`, 43 lines, no `mod` declarations). That split is what let the
integration tests stop `#[path]`-including the source: they now `use atlas_desktop::…`, so they
exercise the same items the binary does instead of a private copy of them. Counted from the same
metadata as the table above — 25 members, 23 binary targets, **24 library targets**, and
`atlas-desktop` itself is one lib, one bin and 40 integration tests (re-counted 2026-11-22 13:18Z;
the crate grew four test files this afternoon):

    $ cargo metadata --offline --no-deps --format-version 1 --manifest-path rust/Cargo.toml \
        | jq -r '[.packages[].targets[] | select(.kind == ["lib"])] | length'
    24

    $ cargo metadata --offline --no-deps --format-version 1 --manifest-path rust/Cargo.toml \
        | jq -r '.packages[] | select(.name == "atlas-desktop") | .targets[] | .kind | join(",")' \
        | sort | uniq -c
           1 bin
           1 lib
          40 test

### 2.6 Nothing in `rust/` spawns an interpreter

    $ grep -rn --include="*.rs" -E '"(python3?|node|bash|sh|ruby|perl|deno|bun)"' rust/*/src
    rust/atlas-config/src/download.rs:344:            if ["python", "python2", "python3", "python.exe", "py"]
    rust/atlas-platform/src/executable.rs:249:        let spaced = path_candidates("node", Os::Windows, r"C:\Users\Ann Marie\bin", ".EXE");
    rust/atlas-platform/src/executable.rs:264:        let found = path_candidates("node", Os::Windows, r"C:\Program Files\nodejs", "EXE;.CMD");
    rust/atlas-platform/src/child.rs:168:    const PREFIXES: [&str; 6] = ["ld_", "dyld_", "malloc_", "git_", "perl5", "ruby"];
    rust/atlas-ui/src/status.rs:35:        "core" | "rust" | "python" => "core",
    rust/atlas-ui/src/status.rs:149:        Some(_) => ("python", Mark::Bad),
    rust/atlas-ui/src/status.rs:187:        "python"
    rust/atlas-ui/src/status.rs:1281:        assert_eq!(core_label(&python), "python");
    rust/atlas-ui/src/status.rs:1290:        assert_eq!(stat_key("python"), "core");
    rust/atlas-ui/src/status.rs:1502:        assert_eq!(core_state(&missing), ("python", Mark::Bad));
    rust/atlas-ui/src/app.rs:444:        assert!(!SERVER_DOWN.to_lowercase().contains("python"), "{SERVER_DOWN}");

Read the hits one by one, because none of them starts a process:

- `atlas-config/src/download.rs:344` — the **detector**, not a spawn: this is the
  `python -m playwright` rule of the download audit, and naming an interpreter is how the audit
  *recognises* a forbidden download.
- `atlas-platform/src/executable.rs:249,264` — a unit test of the Windows `PATH`
  candidate builder, which happens to use `node` as its sample file name.
- `atlas-platform/src/child.rs:168` — the list of environment-variable *prefixes* a child never
  inherits (`perl5`, `ruby` among them). Names, dropped, never run.
- `atlas-ui/src/status.rs` and `atlas-ui/src/app.rs` — a display mapping (`"rust" | "python" =>
  "core"`), its tests, and an assertion that a status sentence contains no `python`.

`atlas-graph` no longer appears at all: its Python row-parity tests were removed with `server/`.

And the full list of what `rust/` spawns. The audit's own needles and the test files are filtered
out, and the output is a count per file rather than a line number, because this tree moves under a
document that pins lines (measured 2026-11-22 14:26Z):

    $ grep -rn --include="*.rs" "Command::new" rust/*/src \
        | grep -v -e tests.rs -e db_tests.rs -e atlas-config/src/download.rs -e atlas-config/src/audit.rs \
        | cut -d: -f1 | sort | uniq -c | sort -rn
       4 rust/atlas-launcher/src/child.rs        # the shared child builder, and three test fixtures
       3 rust/atlas-launcher/src/supervise.rs    # test fixtures: /bin/sleep, /bin/kill
       2 rust/atlas-search/src/rules.rs          # doc comments: why it uses the builder above
       2 rust/atlas-launcher/src/window.rs       # the window, and two test fixtures
       2 rust/atlas-launcher/src/agent.rs        # atlas-monitor, and two test fixtures
       1 rust/atlas-platform/src/child.rs        # the same builder, one layer down
       1 rust/atlas-launcher/src/lock.rs         # test fixture: mkfifo
       1 rust/atlas-launcher/src/llama.rs        # llama-server
       1 rust/atlas-e2e/src/ui/browser.rs        # the test browser
       1 rust/atlas-e2e/src/sockets.rs           # lsof
       1 rust/atlas-e2e/src/server.rs            # the app under test
       1 rust/atlas-e2e/src/repo.rs              # cargo
       1 rust/atlas-bench/src/startup.rs         # the harness's app child

Read down the list: `atlas-bench`'s app child, `atlas-e2e`'s test browser / app under test / `lsof`
/ `cargo`, `atlas-launcher`'s `llama-server` (`llama.rs`), the agent (`agent.rs`) and the window
(`window.rs`), the shared child builder in two places (`atlas-launcher/src/child.rs`,
`atlas-platform/src/child.rs`), test fixtures in `supervise.rs`/`lock.rs`/`child.rs`/`window.rs`,
and two doc comments in `atlas-search` that name `Command::new` only to explain why it uses the
shared builder instead. `opener.rs` is gone from `atlas-launcher` altogether, and the window
copies a source address to the clipboard instead of handing it to the OS (`src/link.rs`).

So every spawn in shipped code is a Rust binary from this workspace (`bin/llama-server` being the
one upstream exception) or a test tool (`lsof`, `cargo`); `atlas-setup` and `atlas-monitor` are the
workspace's own. No `python`, `node`, `bash -c` or equivalent is built into shipped code — and
since the rule engines answer in-process, the *app* spawns no engine either:
`atlas-ingest/src/rules/spawn.rs` still holds the stdin/stdout transport, but `rules::SPAWNED` is
empty, so nothing in the shipped wiring reaches it. (`atlas-search`'s own `rules::Engines` does
spawn `atlas-text` and `atlas-dates`, and no crate
in the workspace links `atlas-search`, so that path is not part of the product — see
`docs/ARCHITECTURE.md` §4.)

### 2.7 The install tool: Rust, install-side, not the app

A fresh machine has no `bin/llama-server` and no model file, and the app may not fetch them.
The answer is a fourth kind of binary in this workspace: `rust/atlas-setup`, one crate, three
subcommands (`check`, `model`, `llama`, plus `all` which runs the two and then checks), ~1.9 MB
release, written in Rust like everything else. The wasm subcommand it used to have went with the
frontend.

    $ cargo run --offline --locked -p atlas-setup -- check
    ATLAS setup - <repo>

      window               ok   /…/rust/target/release/atlas-desktop (6.2 MB)
      archive              ok   /…/data/atlas.db (7.7 MB)
      map outlines         ok   /…/public/vendor/ne_50m_countries.geojson (3.1 MB)
      config               ok   data/config.json: the model server is on port 8081
      model file           ok   models/Qwen3.5-9B-…gguf (6.5 GB, the size this copy of ATLAS expects)
      llama.cpp binary     ok   /…/bin/llama-server (33 kB)
      port                 ok   no port is opened by the window; chat dials port 8081 on this machine
                                only, and only if you start the model server. Nothing listens while
                                the weights are downloaded either: that is one outbound connection a
                                person starts
      start command        ok   /…/run.sh — it opens the window

    ready: the window can run on this machine.

(The path shortenings are this document's: the run prints absolute paths, whose prefix is the
repository root named in §4; the `port` row is wrapped here exactly as the run wraps it. The eight
rows are the eight the window needs — this check answered `toolchain` and `native binaries` in an
earlier shape and no longer does, because a check that answers "can the window run here?" does not
need the compiler. `atlas-setup` has no `ui` subcommand any more — the wasm bundle it built was
deleted with the frontend, and `check.rs::the_report_has_no_wasm_rows` fails if a wasm row
returns. Measured 2026-11-22 14:24Z.)

Three properties keep it from being the loophole in §1–§4:

- **No crate depends on it.** `atlas-guard/tests/audit_manifest.rs::the_install_tool_is_not_linked_into_the_app`
  reads every `Cargo.toml` in the workspace and fails if one names `atlas-setup`. The app
  therefore cannot reach the fetch code even by accident.
- **It never runs unless a person runs it.** `atlas start`'s child list is
  `atlas_web::native::REQUIRED` and `atlas-setup` is asserted absent from it; the launchers
  only *point at* it (`./setup.sh --provision`), and without `--yes` it prints the URL or the
  cmake argv and exits 3.
- **The download audit says so out loud.** Its one exemption is `AUDIT_INSTALL_TOOL`,
  a written constant that covers six files and is published as `exempt_install_tool` in the
  verdict (§4); `atlas-config`'s `install_tool_exemption` test proves the exemption covers that
  directory and no other, with the same `curl -O` line in `rust/atlas-setup/` (exempt) and
  `rust/atlas-web/` (a finding).

Why it exists at all rather than a paragraph of instructions: instructions are the step that
is skipped. `docs/OPERATIONS.md` §4 is the operator's view.

---
## 3. Every non-Rust file that ships

The window changed what "ships" means: `atlas-desktop` is a Rust binary that needs **none** of
the browser rows below. They are still on disk, they still build, and they are kept as history;
the one row the window does read at run time is the GeoJSON outlines, for the map.

| What ships | What it is | Produced by | Why it cannot be Rust |
| --- | --- | --- | --- |
| `bin/llama-server` | the model server, a child process on loopback | `atlas-setup llama --yes`, which runs `cmake --build` on `tools/llama.cpp` (C++) | it **is** upstream C++; ATLAS supervises it, does not implement it |
| `bin/*.dylib` (25 files) | ggml/llama/mtmd shared libraries | same cmake build | llama-server links them at run time |
| `models/*.gguf` (2 files) | quantised model weights | trained upstream; the pinned one is fetched once by `atlas-setup model --yes` (install time, digest-verified), never by the app | weights are data, not code |
| `public/vendor/ne_50m_countries.geojson` (+ `.geo.json` twin, `countries.geo.json`) | Natural Earth country outlines | vendored data | geographic coordinates are data — **this is the one row the window reads**, to draw the map |
| `public/index.html` | the one-page shell, no logic | hand-written | **retired history**: the window replaced it |
| `public/style.css`, `public/css/{core,map,graph,chat}.css` | design tokens and layout | hand-written | **retired history**: CSS is the browser's styling language and the window has none |
| `public/ui/index.html` | copy of the shell | `rust/build-ui.sh`, step 5 | **retired history**, as above |
| `public/ui/atlas_ui_bg.wasm` (1.44 MB) | the old frontend | `cargo build --target wasm32-unknown-unknown -p atlas-ui` | **retired history**; it *is* Rust, compiled |
| `public/ui/atlas_ui.js` (49 KB) | wasm-bindgen glue: instantiate, marshal, exports | `wasm-bindgen 0.2.130` from the build | **retired history**; **generated**, never hand-edited |
| `public/ui/atlas_ui.d.ts`, `atlas_ui_bg.wasm.d.ts` | type declarations for that glue | wasm-bindgen | **retired history**; generated TypeScript syntax, no runtime |
| `public/ui/loader.js` (3 lines, 70 bytes) | module loader | `rust/build-ui.sh` | **retired history**; the only hand-written JS in the tree |
| `public/ui/tiles/` (2 473 tiles + `index.json`; the `N 2.png` duplicates are gone and a test keeps them gone, §5) | greyscale map tile pyramid | `cargo run -p atlas-ui --features tilegen --bin atlas-ui-tiles` | **retired history**: the window draws vector coastlines instead |
| `run.sh`, `setup.sh`, `run.ps1`, `setup.ps1` | launchers/build wrappers | hand-written | must work before Rust is built; they `exec` the binary |
| `rust/build.sh`, `rust/build-ui.sh` | build drivers | hand-written | they call `cargo`; not run at run time |
| `docs/*.md` | documentation | hand-written | prose |
| `data/atlas.db`, `data/*.json`, `data/*.log` | runtime state | the app, at run time | it is the data the app produces |

Everything in that table is either (a) upstream code compiled to a binary ATLAS starts,
(b) generated output of the Rust build, (c) non-executable data, or (d) a retired page the
window made unnecessary. No row is code that the window runs.

---

## 4. The one paragraph a reviewer needs

The guarantee holds only while four things stay true. **First, no generated artifact is
committed by hand:** `public/ui/`, while it is kept as history, must always be the output of
`rust/build-ui.sh`, because a hand-edited `atlas_ui.js` would be JavaScript nobody generated and
nobody reviews — and nothing new may be added to it, since the window replaced it. **Second,
no script may do app work:** the moment a `.sh` parses a feed, walks the database or decides
a route instead of `exec`ing the binary, the app is no longer Rust. **Third, nothing may
shell out to an interpreter:** a single `Command::new("python3")` in shipped code turns the
product into a Rust shim around Python. **Fourth, the window stays a window:** it may not gain a
listener, a spawned process, or a second address it can reach — that is what
`rust/atlas-desktop/tests/no_spawn_no_socket.rs` asserts on the source, with one reviewed
exception for the loopback chat client and one for its worker thread. The checks that catch each:
`atlas-config`'s download audit scans the tree for fetch primitives —

    $ ./rust/target/release/atlas-config audit        # 2026-11-22T11:15Z, this checkout
    {"exempt_install_tool":6,"finding_count":0,"findings":[],"ok":true,
     "root":"<repo>",
     "rules":["pip-install","git-clone","urlretrieve","urlopen-download","wget","curl-download",
              "powershell-download","url-fetch","host-fetch","browser-download","tunnel","program-name"],
     "scanned":459,"skipped":155,"unread":[],"vacuous":false}

    (the `read` key is elided here: it lists the first `AUDIT_LIST_MAX` = 40 files the walk
    actually read, `rust/atlas-config/src/audit.rs:79`)

`exempt_install_tool: 6` is the install tool: six code files under `rust/atlas-setup/`, counted
instead of silently passed, because that crate is the one thing in this tree that fetches — a
model file, on a subcommand a person types, with `--yes` (§5.1 of the threat model). The count
fell from seven to six when `rust/atlas-setup/src/ui.rs` was deleted with the wasm bundle it
built. `atlas-guard`'s `the_install_tool_is_not_linked_into_the_app` proves no crate depends on
it, so the app cannot reach that code.

— and the end-to-end gate `cargo run -p atlas-e2e -- run` fails its `security.boot-audit`
step unless `/api/status` reports `security.http == "rust"`, `security.boot.checked == true`,
`security.boot.ok == true` and `security.downloads == true`; its success line is
`security.http=rust, boot ok at <time> (surface + downloads)`. (That gate starts the app on a
loopback port, so it was **not** re-run while writing this document; the assertions above are
read from `rust/atlas-e2e/src/security.rs`.) §2.6 is the by-hand grep for the third failure, and
it currently finds no interpreter spawn — the hits it prints are the audit's own detector, two
Windows `PATH` test file names, a list of environment prefixes a child never inherits, and the
UI's label mapping.

---

## 5. Where this repository is not yet 100% Rust

Stated here rather than buried, because a proof that hides a red line is not a proof.

**The window still reads one file out of the retired frontend's directory.** The map's
coastlines come from `public/vendor/ne_50m_countries.geojson` (`rust/atlas-desktop/src/assets.rs:34`,
trying three names in one directory), and `public/` is the directory the pivot is retiring. The
data is not code and the window reads it directly from disk, so nothing about the guarantee turns
on it — but a directory that is "history" is the wrong home for a file the product opens on every
Overview. Moving the outlines to a path of their own is a change in crates I do not own; until
then this is the one place the window and the retired frontend touch. `atlas-setup check` reads the same names from the same
directory (the archive at `rust/atlas-setup/src/check.rs:135`, the outlines at `:156-175`), so the
move is two crates and one path, not one line.

**The graph view's red is green, and it is worth keeping the shape of it.** At 13:16Z
`tests/graph_bounds.rs::five_hundred_events_never_lose_the_picture` read:

    frame 1273 of the storm left 27.4% of the picture on the plate, against a 30% floor:
    at zoom 3.96 and pan (-1193, -572) on a 320x240 canvas the drawing is off screen,
    and no gesture in the view brings it back

That was a real bound being missed by a canvas whose owner was rewriting it at the time; at
15:03Z the same target reads `test result: ok. 5 passed; 0 failed`. It is kept
here because a reader comparing this file with a fresh `cargo test` should be able to see that the
red existed and where it went, not only that today is green.

**One scan was briefly red on a spelling collision, and is green again.**
`atlas-desktop/tests/no_spawn_no_socket.rs::the_window_starts_no_process` matches the needle
`Command::`, and `egui::ViewportCommand::Close` on `src/app.rs:987` contains it; a viewport
command is not a child process. For about twenty minutes that file read `19 passed, 1 failed`
(13:08Z), the window having started nothing throughout. Its owner narrowed the scan and the same
command now reads (2026-11-22 13:11Z):

    $ cargo test --offline -p atlas-desktop --test no_spawn_no_socket
    test result: ok. 20 passed; 0 failed; 0 ignored

That is the shape of a scan that keeps a guarantee: the failing run was true about the code and
wrong about the word, and the fix was in the scan, not in the window.

**The two audits that were red this afternoon are green, and how they got there matters.** They
were measured red between 14:11Z and 14:19Z and green at 15:00Z:

    $ cargo test --offline --locked -p atlas-guard
    test result: ok. 138 passed; 0 failed   src/lib.rs
    test result: ok.  13 passed; 0 failed   tests/audit_binds.rs
    test result: ok.  16 passed; 0 failed   tests/audit_downloads.rs
    test result: ok.   6 passed; 0 failed   tests/audit_freshness.rs
    test result: ok.   9 passed; 0 failed   tests/audit_gate.rs
    test result: ok.  12 passed; 0 failed   tests/audit_logs.rs
    test result: ok.  18 passed; 0 failed   tests/audit_manifest.rs
    test result: ok.   8 passed; 0 failed   tests/audit_open_url.rs
    test result: ok.   7 passed; 0 failed   tests/cleantext_collapse.rs
    test result: ok.  12 passed; 0 failed   tests/fuzz_url.rs
    test result: ok.  13 passed; 0 failed   tests/reachability.rs
    test result: ok.   0 passed; 0 failed; 1 ignored                       (15:01Z)

The first red was `atlas-setup/src/disk.rs:124` handing `powershell` to `child::command` to ask
the drive for its free space — a true finding about a real line, and it runs only behind a
provisioning run. It was answered by **deleting the file**, not by an entry on an allowlist:
`atlas-setup/src` is `check import install lib license llama main model progress sha`, and
`INSTALLERS` (`atlas-guard/tests/audit_downloads.rs:261`) still names `"powershell"` and still
fails any spawn site that reaches for it. The second was four build scripts arriving with the
window's GPU stack (`glutin_egl_sys`, `glutin_wgl_sys`, `khronos_api`, `windows_x86_64_msvc`),
which run at **build** time, not at run time; they are recorded with their sentences now
(`atlas-guard/tests/audit_manifest.rs:751-797`). Neither weakens the claim this document makes
about run time, and both files now pass whole.

The crate's own test module, for scale: `cargo test --offline -p atlas-guard --lib` → `138
passed; 0 failed` (15:01Z), and `atlas-guard` carries ten integration targets (`audit_binds`,
`audit_downloads`, `audit_freshness`, `audit_gate`, `audit_logs`, `audit_manifest`,
`audit_open_url`, `cleantext_collapse`, `fuzz_url`, `reachability`).

What is red at 15:01Z is the window's own suite: `cargo test --offline -p atlas-desktop --lib` →
`288 passed; 1 failed`, the failure being
`app::tests::the_rail_lists_every_page_once_and_chat_is_one_of_them` (`src/app.rs:1459`, `left: 7
/ right: 6`) — a rail that gained a page while its test still expected six. That belongs to
`atlas-desktop`'s owner and is listed here so a reader comparing this file with a fresh run is
not surprised. `atlas-ingest/tests/search_privacy.rs` read `5 passed; 1 failed` (14:14Z) because
its fixture linked to `publisher.example` and `.example` is a reserved TLD the gate refuses; the
fixture now uses an ordinary host (`tests/search_privacy.rs:47-56`) and the target reads `6
passed; 0 failed` (15:00Z).

**The chat page's refusal points at a script that opens the window, not the model.** When the
model server is not answering, `rust/atlas-desktop/src/http.rs:1139` tells the reader to start it
"with `{START_COMMAND}`, which launches `llama-server` as a child of `atlas start`". The constant
is right for the platform (`http.rs:66`: `./run.sh` on macOS and Linux, `run.ps1` on Windows), but
the claim in the sentence is not: that script builds and opens **the window** and starts nothing,
so a reader who follows it gets a second window and still no chat. What starts `llama-server` is
`atlas start` or the server's own argv. The sentence is pinned by
`http.rs::the_refusal_names_the_address_and_the_command_that_starts_the_server` (`:1633`), so the
fix is one string in a crate I do not own; the window's behaviour is unaffected, which is why this
is a gap in wording and not in the guarantee.

**The Python spawn in `rust/` is gone.** This section used to report one: `atlas-graph`'s
`db_tests.rs` ran `python3` against the deleted `server/graph.py` and failed on a machine that had
an interpreter. Both the test and the helper are removed; §2.6's interpreter grep returns one
detector, two test file names, a list of environment prefixes and a UI label, and no spawn. §2.6's
spawn list has no interpreter in it at all.

**The crate cache is machine-local; there is no vendored tree.** This is the one honest gap left
in "nothing is downloaded", and it is a *build-time* gap, not a run-time one:

- Every dependency is pinned exactly (`=x.y.z`) in `rust/Cargo.toml` and every cargo invocation in
  the scripts passes `--locked`, so a build either uses the `Cargo.lock` bytes or refuses.
  `atlas-guard/tests/audit_manifest.rs` proves both rules
  (`every_dependency_is_pinned_to_an_exact_version`, `every_cargo_invocation_is_locked`), along
  with "no `git`, no second registry, no `[patch]`", "a checksum for every package the lock names",
  and "no decompressor or second charset decoder in the resolved graph".
- What that does **not** give you is a tree you can build with no network on a machine that has
  never built Rust: there is no `rust/vendor/` directory and no `rust/.cargo/config.toml` in this
  checkout, so `cargo build --offline` works from whatever `~/.cargo` already holds. A fresh clone
  fetches the lock's packages once — `run.sh`, `setup.sh` and `rust/build.sh` retry with the
  network for exactly that case, and say so.
- `rust/build-ui.sh` is the other build-time fetch: the `wasm32-unknown-unknown` target and
  `wasm-bindgen-cli` (version-pinned, `--locked`), installed once. Nothing else in this tree
  installs anything.

`cargo vendor` is the change that would close it (one directory, `--offline` honoured everywhere),
and it is not done here.

**The space-named copies are gone, and a test keeps them gone.** This tree carried
sync-conflict copies — whole files beside their originals, a few bytes apart (`shell 2.rs`,
`status 2.rs`, `views/render 3.rs`, `public/style 2.css`) — plus thousands of duplicated map
tiles. They are made by whatever writes a file while another writer holds it, so a count is
only true for the minute it is taken; the answer is not a cleanup but a rule. Measured now:

    $ find . -path ./tools -prune -o -path ./models -prune -o -path ./rust/target -prune \
        -o -path ./data -prune -o -name "* *" -print | wc -l
           0
    $ find public/ui/tiles -name "*.png" | wc -l      # 2 473, none with a space
        2473
    $ cargo test --offline -p atlas-guard --test audit_manifest
        no_shipped_file_is_named_with_a_space ... ok

The rule (`atlas-guard/tests/audit_manifest.rs`) walks the tree, skips only `target`, `data`,
`.git` and `node_modules`, asserts it read more than 500 entries so a wrong root cannot pass
as a clean tree, and fails with every offending path. It was added after the cleanup for a
reason: the editing tool that splits a file while another write is in flight recreates these
copies, so the cleanup alone would have to be done again. The tiles were the same defect at
scale — 2 401 byte-identical `N 2.png` files beside the real pyramid, verified with `cmp`
before they were deleted, and they are generated output now (2 473 tiles, `index.json`).

`data/` still holds space-named entries of its own (`atlas 2.db-shm`, a Chrome profile's
`Safe Browsing`). Those are SQLite's and Chrome's naming inside the live data directory, not
shipped files, and `atlas-guard`'s rule does not walk `data/` for exactly that reason.

**The gate was run, not just read.** Everything above that names `atlas-e2e` is claimed from
its source *and* from a run — the harness binds one ephemeral loopback port, which is a test,
and it was started from this checkout at 2026-11-21T17:36Z:

    $ cargo run --release --offline --locked -p atlas-e2e -- run
    PASS  31 steps (31 ok, 0 failed, 0 skipped), 49 of 49 routes, 3530 ms

Those 31 steps include the two that make this document's claim about the running app true:
`security.boot-audit` (the app's own no-download audit, its `security.http == "rust"` and its
`security.downloads`) and `security.hermetic-root`, plus `ui.map` and `ui.graph`, which load the
wasm bundle in the machine's own headless Chrome and drive the two views.

**The suite, measured.** The last full workspace run recorded here is
`cargo test --offline --locked --workspace` at 17:52 UTC on 2026-11-21: 108 test targets (84 rustc
harnesses plus 24 doc-test targets), **2 530 passed, 0 failed, 13 ignored**. The 13 are deliberate:
a live read of a public page, a run that needs a real model, and the measurements the optimised
profile would distort; each carries the sentence saying what it would prove.

Today's run of the same command did **not** produce a total either, and now for a sharper reason:
it stopped while *compiling* a test target, before running anything.

    $ cargo test --offline --locked --workspace --exclude atlas-e2e --exclude atlas-bench   # 15:02Z
    error[E0004]: non-exhaustive patterns: `atlas_desktop::app::Page::Settings` not covered
       --> atlas-desktop/tests/ui_layout.rs:101:11
    error[E0004]: non-exhaustive patterns: `atlas_desktop::app::Page::Settings` not covered
       --> atlas-desktop/tests/ui_layout.rs:114:11
    error[E0004]: non-exhaustive patterns: `atlas_desktop::app::Page::Settings` not covered
       --> atlas-desktop/tests/ui_layout.rs:146:38
    error: could not compile `atlas-desktop` (test "ui_layout") due to 3 previous errors

`tests/smoke.rs` fails the same way. A page was added to `Page` (`atlas-desktop/src/app.rs:48`) and
two of that crate's own test targets were not taught about it; the crate is being edited right now
and its owner is the one to answer. `--exclude atlas-e2e --exclude atlas-bench` is this document's
own rule: the first opens a loopback socket, the second is a timing harness whose debug build
distorts its own numbers, and neither is run while a document is being written. The earlier stop,
recorded before this one, was `atlas-bench`'s debug-profile budget test (five paths over their
release budgets at ×1.20 in a debug build, `atlas-bench/tests/budgets.rs:113`).

What the tree does say, measured from `cargo metadata` at 15:06Z rather than by eye: 25 packages,
24 library targets, 23 binary targets, **109 integration test targets** and 25 doc-test targets —
181 harnesses, against the 108 that the dated run above counted. Seven of the 181 belong to the
two excluded packages: `atlas-bench` has a library, a binary, one integration target and a
doc-test target, and `atlas-e2e` the first three of those without an integration target.

That dated number is old on purpose: it was not re-taken while the pivot was written, because the
window's crate was being edited by another agent the whole time and a workspace count would have
described a tree mid-edit. What this pass did measure, in order, on 2026-11-22 between 11:05Z and
13:18Z:

    $ cargo build --offline --locked -p atlas-desktop
        Finished `dev` profile [unoptimized + debuginfo] target(s) in 37.60s   (cold, 11:05Z)
        Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.20s    (after a source edit)
        Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.07s    (nothing changed)

    $ cargo test --offline -p atlas-desktop --lib
        test result: FAILED. 150 passed; 8 failed; 0 ignored   (11:17Z)
        test result: FAILED. 152 passed; 8 failed; 0 ignored   (11:22Z, same command)
        test result: FAILED. 167 passed; 2 failed; 0 ignored   (11:25Z)
        test result: FAILED. 168 passed; 1 failed; 0 ignored   (11:26Z)
        test result: ok.     175 passed; 0 failed; 0 ignored   (11:35Z)
        test result: ok.     255 passed; 0 failed; 0 ignored   (13:08Z)
        test result: ok.     256 passed; 0 failed; 0 ignored   (13:11Z)
        test result: FAILED. 269 passed; 2 failed; 0 ignored   (14:08Z)
        test result: ok.     276 passed; 0 failed; 0 ignored   (14:16Z)
        test result: ok.     282 passed; 0 failed; 0 ignored   (14:41Z)
        test result: FAILED. 288 passed; 1 failed; 0 ignored   (14:59Z)
        test result: ok.      20 passed; 0 failed; 0 ignored   (--test no_spawn_no_socket, 15:04Z)
        test result: FAILED.  6 passed; 4 failed; 0 ignored   (--test disclosure, 15:05Z)

The 14:59Z failure is `app::tests::the_rail_lists_every_page_once_and_chat_is_one_of_them`
(`src/app.rs:1459`, `left: 7 / right: 6`): the rail gained a page — the settings page whose
`Page::Settings` also breaks `ui_layout` above — and the test still expects six. The four
`disclosure` failures are the same page against three rules it has not been taught
(`settings.rs:817` prints a home directory, `settings.rs:575`'s `"{:.1} GB"` trips the shout scan,
`requests.rs:281` names the archive in a comment the scan reads as a write) plus one scan whose own
needle went stale (`disclosure.rs:940`: "the hue scan found no colour call — the needle is wrong").
All five are `atlas-desktop`'s owner's, and none of them is in a file this pass touched.

The failing names before that are in views another agent owns and they change between runs — four in
`views::map` at 11:17Z (`a_lake_inside_a_country_is_a_hole`,
`only_the_rings_on_the_plate_are_filled_and_drawn`,
`a_zoom_sweep_never_loses_a_coast_and_never_leaves_the_plate`,
`the_parse_time_and_the_frame_line_count_are_measured`), two in `views::monitors` and two in
`views::topics`, and only the last of the four `views::map` names at 11:26Z. Twice in between the
crate did not compile at all (`src/views/map.rs:1526`, `missing field article in initializer of
data::Pin`; then `src/views/graph.rs:338`, `cannot find function pair_of`) — that is what a file
being written under a running test run looks like. `--test disclosure` said `10 passed; 0 failed;
0 ignored` at 11:22Z and `9 passed; 1 failed` at 11:26Z; `--test no_spawn_no_socket` said `14
passed; 0 failed` at 11:22Z and `15 passed; 0 failed` at 11:35Z, and by 11:35Z the lib target was
green (`175 passed`). None of this is a claim that the window's suite is green; it is a
statement of what came back, when. §4's audit and §5's greps are commands over files, so they are
unaffected by the churn.

An earlier run the same afternoon was red in four places — `atlas-guard`'s spawn-site audit (a
spawn site in a file that was being split out of `llama.rs` at that moment), one `atlas-ingest`
handler test, and two `atlas-migrate` tests whose expectation belonged to the old child-process
engines (they asserted that a missing engine binary makes the migration skip the place check; the
place rule is answered in-process now, so the check runs). The four were fixed rather than
rounded away, which is why this document states one number and not a range.

The next run, later that evening, was red in exactly one place and it is worth recording: the
new `atlas-setup` crate called `atlas_guard::urlsafe::check` — the raw gate — from its download
loop, and `atlas-guard/tests/audit_gate.rs::the_raw_gate_has_one_caller` failed with
"atlas-setup/src/model.rs:223 calls the raw gate outside atlas-guard/atlas-net; use
atlas_guard::Gated::verify (a public URL) or atlas_net::gate (the request line)". The fix was the
one the audit named: `Gated::verify` for the public URL and `Gated::loopback` for the loopback
test server, so the install tool holds a value only the gate can mint, like every other caller.
