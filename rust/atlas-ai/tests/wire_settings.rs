//! What one request carries, and what a failure says — at the public boundary.
//!
//! Three properties the rest of the app depends on and cannot check from inside its
//! own crate:
//!
//! * a refusal from llama-server keeps its HTTP status, because the reader is told
//!   "HTTP 500: <the server's own sentence>" and not "the pages were too long";
//! * the two sampling numbers on the wire are inside the documented range whatever
//!   the caller computed, and a temperature is the number it asked for
//!   (`0.3`, never `0.30000001192092896`);
//! * a builder chain keeps what it was given — `with_max_body` then `timeout` does
//!   not lose the body cap.
//!
//! No socket is opened here: every case is built from types.

use std::time::Duration;

use serde_json::Value;

use atlas_ai::chat::{ChatMessage, ChatRequest, HttpLlamaClient};
use atlas_ai::error::{looks_like_context_overflow, status_error};

fn body_of(temperature: f32, max_tokens: u32) -> Value {
    let mut request = ChatRequest::new("local", vec![ChatMessage::user("hi")]);
    request.temperature = temperature;
    request.max_tokens = max_tokens;
    request.body()
}

#[test]
fn a_500_from_the_model_server_is_an_error_that_names_the_status() {
    let error = status_error(
        500,
        r#"{"error":{"message":"failed to decode the batch","type":"server_error"}}"#,
    );
    assert_eq!(error.kind(), "http", "a server fault is not a context overflow");
    let human = error.human();
    assert!(human.contains("HTTP 500"), "{}", human);
    assert!(human.contains("failed to decode the batch"), "{}", human);
    assert!(!error.repairable(), "the model cannot fix a broken server");
    assert!(!error.is_transient(), "a 500 is not retried on the socket");
}

#[test]
fn a_status_is_never_dropped_for_a_word_in_the_body() {
    // The defect this pins: a 500 whose text said "context" arrived as a context
    // overflow, which hides the status and makes the agent retry with half the
    // budget while the server is broken.
    for status in [500u16, 502, 503] {
        assert!(
            !looks_like_context_overflow(status, "the context handler failed"),
            "status {} must stay an HTTP error",
            status
        );
        assert_eq!(status_error(status, "context shift failed").kind(), "http");
    }
    // The recovery the loop was built on still works where it belongs.
    assert_eq!(
        status_error(400, "the request exceeds the available context size").kind(),
        "context"
    );
}

#[test]
fn the_sampling_numbers_are_bounded_on_the_wire() {
    let cases = [
        (f32::NAN, 0u32),
        (f32::INFINITY, u32::MAX),
        (-4.0, 0),
        (99.0, 1_000_000),
        (0.25, 2048),
        (0.0, 8192),
        (0.0, 1_000_000),
    ];
    for (temperature, max_tokens) in cases {
        let body = body_of(temperature, max_tokens);
        let sent = body["temperature"]
            .as_f64()
            .unwrap_or_else(|| panic!("temperature must be a number, got {}", body["temperature"]));
        assert!(
            (0.0..=2.0).contains(&sent),
            "temperature {} escaped the bounds",
            sent
        );
        let tokens = body["max_tokens"]
            .as_u64()
            .unwrap_or_else(|| panic!("max_tokens must be a number"));
        // The ceiling moved with the window: 32 768 tokens of context, half of which the
        // reply may claim (`sampling::MAX_TOKENS`), so the clamp is checked against that
        // constant rather than a number copied here.
        assert!(
            (1..=atlas_ai::sampling::MAX_TOKENS as u64).contains(&tokens),
            "max_tokens {} escaped the bounds (1..={})",
            tokens,
            atlas_ai::sampling::MAX_TOKENS
        );
    }
}

#[test]
fn a_temperature_is_the_number_the_caller_asked_for() {
    for (asked, expected) in [
        (0.3f32, "0.3"),
        (0.1, "0.1"),
        (0.2, "0.2"),
        (0.35, "0.35"),
        (0.25, "0.25"),
        (0.0, "0.0"),
    ] {
        let body = body_of(asked, 100).to_string();
        assert!(
            body.contains(&format!("\"temperature\":{}", expected)),
            "asked for {} got {}",
            asked,
            body
        );
    }
}

#[test]
fn a_builder_chain_keeps_what_it_was_given() {
    let client = HttpLlamaClient::new("http://127.0.0.1:8080", "local", Duration::from_secs(600))
        .expect("a loopback endpoint")
        .with_max_body(4096)
        .with_retries(9)
        .timeout(Duration::from_secs(30));
    assert_eq!(
        client.http().max_body(),
        4096,
        "a later .timeout() must not reset the body cap"
    );
    assert_eq!(
        client.http().retries(),
        atlas_ai::http1::MAX_RETRIES,
        "retries are clamped to the documented maximum"
    );
    assert_eq!(client.http().timeout(), Duration::from_secs(30));
    // And the clamp survives a second pass through the chain.
    let rebuilt = client.with_retries(255);
    assert_eq!(rebuilt.http().retries(), atlas_ai::http1::MAX_RETRIES);
    assert_eq!(rebuilt.http().max_body(), 4096);
}

#[test]
fn a_request_is_never_addressed_to_a_port_this_crate_invented() {
    // Not a network call: `Endpoint::parse` refuses before a socket exists, which is
    // the property the whole client rests on.
    assert!(HttpLlamaClient::new("http://127.0.0.1", "local", Duration::from_secs(1)).is_err());
    assert!(HttpLlamaClient::new("http://example.com:8080", "local", Duration::from_secs(1)).is_err());
}
