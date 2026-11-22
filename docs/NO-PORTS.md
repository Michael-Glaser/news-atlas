# No ports, four programs, no downloads at run time

Run on this checkout, 2026-11-22, from `rust/`, with `export PATH="$HOME/.cargo/bin:$PATH"`;
where a comment in the tree contradicts a measurement, the command wins (§5).

## 1. The closure of each shipped binary

`atlas start` is **atlas-launcher**, the window is **atlas-desktop**. `--edges normal` drops
dev- and build-dependencies, so what remains is what each binary carries.

```text
$ cargo tree --offline -p atlas-launcher --edges normal | grep -E "atlas-|tokio v"
atlas-launcher v0.1.0 (…/atlas-launcher)
├── atlas-config v0.1.0 (…/atlas-config)      ├── atlas-guard v0.1.0
├── atlas-platform v0.1.0 (…/atlas-platform)  ├── atlas-store v0.1.0
└── tokio v1.54.1

$ cargo tree --offline -p atlas-desktop --edges normal | grep -E "atlas-|^[├└]"
atlas-desktop v0.1.0 (…/atlas-desktop)
├── eframe v0.37.0   ├── egui v0.37.0 (*)   ├── rusqlite v0.43.0   └── serde_json v1.0.154
```

(`…` stands for `<repo>/rust` in every
transcript here; the trees are otherwise verbatim, four siblings per line to save height.)

Launcher set, **five crates**: `atlas-launcher`, `atlas-config`, `atlas-guard`,
`atlas-platform`, `atlas-store`; its only network-capable external is `tokio`
(`rt-multi-thread, macros, signal, net, time`, which brings `mio` + `socket2`). Window set,
**one crate**: `atlas-desktop`, no other ATLAS crate and no async runtime (`smol_str` in the
tree is a string type, not an executor). Neither tree holds
an HTTP client or TLS: `cargo tree -p <crate> --edges normal | grep -icE
"hyper|axum|ureq|rustls"` → `0`, `0`.

`atlas start` spawns up to four programs; which arc carries network code:

| child | program | spawn site | network code in its closure |
| --- | --- | --- | --- |
| provisioning | `atlas-setup --install` | `setup.rs:368` — **only when provisioning is due *and* the licence is recorded** (§4, point 2), once | `ureq` + `rustls`, the one thing that fetches; **dials only**, behind the licence question (§4) |
| model server | `bin/llama-server` | `llama.rs:317` | **the only listener**: vendored llama.cpp, bound to `127.0.0.1:<llama_port>` — `llama.rs:197-198` writes `--host`, `HOST = "127.0.0.1"` (`llama.rs:82`) |
| window | `atlas-desktop` | `window.rs:105` (`target/release`, then `PATH`) | one outbound dial of its own, hand-written on `TcpStream` (§3), and `wry` — the system web engine for the reader's page view (`docs/THREAT-MODEL.md` §4.19). **The engine opens no listener**: `src/webview.rs` adds a child view to this process, not a server, and the connections the engine makes are the page's, not the app's |
| agent | `atlas-monitor serve` | `agent.rs:95` (`BINARY_NAME`, then `SERVE`) | `ureq` + `rustls` — the product's only HTTP client |

The agent is a **separate program**, never linked: the launcher's closure holds no
`atlas-monitor`/`atlas-ingest`/`atlas-net` (`cargo tree -p atlas-launcher --edges normal |
grep -cE "monitor|ingest|net"` → `0`). Its network code is in its own tree:

```
$ cargo tree --offline -p atlas-monitor --edges normal | grep -E "ureq v|rustls v|atlas-net v"
│   │   ├── atlas-net v0.1.0 (…/atlas-net)   ├── ureq v3.5.2   ├── rustls v0.23.46
```

There is no browser crate to look for: `atlas-browser` was deleted from the workspace, so
`rust/atlas-ingest/Cargo.toml` names no such edge and `cargo tree -p atlas-monitor | grep -c
atlas-browser` is `0` because the name does not exist anywhere in the tree. The walk in
`atlas-guard/tests/reachability.rs` still fails if an edge like that ever comes back.

So the **model-server arc listens** (foreign binary, loopback), the **agent arc dials** (HTTPS,
the product's research), the **provisioning arc dials once** (the model download), and **neither
shipped binary listens** — launcher and window are listener-free. Nor does either fetch software
at run time: the window's own `connect` sites are loopback (§3), and the one thing in the window that
reaches public hosts is the *page* the reader opened, fetched by the system engine (below, and
`docs/THREAT-MODEL.md` §4.19). No shipped binary starts a browser as a child: the engine is a view
inside the window, and the agent's browser seam is `NullBrowser` (§2.1).

The **agent opens no listener at all**. It used to: `Pipeline::native` wired the browser seam to
a local Chrome, which is only drivable through `--remote-debugging-port=0` on an ephemeral
loopback port — measured then, a research pass left
`Google 50238 … TCP 127.0.0.1:59763 (LISTEN)` beside `llama-server` on `127.0.0.1:8081`. The
seam is `NullBrowser` now (`atlas-ingest/src/lib.rs:369`), the browser crate that type came from
is deleted, and `rust/atlas-ingest/Cargo.toml` names no browser edge. A page that needs
JavaScript is a sentence the model can act on, not a browser this product starts.

## 2. Listeners

```bash
grep -rn "TcpListener\|UdpSocket\|::bind(\|axum::serve\|hyper::" \
  atlas-launcher/src atlas-desktop/src atlas-platform/src atlas-config/src
```

**19** lines, re-measured 2026-11-22 18:36Z after the reader's page view landed in the manifest, and
**exactly one of them is shipped code**: `atlas-config/src/ports.rs:64` — `port_free`, which binds
`("127.0.0.1", port)`, asks the kernel "is this free?" and drops the listener in the same
expression. It never calls `accept`; `atlas start` reaches it via `load_managed_at`
(`start.rs:123`), once per port setting with no stored value yet. The other eighteen are tests and
one doc comment: `atlas-launcher/src/tests.rs:1015`, `:1102`, `:1109`,
`doctor/tests.rs:194`, `:195`, `:207`, `:257`, `start.rs:711`, `health.rs:159`,
`ports.rs:112`, `:113`, `:135`, `:141` (a test may hold a loopback socket to stand in for a model
server), `atlas-config/src/ports.rs:10` (a doc comment naming `TcpListener`) and `:172`, `:181`,
`atlas-config/src/tests.rs:393`, `:395`.

**`atlas-desktop/src` contributes no line**, and that now includes the engine: `src/webview.rs`
adds a **child view** to the window's own layout — WKWebView on macOS, WebView2 on Windows, `wry`
compiled only for those two targets — and a child view is not a server. It opens no listening
socket on either platform, so the row for the window below the table is still "no listener", and
the scan above is how that is checked rather than believed. What the engine *does* open is
outbound connections for the page the reader asked for; they are the page's, not the app's, and
`docs/THREAT-MODEL.md` §4.19 is where that trade is written down.

`atlas-launcher`'s own probe does **not** bind: `ports.rs:61` connects and a refusal is the
answer "free" (`ports.rs:6-10`); `atlas doctor` uses the same connect. So the rule "the
launcher's probe connects, it does not bind" holds for `atlas-launcher/src/ports.rs` and does
**not** hold for `atlas-config`'s chooser above — the boot's own line, `settings from … :
model port … (ATLAS opens no port of its own)` (`start.rs:139-143`), is about keeping none
open, which is what the product does.

**The only listening socket in the product** is inside `llama-server`, started by `atlas
start`, bound by llama.cpp to `127.0.0.1` because ATLAS writes `--host 127.0.0.1` into its
argv. ATLAS keeps no listening socket anywhere: not the launcher, not the window, not the
agent.

### 2.1 The agent's own browser is gone, and why that is a port decision

Until 2026-11-22 the agent's pipeline read JavaScript-only pages in a local Chrome it started
itself, over the DevTools protocol. That needed a socket: Chrome can only be driven through a
port it listens on (`--remote-debugging-port=0`, kernel-chosen, loopback), and the agent would
have been the process holding it. So the seam is now `NullBrowser`, and the sentence a reader
gets instead of a rendered page is `read`'s JS-only refusal
(`atlas-ingest/src/read.rs:138`) — "the page needs JavaScript and this build reads over plain
HTTP only - no browser runs here, so the text cannot be rendered. Try another source." It names
no `web_open` on purpose: the comment above it (`read.rs:133-135`) says that would send the model
to the same HTTP reader and fail twice.

That refusal is the **agent's** text reader. It does not describe the window: since the page view
landed (`rust/atlas-desktop/src/webview.rs`) the window can show a page in the system's own engine
when the reader clicks a story's address — and that engine opens no listener either, which is the
row §1 records. The two are separate actors and neither of them is a Chrome started by ATLAS.

Three measurements, in the order a reader checks them:

```text
$ ls rust | grep -c browser                     # the crate itself
0
$ grep -rn atlas-browser rust/*/Cargo.toml      # one comment, in atlas-launcher: "not here"
$ cargo test --offline --locked -p atlas-guard --test reachability
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.79s
```

The wiring is `Pipeline::native` (`rust/atlas-ingest/src/lib.rs:368-410`): "**The browser seam
is `NullBrowser` here, and that is the decision, not a placeholder.** … a browser is a loopback
listener with a remote-control protocol behind it." A caller that really wants a browser passes
its own `BrowserReader` to `Pipeline::new`; nothing in the crate can construct one.

`atlas-browser` does not exist any more. It, `atlas-web` (the listener that used to link it) and
`atlas-e2e` (the harness that drove a real Chrome) were deleted together, so there is no crate
left holding a CDP client and no binary to link it. `reachability` still asserts the absence
three ways — no manifest edge from the agent or from `atlas-ingest`, no remote-control flag or
socket in the agent's closure *source*, and a non-vacuity check that the scanner still finds
capabilities elsewhere — so a comment that re-introduces the flag fails too.

The reader-side files `data/chrome.log` and `data/chrome-profile/` stay in the paths layout
(`atlas-config/src/paths.rs:169`, `:173`) and nothing writes them: the crate that did,
`atlas-browser`, is gone.

## 3. Every dial in the shipped crates

```bash
# the window's own dial, and the launcher's two
grep -rn "::connect(\|::connect_timeout(" rust/atlas-launcher/src rust/atlas-desktop/src
# atlas-launcher/src/health.rs:56   atlas-launcher/src/ports.rs:61   atlas-desktop/src/http.rs:525

# the agent's dial to the model server — a fourth one, in atlas-ai
grep -rn "::connect(\|::connect_timeout(" rust/atlas-ai/src
# atlas-ai/src/http1.rs:209
```

| site | the address | what refuses everything else |
| --- | --- | --- |
| `atlas-desktop/src/http.rs:525` | `SocketAddr::from(([127,0,0,1], port))`, built at `:452` | `endpoint()` (`:368`) refuses any host but the literal `127.0.0.1` (`:430`) — `localhost` too, since a name would need resolving; also `https`, a bare authority, a user name, a query, a fragment, a `..` path, port 0 |
| `atlas-launcher/src/health.rs:56` | `SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)` from settings | a constant address; hand-written `GET /health`, no name resolution, no TLS, body discarded |
| `atlas-launcher/src/ports.rs:61` | `SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)` | the same constant; a refusal is the answer, nothing is sent |
| `atlas-ai/src/http1.rs:209` | the agent's `POST /v1/chat/completions` to the model server | `endpoint` (`atlas-ai/src/endpoint.rs:100-126`) refuses `https`, any host that is not a loopback address, and a name at all — `localhost` is mapped to `127.0.0.1` without a resolver; `atlas_guard::Gated::loopback` is the type that carries it — measured 14:13Z: `grep -rn "::connect(\|::connect_timeout(" rust/atlas-ai/src` returns exactly this one line |

The window dials only when the reader asks the model something on Chat
(`views/chat.rs:658` → `agent::run` → `http::ask`), and that dial carries text the reader typed. It
fetches no image, font or script **for its own drawing** — egui draws from compiled-in fonts and
the files listed in §4. The one exception is the page view the reader opens: there the *system
engine* fetches the page and its subresources, which is a browser's traffic and not this crate's
`connect` sites (`docs/THREAT-MODEL.md` §4.19).
The agent process (`atlas-monitor`) dials the same server once per model turn through
`atlas-ai`, and nothing else: page fetching is `atlas-net`'s, under the gate, and a search goes
to **one** engine — the reader's template if they set one (`meta.search_engine` or
`ATLAS_SEARCH_ENGINE`), and otherwise the built-in `search::DEFAULT_TEMPLATE`, Bing News' RSS
feed (`rust/atlas-ingest/src/search.rs:62-63`). The tool result names the host and whether the reader chose it, never the template
(`search.rs::Engine::report`; `rust/atlas-ingest/tests/search_privacy.rs` is the test, `6 passed;
0 failed` at 15:00Z, and §5 records the fixture that made one assertion red this afternoon).

## 4. Downloads

```text
$ cargo test --offline -p atlas-guard --test audit_downloads
test result: ok. 16 passed; 0 failed; 0 ignored   (14:57Z)
```

The machine-checkable half: no installer process, no `curl`/`wget`/`pip`/`git clone`, no
download primitive spelled through `format!`, exactly one HTTP client (`atlas-net`), no
second one even named, no program built at run time, every spawn site a reviewed file.

What a fresh machine does:

1. **The build** fetches crates from crates.io once, at the versions `Cargo.lock` pins
   (`--locked` refuses anything else). No vendored tree exists here, so an empty `~/.cargo`
   does fetch the lock's packages. That is a build, not a run.
2. **`atlas start` provisions by default**, and the rule is two conditions, not one:
   **no download unless provisioning is due AND the licence is recorded.**

   *Due* means the model file or `bin/llama-server` is missing and `--no-install` was not
   given. That is the default, not an opt-in: `StartOptions` is built with `no_install: false`
   (`rust/atlas-launcher/src/args.rs:144`), and the switch exists so a start can be told *not*
   to — `args.rs:272`, `no_install_is_its_own_switch_and_its_absence_means_the_first_run_provisions`.
   When it is due, `setup::launch` runs `atlas-setup --install --root <root>` with
   `stdin(Stdio::null())`, stdout and stderr inherited (`rust/atlas-launcher/src/setup.rs:368-376`).
   Retrieval is `ureq` in the child, after the printed line that names the amount, the host and
   the destination (`atlas-setup/src/install.rs:130-146`).

   *Recorded* means `data/license.json` holds the accepted document version —
   `apache-2.0+notice-2` (`atlas-setup/src/license.rs:32`), the same string as the window's
   screen (`atlas-desktop/src/views/license.rs:49`, and a test fails if the two drift). The
   child asks that question **first**, before its progress file and before any network. With
   nobody at the terminal its `answer()` returns `None`, `license::decide` returns
   `not_accepted_line()` (`license.rs:199-205`, `:220-234`) and the run stops having fetched
   nothing. So a machine whose owner has not accepted — neither at `atlas-setup --install`,
   nor by `--accept-license` from a script, nor at the window's own screen — cannot be
   provisioned by `atlas start` alone: it prints the refusal and starts nothing else
   `setup.rs:312-345` re-measures and stops, so "installed but missing" cannot pass either).
   The two records are separate files, and that is deliberate: the window's `acceptance` row
   lives in `data/requests.db` (`atlas-desktop/src/requests.rs:90`) and does **not** satisfy
   the install child; the launcher itself never reads either file — the gate is in the child,
   which is the process that fetches.

3. **Nothing else ever fetches software.** The agent fetches *pages* (the product's work,
   `ureq`/`rustls` in `atlas-net`); it installs, upgrades and executes nothing. The reader's page
   view (`docs/THREAT-MODEL.md` §4.19) downloads no program: the engine is the OS component —
   WKWebView on macOS, WebView2 on Windows — and `wry` is a binding to it, not a browser that is
   fetched. A page that cannot be rendered falls back to the text reader instead of fetching
   anything.

**A hand-run tool is outside every lock this document describes.** The instance lock
(`data/`, and `data/atlas.lock` as the record) is taken by `atlas start`
(`rust/atlas-launcher/src/start.rs:109`); `atlas-ingest` takes no lock at all and opens the
archive read-write (`rust/atlas-ingest/src/main.rs:131` → `Store::open`,
`rust/atlas-store/src/lib.rs:164`), whose first pragma is `journal_mode=WAL`
(`atlas-store/src/lib.rs:45`). Measured consequence, and the reason this sentence is here: a
research you start by hand can write into `data/atlas.db` **while `atlas start` and the window
have it open**, with no lock error — WAL is a writer beside readers, and the window's reader is
`READ_ONLY` + `query_only`. Nothing breaks, and the window picks the rows up on its next read,
but "one ATLAS per data directory" is a rule about `atlas start`, not about the tools beside it.

## 5. What is red today

Measured 2026-11-22, this pass. The tree is being edited while this is written, so each line
carries its own run, and a count is a timestamp, not a constant.

**`atlas-guard` has one red left, and it is the window's queue writer.** The four targets a port
question depends on, run in one sitting:

```text
$ cargo test --offline --locked -p atlas-guard --test audit_binds
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.02s
$ cargo test --offline --locked -p atlas-guard --test audit_downloads
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 13.46s
$ cargo test --offline --locked -p atlas-guard --test reachability
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.79s
$ cargo test --offline --locked -p atlas-guard --test audit_manifest
test result: FAILED. 17 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.80s
```

`audit_binds` is the listener proof and it is green with the engine in the tree: the window
contributes no listener line, `src/webview.rs` included. The one red is
`the_desktop_writes_no_file` (`atlas-guard/tests/audit_manifest.rs:1643`), and its message names
the site: `atlas-desktop/src/requests.rs:446 open: { … set_permissions(file, … 0o600) … }`. That
is the window's own queue file being chmodded by hand instead of through
`atlas-platform::private`; it is a product decision and a line in `docs/THREAT-MODEL.md` §5, not
a test edit, and the file belongs to another owner — this document reports it and changes
nothing. `reachability`'s older red (`the_window_links_no_workspace_crate`, a sort order) is
green now: 12 passed.

How each red got there, so the record is a history and not just a tick:

- `audit_downloads` — was `14 passed; 1 failed` at 14:11Z. The finding was
  `atlas-setup/src/disk.rs:124` handing `powershell` to `child::command` to ask the drive for its
  free space. **Fixed by deletion, not by an entry on a list:** `atlas-setup/src` has no
  `disk.rs` now (`check import install lib license llama main model progress sha`), while
  `INSTALLERS` in `atlas-guard/tests/audit_downloads.rs:261` still names `"powershell"` and still
  fails a spawn site that uses it. The target is `18 passed; 0 failed` now; the two extra tests
  arrived after 15:00Z's `16 passed`.
- `audit_manifest` — was `17 passed; 1 failed` at 14:19Z: `glutin_egl_sys 0.7.1`,
  `glutin_wgl_sys 0.6.1`, `khronos_api 3.1.0`, `windows_x86_64_msvc 0.52.6`, four build scripts
  arriving with the window's GPU stack (`eframe`/`glutin`). They are on the list with their
  sentences (`atlas-guard/tests/audit_manifest.rs:751-797`), and the target is `17 passed; 1
  failed` today for a different reason entirely — `the_desktop_writes_no_file`, above. They run
  while cargo builds and never at *run* time; that distinction is the one to keep.
- `search_privacy` — was `5 passed; 1 failed` at 14:14Z, and the failure was the *fixture*, not
  the default: its feed linked to `https://publisher.example/a`, and `.example` is a reserved TLD
  the gate refuses, so the answer read `"count": 0, "refused_links": 2`. The fixture now carries
  an ordinary public host and says why (`rust/atlas-ingest/tests/search_privacy.rs:47-56`);
  `6 passed; 0 failed` (15:00Z).
- `reachability` was `11 passed; 2 failed` this morning because it still encoded the architecture
  it was written against; then `13 passed; 0 failed` (14:13Z), then a sort-order red
  (`the_window_links_no_workspace_crate`), and `12 passed; 0 failed` now — it lost a test when
  `atlas-browser` was deleted from the tree.

**What is red in the window's own suite at 18:08Z belongs to `atlas-desktop`'s owners**, and none of
it is a port: `cargo test --offline --locked -p atlas-desktop --lib` → `326 passed; 4 failed`, the
failures being `requests::tests::the_queue_table_and_index_are_the_stores_word_for_word`,
`views::graph::tests::pinch_zooms_about_the_pointer`,
`views::map::tests::the_parse_time_and_the_frame_line_count_are_measured` and
`views::map::tests::a_zoom_sweep_never_loses_a_coast_and_never_leaves_the_plate`. It is listed here
because this is where the tree's red is recorded, not because it has anything to do with ports.
(At 15:01Z the same command was `288 passed; 1 failed`, the rail's page count — so the number a
reader remembers is the day's, not the suite's. The count could not be refreshed at 18:35Z: the lib
target would not compile while `views/graph.rs` was being edited by its owner —
`place_labels` called with four arguments where it now takes five, `best_slot` with ten where it
takes eight, at `views/graph.rs:1879` and `:2173`. That is a mid-edit, not a finding.)

**Every count in this document is a timestamp, not a constant.** `atlas-desktop --lib` went
256 → 269 → 276 → 282 → 288 → 326 passed and 1 → 4 failed across the day. Quote a number with the
time you measured it or do not quote it.

## 6. How to re-check

```bash
cd rust
cargo tree --offline -p atlas-launcher --edges normal     # five workspace crates
cargo tree --offline -p atlas-desktop  --edges normal     # five entries, wry among them, no HTTP client
grep -rn "TcpListener\|UdpSocket\|::bind(\|axum::serve\|hyper::" \
  atlas-launcher/src atlas-desktop/src atlas-platform/src atlas-config/src
grep -rn "::connect(\|::connect_timeout(" atlas-launcher/src atlas-desktop/src
cargo test --offline -p atlas-launcher --test no_listener
cargo test --offline -p atlas-guard                         # the package §5 quotes
cargo test --offline -p atlas-guard --test audit_binds
cargo test --offline -p atlas-guard --test audit_downloads
cargo test --offline -p atlas-guard --test reachability      # §5: the reviewed list, red until updated
```

Every listener-grep hit that is not `atlas-config/src/ports.rs:64` sits in a `#[cfg(test)]`
module, and `atlas-desktop/src` — `webview.rs` included, once it lands — contributes none: a child
web view is not a server.
