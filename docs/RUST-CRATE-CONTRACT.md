# Crate contract — porting one Python module to one Rust binary

Read this fully before writing code. It is the same for every crate, so the
bridges, tests and fallbacks behave identically everywhere.

## Shape

```
rust/Cargo.toml            workspace, already lists your crate — DO NOT EDIT
rust/atlas-lib/            shared, std-only: atlas_lib::json, atlas_lib::cli
rust/<crate>/Cargo.toml    already created — DO NOT EDIT
rust/<crate>/src/main.rs   YOUR FILE
server/<name>core.py       YOUR FILE (the bridge)
```

* No third-party crates. Ever. `std` plus `atlas-lib` only. There is no cargo
  registry cache on this machine, so a dependency cannot even build — and the
  product rule is that the app never downloads anything.
* `#![forbid(unsafe_code)]` at the top of `main.rs`.
* No threads, no network, no filesystem writes, no `process::Command`, no
  environment reads beyond nothing at all. Read stdin, compute, write stdout.

## Wire format

```
atlas-<crate> <subcommand>   <<< '{"…": …}'
```

* Exactly one JSON object on stdout, then a newline, exit code 0.
* On any failure: `{"error": "short reason"}` and exit 1. Use
  `atlas_lib::cli::fail`.
* `main` is:

```rust
fn main() {
    atlas_lib::cli::serve(&run);
}

fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input = atlas_lib::cli::input_object(raw)?;
    match cmd {
        "thing" => thing(&input),
        other => Err(format!("unknown subcommand: {}", other)),
    }
}
```

* `run` is **public**, and it is exactly what `main` calls:
  `pub fn run(cmd: &str, raw: &str) -> Result<String, String>`. It is the crate's
  whole program — the op dispatch, the caps it applies (how many words, how large
  a request) and the shape of every answer and refusal — so another crate links it
  instead of starting the binary. One program, two transports. A crate that keeps
  its envelope private forces its callers to write a second one, which is the one
  thing this contract forbids; `atlas-text` (`src/envelope.rs`) and `atlas-core`
  (`src/entry.rs`) expose theirs the same way, `atlas-text` without depending on
  `atlas-lib` because it is std-only.
* Build the output with `atlas_lib::json::escape_str` and manual `String`
  pushes, exactly like `rust/atlas-core/src/main.rs`. Never hand-format a
  float: use `json::fmt_score` for scores and `{:.6}` for coordinates.
* Every string that came from user text must go through `escape_str`. A quote
  or a newline in an article title must never produce invalid JSON.

## Fidelity

* Same inputs → same outputs as the Python module, including edge cases. If the
  Python is subtly wrong, port the behaviour first and note the bug in a
  `// BUG:` comment; do not silently change semantics.
* Performance: this path only wins if it is fast. No `O(n²)` scan over the body
  text, no regex engine hand-rolled into a backtracking disaster. Single pass,
  byte/char indices, `Vec<u8>`/`&str` slicing, early exits. Aim for < 5 ms on a
  20 kB article.
* Memory: no unbounded caches, no `static mut`, no leaking `String`s.

## Tests

* `#[cfg(test)] mod tests` in `main.rs`. Port the cases from the matching
  Python checker (`tools/geo_check.py`, `tools/graph_check.py`, …) as `#[test]`
  functions. Every subcommand needs at least: a happy path, a missing-key path,
  and a nasty-input path (empty string, non-ASCII, emoji, 10 kB single line).
* `cargo test --offline -p <crate>` must be green, and
  `cargo build --release --offline` must be green, before you report back.

## The Python bridge (`server/<name>core.py`)

Copy the structure of `server/textcore.py` and `server/rustcore.py`:

* Resolve the binary at `rust/target/release/<crate>`; if it is missing, report
  `{"source": "python-fallback", "available": false}` and let the caller use the
  Python implementation.
* 3 s timeout (kill the child on timeout), 3 consecutive failures → breaker
  open for 60 s, then retry.
* Validate the returned JSON: it must be an object, the expected keys must be
  there and be the right type. Anything else counts as a failure, not a value.
* Never raise into the caller. Never log article text.
* Track counters (`calls`, `failures`, `timeouts`, `ms`, `p95_ms`) and expose
  `status()` like `textcore.status()` so `/api/status` can show it.
* The public function must have the *same name and signature* as the Python
  original plus a `source=` marker, so wiring is a one-line change:

```python
def locate(title, summary="", text="", url="", publisher=""):
    got = placescore.call("locate", {...})   # None when Rust is unavailable
    if got is None:
        return _locate_python(...)
    return got
```

## Hard rules that apply to this crate too

* Never touch unrelated projects, never kill a process, never bind a
  port. This crate does not do I/O at all.
* Never download anything, never add a dependency, never call out to the
  network.
* No hardcoded news-word filters. Geo reference data (city names, coordinates,
  country codes, outlet→country) is fine and expected — that is not a news
  filter. Publisher/source word lists for *classification* are forbidden.
* Monochrome, English-facing strings only where they reach the UI.

## Report back

Short. `crate`, `subcommands`, `tests passed/total`, `cargo build` result, the
exact one-line wiring change for the shared Python file (which you do NOT edit),
and anything you had to leave out.
