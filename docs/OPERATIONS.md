# ATLAS operations

Everything ATLAS needs runs on this machine, for this account, on loopback. This is the
operator's view: how to start and stop it, which log says what, what each `atlas doctor`
check actually measures, and what to do when the model, a port or the archive is the
problem. Nothing here downloads anything while the app runs, there is no update check and no
telemetry. The single exception is provisioning: on a machine where the model file or
`bin/llama-server` is missing, `atlas start` runs `atlas-setup --install` **once**, as a child,
before anything else starts (§1.2, §4). Nothing downloads on a machine that is already
provisioned.

## 1. Start and stop

There are two ways in. `./run.sh` (macOS, Linux) or `run.ps1` (Windows) opens **the window**
alone: the product's face, reading the archive. `atlas start` is the collection process: it
starts **up to four programs** and then waits for the window. Measured from
`rust/atlas-launcher/src/plan.rs::plan_lines` (the lines `--dry-run` prints) and the boot in
`start.rs`, in the order they run:

| # | program | when |
| --- | --- | --- |
| 1 | `atlas-setup --install` | **only** when the model file or `bin/llama-server` is missing, once, and it exits before step 2 — `start.rs:157`, `setup.rs:368` |
| 2 | `llama-server` | every start; `--host 127.0.0.1`, its port from `llama_port` |
| 3 | `atlas-monitor serve` | every start — the always-on agent that claims `data/requests.db` |
| 4 | `atlas-desktop` | every start; `atlas start` waits for it, and closing the window ends the start |

The stop order covers the three that are still running: window, then agent, then model server
(`shutdown.rs:51`). The provisioning child is not in it because it is gone before the others
exist — it is supervised by the boot, which turns a stop request into its exit, and a failed
install is one sentence and no further children.

`atlas start --dry-run` prints the plan — root, provisioning, window, agent, model file, model
server, "no listener", stop order — and starts nothing (`rust/atlas-launcher/src/plan.rs`); the
exact output of a run is quoted in `docs/RUST-PROOF.md` §2.5.


### 1.1 The window

The script, from the project root — this is what `run.sh` and `run.ps1` now do:

```bash
./run.sh            # build rust/target/release/atlas-desktop if it is missing, then open it
./run.sh --help     # the two commands, and nothing else
```

By hand, which is what the script does:

```bash
cd rust
cargo build --release --offline --locked -p atlas-desktop   # once
./target/release/atlas-desktop                              # the window
```

```bash
rust/target/release/atlas-desktop                           # from the project root, same binary
```

The window takes no arguments: it finds the project from its own path, reads `data/atlas.db`,
and refuses an argument rather than ignore it (`run.sh`, `run.ps1`). It starts no program of its own
— not the model server, not the collection process — and it serves nothing. (On Windows the system
WebView2 runtime shows up as child processes of the window; those belong to the OS component that
renders a page, not to a program ATLAS launches.)

What it opens: **the archive, read-only** —
`Archive::open`'s `OpenFlags::SQLITE_OPEN_READ_ONLY` (`rust/atlas-desktop/src/data.rs:318`),
then `pragma_update(None, "query_only", true)` (:324), read back at :328, and a connection that
answers `0` is refused with the path and the reason ("opened read-write — refusing to use it") —
**one outbound socket, only when you ask the
chat a question** — `TcpStream::connect_timeout` (`src/http.rs:626`), to the address built at
`src/http.rs:550` as `SocketAddr::from(([127, 0, 0, 1], port))`, one streamed request and one
connection close; the port is the only number it takes from `data/config.json`
(`llama_port`, `src/assets.rs:101`, parsed by `model_port_from` at `:150`) — and **one page, only
when you click a story's address**.

That last one is the reader's page view (`rust/atlas-desktop/src/webview.rs`,
`src/views/reader.rs`; threat model §4.19). On macOS and Windows the window hands the story's URL to
the web engine the operating system already has — WKWebView, WebView2, through `wry` — and shows it
in a rectangle inside its own layout, under a toolbar: a back arrow (only when there is a page to go
back to), the **story's host**, and three links — reload, ask the model about this story, tag it. On
Linux the engine is not compiled in at all, and if it cannot be created at run time (a Windows box
with no WebView2 runtime, a sandbox that refuses the view) the page falls back to the **text
reader**: the article's stored text in the same pane, no engine, nothing to click but the app's own
buttons. Nothing is downloaded to make a page render.

The engine is never started for anything you did not click: there is no prefetch, no thumbnail, no
opening a page because a row exists, and the search and reading the model does never touch it —
those go through the app's own client (`docs/NO-PORTS.md` §3). Once a page *is* open it behaves like
a browser, because that is what it is: it fetches its images, styles and scripts itself, keeps its
cookies, and the site sees a request from this machine. Two honest limits, both in the threat model
§4.19: the toolbar shows the host the *story* came from, not a live address, so a link you click
inside the page can take the frame somewhere the toolbar does not name; and until two wry defaults
are overridden a page can start a download or ask for the camera.

What it never does: it **listens on nothing** — no port, no socket, nothing can connect *to* the
window (§9); the page view is a child view inside this process and adds no listener — and it
launches no program of its own: `rust/atlas-desktop/tests/no_spawn_no_socket.rs` holds both rules
as a scan over `src/` (a listener bind, a `std::process`, `Command::new`), so neither can quietly
stop being true. (On Windows the WebView2 runtime's helper processes appear as children of the
window; they are the OS component that draws a page, not a program ATLAS starts.) It takes no lock
either: you can leave the window open while
`atlas start` collects, and a second window is a second reader.

What it **does** write, and it is exactly one file: `data/requests.db`
(`rust/atlas-desktop/src/requests.rs`). A click that means *go and look* — research something,
run a monitor now, add a topic, enrich the graph — is one row in that queue, with a kind from
`KINDS` (`requests.rs:60`) and a JSON payload. **`KINDS` has six entries**, and the fifth is
`settings_set` (`requests.rs:65`): a setting you changed on the Settings page, which the window
cannot write itself; the sixth is `brief`, the model's prepared summary of one scope. The other
four are `research`, `monitor_run`, `topic_add` and `enrich`.
Every row is an intent, not an action: nothing in the window runs because of it, the archive is
still `READ_ONLY` with `PRAGMA query_only`, and the process that claims the row is `atlas start`'s
agent (§1.3). The same file carries a second table, `acceptance`: one row per licence-text
version, written when you tick the box on the first screen (`src/requests.rs:90`, key
`apache-2.0+notice-2` from `src/views/license.rs:49`), so the screen does not come back until
the text changes. `rust/atlas-desktop/tests/` `no_spawn_no_socket.rs` allows that one read-write
open and fails on a second.

A `settings_set` row is the only one whose answer can touch a file other than the queue, and it
does it in the agent, not in the window (`rust/atlas-monitor/src/settings_set.rs`):

- **A model setting** goes through `atlas_config::model_settings` — the same validator the CLI
  runs — and is written by `atlas_config::Config::save`, atomically, `0600`, refusing a
  symlinked `config.json` (`settings_set.rs:85-93`). A value out of its range is **clamped** and
  the clamp is printed as a note; a key that is not settable, a value that is not a number, or a
  payload carrying a credential is **refused by name**, and the row is finished with that
  sentence instead of writing anything (`settings_set.rs:96-100`). "Refused" here means: the
  row carries the sentence, `data/config.json` is untouched.
- **A model to bring in** (`{"import_model": "<path or https url>"}`) starts
  `atlas-setup --import-model <value> --root <root>` **once**, as a supervised child of the
  agent, waits for it, and finishes the row with the tool's own answer (`atlas-monitor/src/import_model.rs:88-180`,
  `atlas-setup/src/main.rs:118`). The window never starts it, and the value is
  passed through rather than decided: `.gguf`, `https`, size and digest are `atlas-setup`'s
  rules.
- **`search_engine`** is not a `data/config.json` key at all: it lives in the archive's `meta`
  table, and a request naming it is sent on its own (`atlas-config/src/checks.rs:57`).

The first launch is that screen and nothing else: a page title ("Read this before you start."),
three `DISCLAIMER` paragraphs — the app runs on this machine; the model summarises and does not
verify; the license covers this app's own source, not the libraries, the model server or the
weights — the Apache-2.0 text in full, embedded with `include_str!` so it
draws on a machine where nothing is set up. One tick — "I have read and accept the Apache
License 2.0…" — enables a button reading "Accept and open ATLAS"; beside it, "Quit". The rail,
the pages and the status row are not drawn behind it.

One sentence on that screen is **stale and must be fixed where it lives**: the first paragraph says
"Nothing is sent to a server", which stopped being true when the app shipped a default search engine
(`docs/THREAT-MODEL.md` §4.16) and when the reader's page view began fetching pages the reader opens
(§4.19). `rust/atlas-desktop/src/views/license.rs` is the file; this document does not quote the
sentence as a fact.

What it reads, and when (the line numbers in this section were re-read on 2026-11-22 after the
crate became a library + binary; the file and function names are the durable part):

| What | From | When |
| --- | --- | --- |
| the eight counts, the pins, the countries, the articles, the entities, the edges, the topics, the monitors | `data/atlas.db` (`rust/atlas-desktop/src/data.rs`) | when the page, the 24 h / 3 d / 7 d window or the search box changes, and when Refresh is pressed — never per frame |
| the world map's coastlines | `public/vendor/ne_50m_countries.geojson` (3 083 490 bytes) | once, when the Overview page is first drawn |

How it finds the files: `rust/atlas-desktop/src/assets.rs` walks up from the executable to the
nearest ancestor holding `rust/Cargo.toml`, so `rust/target/release/atlas-desktop` reads the
checkout it was built in and `--root` is not needed. The status row prints the archive path, the
number of stories read and how long the read took. Its right-hand side is the window's own label,
and today that label still reads `native window · no browser` — find it with
`grep -n "native window · no browser" rust/atlas-desktop/src/app.rs` (two hits at the time of
writing: the status row and the test that asserts its text; the line numbers move with the file, so
the pattern is the citation)
— **stale since the page view landed**, and the code owner's one-line fix: it should name what the
window is showing, the page view or the text reader. The page view itself reads no file of its own:
a page comes from the network when you ask for one, and the text reader reads the archive row you
already had.

If there is no archive the window still opens: the status row reads
`no archive at …/data/atlas.db — start ATLAS once so it is created`. Nothing is created, because
the connection is read-only. If the outlines are missing the map draws its graticule and the
pins, and no coastlines.

### 1.1.1 The model settings

Six keys in `data/config.json` — `model_local` and five numbers — set from the Settings page
or from a shell. `rust/atlas-config/src/spec.rs:88-108` is the table that defines them; the window keeps its own
copy of the same ranges (`rust/atlas-desktop/src/views/settings.rs:358-371`) because it links no
workspace crate. The two agree as measured today — every bound and default in that block matches
its `SPEC` row — but only the *flag order* is held by a test
(`the_argv_preview_is_the_launchers_order`, `views/settings.rs:1383`); a range changed in
`atlas-config` would not fail the window's suite.

| Key | What it is | Range | Default | Where it is enforced |
| --- | --- | --- | --- | --- |
| `model_local` | the path of the `.gguf` the server is started with; absolute, ends `.gguf`, must exist, not be a directory, and must resolve to a file inside `<root>/models` (`atlas-config/src/checks.rs:157-243`) | any absolute path that passes those rules | empty — the launcher tries the pinned file name in `models/` first and then any other `.gguf` there (`atlas-launcher/src/model.rs:135-170`) | `llama.cpp`, as `-m` |
| `model_context` | the window, in tokens | 1 024–262 144 | 32 768 | `llama.cpp`, as `-c` |
| `model_reasoning` | tokens the model may spend thinking | 0–32 768 | 8 192 | ATLAS's own request, **not** a llama.cpp flag |
| `model_answer` | tokens one answer may use | 256–32 768 | 8 192 | ATLAS's own request, **not** a llama.cpp flag |
| `model_gpu_layers` | how much of the model goes on the GPU | −1–999 | −1 (llama.cpp decides) | `llama.cpp`, as `-ngl` |
| `model_threads` | processor threads | 0–256 | 0 (llama.cpp decides) | `llama.cpp`, as `-t` |

The division is not cosmetic. `atlas-ai` composes **one** completion budget, reasoning included,
so a turn asks for `model_reasoning + model_answer` tokens; with `model_reasoning = 0` the
request carries `model_answer` alone and no `enable_thinking` (`rust/atlas-ai/src/budget.rs:20-27`,
`Budgets::max_tokens`). The window's Settings page prints the exact argv those numbers produce —
`llama-server -m <model> -c <context> -ngl <layers> -t <threads> --host 127.0.0.1 --port <port>
-np 1 --jinja` (`views/settings.rs:516-543`) — and one line under it saying that the context is
handed to llama.cpp while the two budgets are ATLAS's (`views/settings.rs:545`, `ENFORCED`). The
page shows context, thinking and answer in front of you and puts `-ngl` and `-t` under
"Advanced" (`views/settings.rs:628-661`).

**How a value gets written.** The page cannot write `data/config.json` — the window writes one
file, the queue — so "use these numbers" writes a `settings_set` row (`views/settings.rs:717`)
and the agent's arm validates and saves it (§1's `settings_set` bullets). A number you type
outside its range is **refused on the page, before a row exists**, with the field and both bounds
named (`views/settings.rs:743-774`); a number already in the file and out of range is clamped
with a note when the file is read, because a file is something to read, not a question to answer.
From a shell the same validation is `atlas-config`:

```bash
rust/target/release/atlas-config set --root . model_context=65536 model_answer=4096
# answers changed, values, notes, config_file (its USAGE, `atlas-config/src/cli.rs:45-52`);
# a value out of range is clamped into `notes`, a key that is not settable is refused by name.
```

Every key also has an environment override (`atlas-config/src/spec.rs:110-124`):
`ATLAS_MODEL_CONTEXT`, `ATLAS_MODEL_REASONING`, `ATLAS_MODEL_ANSWER`,
`ATLAS_MODEL_GPU_LAYERS`, `ATLAS_MODEL_THREADS`, `ATLAS_MODEL`. **A change takes effect the next
time `atlas start` runs**: the boot builds the argv once (`rust/atlas-launcher/src/llama.rs:182`)
and a respawn inside a run re-uses the same command line (`start.rs:570-590`), so the running
server keeps the flags it was started with.

The window writes no log file (see §2) and holds nothing open after it exits. Close it with the
window controls, `Cmd-Q` or `SIGTERM`; there is no stop sequence, because it started nothing.

### 1.2 The model server

The window cannot start it, and it talks to it over one hand-written request. If you want the
model, it is a separate process — and there is no `atlas` subcommand that starts *only* the
model yet: `atlas start` starts it and then opens the window, so the alternative is the binary's
own argv:

```bash
# what `atlas start` runs, printed by rust/atlas-launcher/src/llama.rs
./bin/llama-server -m models/<your>.gguf -c 32768 --host 127.0.0.1 --port 8080 \
  -np 1 --jinja --log-disable
```

The flags are not preferences. `-c 32768 -np 1` is one slot of 32 768 tokens: the number is
`model_context`'s default (`rust/atlas-config/src/spec.rs:99`, range 1 024–262 144, moved by
`data/config.json` or `ATLAS_MODEL_CONTEXT`) written into the argv at
`rust/atlas-launcher/src/llama.rs:183-187`, and the one slot is `SLOTS` (`llama.rs:79`). It is the
size `atlas-ai`'s `context::CONTEXT_TOKENS` (`atlas-ai/src/context.rs:59` — the fallback a run
uses when no window is configured) and the window's own loop are budgeted against: the model
thinks before it answers, so a smaller window spends its room on the thought and returns an empty
reply (the measured turn in §1.2's next paragraph). `-np` above 1 divides `-c` by the slots, and
llama.cpp will not fit the prompt in one of them; `--jinja` is what makes tool
calling work with this model's chat template; `--host 127.0.0.1` is written out rather than
relied on; `--log-disable` is passed always, and dropped exactly once if the binary refuses it
(it would otherwise write the prompt into its own log). `--no-webui` is added when
`llama-server --help` advertises it. `8080` is the `llama_port` default
(`rust/atlas-config/src/spec.rs:90`); `data/config.json` moves it.

**The model thinks.** `atlas-ai`'s `ChatRequest::new` defaults `enable_thinking: true` and
`DEFAULT_MAX_TOKENS = 8192` (`rust/atlas-ai/src/chat.rs:37`, `:150`), and the agent's own default
matches (`AgentConfig::default()`: `enable_thinking: true`, `FINAL_MAX_TOKENS = 8192` — the final
turn gets the same room as every other, because a 1 024-token ceiling came back with an empty
`content`). One run may take `MAX_STEPS = 10` model turns, `TOTAL_TOKEN_BUDGET = 180 000` tokens
and a wall clock of `TURN_DEADLINE = DEFAULT_TIMEOUT × (MAX_STEPS + 1)`
(`rust/atlas-ai/src/agent/state.rs:31-75`). The measurement those numbers come from, quoted from
that file: one classification with thinking on took **239 s** and **4 633** completion tokens.
The window's own chat loop is tighter — seven tools and `tools::MAX_STEPS = 6`
(`rust/atlas-desktop/src/tools.rs`) — and a reply carries the thinking in a separate field from
the answer, never substituted for it (`atlas-ai/src/chat.rs:241`).

I did not start that server while writing this: the argv is read from
`atlas-launcher/src/llama.rs::invocation`, and the model file name is the one in `models/`.

Where the window looks for it: `ATLAS_MODEL_URL` if you set it, else `http://127.0.0.1:8080`
(`rust/atlas-desktop/src/app.rs`, `model_url_from`). The address rule is an address rule, not a
host allowlist: `http://` plus a `127.0.0.1` literal plus a port, and anything else — `https://`,
a name, a user name in the authority, no port — is refused with a sentence before a socket is
touched. So a model server on another port is one environment variable away, and a model server
on another machine is not possible at all.

When nothing is listening, the reply in the transcript names the address it tried and the start
script. The script is a platform decision made in one place
(`rust/atlas-desktop/src/http.rs:66`): `./run.sh` on macOS and Linux, `run.ps1` on Windows. What
that script does is open **the window**; it starts nothing, so the sentence points at the
project, not at the model server. The command that starts the model server is `atlas start`
(§1.3), or the argv above.

### 1.3 Collecting: `atlas start`
Starting is the launcher's job, and it is not wrapped in a script any more: `run.sh` and
`run.ps1` build and open the window (§1.1). Call the binary directly, from the project root:

```bash
cargo build --release --offline --locked -p atlas-launcher -p atlas-desktop   # once
./rust/target/release/atlas start                            # provision if needed, model server, agent, window
./rust/target/release/atlas start --dry-run                   # print that plan, start nothing
./rust/target/release/atlas start --no-install                # never provision; report instead
./rust/target/release/atlas start --root /srv/atlas           # another project root
./rust/target/release/atlas doctor                            # what this machine has
```

A workspace that builds but is not provisioned — no model server, no model file — is provisioned
by the first `atlas start` itself: when the model file or `bin/llama-server` is missing it runs
`atlas-setup --install` as a child, says so in one line before and one after, waits for it (the
download is 6 526 302 432 bytes, once — progress goes to `data/setup.json`, which the Chat page
draws), and stops the boot with one sentence if the child fails; `--no-install` turns that step
off for a machine whose owner provisions by hand, and §4 is what it runs either way.

**`atlas start` opens no port.** It used to serve the collection API itself (`atlas-web` on
`127.0.0.1:8787`) and to start a reader browser over the DevTools protocol; neither is in this
binary, and nothing it starts does either (§9, and `docs/NO-PORTS.md` §2.1 for the browser's
removal). It runs `llama-server` as a child, starts the agent, starts the product — the native
window, `atlas-desktop` — as another child, and waits for it. The window reads `data/atlas.db`
itself and dials the model outbound; nothing in ATLAS listens.
`rust/atlas-launcher/tests/no_listener.rs` pins this (the binary's whole dependency closure,
five workspace crates, holds no listener and no browser; measured 14:15Z: `4 passed`), and §9 is
the table.

A start prints, in order: the root, the settings file and the model port, the loopback line, the
model file and its size, the window binary, the model server's command line and pid, how long it
waited on `/health`, then the window's own line. From this checkout:

```text
ATLAS — fully local news intelligence (loopback only, nothing downloaded)
ATLAS: root .../news-atlas
ATLAS: settings from .../data/config.json: model port 8081 (ATLAS opens no port of its own)
ATLAS: the model server is started on 127.0.0.1 only, and nothing else in ATLAS listens
ATLAS: model Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf (6.5 GB) at .../models/...
ATLAS: window .../rust/target/release/atlas-desktop
ATLAS: agent .../rust/target/release/atlas-monitor serve
ATLAS: model server llama-server -m Qwen3.5-9B-... -c 32768 --host 127.0.0.1 --port 8081 -np 1 --jinja --no-webui --spec-type draft-mtp
ATLAS: model server pid 57111 — waiting up to 4m 0s for http://127.0.0.1:8081/health
ATLAS: the model server is answering after 2s
ATLAS: the agent is .../rust/target/release/atlas-monitor serve (pid 57142); it prints one JSON line per action
ATLAS is open. Close the window, or press Ctrl-C, to stop it.
```

A machine that has never been provisioned has two more lines in exactly that place — after the
loopback line, before the model line — and the boot waits for the child between them:

```text
ATLAS: the model file (.../models/Qwen3.5-...gguf) and bin/llama-server (.../bin/llama-server) are not here yet: atlas-setup --install runs once — a multi-gigabyte download, the one time this machine touches the network — and writes its progress to .../data/setup.json, which the window reads
ATLAS: .../rust/target/release/atlas-setup --install --root ... (pid 57103) — its own output follows
ATLAS: the model file (.../models/Qwen3.5-...gguf) and bin/llama-server (.../bin/llama-server) are in place now, so this step does not run on the next start
```


The model port in those lines is `8081` because this checkout's `data/config.json` says so; the
default is `8080` (`rust/atlas-config/src/spec.rs`). The file holds one key — `{"llama_port":
8081}` — so the web and browser port numbers the config layer still carries
(`rust/atlas-config/src/spec.rs:90-92`) are not printed and not bound by anything (§9).
`--dry-run` prints those same lines without taking the lock, loading weights or opening a window.

To stop: close the window, press Ctrl-C, or send `SIGTERM` to the `atlas` process. The stop order
is fixed and is not the order things were started in: **the window, then the agent, then the model
server**. The window goes first because it is the only thing that shows anything and the only thing
that asks the model anything; stopping the model under it would leave a Chat page asking a port
nothing answers. The agent goes second because it does that same work from the queue — it claims a
request and drives a research pass through the model, and it answers `SIGTERM` by finishing the line
it is printing and exiting 0 — so it is asked to stop while the model can still answer it and the
model itself goes last. Every child ATLAS started is reaped before it exits, and a process it did
not start is never signalled.

#### How a stop arrives, per platform — including the Windows console's X

On POSIX there are two ways in and both are handled: `SIGINT` (Ctrl-C) and `SIGTERM`
(`rust/atlas-launcher/src/signals.rs:98-139`).

Windows is different, and it used to be the one place where a stop could lose a child. A console
program there is told to stop in three ways, and Ctrl-C is only one of them:

| event | what raises it | was handled |
| --- | --- | --- |
| `CTRL_C_EVENT` | Ctrl-C in the console | before, and now |
| `CTRL_BREAK_EVENT` | Ctrl-Break | before, and now |
| `CTRL_CLOSE_EVENT` | the console window closed with its **X** | **not before**; now it is |

The X is the ordinary way a person closes a console on Windows. Windows ends the process shortly
after that event whether or not anything cleans up, and — unlike POSIX — it does not kill a child
when its parent exits, so `llama-server` kept running with nothing on screen and the loopback port
still taken; the report that produced this section was "the app is gone but chat answers until I
reboot". The fix is in `rust/atlas-launcher/src/signals.rs:140-182`: the `#[cfg(windows)]` arm
installs all three listeners (`tokio::signal::windows::{ctrl_c, ctrl_break, ctrl_close}`, tokio
1.54.1) and waits on whichever exists, so the X runs the same `[window, agent, model server]` order
as Ctrl-C; a listener that cannot be installed waits forever instead of being skipped, and the
fallback when none of the three registers is the plain Ctrl-C handler. The comment beside it keeps
the measured shape of the bug, so the next reader knows why three listeners exist for one event.

**What this document cannot verify.** The arm is `#[cfg(windows)]`, and this pass was written on
macOS: it cannot be compiled here (a `--target x86_64-pc-windows-msvc` build needs the target
installed, and this tree installs nothing), and no Windows machine was available. So the claim is
"the code is in, and it is written the way the failing case requires", not "a closed console was
watched reaping its children". The part that *is* measured here is the platform-independent half:
`cargo test --offline -p atlas-launcher --lib` → `160 passed; 0 failed; 1 ignored` (14:22Z),
which includes `signals::tests` (`the_future_does_not_resolve_on_its_own`,
`the_installed_flag_is_false_and_a_wait_can_be_abandoned`,
`the_two_test_flags_read_as_idle_and_fired`) — the stop future every boot wait watches.

One ATLAS per data directory, and it takes **two** locks to say so. The first is on the data
directory itself (`data/`, opened and advisory-locked), because a lock on a *name* is not a lock
on a process: delete `data/atlas.lock`, start a second instance, and the second one locks a fresh
file and runs beside the first. A directory inode cannot be swapped for a new one the same way.
The second is on `data/atlas.lock`, the record file — it is what Windows can lock (a directory
cannot be opened as a file handle there), and it is what says who holds the instance. Both are
released by the kernel when the holder dies. A second `atlas start` on the same `data/` refuses
and names the pid and start time that hold it. A lock file nobody holds is reported and taken
over:

```text
ATLAS: warning: took over a lock left by pid 55261, started 2026-11-21T13:19:53Z, for ... — that process is gone and nothing of it is running
```

The record file is opened with `O_NOFOLLOW` and must be a regular file, so a symlink planted at
that name is refused rather than followed; it is created `0600` from the first byte and its
contents are read through an 8 KiB cap. It is not deleted on exit: it is the first thing the next
start can tell you about the previous run.

The lock is taken before anything is read, and `data/` and its files are then made owner-only
(`0700`/`0600`) before the settings are read — the step `atlas doctor` has always told you a start
performs, and the start prints how many paths it repaired.

## 2. What each log says

The window writes no log: it has no log file, and
`grep -rn "File::create\|OpenOptions\|fs::write" rust/atlas-desktop/src` prints only two test
fixtures (`requests.rs:426`, `views/topics.rs:1277`), never shipped code. It reports in its status
row instead — the archive path, the number of stories, how long the last read took, and what
went wrong if the archive could not be opened. Its one written file is the request queue
(`data/requests.db`, §1.1), which is a queue of intents, not a log. Everything below belongs to
the collection process.

| File | Who writes it | What is in it |
| --- | --- | --- |
| `data/app.log` | **nothing, any more** — it was the listener's request-gate log, and `atlas-web` is not in the tree; a start creates the file `0600` and writes no line | was one line per refused or slow request: `<ISO-8601 UTC> <METHOD> <path> <status> <n>ms` |
| `data/app.log.1` | the same, one generation back | `app.log` is rotated when it reaches 4 MiB (`LOG_MAX_BYTES`), and the older generation is replaced, not kept: the pair is at most two caps |
| `data/llama.log` | `llama-server`, its stdout and stderr | the model server's own output: load progress, the argv it accepted, a bind failure, slot activity |
| `data/chrome.log` | **nothing**, any more — the crate that wrote it was `atlas-browser`, deleted from the workspace with the frontend | was Chrome's stderr when a page needed a headless browser. The path stays in `atlas-config`'s layout, so it is still created `0600` and still reported by `atlas doctor`'s permissions check; it stays empty |
| `data/chrome-profile/` | **nothing**, any more, for the same reason | was the reader's cookies, the `DevToolsActivePort` file the app read the debug endpoint from; still inside `data/` (and so inside the `0700` tree), still empty |
| `data/atlas-run.log` | the shell, not the app | whatever `run.sh`'s stdout was redirected to. That redirect belonged to the collection start; `run.sh` now execs the window, whose output is the terminal |
| the launcher's own stdout, by inheritance | `atlas-monitor serve`, the agent | one JSON object per action — `{"kind":"research","id":7,"ms":41213,"stored":2,"tool":"web_search"}` — and nothing else, so `atlas start`'s log is the record of what the AI is doing: claim, tool, duration, articles stored, and the sentence if it failed |

Every log the **app** creates is created `0600` by the `open` call that creates it
(`atlas_platform::private::open_append`), not `chmod`ed afterwards, and an older `0644` file is
repaired the next time it is opened. `data/atlas-run.log` is the exception, because the shell
redirect creates it and the shell's umask decides its mode. `atlas doctor`'s `data permissions`
check reports anything that is still readable beyond its owner.

`data/app.log` is deliberately narrow. A line is written only when the answer is a status of
400 or above, or when the request took more than two seconds, so a normal browsing session
leaves the file alone. Every line goes through the redactor first (key shapes, `name = value`
assignments, bearer tokens, JWTs, e-mail addresses), control characters are dropped so a path
cannot invent a second line, the result is collapsed to single spaces and capped at 400
characters, and a line that is nothing but a secret is not written at all. No page text, no
prompt and no model answer ever reaches it.

Reading them:

```bash
tail -n 40 data/app.log       # which requests are being refused, and how slowly
grep -n "couldn't bind" data/llama.log     # the model server failed to take its port
tail -n 40 data/llama.log     # model load, then one slot's activity
# what the agent did, from the start's own output (it inherits the launcher's stdout)
grep -o '{"id":[^}]*}' data/atlas-run.log | tail -n 20
```

The llama log is the one to read when the app says the model is not answering: it records the
port it tried, whether the weights loaded, and whether it was asked to stop.

## 3. `atlas doctor`, check by check

```bash
rust/target/release/atlas doctor
rust/target/release/atlas doctor --root /srv/atlas
```

Eight checks, one line each, three levels: `ok`, `warn` (ATLAS starts, with less than the
machine could give it) and `FAIL` (ATLAS cannot start until this is fixed). The exit code is 1
when anything failed and 0 otherwise. No check makes a network call, nothing is written
outside `data/` (the throwaway probe files there are removed again), and the port probe
**connects** and hangs up instead of binding — a check must not open the port it is asking
about. The rule the checks keep: a check fails only when `atlas start` would stop.

- **toolchain** — a `cargo` and a `rustc`, with the versions they report. `FAIL` means no
  Rust: install it once from rustup.rs. ATLAS itself never needs the toolchain, but the build
  does.
- **window** — `rust/target/release/atlas-desktop` (or `atlas-desktop` on `PATH`), the product.
  `FAIL` names both places it looked and the command that builds it; without the window a start
  has nothing to show. This check replaced "native binaries", which listed the nine engine
  binaries out of `atlas_web::native::REQUIRED` — the listener crate's boot gate, which no
  shipped binary links.
- **model file** — the settings load, the model resolves, and the file's size is reported.
  `FAIL` when the configuration cannot be read (fix or remove `data/config.json`) or when no
  GGUF is where the resolution expects one.
- **model server** — `bin/llama-server` (or one on `PATH`). `FAIL` names both places it
  looked; the fix is `rust/target/release/atlas-setup llama --yes`, which runs the CMake build
  in `tools/llama.cpp` and copies the binary and its libraries into `bin/` (§4).
- **loopback invariant** — `127.0.0.1` is judged loopback by `atlas-guard`, and the
  configuration carries no host-like keys (the model server's address is that constant, in
  `llama-server`'s own argv; ATLAS binds nothing, so there is no other address to check).
  A failure here is a bug or a hand-edited key, never a setting you can relax.
- **sqlite** — a throwaway database in `data/` is created, written, read back and removed.
  It proves the directory is writable and that the bundled SQLite can open a file there
  (no system library, no missing WAL support). It does not touch the archive.
- **ports** — the kernel is asked, once, about the model server's port: free, already answering
  (in which case `atlas start` reuses it), or held by something else — a `warn`, because the
  window opens without its local model. The web and browser port numbers are not probed and not
  mentioned: nothing binds them (§9).
- **data permissions** — `data/` is 0700 and the files ATLAS writes there are 0600, and the
  directory is writable by you. `warn` when the directory is readable beyond your account
  (run `atlas start` once and it hardens the modes); `FAIL` when it is not writable at all.

## 4. A fresh machine: `atlas-setup`

Two pieces of ATLAS are not built by `cargo build`: `bin/llama-server` (compiled from the
vendored `tools/llama.cpp`) and the 6.5 GB GGUF model file (6 526 302 432 bytes here). A fresh
machine has neither, and the app cannot fetch them — its whole network story is loopback. So
they are provisioned before it runs, by one binary that is not linked into the app: `atlas-setup`,
which `atlas start` runs once as a child when a piece is missing (§1.3) and which you can also
run yourself.

```bash
rust/target/release/atlas-setup check            # eight lines: what is there, what is not
rust/target/release/atlas-setup check --verify   # and hash the model file (~90 s for 6.5 GB)
rust/target/release/atlas-setup llama            # print the cmake plan, change nothing
rust/target/release/atlas-setup llama --yes      # build bin/llama-server from tools/llama.cpp
rust/target/release/atlas-setup model            # print the URL, the size and the sha256
rust/target/release/atlas-setup model --yes      # fetch, verify, install; the one download
rust/target/release/atlas-setup all --yes        # model, then llama, then check
```

Four commands exist — `check`, `model`, `llama` and `all` — and the list above is the whole
interface. There is no `ui` subcommand any more: the wasm target and bundle went with the
frontend the window replaced, and the binary says so rather than pretending:

```text
$ rust/target/release/atlas-setup ui
atlas-setup: unknown command "ui"
```

`./setup.sh --provision` (or `setup.ps1 -Provision`) is the same thing after a cargo build,
and it runs `check` first either way — the second half of a fresh install is never a guess.

`check` prints one line per piece and exits 0 when ready, 1 when something is missing. The ten
rows are the window's, in the order the window needs them: the window binary, the page view, the
archive, the map outlines, the settings, the model file, which model is served, the `llama.cpp`
binary, the port rule, and the start command. Measured in this checkout
(`cd rust && cargo run --offline --locked -p atlas-setup -- check`, 2026-11-22; absolute paths
shortened to the root here):

```text
  window               ok   rust/target/release/atlas-desktop (6.5 MB)
  page view            ok   on macOS the page view uses WebKit, which is part of the system -
                            there is nothing to install, and ATLAS still starts no browser of its own
  archive              ok   data/atlas.db (7.7 MB)
  map outlines         ok   public/vendor/ne_50m_countries.geojson (3.1 MB)
  config               ok   data/config.json: the model server is on port 8081
  model file           ok   models/Qwen3.5-9B-...gguf (6.5 GB, the size this copy of ATLAS expects)
  model served         ok   data/config.json names no model_local, so the file the installer
                            pinned is in force: models/Qwen3.5-9B-...gguf (6.5 GB)
  llama.cpp binary     ok   bin/llama-server (33 kB)
  port                 ok   no port is opened by the window; chat dials port 8081 on this machine
                            only, and only if you start the model server. Nothing listens while the
                            weights are downloaded either: that is one outbound connection a person starts
  start command        ok   run.sh - it opens the window
ready: the window can run on this machine.
```

(The run prints absolute paths, and the `page view`, `model served` and `port` lines wrap; the
ten rows and their order are verbatim, `rust/atlas-setup/src/check.rs:44-58`. The `toolchain`
and `native binaries` rows this document used to quote are gone — a check that answers "can the
window run here?" does not need the compiler — and `page view` is the row that replaced the two
wasm ones: it asks the system web view question on the platform reading it, so macOS prints
WebKit, Windows prints WebView2 and Linux prints the text reader.)

The window is checked as a **file**, never started: that row is
`std::fs::metadata(…).is_file()` on `rust/target/release/atlas-desktop`, so a check cannot open
a GPU surface to answer a question about a checkout. There is no wasm row and no `ui` row
any more: `public/ui/`, `atlas-ui` and `rust/build-ui.sh` are gone, and `check` never mentions
them (`grep -c "public/ui" rust/atlas-setup/src/check.rs` → `0`); the `page view` row above is
what a reader's engine question turned into.

What keeps this honest, and why it is a separate binary:

- **Consent is a subcommand plus `--yes`.** Without `--yes` a step prints exactly what it
  would do — the URL, the size, the sha256, the destination, or the cmake argv — and exits
  3. Nothing large happens because a script was run.
- **The one transfer is pinned twice.** `models/model.json` names the file, the byte count
  and the sha256; the download goes to `<name>.part`, is verified against both, and only then
  is renamed into place. A truncated or swapped file never becomes the model.
- **No crate links it.** `atlas-guard`'s `the_install_tool_is_not_linked_into_the_app` reads
  the manifests, and `atlas start` has no code path to it. The app's no-download audit has
  one written exemption (`rust/atlas-setup/`), counted in the verdict rather than hidden by
  a phrasing that happens not to match.
- **https only, one host per hop, redirects re-checked.** `atlas-guard::urlsafe::check`
  judges every URL before it is opened.

When the file is missing anyway, two things say so and neither guesses: `atlas doctor`
fails **model file** with the path it looked in, and `atlas start` stops at step 5, before any
model server exists, with the same sentence.

The resolution order is: the configured path (`model_local` in `data/config.json`, or
`ATLAS_MODEL`), then `models/<derived model name>`, then the newest other `.gguf` in
`models/` — that last one is why a file copied in under a different name still works, and
the report says so ("found under another name; set model_local to keep it stable").

To fix it: `rust/target/release/atlas-setup model --yes` fetches the pinned file (it prints the
URL, the size and the sha256 first) and installs it under the name the resolution looks for.
Offline, put any `.gguf` in `models/` by hand instead and run `atlas doctor` again. If you
changed the file's name on purpose, set `model_local` so the choice is stable.

A model server that starts and never answers is a different case: the start reports it and
keeps running without it. The archive still answers; chat, research and monitors report that
the model is not answering, and `data/llama.log` says why.

## 5. When a port is held

One port matters now, and it is not ATLAS's: the model server's. ATLAS opens no listening socket
at all, so a held port can never stop the window from opening and the window can never be the
reason one is held.

```text
  ports                warn  something else holds the model server 8081 (it is already in use), so ATLAS opens without its local model
                        -> stop what holds port 8081, or set llama_port in data/config.json to a free port
```

The usual cause is another ATLAS (or another `llama-server`): it holds the model port for as long
as it runs. Check with `lsof -nP -iTCP:8081 -sTCP:LISTEN` (macOS and Linux) or
`Get-NetTCPConnection -LocalPort 8081` (Windows), and stop that instance properly — close its
window or Ctrl-C it, not a kill, so its children go with it.

The model port is never *demanded* before a child is spawned: a `llama-server` already answering
on it is reused, which is what makes a restart cheap. That is also why `ports` is a `warn` and not
a `FAIL` — the start proceeds and the window says the model is not answering.

If you want a second instance, give it its own root, its own data directory and its own model
port in that root's own settings file:

```bash
mkdir -p /srv/atlas2/data && printf '{"llama_port": 8082}\n' > /srv/atlas2/data/config.json
./rust/target/release/atlas start --root /srv/atlas2
```

Settings are validated on the way in: a port has to be between 1024 and 65535, and a value that
fails validation stops the start with the key and where the value came from — the message ends
in `data/config.json:<key>` when the file set it, or `env:ATLAS_<KEY>` when an environment
variable did (`rust/atlas-config/src/spec.rs:413`, `:477`). A key that names a *host* is refused
outright: the model server's address is the loopback constant, not a setting.

A model port taken by something else shows up as a bind failure at the top of
`data/llama.log` (`couldn't bind HTTP server socket, hostname: 127.0.0.1, port: 8081`) and in
the llama log's cleanup line. ATLAS stays up: stop it, free the port or change `llama_port`,
start again.

## 6. When the archive is corrupt

Symptoms: the boot stops with a sentence naming `data/atlas.db` instead of listening, or
`atlas-migrate verify` reports broken findings.

Stop ATLAS first. The WAL and SHM files (`data/atlas.db-wal`, `data/atlas.db-shm`) belong to
the database file; any repair or copy happens with the app stopped.

```bash
sqlite3 -readonly data/atlas.db "PRAGMA integrity_check;"     # "ok" or the first complaints
rust/target/release/atlas-migrate verify                      # from the repository root
rust/target/release/atlas-migrate verify --db data/atlas.db   # from anywhere
```

The default archive path is `data/atlas.db` relative to the directory you run the command in,
and `verify` refuses a path that does not exist rather than reporting on an empty file.

`verify` prints the row counts per table, the state of the FTS index, and a findings list with
a severity per row; its exit code is 0 when the archive is clean and 1 when it has broken
findings, so a script can gate on it. In this checkout it reports 0 broken and 4 notes:
orphaned entities (nothing links them), two place notes where the stored place cannot be
re-derived, and tags whose provenance cannot be told apart. Notes are not damage; they are what
the archive is.

Repairs are explicit and listed before they run:

```bash
rust/target/release/atlas-migrate normalize --db data/atlas.db            # what it would do
rust/target/release/atlas-migrate normalize --db data/atlas.db --apply    # do it
rust/target/release/atlas-migrate carry --db data/atlas.db --apply        # re-derive the model's fields
```

`normalize` touches a stale FTS row, an orphaned link and a wrong publisher, and it is
idempotent: the second run reports zero changes. `carry` re-derives the fields only the local
model may decide, which needs the model server answering.

What not to do: do not edit columns by hand to silence a finding (a tag's provenance comes
from `tag_article`, not from a rewritten column), and do not delete `data/atlas.db` to "fix"
a start failure — that is the archive. If starting empty really is the decision, move the
file aside (`mv data/atlas.db data/atlas.db.broken`) and start again; a fresh empty database
is created, and nothing re-collects by itself.

## 7. Back up `data/`

Two honest options.

**With the app stopped** (simplest, and the only one that is obviously complete):

```bash
# Ctrl-C ATLAS first
dest="backups/atlas-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$dest"
cp -p data/atlas.db data/atlas.db-wal data/atlas.db-shm data/config.json "$dest/"
```

Copy all three database files together or none: a `-wal` left behind belongs to a database
file that is no longer there.

**With the app running**, ask SQLite for a consistent copy; `VACUUM INTO` folds the WAL in
and writes one file:

```bash
mkdir -p data/backups
sqlite3 data/atlas.db "VACUUM INTO 'data/backups/atlas-$(date -u +%Y%m%dT%H%M%SZ).db'"
```

`atlas-store` also has `Store::backup` (destination `data/backups/atlas-<UTC>.db`, an existing
destination is refused, the copy is 0600), but no command exposes it yet, so the statement
above is the operator's route today.

Restore: stop ATLAS, put the files back, keep the modes (`data/` 0700, the database 0600 — a
start re-applies them), start, then `atlas doctor` and `atlas-migrate verify`. The archive in
this checkout is 7.7 MB, so copies are cheap; keep the ones you care about, because nothing
prunes `data/backups/` for you.

## 8. Moving the install

The root is the directory holding `bin/`, `models/`, `public/`, `rust/` and `data/`. Move all
of it together. Nothing inside depends on where it was, except absolute paths you wrote into
`data/config.json`.

1. Stop ATLAS.
2. Move the whole root (or copy it, `bin/llama-server`'s libraries included).
3. Update absolute paths in `data/config.json` if you set any: `model_local` and
   `browser_path` are the two that take a path — and `browser_path` is a leftover: no shipped
   binary starts a browser, so the key is read and nothing opens it. Ports, windows and retention
   are numbers and travel.
4. Start from the new place: `./run.sh` opens the window out of its own directory, and
   `rust/target/release/atlas start --root /new/place` collects out of an explicit one.
   The window needs no `--root`: it finds the checkout from its own executable.
5. Run `atlas doctor` there and check the `data permissions` line.

Things that are already handled: the engine binaries are found next to the `atlas`
executable, with `<root>/rust/target/release` as the fallback, so a moved tree still finds its
engines; the window finds its archive and its map outlines the same way, by walking up to the
nearest `rust/Cargo.toml` (`rust/atlas-desktop/src/assets.rs`); and `public/` holds only
`vendor/` (the map outlines), so there is no bundle to carry. The lock file records the root
of the last run but is only a report, never a requirement.

A move to a different OS or CPU family is a rebuild, not a copy: `cargo build --release
--offline --locked` in the new root, plus a `llama-server` built for that platform in `bin/`.
There is no web bundle to move any more: `public/ui/` and `rust/build-ui.sh` are gone.

## 9. Ports: one listened on by `llama-server`, none opened by ATLAS

**ATLAS opens no listening socket — not the window, not `atlas start`.** There is no port of
ours to find, no URL to open and nothing another process can connect to. The reader's page view
(§1.1) is a child view inside the window's own process and adds no listener either; what it adds is
the page's outbound traffic, which is the browser's, not a port of ours (`docs/THREAT-MODEL.md`
§4.19). Three re-runnable
proofs:

```bash
$ grep -rn "TcpListener\|accept(" rust/atlas-launcher/src   # production code: no line
$ cargo tree -p atlas-launcher --edges normal | grep -E "atlas-(web|browser|ingest|net)"
$ cargo test -p atlas-launcher --test no_listener
```

The first prints lines from `#[cfg(test)]` modules only — a test may hold a loopback socket to
stand in for a model server; production code has none, and
`tests/no_listener.rs::this_crate_never_binds_a_port_at_all` is the mechanical version of that
sentence. The second prints nothing: the closure is five workspace crates —
`atlas-launcher`, `atlas-config`, `atlas-guard`, `atlas-platform`, `atlas-store` — and the
fetcher (`atlas-ingest`) and the HTTP client (`atlas-net`) are none of them. The listener
(`atlas-web`) and the Chrome launcher (`atlas-browser`) are not in the workspace at all any
more, so there is no edge to print. The third pins both facts and the dry-run plan;
`rust/atlas-guard/tests/reachability.rs` is the workspace-wide version of the same rule.

Re-runnable proof — both closures, every dial, every listener hit, the download audit, and what is red on 2026-11-22: [`NO-PORTS.md`](NO-PORTS.md).

| Port | Who opens it | Held for |
| --- | --- | --- |
| 8081 — `llama_port` (8080 by default) | the `llama-server` child, `--host 127.0.0.1` in its own argv | the whole run |
| ephemeral | nothing shipped, any more. A local Chrome over the DevTools protocol used to be `atlas-monitor`'s child and listened on a kernel-chosen port; the pipeline now wires `NullBrowser`, so a page that needs JavaScript is a sentence, not a browser (`docs/NO-PORTS.md` §2.1) | — |
| 8787 — the "web port", 9222 — the "browser port" | **nothing, ever** | — |

The last row is a number the config layer still carries and reports
(`rust/atlas-config/src/spec.rs:90-92`: `app_port` default 8787, `cdp_port` default 9222, with
`ATLAS_PORT` / `ATLAS_CDP_PORT` as their environment names, `:112-114`). Nothing binds either, no check prints them any more,
and editing them changes nothing about what ATLAS opens. `9222` was never used even when the
browser existed: Chrome was started with `--remote-debugging-port=0` and ATLAS read the port it
chose out of `DevToolsActivePort`.

One honest footnote: `atlas-config` asks the kernel whether a port is *free* by binding it and
dropping it in the same statement (`rust/atlas-config/src/ports.rs:64`). That is a question, not
a listener: it never accepts a connection. `atlas doctor`'s own port check no longer binds
anything — it connects, and a refusal is the answer "free".

A held port can never stop the window from opening, and the window can never be the reason a
port is held.
