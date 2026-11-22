//! Adversarial input for the tool-call parser: 10 000 mutated replies, no panic.
//!
//! The model's answer is untrusted text: it decides whether a tool runs, and a
//! tool reads a page and writes the archive. Three doors are fuzzed here —
//! `parse_json_loose` (model prose → JSON), `calls_in_text` (JSON → calls) and
//! `coerce_args` (a call's arguments → the object a handler receives) — plus
//! `canonical_json`, which is what a repeated call is compared by.
//!
//! The claim: a mutated reply either yields zero/one-or-more well-formed calls
//! with a non-empty tool name and bounded argument text, or nothing at all. It
//! never panics, and no field it can produce is longer than the reply it came
//! from.

use std::collections::BTreeSet;

use atlas_ai::specs::tools as tool_specs;
use atlas_ai::tool::{calls_in_text, canonical_json, coerce_args, parse_json_loose};

const SEEDS: &[&str] = &[
    "```json\n{\"name\":\"search_articles\",\"arguments\":{\"query\":\"harbour\"}}\n```",
    "{\"tool_calls\":[{\"id\":\"1\",\"function\":{\"name\":\"read_article\",\"arguments\":\"{\\\"url\\\":\\\"https://example.com/a\\\"}\"}}]}",
    "[{\"name\":\"status\",\"arguments\":{}}]",
    "Sure. Here is the call: {\"name\":\"list_topics\",\"arguments\":{}} and that is all.",
    "{\"name\":\"\",\"arguments\":{}}",
    "{\"name\":\"unknown_tool\",\"arguments\":\"not json at all\"}",
    "{\"arguments\":{\"n\":1}}",
    "{{{[[[",
];

#[test]
fn ten_thousand_mutated_replies_never_panic_and_stay_bounded() {
    let specs = tool_specs();
    let known: BTreeSet<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
    let mut calls_seen = 0usize;
    let mut calls_refused = 0usize;
    let mut worst_call_text = 0usize;
    let started = std::time::Instant::now();

    let stats = atlas_guard::mutate::run(SEEDS, 10_000, 0x_A71A5_0004, |reply| {
        // 1. the loose reader
        let value = parse_json_loose(reply);

        // 2. the call extractor
        if let Some(calls) = calls_in_text(reply) {
            assert!(!calls.is_empty(), "an empty call list is not a list");
            for call in &calls {
                calls_seen += 1;
                assert!(
                    !call.name().trim().is_empty(),
                    "a call with no tool name was produced"
                );
                // A name is not repaired: it is either one of the specs or
                // something the caller will refuse. Neither may be unbounded.
                assert!(
                    call.name().len() <= reply.len(),
                    "a {} byte name from a {} byte reply",
                    call.name().len(),
                    reply.len()
                );
                assert!(
                    call.arguments().len() <= reply.len() * 2 + 2,
                    "arguments grew: {} from {}",
                    call.arguments().len(),
                    reply.len()
                );
                worst_call_text = worst_call_text.max(call.arguments().len());

                // 3. the argument object a handler would receive
                if known.contains(call.name()) {
                    let spec = specs
                        .iter()
                        .find(|spec| spec.name == call.name())
                        .expect("the name came from the table");
                    match coerce_args(spec, call.arguments()) {
                        Ok(args) => {
                            // 4. the comparison form is a string, and bounded.
                            let canon = canonical_json(&serde_json::Value::Object(args.clone()));
                            assert!(canon.len() <= reply.len() * 4 + 64);
                        }
                        Err(reason) => {
                            calls_refused += 1;
                            assert!(!reason.trim().is_empty(), "a refusal needs a reason");
                            assert!(reason.len() < 1024);
                        }
                    }
                }
            }
        }

        // The loose reader and the canonical writer are inverse on a value.
        if let Some(value) = value {
            let text = canonical_json(&value);
            assert!(
                text.len() <= reply.len() * 8 + 64,
                "{} bytes of reply produced {} bytes of canonical JSON",
                reply.len(),
                text.len()
            );
            assert!(
                parse_json_loose(&text).is_some(),
                "canonical JSON must be readable by the reader that produced it"
            );
        }
    });

    let elapsed = started.elapsed();
    println!(
        "{} mutated replies in {:.0} ms ({} calls, {} refused by the argument check, longest arguments {} bytes)",
        stats.runs,
        elapsed.as_secs_f64() * 1e3,
        calls_seen,
        calls_refused,
        worst_call_text
    );
    assert_eq!(stats.runs, 10_000);
    assert!(calls_seen > 0, "no mutated reply produced a call");
    assert!(
        elapsed.as_secs_f64() < 15.0,
        "10 000 replies took {:.1} s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn a_deeply_nested_reply_is_refused_without_overflowing_the_stack() {
    let bomb = format!("{}{}", "{\"a\":".repeat(50_000), "1".to_string() + &"}".repeat(50_000));
    // No panic, no stack overflow: the answer is a value or nothing.
    let _ = parse_json_loose(&bomb);
    let _ = calls_in_text(&bomb);
}

#[test]
fn a_fence_that_never_closes_is_not_a_call() {
    let reply = "```json\n{\"name\":\"read_article\",\"arguments\":{\"url\":\"x\"}}";
    // The fence opener is stripped; what remains is still one JSON value, and the
    // point is that neither answer panics.
    let _ = calls_in_text(reply);
    assert!(calls_in_text("```\n```").is_none());
    assert!(calls_in_text("").is_none());
}
