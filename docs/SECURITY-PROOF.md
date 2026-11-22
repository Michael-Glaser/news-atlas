# ATLAS security proof — the claims, the tests, the commands

This is the adversarial audit of `rust/`: what the app claims about security, the test that
proves each claim, and the command that re-runs it. Every row below is a test that exists in
this tree; where a claim could not be made fully, that is written down in §7 instead of being
implied.

Scope: every crate in `rust/` — `atlas-guard`, `atlas-config`, `atlas-lib`, `atlas-core`,
`atlas-text`, `atlas-places`, `atlas-graph`, `atlas-topics`, `atlas-dates`, `atlas-store`,
`atlas-net`, `atlas-browser`, `atlas-ai`, `atlas-ingest`, `atlas-search`, `atlas-web`,
`atlas-launcher`, `atlas-monitor`, `atlas-platform`, `atlas-ui`, plus the `atlas-e2e` /
`atlas-bench` harnesses. There is no Python in the product and no JavaScript logic: the frontend
is `atlas-ui` compiled to wasm, and the only hand-written script is the three-line
`public/ui/loader.js`.

## 0. Run all of it

```bash
cd rust
cargo build --release --offline --locked             # the whole workspace, nothing fetched

# the proof itself (the crates this audit added tests to)
cargo test --offline --locked -p atlas-guard -p atlas-lib -p atlas-core -p atlas-net \
           -p atlas-ai -p atlas-web -p atlas-ingest -p atlas-browser

# one at a time, if you want to watch a single claim
cargo test --offline -p atlas-guard --test audit_binds        # §1 loopback only
cargo test --offline -p atlas-guard --test audit_gate         # §2 the gate is not bypassed
cargo test --offline -p atlas-ai    --test loopback_endpoint  # §2 the model endpoint
cargo test --offline -p atlas-guard --test audit_downloads    # §3 nothing downloads
cargo test --offline -p atlas-lib   --test fuzz_json          # §4 untrusted input
cargo test --offline -p atlas-core  --test fuzz_extract
cargo test --offline -p atlas-ai    --test fuzz_sse
cargo test --offline -p atlas-ai    --test fuzz_tool
cargo test --offline -p atlas-guard --test fuzz_url
cargo test --offline -p atlas-guard --test audit_logs         # §5 secrets never logged
cargo test --offline -p atlas-web   --lib logline             # §5 the runtime check
cargo test --offline -p atlas-web   --test headers            # §6 CSP and headers
cargo test --offline -p atlas-web   --test bind               # §1 the refusal at run time
cargo test --offline -p atlas-web   --test gate               # §1/§7 the request gate

# the system-level version of §1, §3 and §5's traffic: the whole app, once
cargo run -p atlas-e2e -- run
```

No test in §1–§6 opens a socket, resolves a name, downloads anything or reads a page. The
exceptions are named where they appear: `atlas-net`'s live test is `#[ignore]`d,
`atlas-web/tests/bind.rs` binds `127.0.0.1:0` for a moment, and `atlas-e2e` starts the app on a
loopback port inside a temporary root.

## 1. Loopback only — no listener can be reached from another machine

| Claim | Proved by |
| --- | --- |
| Every listener in the workspace is loopback | `atlas-guard/tests/audit_binds.rs::every_listener_in_this_workspace_is_a_loopback_listener` |
| Every outbound connection is loopback | `::every_outbound_connection_goes_to_a_loopback_address` |
| The one variable bind is the guard itself | `::the_one_variable_bind_is_the_guard_itself` |
| `main.rs` cannot bind on its own | `::the_bind_guard_is_the_one_reachable_through_the_crate_root` |
| No crate holds the wildcard as a value | `::no_wildcard_literal_is_ever_a_bind_target` |
| `0.0.0.0`, `::`, a LAN address and a name are refused at startup | `atlas-web/tests/bind.rs::the_documented_literal_is_the_only_host`, `::the_wildcard_address_is_refused_at_startup`, `::a_lan_address_is_refused_at_startup`, `::a_host_name_is_refused_rather_than_resolved` |
| The bound socket is re-checked after the bind | `::a_bound_ephemeral_listener_is_loopback_and_nothing_else_binds`, `::the_listener_serves_a_router_and_answers_a_loopback_request` |
| The app itself only opened the socket it reports | `atlas-e2e` step `security.listener` + `security.sockets` (`rust/atlas-e2e/src/security.rs::listener`, `::sockets`) |

How the static half works: `atlas-guard/src/source.rs` reads every `rust/*/src/**/*.rs` (tests
excluded by `#[cfg(test)]` and by file name) and the audit test fails on any `TcpListener::bind`
/ `UdpSocket::bind` / `axum::serve` / `TcpStream::connect*` hit that is not in a per-line
allowlist where each entry carries the reason it is loopback. A new listener therefore needs a
decision in review, in the diff.

The allowlist as it stands: one real listener (`atlas-web/src/boot.rs`), and probes only —
`atlas-config`'s port availability check, `atlas-launcher`'s supervision and health probe, the
browser's own loopback connect in `atlas-browser/src/status.rs`, and the `atlas-bench` /
`atlas-e2e` harnesses that bind `127.0.0.1:0` for their own fake model.

```bash
cargo test --offline -p atlas-guard --test audit_binds
cargo test --offline -p atlas-web   --test bind
```

**The test was itself tested.** Adding this to a fresh `atlas-guard/src/planted.rs` makes 4 of
its 5 tests fail, and removing the file makes them pass again:

```rust
fn oops() { let _ = std::net::TcpListener::bind(("0.0.0.0", 1)); }
```

## 2. The gate is never bypassed — a `Gated` URL is the only thing a request is built from

`atlas-guard/src/gated.rs` adds `Gated`: a URL that already holds the gate's answer. Its field
is private, there is no `From`/`Default`, and it has exactly two constructors — `Gated::verify`
(public URL, `urlsafe::check(url, false)`) and `Gated::loopback` (the app's own traffic:
`urlsafe::check(url, true)` **and** `address::check(host)`, because `allow_private` alone would
also accept `10.0.0.5`). A caller outside `atlas-guard` cannot write one.

| Claim | Proved by |
| --- | --- |
| A refusal never produces a token; both doors agree | `atlas-guard/src/gated.rs::every_class_the_gate_refuses_produces_no_gated_value` |
| The loopback door accepts only `127.0.0.0/8` and `::1` | `::the_loopback_gate_accepts_only_loopback`, `::a_public_url_cannot_be_passed_off_as_loopback`, `atlas-guard/tests/fuzz_url.rs` |
| The field stays private, there are exactly two doors | `::the_field_is_private_and_there_are_exactly_two_doors` |
| The request line is built from a verdict | `atlas-guard/tests/audit_gate.rs::the_request_line_is_built_from_a_verdict` |
| `Verdict` hands out a token, not a bare `String` | `::the_verdict_does_not_hand_out_a_bare_string` |
| A page is opened from a verdict (`Page.navigate`) | `::a_page_is_opened_from_a_verdict` |
| The model socket is opened from a verdict | `::the_model_socket_is_opened_from_a_verdict`, `atlas-ai/tests/loopback_endpoint.rs::the_token_names_the_address_that_is_dialled` |
| The name `localhost` is mapped to the literal before it is approved | `atlas-ai/tests/loopback_endpoint.rs::the_name_localhost_is_mapped_to_the_literal_and_then_approved` |
| A base URL that is not this machine yields no endpoint | `atlas-ai/tests/loopback_endpoint.rs::a_base_url_that_is_not_this_machine_yields_no_endpoint` |
| The raw gate has one caller per crate | `::the_raw_gate_has_one_caller` |
| Nobody builds a token by hand | `::nobody_constructs_the_token_by_hand` |

The ported call sites (the one cross-crate change this audit makes):

* `atlas-net` — `Verdict.url` is not a public field; `Verdict::gated()` hands out the token.
  `Client::fetch` turns a `&str` into a token and delegates to `fetch_gated`, and the single
  place a request line is written is `fn exchange(&self, first: Gated, cap: usize)`; every
  redirect hop re-enters through `check_hop`, which returns a `Gated` as well.
* `atlas-browser` — `gate_page`/`check_landing` return `Gated`, and the CDP navigation step
  takes `&Gated`. A page cannot be opened from a string.
* `atlas-ai` — `Endpoint` keeps the `Gated` it was approved with (`Gated::loopback` on the
  literal `Endpoint::addr()` dials), so "loopback only" is one decision asked twice, never a
  rule re-implemented.
* `atlas-ingest`, `atlas-search` — both reach the network only through `atlas_net::Client`
  (proved below: `ureq` exists in exactly one crate), and `atlas-ingest` gained `gate::gated_url`
  for callers that want the token itself.

```bash
cargo test --offline -p atlas-guard --test audit_gate
cargo test --offline -p atlas-ai   --test loopback_endpoint
cargo test --offline -p atlas-net            # the verdict and hop tests
```

## 3. Nothing is downloaded at run time

Two rules, because there are two kinds of file to check. `atlas-config`'s audit walks the tree
over a fixed extension list and reports findings — `AUDIT_EXTENSIONS` is
`.py/.sh/.ps1/.cmd/.bat/.js`, and with the cut-over that covers `run.sh`, `setup.sh`, the
PowerShell wrappers and the two JavaScript files in `public/ui/`; the only Python left in the
tree is under `tools/llama.cpp`, which `AUDIT_SKIP_PATHS` skips whole. The Rust sources are the
other half, and they get a stricter rule:

| Claim | Proved by |
| --- | --- |
| No process is started with an installer (`curl`, `pip`, `git`, …) | `atlas-guard/tests/audit_downloads.rs::no_process_is_ever_started_with_an_installer` |
| No installer phrase is a string the app could run | `::no_installer_phrase_is_a_string_the_app_could_run` |
| There is exactly one HTTP client (`ureq`, in `atlas-net`) | `::there_is_exactly_one_http_client` |
| No second HTTP crate is even named | `::no_other_http_client_is_even_named` |
| The scanner sees a real hit when one exists | `::the_scan_would_notice_a_download_if_one_appeared` |
| The running app agrees: its own audit passed | `atlas-e2e` step `security.boot-audit` (`atlas-e2e/src/security.rs::boot_audit`, reading `/api/status.security.downloads`) |
| The verdict is published, not inferred | `/api/security` → `machine` (the configuration report) and `boot` (the audit that ran at boot) |

The two allowlisted places are `atlas-config/src/download.rs` (the rule table: one predicate per
download shape, which is the opposite of performing one) and `atlas-config/src/spec.rs`'s
default model URL — the URL that the missing-model sentence **prints**, exactly as `setup.sh`
does. Neither is a call site: nothing fetches it, which is what the test checks.

```bash
cargo test --offline -p atlas-guard --test audit_downloads
```

Planted `std::process::Command::new("curl")` in a fresh `src/` file: 2 tests fail.

## 4. Untrusted input — 10 000 mutated inputs per parser, no panic, bounded

`atlas-guard/src/mutate.rs` is the generator: an xorshift64\* stream (same bytes on every
machine), eight mutation operators over **bytes** (so invalid UTF-8 reaches the parser, as a
socket would deliver it) and a table of hostile fragments (nested markup, entities, bidi
overrides, lone NUL, JSON punctuation, an unterminated CDATA). No external crate, no
`catch_unwind`: a panic fails the test.

| Target | Test | What is asserted |
| --- | --- | --- |
| CLI JSON parser (`atlas_lib::json`) | `atlas-lib/tests/fuzz_json.rs` | 10 000 docs: value or a refusal with a reason; output ≤ 8×input+64; the writer's output parses again; a 100 000-deep nesting bomb is refused, not a stack overflow; a 1 MB document parses in linear time |
| HTML extractor (`atlas_core::extract`) | `atlas-core/tests/fuzz_extract.rs` | 10 000 pages: always JSON with `text`/`chars`; output ≤ 8×input+4096; a 1 MB page and a 500 kB single token stay linear |
| SSE parser (`atlas_ai::sse`) | `atlas-ai/tests/fuzz_sse.rs` | 10 000 streams, fed in irregular slices: data out ≤ bytes in, events ≤ in/2+1, no line over `MAX_LINE_BYTES`; an unending line is a `Protocol` error; a stream that stops mid-frame is `truncated` |
| Tool-call parser (`atlas_ai::tool`) | `atlas-ai/tests/fuzz_tool.rs` | 10 000 replies: any call carries a non-empty name and args ≤ 2×reply; `coerce_args` on the real spec table refuses with a reason; the loose reader and the canonical writer agree |
| URL parser (`atlas_guard::urlsafe`, `address`) | `atlas-guard/tests/fuzz_url.rs` | 10 000 URLs, both `allow_private` modes: an allowed URL has scheme/host/port and no reason; a refusal always has a reason and never becomes a `Gated`; a `loopback` answer only for `127.*`/`::1`; a 2×2048 URL is refused before parsing |

Measured when this audit was written (debug profile, Apple silicon):

| Target | Time for 10 000 | Outcome |
| --- | --- | --- |
| JSON documents | 25 ms | 1 673 parsed / 8 327 refused, longest output 112 bytes, worst expansion 1.8×; a 1 MB document parses in 43.8 ms |
| HTML pages | 104 ms | 0 empty, worst expansion 36× (a 17-byte page producing a 628-byte JSON body); a 1.1 MB page costs 144.6 ms |
| SSE streams | 40 ms | 6 437 events, 106 289 bytes of data, 0 refusals |
| Tool replies | 108 ms | 2 716 calls, 121 refused by the argument check, longest args 96 bytes |
| URLs | 136 ms | 3 457 accepted / 16 544 refused (both `allow_private` modes), 1 307 tokens, 529 loopback approvals |

```bash
cargo test --offline -p atlas-lib --test fuzz_json      -- --nocapture
cargo test --offline -p atlas-core --test fuzz_extract  -- --nocapture
cargo test --offline -p atlas-ai --test fuzz_sse        -- --nocapture
cargo test --offline -p atlas-ai --test fuzz_tool       -- --nocapture
cargo test --offline -p atlas-guard --test fuzz_url     -- --nocapture
```

## 5. Secrets — no page text, prompt or token reaches a log

| Claim | Proved by |
| --- | --- |
| `data/app.log` has exactly one writer | `atlas-guard/tests/audit_logs.rs::the_app_log_has_exactly_one_writer` |
| The only caller is the request line, and it logs a method/path/status/ms | `::the_only_caller_is_the_request_line` |
| The writer cannot be bypassed (one redaction decision) | `::the_writer_cannot_be_bypassed` |
| No logging framework exists to call (`log`, `tracing`, `dbg!`, `env_logger`) | `::there_is_no_logging_framework` |
| Console output stays at the argv/stdout boundary | `::console_output_stays_at_the_process_boundary` |
| Every allowlist entry states its reason | `::every_allowlist_entry_says_why` |
| **The runtime check**: a token is replaced, a `\n` cannot start a second line, a line of only a secret is not written, nothing exceeds 400 characters | `atlas-web/src/logline.rs` (7 tests) |
| The file really contains one line after an injection attempt | `atlas-web/src/applog.rs::a_second_line_cannot_be_injected_through_the_path` |

The logging path is `State::log` → `applog::write_line` → `logline::prepare`, which redacts
(`atlas_guard::redact`), drops control bytes, collapses to one line and caps at
`logline::LINE_MAX` (400 characters). A line that is nothing but a secret is dropped instead of
written. `gate::finish` writes exactly one line per request that failed (`status >= 400`) or was
slow (`> 2000 ms`), and never the body.

Measured cost of the logging path (10 000 lines, per line): **3.48 µs** in the release profile,
53.3 µs in the debug profile.

```bash
cargo test --release --offline -p atlas-web --lib logline -- --nocapture
cargo test --offline -p atlas-guard --test audit_logs
cargo test --offline -p atlas-web --lib logline
```

Planted `println!("page text: {}", "x")` in a fresh `src/` file: the console test fails.

## 6. CSP and headers — the policy the wasm frontend actually needs

**A real bug was found and fixed here.** The policy sent `script-src 'self'`, and the frontend
is Rust compiled to wasm: `public/ui/atlas_ui.js` calls `WebAssembly.instantiateStreaming`, which
Chrome and Firefox refuse to compile under a `script-src` without `'wasm-unsafe-eval'`. With the
old policy the page loaded its loader and then failed to start. The token is now sent (and not
`'unsafe-eval'`, which would also allow string eval).

The policy (`atlas-web/src/gate.rs::SECURITY_HEADERS`, delivered enforcing on every response),
and why each source it names is there:

| Directive | Value | Needed by |
| --- | --- | --- |
| `script-src` | `'self' 'wasm-unsafe-eval'` | `/ui/loader.js` (the only script) + `WebAssembly.instantiate` |
| `style-src` | `'self' 'unsafe-inline'` | the local stylesheets in `public/css/` + `style="…"` attributes the UI sets |
| `img-src` | `'self' data:` | the inline-SVG favicon, canvas and tile images under `/ui/tiles` |
| `connect-src` | `'self'` | every `fetch` to `/api/…`, including the `/api/chat` SSE stream |
| `object-src`, `base-uri`, `form-action`, `frame-ancestors`, `media-src` | `'none'` | nothing — that is the point |

The seven headers, in the order they are inserted: `Content-Security-Policy`,
`X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
`Cross-Origin-Resource-Policy: same-origin`, `Cross-Origin-Opener-Policy: same-origin`,
`X-Frame-Options: DENY`, `Permissions-Policy` (geolocation, microphone, camera, usb off).

| Claim | Proved by |
| --- | --- |
| Every response carries all 7 documented headers (200, 404, 405, 413, 403, static) | `atlas-web/tests/headers.rs::every_response_carries_the_documented_headers` |
| A static asset carries them too | `::a_static_asset_carries_them_too` |
| The policy permits what the shell + wasm loader need, and no more | `::the_policy_permits_exactly_what_the_wasm_frontend_needs` |
| No directive allows the whole web (`*`, `'unsafe-eval'`, an origin) | `::no_directive_allows_the_whole_web` |
| The shell references nothing the policy forbids | `::the_shell_needs_nothing_the_policy_forbids` |
| The header set is exactly the documented seven, and every value `http` accepts | `::the_headers_are_the_documented_seven` |

The last one matters: the middleware inserts each header through `HeaderValue::from_str` and
**skips the insert if the value is rejected**, so a typo would drop a header silently. The test
checks each value is accepted, and that each one is present on a real response.

```bash
cargo test --offline -p atlas-web --test headers
```

The fix was verified in the failing direction: removing `'wasm-unsafe-eval'` from the policy
makes `the_policy_permits_exactly_what_the_wasm_frontend_needs` fail with `WebAssembly.instantiate
is blocked without 'wasm-unsafe-eval'`.

## 7. What this audit does **not** prove, and where it found something open

Honest limits first:

* **A source scan is not a type system.** It sees a `TcpListener::bind` on a line; it does not
  see a bind reached through a trait object in another crate. What closes that gap is the
  runtime check (`boot.rs` re-reads the bound socket) and the fact that `atlas-web` owns the only
  listener.
* **Comments are not stripped** by the scanner (a `//` inside `https://` would truncate a third
  of the workspace), so a *comment* that names a primitive counts as a hit. Comment-only lines
  are excluded per test. The failure direction is noise, never a silent pass.
* **`#[cfg(test)]` detection is brace-counted**, so a test module that starts and ends on one
  line takes the rest of that line with it. No file here writes that, and it can only hide code
  already inside the test item (`source.rs` says so in its own test).
* **The DNS-rebinding window is closed on the HTTP path.** This bullet used to say "narrow, not
  closed": `resolve_gate` resolved the name and stopped, and the connector resolved it again.
  `atlas-net/src/resolver.rs` closes it by making the filter the resolver the transport uses — a
  refused answer is dropped, a name whose every answer is refused never connects, and the lookup
  spends the connector's own budget
  (`atlas-net/src/attacks.rs::a_name_that_rebinds_between_the_verdict_and_the_connect_is_refused`
  stages the answer change). **It is open on the browser path**: Chrome resolves for itself, and
  `atlas-browser` does not link `atlas-net`, so a rebinding name is filtered only before the
  navigation and on the URL the tab reports.
* **Third-party processes are out of reach.** `llama-server` and Chrome make their own
  connections; what is proved here is that *our* code hands them a loopback address and nothing
  else.

### Findings, as they stand now

1. **RESOLVED — the publisher/host list is gone.** This section used to report a hardcoded
   `BLOCKED_HOSTS`/`SEARCH_HOSTS` list in `atlas-ingest/src/gate.rs` deciding what is *not* news.
   The file now says, and the code shows, that there is **no host list**: `is_article_url` is the
   only rule, and it is structural (a site root, a file, a search-results page), so no publisher
   is privileged and none is banned — `atlas-ingest`'s own
   `gate::tests::no_host_is_banned_by_name` asserts that for `wikipedia.org`, `facebook.com`,
   `x.com`, `youtube.com` and `tiktok.com`. `is_blocked_host` survives as a deprecated alias for
   `!is_article_url`, kept for the call sites that still use the old name (and for the stale
   comments that still call it a host rule). Whether a page is reporting is the local model's
   verdict.
2. **PARTLY RESOLVED — `data/llama.log` receives the model server's stdout and stderr.**
   `atlas-launcher`'s spawn still redirects both to that file, and the invocation now passes
   `--log-disable` **whenever the binary's `--help` advertises it** — which the `bin/llama-server`
   this checkout builds does (`--log-disable   Log disable`). The code is
   `atlas-launcher/src/llama.rs::invocation`, and `docs/THREAT-MODEL.md` §4.9 carries the same
   statement. What remains open is the build whose `--help` probe failed: `help` is then `""`, no
   flag is passed, and a build that logs at its default verbosity writes the prompt of every turn
   into a `0600` file that any backup or sync client copies.
   `atlas-launcher/src/llama.rs::the_model_server_cannot_be_told_to_log_a_prompt` is the test that
   names this, `#[ignore]`d with the trade spelled out: pass the flag unconditionally and a build
   that does not know it cannot start at all; keep it conditional and the diagnostics the file
   exists for have to go. Closing it is the crate owner's decision, not this audit's.
3. **OPEN — `MAX_URL_CHARS` exists more than once.** 2 048 is `atlas-guard/src/urlsafe.rs`'s
   private `MAX_URL_CHARS` (the decision), with public copies in `atlas-net/src/gate.rs`,
   `atlas-ingest/src/gate.rs` and `atlas-browser/src/extract.rs` — four numbers that must move
   together, which is a drift waiting to happen; the guard's is the one that decides.
   `atlas-guard/tests/fuzz_url.rs` writes the value out with a comment instead of reaching into a
   private constant.
4. **RESOLVED — `atlas-browser` was one 3 540-line file.** It is now `lib.rs` (233 lines) plus
   `spawn.rs`, `lifecycle.rs`, `cdp.rs`, `commands.rs`, `open.rs`, `page.rs`, `scripts.rs`,
   `status.rs`, `find.rs`, `wire.rs`, `tests.rs`. The `Gated` port survived the split, which is
   what `audit_gate.rs` now pins.
5. **RESOLVED — `atlas-graph`'s row-parity test is gone.** It asserted that the deleted
   `server/graph.py` was a file, so on a machine with a Python interpreter it panicked instead of
   skipping. `rust/atlas-graph/src/db_tests.rs` no longer names Python at all, which is what makes
   a workspace-wide `cargo test --offline --workspace` meaningful again. The parity it proved was
   proved during the migration; keeping a test that needs the old tree was the leftover.
6. **RESOLVED — `atlas-config`'s audit reads `.rs`.** `AUDIT_EXTENSIONS` is now
   `.py/.sh/.ps1/.cmd/.bat/.js/.rs` (seven entries), so the download rule's walk includes the Rust
   sources and not only the scripts. `audit_downloads.rs` keeps its own stricter pass (it also
   refuses a second HTTP client): one rule, two readers, and the `.rs` half is now in both.

## 8. Files this audit added or changed

Added:

* `rust/atlas-guard/src/gated.rs` — the `Gated` newtype and its 5 tests.
* `rust/atlas-guard/src/mutate.rs` — the deterministic mutation generator, 5 tests.
* `rust/atlas-guard/src/source.rs` — "what counts as shipped code", 4 tests.
* `rust/atlas-guard/tests/audit_binds.rs` — 5 tests (loopback only).
* `rust/atlas-guard/tests/audit_gate.rs` — 6 tests (no bypass).
* `rust/atlas-guard/tests/audit_downloads.rs` — 5 tests (nothing downloads).
* `rust/atlas-guard/tests/audit_logs.rs` — 6 tests (secrets).
* `rust/atlas-guard/tests/fuzz_url.rs` — 3 tests (10 000 URLs).
* `rust/atlas-lib/tests/fuzz_json.rs` — 3 tests (10 000 documents).
* `rust/atlas-core/tests/fuzz_extract.rs` — 3 tests (10 000 pages).
* `rust/atlas-ai/tests/fuzz_sse.rs` — 3 tests (10 000 streams).
* `rust/atlas-ai/tests/fuzz_tool.rs` — 3 tests (10 000 replies).
* `rust/atlas-ai/tests/loopback_endpoint.rs` — 3 tests.
* `rust/atlas-web/src/logline.rs` — the log-line decision, 7 tests.
* `rust/atlas-web/tests/headers.rs` — 6 tests (CSP against the wasm frontend).

Changed:

* `rust/atlas-net/src/{gate,hop,fetch}.rs` — `Verdict::gated()`, private `url`,
  `check_hop → Gated`, `exchange(Gated)`, `fetch_gated`.
* `rust/atlas-browser/src/open.rs` — `gate_page`, `check_landing`, `navigate` take/return `Gated`.
* `rust/atlas-ai/src/endpoint.rs` — `Endpoint` keeps the `Gated` it was approved with.
* `rust/atlas-ingest/src/gate.rs` — `verdict.url()`, `gated_url`.
* `rust/atlas-guard/src/lib.rs` — the three new modules and `pub use Gated`.
* `rust/atlas-web/src/{lib,applog}.rs` — `logline` wired into the one writer.
* `rust/atlas-web/src/gate.rs` — the CSP now carries `'wasm-unsafe-eval'`.
* `rust/atlas-lib/Cargo.toml`, `rust/atlas-core/Cargo.toml` — a `[dev-dependencies]` entry on
  `atlas-guard` (test-only, for the mutation generator; `atlas-guard` has no dependencies of its
  own, so the shipped crates are unchanged).

## 9. State of the suite

The audit's own run recorded **566 passing tests across the nine crates it touched or leant on**
(`atlas-guard` 96, `atlas-lib` 5, `atlas-core` 26, `atlas-net` 26, `atlas-ai` 96, `atlas-web`
122, `atlas-ingest` 112, `atlas-browser` 39, `atlas-search` 44), with the `#[ignore]`d tests
being the ones that need the network or a built binary.

Those numbers move: every crate in the workspace has gained tests since, and the declaration
count in the same nine crates was 614 when this file was last edited (counted with
`grep -rc '#\[test\]\|#\[tokio::test\]' rust/<crate>/src rust/<crate>/tests`; the nine were
`atlas-guard` 97, `atlas-lib` 5, `atlas-core` 26, `atlas-net` 27, `atlas-ai` 103, `atlas-web` 156,
`atlas-ingest` 116, `atlas-browser` 40, `atlas-search` 44). The number that matters is not the
total but that no row in §1–§6 lost its test: a claim that stops being true makes a named test
fail, and the fix is a decision, not a comment.

`cargo test --offline --workspace` runs the whole tree, not just those nine, and it is green.
