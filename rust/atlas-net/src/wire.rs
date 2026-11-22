//! What the `atlas-net` binary answers on stdout, one subcommand at a time.
//!
//! The decision this file makes is the wire shape: `gate` answers a verdict
//! object, `fetch` answers the page (or, for a refusal or a transport failure, its
//! reason alone as `{"refused": …}` / `{"error": …}`), and anything else is bad
//! input with exit 1. A refused URL and a timed-out page both exit 0 — both are
//! facts, not broken invocations.
//!
//! It does not: decide what a URL may do, or perform a request. It parses argv's
//! subcommand, reads the JSON object on stdin and calls the library.

use atlas_lib::json::Json;
use serde_json::json;

/// Run the binary: argv, stdin, stdout, exit code.
pub fn serve() {
    atlas_lib::cli::serve(&run);
}

/// One subcommand, one JSON answer.
pub fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input = atlas_lib::cli::input_object(raw)?;
    match cmd {
        "gate" => {
            let url = need_str(&input, "url")?;
            Ok(crate::gate(&url).to_json().to_string())
        }
        "fetch" => {
            let url = need_str(&input, "url")?;
            let max_bytes = input
                .get("max_bytes")
                .and_then(Json::as_usize)
                .unwrap_or(0);
            let client = crate::Client::new();
            let page = client.fetch(&url, max_bytes);
            Ok(wire(&page))
        }
        other => Err(format!("unknown subcommand: {}", other)),
    }
}

/// A refused or failed fetch answers with its reason alone; a page answers with
/// the page. Nothing here is logged, only returned.
fn wire(page: &crate::Page) -> String {
    if let Some(reason) = page.refused.as_deref() {
        return json!({ "refused": reason }).to_string();
    }
    if let Some(reason) = page.error.as_deref() {
        return json!({ "error": reason }).to_string();
    }
    match serde_json::to_string(page) {
        Ok(text) => text,
        Err(_) => json!({ "error": "cannot serialise the answer" }).to_string(),
    }
}

fn need_str(obj: &Json, key: &str) -> Result<String, String> {
    match obj.get(key).and_then(Json::as_str) {
        Some(value) => Ok(value.to_string()),
        None => Err(format!("missing or non-string \"{}\"", key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_url_is_bad_input() {
        assert!(run("gate", "{}").is_err());
        assert!(run("fetch", r#"{"url": 7}"#).is_err());
        assert!(run("nonsense", "{}").is_err());
    }

    #[test]
    fn gate_refusal_is_a_verdict_not_an_error() {
        let answer = run("gate", r#"{"url":"http://127.0.0.1:8787/api/status"}"#).unwrap();
        assert!(answer.contains("\"ok\":false"), "{}", answer);
        assert!(answer.contains("\"source\":\"rust\""), "{}", answer);
    }

    #[test]
    fn fetch_refuses_without_a_request() {
        let answer = run(
            "fetch",
            r#"{"url":"http://169.254.169.254/latest/meta-data/","max_bytes":600000}"#,
        )
        .unwrap();
        assert!(answer.starts_with("{\"refused\":"), "{}", answer);
    }

    #[test]
    fn gate_accepts_a_public_url() {
        let answer = run("gate", r#"{"url":"https://example.com/a"}"#).unwrap();
        assert!(answer.contains("\"ok\":true"), "{}", answer);
        assert!(answer.contains("\"host\":\"example.com\""), "{}", answer);
    }
}
