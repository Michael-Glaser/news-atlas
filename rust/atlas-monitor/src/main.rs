//! `atlas-monitor` — topic monitoring, twice: one-shot on the workspace's stdin/stdout
//! contract, and always-on as the agent `atlas start` supervises.
//!
//! ```text
//! atlas-monitor init      <<< '{}'
//! atlas-monitor list      <<< '{}'
//! atlas-monitor add       <<< '{"name":"Chips","query":"chip export rules","threshold":6,"cadence_minutes":120}'
//! atlas-monitor run       <<< '{"id":1,"reason":"button"}'      (drives one research pass)
//! atlas-monitor run_all   <<< '{"reason":"check-all"}'
//! atlas-monitor tick      <<< '{"run":false}'                  (who is due, at this instant)
//! atlas-monitor alerts    <<< '{"hours":24,"limit":30}'
//! atlas-monitor runs      <<< '{"id":1,"limit":20}'
//! atlas-monitor status    <<< '{}'
//! atlas-monitor serve                                          (the always-on agent)
//! ```
//!
//! The one-shot ops print one JSON object on stdout and exit 0; failure prints
//! `{"error": "…"}` and exits 1 — never a panic, never a partial answer, like every other
//! ATLAS binary (`atlas_lib::cli`).
//!
//! `serve` is the exception to "one call, one answer": it is a loop, it runs until
//! `SIGTERM`/`SIGINT`, and it prints **one JSON line per action** — the same JSON-object
//! discipline, one object per line, so a reader can `jq` the stream and the launcher can
//! leave it unparsed. Its exit code is 0 even when actions failed: each failure is a line
//! and a `finish`d row, and a stop is not an error.
//!
//! Nothing here loops, sleeps or schedules anything by itself except `serve`: `tick`
//! answers for the instant it is called, and `run` performs exactly one check.

#![forbid(unsafe_code)]

fn main() {
    match std::env::args().nth(1).as_deref() {
        // The agent: argv decides before stdin is touched, because `serve` has no stdin
        // (the launcher spawns it with `/dev/null`) and `atlas_lib::cli` would read it.
        Some(atlas_monitor::serve::SERVE) => atlas_monitor::serve::main_entry(),
        _ => atlas_lib::cli::serve(atlas_monitor::wire::run),
    }
}
