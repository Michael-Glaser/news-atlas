//! Cross-module tests: one fetch, over a real socket on loopback.
//!
//! Everything else lives next to the code it tests. These need the gate, the hop
//! rule, the client, the budget and the page at once, and they need a server that
//! misbehaves — a truncated body, a redirect to a metadata address, a redirect
//! loop, a site that goes quiet — so they use [`crate::server`], which listens on
//! `127.0.0.1:0` inside this test process.
//!
//! Why the loopback door and not a plain URL: the public gate refuses every
//! loopback address on purpose, so a test could not reach a local server with
//! `Client::fetch` — and going around the gate to make a test pass would be testing
//! a client that does not ship. `Gated::loopback` is the guard's own door for the
//! app's local traffic, and the transport is exercised through it unchanged.
//!
//! One test in this file guards all of that: `the_public_door_still_refuses_this_machine`
//! proves the entry point a URL from a feed goes through did not inherit the door.

use super::*;
use crate::server::{Reply, Server};
use std::time::{Duration, Instant};

/// A client that cannot wait twenty seconds in a unit test.
fn quick(total: u64, connect: u64) -> Client {
    Client::with_limits(
        HARD_MAX_BYTES,
        Duration::from_millis(total),
        Duration::from_millis(connect),
    )
}

/// One test touches the process environment, so it holds this lock: `std::env` is
#[test]
fn a_gated_page_is_read_over_the_wire_and_carries_no_referer() {
    let body = "<html><head><title>t</title></head><body><p>story</p></body></html>";
    let server = Server::always(Reply::typed(
        200,
        "text/html; charset=utf-8",
        body.as_bytes().to_vec(),
    ));
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/a?b=1"), DEFAULT_MAX_BYTES);

    assert!(page.refused.is_none(), "{:?}", page.refused);
    assert!(page.error.is_none(), "{:?}", page.error);
    assert_eq!(page.status, 200);
    assert_eq!(page.final_url, server.url("/a?b=1"));
    assert_eq!(page.content_type, "text/html; charset=utf-8");
    assert_eq!(page.charset, "utf-8");
    assert_eq!(page.text, body);
    assert_eq!(page.bytes, body.len());
    assert!(!page.truncated);
    assert_eq!(page.redirects, 0);

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), "GET");
    assert_eq!(requests[0].path(), "/a?b=1");
    // The one header that identifies us to a publisher, and the two that keep a
    // request cheap and explainable.
    assert_eq!(requests[0].header("user-agent"), client.user_agent());
    assert_eq!(requests[0].header("accept-encoding"), "identity");
    // A direct request has no referrer: the page was not reached from anywhere.
    assert_eq!(requests[0].header("referer"), "");
}

#[test]
fn one_client_reuses_one_connection_for_two_pages() {
    // The client is built once per process so the second request to the same host
    // reuses the pooled connection. The per-request timeout config added for the
    // whole-fetch budget is the kind of change that silently kills pooling, so the
    // server counts connections as well as requests.
    let server = Server::always(Reply::html("<html><body>pooled</body></html>").keep_alive());
    let client = Client::new();

    for _ in 0..2 {
        let page = client.fetch_gated(&server.token("/a"), DEFAULT_MAX_BYTES);
        assert_eq!(page.status, 200, "{:?}", page.error);
        assert!(page.text.contains("pooled"));
    }

    assert_eq!(server.count(), 2, "both requests arrived");
    assert_eq!(
        server.connections(),
        1,
        "the second request opened a new connection instead of reusing the pooled one"
    );
}

#[test]
fn a_body_over_the_cap_is_cut_and_says_so() {
    let body = vec![b'x'; 200_000];
    let server = Server::always(Reply::typed(200, "text/html", body));
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/big"), 1_000);

    assert_eq!(page.status, 200);
    assert_eq!(page.bytes, 1_000, "the cap is a byte count, not a hope");
    assert!(page.truncated);
    assert_eq!(page.text.len(), 1_000);
    // The cap is also a promise to the caller: `Client::with_cap` cannot be raised
    // by asking for more bytes.
    let capped = Client::with_cap(100);
    let page = capped.fetch_gated(&server.token("/big"), HARD_MAX_BYTES);
    assert_eq!(page.bytes, 100);
    assert!(page.truncated);
}

#[test]
fn a_redirect_is_followed_only_after_the_gate_approves_the_hop() {
    // A page on this machine redirecting to a metadata address, a file, and a
    // service port: three refusals, and not one of them may be requested.
    for (location, needle) in [
        ("http://169.254.169.254/latest/meta-data/", "private"),
        ("file:///etc/passwd", "scheme"),
        ("http://127.0.0.1:22/", "service port"),
    ] {
        let location = location.to_string();
        let server = Server::scripted(move |index, _| {
            if index == 0 {
                Reply::redirect(&location)
            } else {
                Reply::html("<html>should never be read</html>")
            }
        });
        let client = Client::new();
        let page = client.fetch_gated(&server.token("/a"), DEFAULT_MAX_BYTES);

        // A refusal, not a transport failure: the URL must never be fetched again.
        let reason = page.refused.clone().expect("a refusal with a reason");
        assert!(reason.contains("redirect refused at hop 1 of 5"), "{}", reason);
        assert!(
            reason.to_lowercase().contains(needle),
            "{} did not say why ({})",
            reason,
            needle
        );
        assert!(page.error.is_none(), "{:?}", page.error);
        assert_eq!(page.status, 0);
        assert!(page.text.is_empty());
        // The proof that the hop was gated *before* it was requested.
        assert_eq!(server.count(), 1, "the refused hop was requested anyway");
    }
}

#[test]
fn a_redirect_loop_is_bounded_and_says_so() {
    let server = Server::always(Reply::redirect("/again"));
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/loop"), DEFAULT_MAX_BYTES);

    let reason = page.refused.clone().expect("a refusal with a reason");
    assert!(reason.contains("more than 5 redirects"), "{}", reason);
    // Five hops were followed, so six requests were made: the request that started
    // the chain plus five. A seventh would mean the bound is off by one.
    assert_eq!(server.count(), MAX_REDIRECTS + 1, "{}", server.count());
    assert!(page.error.is_none());
}

#[test]
fn a_relative_redirect_lands_on_the_absolute_url() {
    let server = Server::scripted(|index, _| match index {
        0 => Reply::redirect("/b?x=1"),
        _ => Reply::html("<html><body>landed</body></html>"),
    });
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/a"), DEFAULT_MAX_BYTES);

    assert!(page.refused.is_none(), "{:?}", page.refused);
    assert!(page.error.is_none(), "{:?}", page.error);
    assert_eq!(page.status, 200);
    assert_eq!(page.final_url, server.url("/b?x=1"));
    assert_eq!(page.redirects, 1);
    assert!(page.text.contains("landed"));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    // A same-site hop says where it came from, as a browser would.
    assert_eq!(requests[1].header("referer"), server.url("/a"));
}

#[test]
fn a_hop_to_another_site_carries_no_referer() {
    let target = Server::always(Reply::html("<html><body>elsewhere</body></html>"));
    let url = target.url("/x?token=secret");
    let source = Server::scripted(move |_, _| Reply::redirect(&url));
    let client = Client::new();

    let page = client.fetch_gated(&source.token("/a?token=secret"), DEFAULT_MAX_BYTES);

    assert_eq!(page.status, 200, "{:?}", page.refused);
    assert_eq!(page.final_url, target.url("/x?token=secret"));
    let landed = target.requests();
    assert_eq!(landed.len(), 1);
    // The query string of the page we were asked to read is not a stranger's
    // business, so the hop that leaves the site carries no referer at all.
    assert_eq!(landed[0].header("referer"), "");
}

#[test]
fn a_silent_site_is_cut_by_the_read_timeout() {
    let server = Server::always(Reply::silent());
    // Connection setup is instant here; only the wait for the answer can expire.
    let client = quick(200, 5_000);

    let start = Instant::now();
    let page = client.fetch_gated(&server.token("/quiet"), DEFAULT_MAX_BYTES);
    let elapsed = start.elapsed();

    let error = page.error.clone().expect("a transport failure with a reason");
    assert!(error.contains("timed out"), "{}", error);
    assert!(error.contains("200 ms"), "the message must name the budget: {}", error);
    assert!(page.refused.is_none(), "a timeout is not a refusal");
    assert_eq!(page.status, 0);
    // The clock is a promise: the fetch cannot take longer than the budget plus
    // the time it takes to give up on a socket.
    assert!(elapsed < Duration::from_millis(1_500), "{:?}", elapsed);
    assert_eq!(server.count(), 1);
}

#[test]
fn one_budget_covers_every_hop_of_a_fetch() {
    // Two hops that are each well inside the budget and together are outside it.
    // Each hop used to get a fresh global timeout — six hops meant six budgets.
    let server = Server::scripted(|index, _| match index {
        0 => Reply::redirect("/b").after(Duration::from_millis(150)),
        _ => Reply::html("<html><body>late</body></html>").after(Duration::from_millis(150)),
    });
    let client = quick(250, 5_000);

    let start = Instant::now();
    let page = client.fetch_gated(&server.token("/a"), DEFAULT_MAX_BYTES);
    let elapsed = start.elapsed();

    let error = page.error.clone().expect("the fetch ran out of time");
    assert!(
        error.contains("timed out") || error.contains("out of time"),
        "{}",
        error
    );
    // The sentence names the *whole fetch*, so a reader can tell this from a site
    // that was merely slow to answer.
    assert!(error.contains("250 ms in all, redirects included"), "{}", error);
    assert!(
        elapsed < Duration::from_millis(1_500),
        "one fetch took {:?} for a 250 ms budget",
        elapsed
    );
    assert_eq!(page.status, 0);
}

#[test]
fn an_https_url_pointed_at_a_plain_server_is_never_read() {
    // Nothing is negotiated: the server answers in plaintext where a certificate
    // chain was required. A client that accepted it would hand the caller a page
    // that no certificate stood behind.
    let server = Server::always(Reply::html("<html><body>not over tls</body></html>"));
    let token = atlas_guard::Gated::loopback(&format!("https://127.0.0.1:{}/x", server.addr().port()))
        .expect("an https loopback verdict");
    let client = Client::new();

    let page = client.fetch_gated(&token, DEFAULT_MAX_BYTES);

    let error = page.error.clone().expect("a plaintext answer to https was accepted");
    // What rustls reports for this, recorded so the wording is pinned: the bytes do
    // not parse as a TLS record, which arrives as an `io::Error` of kind
    // `InvalidData` and is classified as a TLS failure rather than a connection that
    // failed for no reason.
    assert!(error.starts_with("TLS failed"), "{}", error);
    assert_eq!(page.status, 0);
    assert!(page.text.is_empty());
    assert!(page.refused.is_none(), "a certificate failure is not a refusal");
    assert!(client.verifies_tls(), "and the client still verifies certificates");
}

#[test]
fn a_page_is_decoded_with_the_charset_it_declares() {
    // The whole charset path over the wire: no charset in the header, a
    // `windows-1252` declaration in the body, and a byte (0x92) that reads as a
    // right single quote in windows-1252 and as a control character in latin-1.
    let body = b"<html><head><meta charset=windows-1252></head><body>caf\x92</body></html>";
    let server = Server::always(Reply::typed(200, "text/html", body.to_vec()));
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/charset"), DEFAULT_MAX_BYTES);

    assert_eq!(page.charset, "windows-1252");
    assert!(page.text.contains("caf\u{2019}"), "{:?}", page.text);
}

#[test]
fn a_compressed_body_never_becomes_a_page_we_did_not_ask_for() {
    // We ask for `identity`, so a compressed answer is a server ignoring the
    // request, and the workspace builds `ureq` without its `gzip` feature
    // (`rust/Cargo.toml`), so the transport cannot decompress one behind this
    // code's back. The header therefore survives, and *this* branch is the one
    // that decides — which is what makes the refusal a decision of this crate
    // rather than a side effect of a feature nobody looked at.
    //
    // The assertion is the sentence, not "some error": when the transport
    // decompressed, a body that was not really gzip failed while *reading* and the
    // reason was the decoder's; a test that accepted either outcome would pass
    // again the day somebody re-enabled the feature.
    let gzip = Reply::typed(200, "text/html", b"\x1f\x8b\x08not really".to_vec())
        .with_header("Content-Encoding", "gzip");
    let server = Server::always(gzip);
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/gz"), DEFAULT_MAX_BYTES);

    let error = page.error.clone().expect("a refusal to read a compressed body");
    assert!(
        error.contains("compressed response refused") && error.contains("gzip"),
        "{}",
        error
    );
    assert!(page.text.is_empty());
    assert_eq!(page.bytes, 0);
    // And the request really did ask for an uncompressed body.
    assert_eq!(server.requests()[0].header("accept-encoding"), "identity");
}

#[test]
fn an_encoding_the_transport_cannot_decode_is_refused_with_its_name() {
    // `deflate` is not decoded by the transport, so the header survives and the
    // policy in `exchange` decides: a body we cannot read uncompressed is a body
    // we do not read at all.
    let deflate = Reply::typed(200, "text/html", b"raw bytes".to_vec())
        .with_header("Content-Encoding", "deflate");
    let server = Server::always(deflate);
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/deflate"), DEFAULT_MAX_BYTES);

    let error = page.error.clone().expect("a refusal to decompress");
    assert!(error.contains("deflate"), "{}", error);
    assert!(page.text.is_empty());
    assert_eq!(page.bytes, 0);
}

#[test]
fn a_fetch_over_loopback_costs_what_the_transport_costs() {
    // What our own plumbing adds to a page that is already in memory: the verdict,
    // the per-request config, the header writes and the cap. Measured, not claimed:
    // the number is printed, and the assertion is a ceiling that would catch a
    // client rebuilt per request or a body copied twice.
    let body = vec![b'x'; 4_000];
    let server = Server::always(Reply::typed(200, "text/html", body));
    let token = server.token("/a");
    let client = Client::new();
    let rounds = 200;

    let start = Instant::now();
    let mut bytes = 0usize;
    for _ in 0..rounds {
        let page = client.fetch_gated(&token, DEFAULT_MAX_BYTES);
        bytes += page.bytes;
    }
    let elapsed = start.elapsed();
    let per_fetch_us = elapsed.as_secs_f64() * 1e6 / rounds as f64;
    println!(
        "{} loopback fetches in {:.1} ms -> {:.0} µs each ({} bytes read)",
        rounds,
        elapsed.as_secs_f64() * 1e3,
        per_fetch_us,
        bytes
    );
    assert_eq!(bytes, rounds * 4_000);
    assert!(per_fetch_us < 10_000.0, "{:.0} µs per fetch", per_fetch_us);
}

#[test]
fn the_public_door_still_refuses_this_machine() {
    // The loopback seam above is the guard's own door, held only by a caller who
    // asked it. `Client::fetch` — what a URL from a feed goes through — must not
    // have inherited it, and must refuse before any socket is opened.
    let server = Server::always(Reply::html("<html><body>never</body></html>"));
    let client = Client::new();

    let page = client.fetch(&server.url("/a"), DEFAULT_MAX_BYTES);

    let reason = page.refused.clone().expect("a refusal with a reason");
    assert!(reason.contains("private") || reason.contains("loopback"), "{}", reason);
    assert!(page.error.is_none());
    assert_eq!(page.status, 0);
    assert_eq!(server.count(), 0, "a refused url was requested anyway");
}

/// One real page, fetched only when asked for:
///
/// ```text
/// cargo test --offline -p atlas-net -- --ignored --nocapture live_news_page
/// ```
///
/// Cold = the first request through a fresh client (DNS + TCP + TLS + body);
/// warm = the second request to the same host through the same client, which
/// reuses the pooled TLS connection.
#[test]
#[ignore = "reads one public news page: run it explicitly"]
fn live_news_page_cold_and_warm() {
    let url = std::env::var("ATLAS_NET_TEST_URL")
        .unwrap_or_else(|_| "https://www.theguardian.com/world".to_string());
    let client = Client::new();
    assert_eq!(client.user_agent(), user_agent());

    let verdict = client.gate_resolved(&url);
    assert!(verdict.ok, "the gate refused {}: {}", url, verdict.reason);

    let start = std::time::Instant::now();
    let cold = client.fetch(&url, 600_000);
    let cold_ms = start.elapsed().as_secs_f64() * 1000.0;

    let start = std::time::Instant::now();
    let warm = client.fetch(&url, 600_000);
    let warm_ms = start.elapsed().as_secs_f64() * 1000.0;

    for (label, page, ms) in [("cold", &cold, cold_ms), ("warm", &warm, warm_ms)] {
        println!(
            "{:>4} {} ms  status={} bytes={} charset={} truncated={} hops={} text_chars={} ctype={:?}{}",
            label,
            ms.round() as i64,
            page.status,
            page.bytes,
            page.charset,
            page.truncated,
            page.redirects,
            page.text.chars().count(),
            page.content_type,
            match (&page.error, &page.refused) {
                (Some(e), _) => format!(" error={}", e),
                (_, Some(r)) => format!(" refused={}", r),
                _ => String::new(),
            }
        );
    }

    assert!(
        cold.error.is_none() && cold.refused.is_none(),
        "cold fetch failed: {:?} {:?}",
        cold.error,
        cold.refused
    );
    assert_eq!(cold.status, 200, "unexpected status {}", cold.status);
    assert!(cold.bytes > 1000, "only {} bytes", cold.bytes);
    assert!(!cold.text.is_empty());
    assert!(!cold.text.contains('\u{0}'), "text carries a NUL");
    assert!(!cold.charset.is_empty());
    assert_eq!(cold.final_url, warm.final_url);
    assert!(cold.final_url.starts_with("http"));
    assert_eq!(gate(&cold.final_url).ok, true);
    println!(
        "cold {:.1} ms, warm {:.1} ms, warm/cold = {:.2}",
        cold_ms,
        warm_ms,
        warm_ms / cold_ms
    );
}
