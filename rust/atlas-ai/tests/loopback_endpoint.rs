//! The model endpoint carries the gate's token, at run time.
//!
//! `atlas-ai` never asks the network for anything but the local model, and the
//! endpoint it dials is approved once by `atlas-guard`: `Gated::loopback` on the
//! literal `['Endpoint::addr']` returns. This test pins the two halves together —
//! the token names the same address that is dialled, and a base URL that is not
//! this machine produces no endpoint at all, so there is nothing to dial.

use atlas_ai::Endpoint;

#[test]
fn the_token_names_the_address_that_is_dialled() {
    for base in [
        "http://127.0.0.1:8080",
        "http://127.0.0.1:8080/llm/",
        "http://[::1]:8080",
    ] {
        let endpoint = Endpoint::parse(base).expect("a loopback base URL");
        let token = endpoint.gated();
        assert!(token.is_loopback(), "{}: the token is not a loopback approval", base);
        let dialled = endpoint.addr();
        assert!(
            token.as_str().contains(&dialled.ip().to_string()),
            "{}: the token {:?} does not name the dialled address {}",
            base,
            token.as_str(),
            dialled.ip()
        );
        assert_eq!(token.as_str().split(':').last(), Some(dialled.port().to_string().as_str()));
    }
}

#[test]
fn a_base_url_that_is_not_this_machine_yields_no_endpoint() {
    for base in [
        "http://10.0.0.5:8080",
        "http://192.168.1.20:8080",
        "http://example.com:8080",
        "http://169.254.169.254:80",
        "https://127.0.0.1:8080",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://user:pw@127.0.0.1:8080",
        "127.0.0.1:8080",
    ] {
        let error = Endpoint::parse(base).expect_err("must be refused before a socket exists");
        let sentence = error.to_string();
        assert!(!sentence.trim().is_empty(), "{}: a refusal needs a sentence", base);
    }
}

#[test]
fn the_name_localhost_is_mapped_to_the_literal_and_then_approved() {
    // The one name that is allowed, because it is mapped to `127.0.0.1` without
    // asking the resolver — so the token the gate issues is the literal.
    let endpoint = Endpoint::parse("http://localhost:8090").expect("the loopback name");
    assert_eq!(endpoint.addr().to_string(), "127.0.0.1:8090");
    assert!(endpoint.gated().as_str().contains("127.0.0.1"));
    assert!(endpoint.gated().is_loopback());
}
