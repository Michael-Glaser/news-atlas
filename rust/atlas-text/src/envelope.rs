//! `envelope` — one request in, one JSON object out: the angle brackets of every
//! operation in this crate.
//!
//! Decides how large one request may be ([`MAX_INPUT`]), which operation the
//! request's first word names, the one JSON object each operation answers with,
//! and the `{"error": …}` line a refusal prints.
//!
//! [`run`] **is** that whole program, and it is the only place it is written: the
//! binary's `main` reaches it through [`crate::main_entry`], and a caller that
//! links this crate reaches the same function directly (atlas-ingest's
//! `rules::inproc` does, so the 13 text questions one stored article asks cost no
//! processes). The process is a transport, not a second implementation.
//!
//! It decides no string rule: every decision lives in the operation module the
//! word names — `clean`, `tokens`, `title`, `publisher`, `url`. The two numbers
//! this file applies are the crate's own and are never restated by a caller:
//! [`MAX_INPUT`] is built from [`clean::MAX_FIELD`], and `max_words` is clamped to
//! [`tokens::MAX_WORDS`] with [`tokens::DEFAULT_MAX_WORDS`] when the request names
//! none.

use crate::clean::{self, clean_text};
use crate::json;
use crate::publisher::publisher;
use crate::title::clean_title;
use crate::tokens::{self, tokenize};
use crate::url::norm_url;

/// The most one request may carry on its way in: four fields' worth, because a
/// request is one field plus a key and a caller may send the field it is about to
/// clean. It is a request cap, not a field cap — a field is cut by
/// [`clean::MAX_FIELD`], and the two are spelled once, here.
pub const MAX_INPUT: usize = 4 * clean::MAX_FIELD;

/// Answer one request: `raw` is the whole of stdin, `op` is `argv[1]`.
///
/// `Ok(text)` is exactly the line the binary prints, without its newline;
/// `Err(message)` is the refusal, and the message is the words the binary would
/// have escaped into `{"error": …}` (see [`refusal`]). An in-process caller gets
/// the same two things from the same function, which is what makes its answer the
/// binary's answer instead of a lookalike.
pub fn run(op: &str, raw: &str) -> Result<String, String> {
    if raw.len() > MAX_INPUT {
        return Err("input too large".to_string());
    }
    match op {
        "clean_text" => {
            let (text, removed, truncated) = clean_text(&json::get(raw, "text"));
            Ok(format!(
                "{{\"text\":\"{}\",\"removed\":{},\"truncated\":{}}}",
                json::esc(&text),
                removed,
                truncated
            ))
        }
        "tokenize" => {
            let max = json::num(raw, "max_words")
                .unwrap_or(tokens::DEFAULT_MAX_WORDS)
                .clamp(1, tokens::MAX_WORDS);
            let words = tokenize(&json::get(raw, "text"), max);
            let body: Vec<String> = words
                .iter()
                .map(|word| format!("\"{}\"", json::esc(word)))
                .collect();
            Ok(format!("{{\"tokens\":[{}]}}", body.join(",")))
        }
        "clean_title" => {
            let title = clean_title(&json::get(raw, "title"), &json::get(raw, "publisher"));
            Ok(format!("{{\"title\":\"{}\"}}", json::esc(&title)))
        }
        "publisher" => Ok(format!(
            "{{\"publisher\":\"{}\"}}",
            json::esc(&publisher(&json::get(raw, "url")))
        )),
        "norm_url" => Ok(format!(
            "{{\"url\":\"{}\"}}",
            json::esc(&norm_url(&json::get(raw, "url")))
        )),
        "" => Err(
            "usage: atlas-text <clean_text|tokenize|clean_title|publisher|norm_url>".to_string(),
        ),
        other => Err(format!("unknown op: {}", other)),
    }
}

/// The line a refusal prints: `{"error": …}`, the message escaped exactly as an
/// answer field would be.
///
/// One function, so the binary's stdout and the `{"error": …}` an in-process
/// caller assembles from the same `Err` are the same bytes. `atlas-ingest` builds
/// that envelope through `serde_json`; the spelling here is the same one
/// `atlas-lib`'s `json::escape_str` uses, which is why the two agree character for
/// character (`rules::tests::the_text_and_core_envelopes_are_the_binaries` prints
/// both).
pub fn refusal(message: &str) -> String {
    format!("{{\"error\":\"{}\"}}", json::esc(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_answers_one_object() {
        let cases = [
            ("clean_text", r#"{"text":"  a\u0000  b "}"#),
            ("tokenize", r#"{"text":"AI chips and TSMC","max_words":2}"#),
            (
                "clean_title",
                r#"{"title":"Council reviews budget | Outlet 1","publisher":"Outlet 1"}"#,
            ),
            ("publisher", r#"{"url":"https://www.outlet1.example.invalid/story"}"#),
            ("norm_url", r#"{"url":"https://Outlets.Example/Story/?utm_source=x#top"}"#),
        ];
        for (op, raw) in cases {
            let answer = run(op, raw).unwrap_or_else(|error| panic!("{} refused: {}", op, error));
            assert!(
                answer.starts_with('{') && answer.ends_with('}'),
                "{} answered something that is not one object: {}",
                op,
                answer
            );
            assert!(
                !answer.contains('\n'),
                "{} must answer one line, the way the binary prints it: {:?}",
                op,
                answer
            );
        }
        // The answers themselves are the operations' own; this only pins the
        // shape: `clean_text` reports what it removed, `tokenize` its words.
        assert_eq!(
            run("clean_text", r#"{"text":"  a\u0000b "}"#).unwrap(),
            r#"{"text":"ab","removed":1,"truncated":false}"#
        );
        assert_eq!(
            run("tokenize", r#"{"text":"one two three four five","max_words":3}"#).unwrap(),
            r#"{"tokens":["one","two","three"]}"#
        );
    }

    #[test]
    fn the_caps_are_the_crates_own_and_are_applied_here() {
        // `max_words`: absent reads as DEFAULT_MAX_WORDS, and anything outside
        // 1..=MAX_WORDS lands on the bound instead of being obeyed.
        let words = r#"{"text":"one two three four five six seven eight nine ten"}"#;
        let answered: Vec<String> =
            serde_free_tokens(&run("tokenize", words).expect("an answer"));
        assert_eq!(answered.len(), tokens::DEFAULT_MAX_WORDS);
        for (asked, expected) in [(0usize, 1usize), (400, tokens::MAX_WORDS)] {
            let words: String = (0..400).map(|i| format!("w{} ", i)).collect();
            let raw = format!("{{\"text\":\"{}\",\"max_words\":{}}}", words, asked);
            let answered = serde_free_tokens(&run("tokenize", &raw).expect("an answer"));
            assert_eq!(
                answered.len(),
                expected,
                "max_words={} must be clamped to {}",
                asked,
                expected
            );
        }
        // `MAX_INPUT`: one byte past the ceiling is refused before any operation
        // reads the request, and the refusal is the message, not an answer.
        let huge = format!(r#"{{"text":"{}"}}"#, "x".repeat(MAX_INPUT));
        assert_eq!(run("clean_text", &huge).unwrap_err(), "input too large");
        assert_eq!(MAX_INPUT, 4 * clean::MAX_FIELD);
    }

    #[test]
    fn refusals_are_the_two_shapes_the_binary_prints() {
        assert_eq!(
            run("no_such_op", "{}").unwrap_err(),
            "unknown op: no_such_op"
        );
        assert_eq!(
            run("", "{}").unwrap_err(),
            "usage: atlas-text <clean_text|tokenize|clean_title|publisher|norm_url>"
        );
        assert_eq!(
            refusal("unknown op: no_such_op"),
            r#"{"error":"unknown op: no_such_op"}"#
        );
        // The spelling a quote, a backslash and a control character take: the same
        // one `serde_json` (atlas-ingest's envelope) gives, so a refusal assembled
        // on either side of the library call is the same bytes.
        assert_eq!(
            refusal("a \"b\\c\u{1}d"),
            "{\"error\":\"a \\\"b\\\\c\\u0001d\"}"
        );
    }

    /// The words out of a `{"tokens":[…]}` answer, without serde in a crate that
    /// has no dependencies: the answer is this crate's own, flat and quoted.
    fn serde_free_tokens(answer: &str) -> Vec<String> {
        let body = answer
            .trim_start_matches("{\"tokens\":[")
            .trim_end_matches("]}");
        if body.is_empty() {
            return Vec::new();
        }
        body.split("\",\"")
            .map(|word| word.trim_matches('"').to_string())
            .collect()
    }
}
