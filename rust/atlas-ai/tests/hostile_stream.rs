//! One hostile stream, measured where its memory is decided.
//!
//! Two caps, both about what a *stream* may make this crate hold, and both tested
//! through the public API because that is where the streams arrive:
//!
//! * [`atlas_ai::chat::Aggregator`] keeps one slot per `tool_calls[].index`, so the
//!   8-call cap has to be applied while the fragments are collected — the loop's
//!   `tool::turn_of_counted` only ever sees the list that is already there.
//! * [`atlas_ai::tool::PendingCall::key`] is the repeat-detector's identity, held for
//!   `REPEAT_WINDOW` steps of the whole run: a digest and a length, never the
//!   model's argument text.

use std::collections::BTreeSet;

use atlas_ai::chat::{Aggregator, DeltaCall, Frame};
use atlas_ai::tool::{CallSource, PendingCall, MAX_CALLS_PER_TURN};

/// One `tool_calls[]` fragment for `index`, as a frame.
fn call_fragment(index: usize) -> Frame {
    Frame::Delta {
        content: String::new(),
        thinking: String::new(),
        calls: vec![DeltaCall {
            index,
            id: Some(format!("call_{}", index)),
            name: Some("search_news".to_string()),
            arguments: Some("{\"query\":\"chips\"}".to_string()),
        }],
        finish_reason: None,
    }
}

#[test]
fn a_stream_naming_a_thousand_call_slots_keeps_eight() {
    // Measured before the cap: 1 000 slots, one per frame, collected for a turn that
    // may make 8 calls. The loop's cap ran afterwards, on the finished list.
    let mut aggregator = Aggregator::new();
    for index in 0..1_000 {
        aggregator.push(call_fragment(index));
    }
    let reply = aggregator.finish("scripted");
    assert_eq!(
        reply.calls.len(),
        MAX_CALLS_PER_TURN,
        "the cap belongs where the fragments are collected, not after them"
    );
    // The slots kept are the first ones the stream named, in its own order.
    assert_eq!(reply.calls[0].id, "call_0");
    assert_eq!(reply.calls[MAX_CALLS_PER_TURN - 1].id, "call_7");
    // And the loop's own cap still refuses whatever the aggregator kept, so the two
    // caps agree: no call this list carries can be the ninth.
    let names: BTreeSet<&str> = reply.calls.iter().map(|call| call.id.as_str()).collect();
    assert_eq!(names.len(), MAX_CALLS_PER_TURN);
}

#[test]
fn fragments_of_a_kept_call_still_merge() {
    // The cap must not become a per-slot limit: llama.cpp sends a call in pieces —
    // the id once, then the name, then the argument text in fragments.
    let mut aggregator = Aggregator::new();
    aggregator.push(Frame::Delta {
            thinking: String::new(),
        content: "Let me look. ".to_string(),
        calls: vec![DeltaCall {
            index: 0,
            id: Some("call_0".to_string()),
            name: Some("search_news".to_string()),
            arguments: Some(String::new()),
        }],
        finish_reason: None,
    });
    for piece in ["{\"query\":", "\"chips\"", "}"] {
        aggregator.push(Frame::Delta {
            thinking: String::new(),
            content: String::new(),
            calls: vec![DeltaCall {
                index: 0,
                id: None,
                name: None,
                arguments: Some(piece.to_string()),
            }],
            finish_reason: None,
        });
    }
    let reply = aggregator.finish("scripted");
    assert_eq!(reply.content, "Let me look. ");
    assert_eq!(reply.calls.len(), 1);
    assert_eq!(reply.calls[0].name, "search_news");
    assert_eq!(reply.calls[0].arguments, "{\"query\":\"chips\"}");
}

#[test]
fn a_call_key_is_a_digest_not_a_megabyte_of_arguments() {
    // The brake holds `REPEAT_WINDOW` steps of keys for the whole run to answer one
    // question — "have I seen this call?" — and a key was the name plus the raw
    // canonicalised argument text: measured, 8 × 1 MiB = 8.4 MB of key.
    let huge = "x".repeat(1024 * 1024);
    let arguments = format!("{{\"query\":\"{}\"}}", huge);
    let key = PendingCall::new("call_0", "search_news", arguments.clone(), CallSource::Native).key();
    assert!(
        key.len() <= 128,
        "a repeat-detector key is a digest plus a length, not the arguments: {} bytes",
        key.len()
    );

    // Same call, same key — the id is not part of the identity.
    let again = PendingCall::new("call_9", "search_news", arguments, CallSource::Native).key();
    assert_eq!(key, again);

    // Different arguments, different key — even at the same length.
    let other = PendingCall::new(
        "call_0",
        "search_news",
        format!("{{\"query\":\"{}\"}}", "y".repeat(1024 * 1024)),
        CallSource::Native,
    )
    .key();
    assert_ne!(key, other, "a digest that collided here would hide a real call");

    // One call, two spellings, one key: whitespace is not identity.
    let small = PendingCall::new("c", "search_news", "{\"query\":\"chips\"}", CallSource::Native);
    let spaced = PendingCall::new(
        "c",
        "search_news",
        "{ \"query\" : \"chips\" }",
        CallSource::Native,
    );
    assert_eq!(small.key(), spaced.key());
    assert_ne!(small.key(), other);
    // A different tool with the same arguments is a different call.
    let named = PendingCall::new("c", "stats", "{\"query\":\"chips\"}", CallSource::Native);
    assert_ne!(small.key(), named.key());
}
