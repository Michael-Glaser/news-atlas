//! The process contract: one JSON object on stdout, an exit code, and no panic.
//!
//! These run the real binary, so they catch what a unit test cannot: a `main`
//! that prints two objects, a missing subcommand that panics, an engine that is
//! not built (which must be a stated reason, not a crash), and an unknown
//! subcommand that exits 0 by accident.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the binary with one payload. Returns (exit code, stdout).
fn run(op: &str, payload: &str) -> (i32, String) {
    let exe = env!("CARGO_BIN_EXE_atlas-search");
    let mut child = Command::new(exe)
        .arg(op)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary starts");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("the payload is written");
    let out = child.wait_with_output().expect("the binary exits");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (out.status.code().unwrap_or(-1), stdout)
}

/// Exactly one JSON object, one line, nothing else.
fn one_object(stdout: &str) -> serde_json::Value {
    let trimmed = stdout.trim_end_matches('\n');
    assert!(!trimmed.contains('\n'), "more than one line: {:?}", trimmed);
    serde_json::from_str(trimmed).unwrap_or_else(|error| {
        panic!("not one JSON object ({}): {:?}", error, trimmed);
    })
}

#[test]
fn an_unknown_subcommand_is_an_error_object_with_exit_1() {
    let (code, stdout) = run("crawl_everything", "{}");
    assert_eq!(code, 1, "{:?}", stdout);
    let answer = one_object(&stdout);
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("unknown subcommand"),
        "{:?}",
        answer
    );
}

#[test]
fn input_that_is_not_one_object_is_refused_without_a_panic() {
    for payload in ["", "[]", "not json", "{\"query\": 42}"] {
        let (code, stdout) = run("web_search", payload);
        let answer = one_object(&stdout);
        if code == 0 {
            // `{"query": 42}` is legal JSON with a missing string field: the tool
            // answers "query required" rather than a wire error.
            assert!(answer["error"].as_str().is_some(), "{:?}", answer);
        } else {
            assert_eq!(code, 1, "{:?}", stdout);
            assert!(answer["error"].as_str().is_some(), "{:?}", answer);
        }
    }
}

#[test]
fn a_search_with_no_engine_reachable_still_answers_one_object() {
    // The rule engines are found next to the binary; when the workspace is built
    // they are there and this answer carries results or an error. Either way the
    // process contract holds, and the "nothing is built" case names the command.
    let (code, stdout) = run("web_search", "{\"query\":\"chip export rules\",\"max_results\":3}");
    let answer = one_object(&stdout);
    if code != 0 {
        let error = answer["error"].as_str().unwrap_or("");
        assert!(
            error.contains("not built") || error.contains("cargo build"),
            "{:?}",
            answer
        );
    } else if let Some(error) = answer["error"].as_str() {
        // No network in a test run: the engine cannot answer, which is a reason.
        assert!(
            error.contains("did not answer") || error.contains("query required"),
            "{:?}",
            answer
        );
    }
}

#[test]
fn a_template_that_is_not_a_url_is_refused_before_any_request() {
    let (code, stdout) = run(
        "web_search",
        "{\"query\":\"chip\",\"web\":\"http://127.0.0.1:8092/?q={query}\"}",
    );
    let answer = one_object(&stdout);
    assert_eq!(code, 1, "{:?}", stdout);
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("may open"),
        "{:?}",
        answer
    );
}
