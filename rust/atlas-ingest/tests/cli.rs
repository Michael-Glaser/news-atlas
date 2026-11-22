//! The wire contract of `atlas-ingest`, tested through the real binary.
//!
//! `server/ingestcore.py` runs `atlas-ingest <op>` with one JSON object on stdin
//! and parses one JSON object from stdout. That is the whole contract, and it is
//! the same contract `atlas-guard`, `atlas-text` and every other ATLAS binary
//! keeps: success is exit 0 with an answer, failure is exit 1 with
//! `{"error": "…"}` — never a panic, never a stack trace, never partial output.
//!
//! These tests use the binary `cargo test` just built
//! (`CARGO_BIN_EXE_atlas-ingest`), so a bridge that "looks right" cannot pass
//! while the binary answers something else.

use std::io::Write;
use std::process::{Command, Stdio};

fn run(args: &[&str], stdin: &str) -> (i32, String) {
    let exe = env!("CARGO_BIN_EXE_atlas-ingest");
    let root = std::env::temp_dir().join("atlas-ingest-cli");
    let mut child = Command::new(exe)
        .args(args)
        .env("ATLAS_ROOT", &root)
        .env_remove("ATLAS_SEARCH_ENGINE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary must start");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for the binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not JSON: {:?}", text))
}

#[test]
fn an_unknown_subcommand_is_a_json_error_and_exit_one() {
    let (code, out) = run(&["no_such_op"], "{}");
    assert_eq!(code, 1, "a refused call must exit 1");
    let answer = json(&out);
    let message = answer.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("unknown subcommand"), "{}", message);
    // the message names the tools that do exist, so a caller can correct itself
    assert!(message.contains("research"), "{}", message);
    assert!(message.contains("find_news"), "{}", message);
}

#[test]
fn input_that_is_not_an_object_is_refused_before_the_pipeline() {
    for bad in ["[1,2,3]", "\"text\"", "not json at all"] {
        let (code, out) = run(&["status"], bad);
        assert_eq!(code, 1, "{:?} must be refused", bad);
        let answer = json(&out);
        assert!(
            answer.get("error").and_then(|v| v.as_str()).is_some(),
            "{}",
            out
        );
    }
}

#[test]
fn status_answers_with_the_engines_and_nothing_from_a_page() {
    let (code, out) = run(&["status"], "");
    assert_eq!(code, 0, "{}", out);
    let answer = json(&out);
    for key in ["required", "present", "missing", "native", "cache_dir", "search_engine"] {
        assert!(answer.get(key).is_some(), "status has no {}", key);
    }
    // loopback only, and no page text anywhere in the answer
    assert_eq!(answer["loopback_only"], serde_json::json!(true));
    let text = out.to_string();
    assert!(!text.contains("<html"), "status must never carry page text");
    assert!(!text.contains("http://"), "status must never carry a page URL");
}
