//! The gate is never bypassed — proved on the types and on the call sites.
//!
//! `atlas_guard::Gated` is the structural half: a URL that already holds the
//! gate's answer. It has a private field, two constructors that both call
//! `urlsafe::check`, and no `From`/`Default` impl — so the only way to hold one is
//! to have asked. These tests check that the crates which actually reach the
//! network ask for it:
//!
//! | Crate | What it must hold | Test |
//! | --- | --- | --- |
//! | `atlas-net` | a `Gated` before a request line is written | [`the_request_line_is_built_from_a_verdict`] |
//! | `atlas-ai` | a `Gated` on the loopback literal it dials | [`the_model_socket_is_opened_from_a_verdict`] |
//! | all | the raw gate is called in one place | [`the_raw_gate_has_one_caller`] |
//! | all | nobody builds a `Gated` by hand | [`nobody_constructs_the_token_by_hand`] |
//!
//! The `atlas-browser` row went with the CDP page reader (deleted with the web face
//! in this task): there is no page to navigate, so there is no call site to gate.

use atlas_guard::source::{dead_copy, find, find_wrapped, shipped_code, SourceFile};
use std::path::Path;

fn workspace() -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let files = shipped_code(&root);
    assert!(files.len() > 50, "the scanner read only {} files", files.len());
    files
}

/// The function a line sits in, by walking the code upwards.
fn enclosing_function(file: &SourceFile, line: usize) -> (String, String) {
    let lines: Vec<&str> = file.code.lines().collect();
    let mut index = line.min(lines.len());
    let mut signature = String::new();
    while index > 0 {
        index -= 1;
        let text = lines[index].trim_start();
        if text.starts_with("fn ")
            || text.starts_with("pub fn ")
            || text.starts_with("pub(crate) fn ")
            || text.starts_with("async fn ")
            || text.starts_with("pub async fn ")
        {
            // The signature can wrap; take this line and the next three.
            signature = lines[index..(index + 4).min(lines.len())].join(" ");
            let name = text.split("fn ").nth(1).unwrap_or(text);
            return (name.split('(').next().unwrap_or(name).trim().to_string(), signature);
        }
    }
    (String::new(), signature)
}

/// True when the line carries no code — asked of the scanner, which knows a block
/// comment from a dereference. A local "does the trimmed line start with `*`?"
/// version of this lived here, and `*slot = …` read as a doc-comment continuation:
/// the same bypass this file now shares one answer for with the other audits.
fn is_comment(file: &SourceFile, line: usize) -> bool {
    file.is_comment(line)
}

#[test]
fn the_request_line_is_built_from_a_verdict() {
    let files = workspace();
    // A copy nothing compiles is not a second request line: `atlas-net/src/fetch
    // 2.rs` is a sync conflict's leftover, and `audit_logs.rs`'s drift rule is
    // what keeps it unreachable.
    let hits: Vec<_> = find(&files, ".get(current.as_str())")
        .into_iter()
        .filter(|(path, _, _, _)| !dead_copy(&files, path))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "the one place a request line is written moved or multiplied: {:?}",
        hits.iter().map(|(path, line, _, _)| (path.clone(), *line)).collect::<Vec<_>>()
    );
    let (path, line, _, _) = &hits[0];
    assert_eq!(path, "atlas-net/src/fetch.rs", "a request line outside atlas-net");
    let file = files.iter().find(|f| &f.path == path).expect("the file");
    let (function, signature) = enclosing_function(file, *line);
    assert_eq!(function, "exchange", "the request line moved out of `exchange`");
    assert!(
        signature.contains("Gated"),
        "`exchange` no longer takes a Gated: a raw string could reach the socket — {}",
        signature
    );
    // A second request line elsewhere would be a second path; `audit_downloads`
    // proves `ureq` appears in `atlas-net` and nowhere else, so one call site in
    // one file is the whole surface.
    let all = find(&files, "current.as_str()");
    for (path, _, _, text) in &all {
        assert!(
            path.starts_with("atlas-net/"),
            "{}: a request line outside atlas-net — {}",
            path,
            text
        );
    }
}

#[test]
fn the_verdict_does_not_hand_out_a_bare_string() {
    let files = workspace();
    let gate = files
        .iter()
        .find(|f| f.path == "atlas-net/src/gate.rs")
        .expect("atlas-net/src/gate.rs");
    // The canonical URL is a private field with two accessors, one of which is
    // the token. A `pub url: String` would let a caller skip the token.
    //
    // The scan reads the `Verdict` struct body rather than the whole file: a
    // private helper struct inside `atlas-net` (a canonicalised request line,
    // for instance) may legitimately have public fields, and the claim is about
    // what a *caller* of the gate receives. Reading the whole file made this
    // fail on an internal field that a caller cannot reach.
    let struct_at = gate
        .code
        .find("pub struct Verdict {")
        .expect("the Verdict struct");
    let body = &gate.code[struct_at..];
    let end = body.find("\n}").expect("the struct's end");
    for line in body[..end].lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        assert!(
            !trimmed.starts_with("pub url:"),
            "`Verdict.url` is public again: a caller can build a request from it"
        );
    }
    assert!(gate.code.contains("pub fn url(&self)"), "no accessor for the url");
    assert!(
        gate.code.contains("pub fn gated(&self)"),
        "no accessor for the token"
    );
    assert!(
        gate.code.contains("gated: Option<Gated>"),
        "the verdict no longer carries the token it approved"
    );
    // A refusal never carries one.
    let refused = gate
        .code
        .split("fn refused(")
        .nth(1)
        .expect("Verdict::refused");
    let body = &refused[..refused.find('}').unwrap_or(refused.len())];
    assert!(
        body.contains("gated: None"),
        "a refused verdict can carry a token: {}",
        body
    );
}

// `a_page_is_opened_from_a_verdict` was here. It pinned that `atlas-browser` built
// its `Page.navigate` / `Page.navigate`-adjacent commands from a `Gated`, and that
// `atlas-browser/src/open.rs::gate_page` returned the token rather than a `String`.
// The CDP page reader was deleted with the web face (this task), so the rule has no
// call site left: `find("Page.navigate")` returns nothing, and an assertion that a
// removed crate gates a removed call would only fail. The invariant it protected —
// a network door is opened from a verdict, never from a raw string — is still enforced
// for the two doors that remain, by `the_request_line_is_built_from_a_verdict`
// (atlas-net) and `the_model_socket_is_opened_from_a_verdict` (atlas-ai).

#[test]
fn the_model_socket_is_opened_from_a_verdict() {
    let files = workspace();
    let endpoint = files
        .iter()
        .find(|f| f.path == "atlas-ai/src/endpoint.rs")
        .expect("atlas-ai/src/endpoint.rs");
    assert!(
        endpoint.code.contains("atlas_guard::Gated::loopback("),
        "the model endpoint no longer asks the gate for a loopback token"
    );
    assert!(
        endpoint.code.contains("gated: atlas_guard::Gated")
            || endpoint.code.contains("gated: Gated"),
        "the endpoint does not keep the token"
    );
    // The socket is dialled from `Endpoint::addr()`, which is built from the ip
    // the gate approved — never from a string.
    let http1 = files
        .iter()
        .find(|f| f.path == "atlas-ai/src/http1.rs")
        .or_else(|| files.iter().find(|f| f.path == "atlas-ai/src/transport.rs"))
        .expect("the model transport");
    assert!(
        http1.code.contains("endpoint.addr()"),
        "the transport no longer dials the endpoint's own address"
    );
}

#[test]
fn the_raw_gate_has_one_caller() {
    let files = workspace();
    let mut callers: Vec<(String, usize)> = Vec::new();
    // Wrapped matching: `urlsafe::\n check(` is the same call, and a copy nothing
    // compiles (`x 2.rs`) is not a caller at all.
    for (path, line, _, _) in find_wrapped(&files, "urlsafe::check(", 2) {
        let file = files.iter().find(|f| f.path == path).expect("the file");
        if is_comment(file, line) || dead_copy(&files, &path) {
            continue;
        }
        callers.push((path, line));
    }
    assert!(!callers.is_empty(), "the gate is never called — the scan is blind");
    for (path, line) in &callers {
        let allowed = path.starts_with("atlas-guard/") || path == "atlas-net/src/gate.rs";
        assert!(
            allowed,
            "{}:{} calls the raw gate outside atlas-guard/atlas-net; use \
             atlas_guard::Gated::verify (a public URL) or atlas_net::gate (the request line)",
            path, line
        );
    }
}

#[test]
fn nobody_constructs_the_token_by_hand() {
    let files = workspace();
    for (path, line, _, text) in find_wrapped(&files, "Ok(Gated {", 2) {
        if dead_copy(&files, &path) {
            continue;
        }
        assert_eq!(
            path, "atlas-guard/src/gated.rs",
            "{}:{} builds a Gated outside the guard: {}",
            path, line, text
        );
    }
    // The two doors, and no other: `verify` for a public URL, `loopback` for the
    // app's own traffic.
    let gated = files
        .iter()
        .find(|f| f.path == "atlas-guard/src/gated.rs")
        .expect("atlas-guard/src/gated.rs");
    assert_eq!(
        gated.code.matches("-> Result<Gated, String>").count(),
        2,
        "the number of ways to obtain a Gated changed"
    );
    // The crates that reach the network. `atlas-browser` left this list with the CDP
    // page reader (deleted with the web face in this task), and `atlas-web` was never
    // on it — it was the listener, with nothing to gate, covered by `audit_binds`.
    for crate_name in ["atlas-net", "atlas-ai", "atlas-ingest"] {
        let uses = files
            .iter()
            .filter(|f| f.path.starts_with(&format!("{}/", crate_name)))
            .any(|f| f.code.contains("Gated"));
        assert!(uses, "{} does not mention the token at all", crate_name);
    }
}

// ---------------------------------------------------------------------------
// planted regressions: each one fails if its rule is reverted
// ---------------------------------------------------------------------------

fn planted(path: &str, code: &str) -> Vec<SourceFile> {
    vec![SourceFile {
        path: path.to_string(),
        code: code.to_string(),
    }]
}

/// The raw-gate rule, over a synthetic corpus.
fn raw_gate_callers(files: &[SourceFile]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (path, line, _, _) in find_wrapped(files, "urlsafe::check(", 2) {
        let file = files.iter().find(|f| f.path == path).expect("the file");
        if is_comment(file, line) || dead_copy(files, &path) {
            continue;
        }
        out.push((path, line));
    }
    out
}

#[test]
fn a_dereference_line_cannot_call_the_raw_gate_unseen() {
    // Before: a line whose trimmed text began with `*` was skipped as a doc
    // comment, so `*verdict = urlsafe::check(url, false)` was not a caller.
    let files = planted(
        "atlas-monitor/src/alert.rs",
        "fn judge(url: &str) {\n    *verdict = atlas_guard::urlsafe::check(url, false);\n}\n",
    );
    let callers = raw_gate_callers(&files);
    assert_eq!(callers.len(), 1, "the dereference hid the raw gate");
    assert_eq!(callers[0].0, "atlas-monitor/src/alert.rs");
}

#[test]
fn a_raw_gate_split_across_lines_is_still_a_caller() {
    let files = planted(
        "atlas-monitor/src/alert.rs",
        "fn judge(url: &str) {\n    let gate = atlas_guard::urlsafe\n        ::check(url, false);\n    gate\n}\n",
    );
    assert_eq!(
        raw_gate_callers(&files).len(),
        1,
        "a wrapped call was not a caller"
    );
}

#[test]
fn prose_about_the_gate_is_not_a_caller() {
    let files = planted(
        "atlas-monitor/src/alert.rs",
        "/// never call `urlsafe::check` here; hold a Gated instead\nfn judge() {}\n",
    );
    assert!(
        raw_gate_callers(&files).is_empty(),
        "a doc comment was read as a caller"
    );
}
