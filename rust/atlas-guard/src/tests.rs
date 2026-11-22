//! `tests` — the requests that cross a module boundary.
//!
//! A test lives here only when it needs more than one module at once: the
//! dispatch and several subcommands in a single assertion. A test that
//! exercises one module sits next to that module's code, in that file.
//!
//! This module is only compiled for tests (`mod tests` in `lib.rs` is
//! `#[cfg(test)]`), so everything here is test code. `json_of` is the one helper
//! the per-module test modules share.

use crate::entry::run;
use crate::json::{self, Json};

/// Parse a request literal, failing the test (not the crate) on bad JSON.
pub(crate) fn json_of(input: &str) -> Json {
    json::parse(input).expect("valid json")
}

#[test]
fn run_rejects_bad_requests() {
    assert!(run("url", "not json").is_err());
    assert!(run("url", "[]").is_err());
    assert!(run("url", "{}").is_err(), "missing url field");
    assert!(run("clean_text", "{\"text\":5}").is_err(), "not a string");
    assert!(run("nope", "{}").is_err(), "unknown subcommand");
}

#[test]
fn the_subcommand_list_the_caller_reads_matches_what_runs() {
    let subs = ["url", "clean_text", "sanitize_html", "token", "redact", "address"];
    // The error a bare invocation prints is the only list of subcommands a
    // person or a script ever reads; `address` was missing from it while it
    // answered requests, so the list said the listener audit did not exist.
    let entry = include_str!("entry.rs");
    let line = entry
        .lines()
        .skip_while(|line| !line.contains("missing subcommand"))
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(line.contains("missing subcommand"), "the list moved");
    for cmd in subs {
        let dispatched = run(cmd, "{}");
        assert!(
            dispatched
                .as_ref()
                .err()
                .map(|e| !e.contains("unknown subcommand"))
                .unwrap_or(true),
            "{} is not dispatched",
            cmd
        );
        assert!(line.contains(cmd), "the subcommand list omits {}", cmd);
    }
}

