//! The attacks this crate is built against, staged and then refused.
//!
//! These need everything at once — the gate, the resolver inside the transport, the
//! hop rule, the client, the budget and a server that misbehaves — so they live
//! beside [`crate::tests`] rather than next to one module. What they have in common
//! is the shape of the staging: a name that answers from a script
//! ([`Scripted`]), so an answer can change between the verdict and the connection,
//! and a loopback server whose request count is the proof of what was *not* sent.
//!
//! Why the loopback door and not a plain URL: the public gate refuses every loopback
//! address on purpose, so a test cannot reach a local server with `Client::fetch`.
//! `Gated::loopback` is the guard's own door for the app's local traffic, and the
//! transport is exercised through it unchanged. The one test that does use
//! `Client::fetch` (`a_name_that_rebinds_between_the_verdict_and_the_connect_is_refused`)
//! is the rebinding attack, whose whole point is the public entry point.

use super::*;
use crate::resolver::Lookup;
use crate::server::{gzip_of, Reply, Server};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A client that cannot wait twenty seconds in a unit test.
fn quick(total: u64, connect: u64) -> Client {
    Client::with_limits(
        HARD_MAX_BYTES,
        Duration::from_millis(total),
        Duration::from_millis(connect),
    )
}

/// The client that can be lied to: the same one, with the names' answers injected.
fn scripted(lookup: &Arc<Scripted>) -> Client {
    Client::with_lookup(
        lookup.answers(),
        HARD_MAX_BYTES,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
}

static ENV: Mutex<()> = Mutex::new(());

/// A resolver whose answers are a script, including one that changes between the
/// verdict and the connection — the only way to stage a rebinding name offline.
#[derive(Debug)]
struct Scripted {
    answers: Mutex<Vec<Vec<SocketAddr>>>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(answers: Vec<Vec<SocketAddr>>) -> Arc<Scripted> {
        Arc::new(Scripted {
            answers: Mutex::new(answers),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn answers(self: &Arc<Scripted>) -> Arc<dyn Lookup> {
        Arc::clone(self) as Arc<dyn Lookup>
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("the lookup log").clone()
    }
}

impl Lookup for Scripted {
    fn addresses(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        self.asked
            .lock()
            .expect("the lookup log")
            .push(format!("{}:{}", host, port));
        let mut answers = self.answers.lock().expect("the script");
        if answers.is_empty() {
            return Err("no more answers".to_string());
        }
        Ok(answers.remove(0))
    }
}

/// An address no test connects to: if a fetch reaches this one, the network was
/// going to be used and the test failed before the connect happened.
fn never_dialled() -> SocketAddr {
    "93.184.216.34:80".parse().expect("a public address")
}

#[test]
fn a_name_that_rebinds_between_the_verdict_and_the_connect_is_refused() {
    // The attack: the name answers with a public address when the gate asks, and
    // with this machine when the connector asks. Before the resolver was the last
    // word on addresses, the second answer was dialled having been judged on the
    // first — the gate said "public", the socket went to 127.0.0.1:8787.
    let secret = Server::always(Reply::html("<html><body>the local api</body></html>"));
    let lookup = Scripted::new(vec![
        // Answer 1, for the verdict: a public address.
        vec![never_dialled()],
        // Answer 2, for the connection: this machine's own API.
        vec![SocketAddr::from(([127, 0, 0, 1], secret.addr().port()))],
    ]);
    let client = scripted(&lookup);
    let page = client.fetch("http://rebind.example.org/api/status", DEFAULT_MAX_BYTES);

    let reason = page.refused.clone().expect("a refusal with a reason");
    // The sentence says what happened, where it happened, and why.
    assert!(reason.contains("127.0.0.1"), "{}", reason);
    assert!(reason.contains("may not reach"), "{}", reason);
    assert!(reason.contains("public"), "{}", reason);
    // A refusal, not a transport failure: nothing retries this.
    assert!(page.error.is_none(), "{:?}", page.error);
    assert_eq!(page.status, 0);
    assert!(page.text.is_empty());
    // The proof that the second answer was never connected to.
    assert_eq!(
        secret.count(),
        0,
        "the rebinding answer was connected to anyway"
    );
    // And that the name really was asked twice: the filter ran at the connection on
    // the answer that would have been dialled, not on a remembered one.
    assert_eq!(
        lookup.asked(),
        vec![
            "rebind.example.org:80".to_string(),
            "rebind.example.org:80".to_string()
        ]
    );
}

#[test]
fn a_name_whose_only_answer_is_this_machine_is_refused_before_a_socket() {
    // The same attack with no second opinion: every answer is private. The verdict
    // refuses it, so not even a connection attempt happens.
    let here = Server::always(Reply::html("<html><body>never read</body></html>"));
    let lookup = Scripted::new(vec![vec![SocketAddr::from((
        [127, 0, 0, 1],
        here.addr().port(),
    ))]]);
    let client = scripted(&lookup);
    let page = client.fetch("http://private-answer.example.org/a", DEFAULT_MAX_BYTES);

    let reason = page.refused.clone().expect("a refusal with a reason");
    assert!(reason.contains("resolves to the private address"), "{}", reason);
    assert!(reason.contains("127.0.0.1"), "{}", reason);
    assert_eq!(here.count(), 0);
    // One lookup: the verdict's. Nothing was resolved for a connection.
    assert_eq!(lookup.asked().len(), 1, "{:?}", lookup.asked());
}

#[test]
fn an_environment_proxy_cannot_take_over_a_gated_request() {
    // ureq reads `HTTP_PROXY`/`ALL_PROXY` by default (`Config::default()` is
    // `Proxy::try_from_env()`), and a proxy would answer for a page this app
    // believes it gated: the address this app resolved would not be the address
    // the request went to, and the body cap and certificate check would describe
    // the proxy's answer instead of the site's. The client sets `proxy(None)`, and
    // this is the proof on the wire rather than on the config: with the variables
    // pointing at a second server on this machine, the page still comes from the
    // site and the "proxy" is never spoken to.
    let _guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let target = Server::always(Reply::html("<html><body>from the site</body></html>"));
    let proxy = Server::always(Reply::html("<html><body>from the proxy</body></html>"));
    // A loopback-class request is the one a test can make without the network: the
    // proxy decision is made once, in `agent_for`, and this is that agent.
    let token = target.token("/story");
    std::env::set_var("HTTP_PROXY", proxy.url("/"));
    std::env::set_var("http_proxy", proxy.url("/"));
    std::env::set_var("ALL_PROXY", proxy.url("/"));
    std::env::set_var("all_proxy", proxy.url("/"));
    let client = Client::new();
    let configured = client.uses_proxy();
    let page = client.fetch_gated(&token, DEFAULT_MAX_BYTES);
    std::env::remove_var("HTTP_PROXY");
    std::env::remove_var("http_proxy");
    std::env::remove_var("ALL_PROXY");
    std::env::remove_var("all_proxy");

    assert!(
        !configured,
        "an environment variable became a transport decision"
    );
    assert!(client.verifies_tls());
    assert_eq!(page.status, 200, "{:?} {:?}", page.error, page.refused);
    assert!(page.text.contains("from the site"), "{:?}", page.text);
    assert_eq!(target.count(), 1, "the request did not reach the site");
    assert_eq!(
        proxy.count(),
        0,
        "the environment's proxy answered for the site"
    );
}

#[test]
fn a_dribbled_response_body_is_cut_by_the_whole_fetch_budget() {
    // Slow loris, body half: the head arrives at once, then the body dribbles one
    // byte every 20 ms. A read with no whole-fetch clock would sit there for as long
    // as the site cares to dribble.
    let server = Server::always(
        Reply::html("<html><body>dribbled</body></html>").dribbling(Duration::from_millis(20)),
    );
    let client = quick(300, 5_000);

    let start = Instant::now();
    let page = client.fetch_gated(&server.token("/slow"), DEFAULT_MAX_BYTES);
    let elapsed = start.elapsed();

    let error = page.error.clone().expect("a transport failure with a reason");
    // The sentence names the phase, which is what a reader needs: this page started
    // answering and then would not finish.
    assert!(
        error.contains("timed out while reading the page"),
        "{}",
        error
    );
    assert!(page.refused.is_none());
    assert!(page.text.is_empty());
    assert!(
        elapsed < Duration::from_millis(1_500),
        "a 300 ms fetch took {:?}",
        elapsed
    );
}

#[test]
fn a_dribbled_response_head_is_cut_by_the_whole_fetch_budget() {
    // Slow loris, head half: not one complete status line arrives inside the budget.
    let server = Server::always(Reply::dribbled(Duration::from_millis(20)));
    let client = quick(300, 5_000);

    let start = Instant::now();
    let page = client.fetch_gated(&server.token("/slow"), DEFAULT_MAX_BYTES);
    let elapsed = start.elapsed();

    let error = page.error.clone().expect("a transport failure with a reason");
    assert!(error.contains("timed out"), "{}", error);
    assert!(error.contains("300 ms in all"), "{}", error);
    assert_eq!(page.status, 0);
    assert!(
        elapsed < Duration::from_millis(1_500),
        "a 300 ms fetch took {:?}",
        elapsed
    );
}

#[test]
fn a_compressed_body_from_a_site_that_ignores_identity_is_refused_before_it_is_read() {
    // The request said `Accept-Encoding: identity`. A site that sends a gzip body
    // anyway gets a refusal, and the refusal happens *before* the body is read: the
    // body here is a real gzip member (valid, decodes to the page below), and not one
    // byte of it is taken. This is the branch that decides because the workspace
    // builds ureq with `default-features = false` (no `gzip` feature to strip the
    // header and decompress below the cap); if that ever changes, this test fails and
    // the cap becomes the only bound on a decompression bomb.
    let plain = b"<html><body>compressed but not asked for</body></html>";
    let server = Server::always(
        Reply::typed(200, "text/html", gzip_of(plain)).with_header("Content-Encoding", "gzip"),
    );
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/gz"), DEFAULT_MAX_BYTES);

    let error = page.error.clone().expect("a refusal to read a compressed body");
    assert!(error.contains("gzip"), "{}", error);
    assert!(error.contains("refused"), "{}", error);
    assert!(page.text.is_empty());
    assert_eq!(page.bytes, 0, "the body was read anyway");
    assert_eq!(server.requests()[0].header("accept-encoding"), "identity");
}

#[test]
fn the_transport_hands_over_the_bytes_the_page_sent() {
    // One decoder, not two. The workspace turns ureq's `charset` feature off
    // (`rust/Cargo.toml`), so a `text/*` body declaring a legacy charset arrives as
    // the bytes the server wrote and `charset::decode` is the only thing that reads
    // them. When the transport did the conversion too, every such page came out as
    // mojibake: `caf’` (cp1252 0x92) arrived as `cafâ€™`.
    let body = b"<html><body>caf\x92 today</body></html>".to_vec();
    let server = Server::always(Reply::typed(
        200,
        "text/html; charset=windows-1252",
        body,
    ));
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/charset"), DEFAULT_MAX_BYTES);

    assert_eq!(page.charset, "windows-1252");
    assert!(page.text.contains("caf\u{2019}"), "{:?}", page.text);
    assert!(!page.text.contains("â"), "{:?} (decoded twice)", page.text);
}

#[test]
fn a_redirect_location_with_a_control_character_is_refused() {
    // A `Location` that carries a control character is not a URL. A CR or LF cannot
    // survive a header line at all (it ends the line, so two headers arrive instead),
    // but a TAB and a NUL can, and both parsers involved here *silently drop* them:
    // `Location: /a<TAB>b` becomes a request for `/ab`, a different resource from the
    // one the server named. No header is injected either way — a request line built
    // from a `url` can never carry a control character — so the defect is the silent
    // rewrite, and the fix is to refuse it.
    let server = Server::scripted(|index, _| {
        if index == 0 {
            Reply::redirect("/a\tX-Injected: yes")
        } else {
            Reply::html("<html><body>should never be read</body></html>")
        }
    });
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/start"), DEFAULT_MAX_BYTES);

    let reason = page.refused.clone().expect("a refusal with a reason");
    assert!(reason.contains("control character"), "{}", reason);
    assert!(reason.contains("redirect refused at hop 1 of 5"), "{}", reason);
    assert_eq!(page.status, 0);
    assert_eq!(server.count(), 1, "the mangled hop was requested anyway");
    // The one request that was made is the one that was asked for.
    let first = &server.requests()[0];
    assert_eq!(first.path(), "/start");
    assert_eq!(first.header("x-injected"), "");
}

#[test]
fn no_request_line_a_hop_produces_can_carry_a_control_character() {
    // The belt to that brace: whatever a `Location` says, a hop that would become a
    // request line holding a control character is refused, so a CR can never be
    // written into a header block — the injection path, not only the rewrite one.
    for location in ["/a\r\nX-Injected: yes", "/a\nX-Injected: yes", "/a\0b", "/a\rb"] {
        let refused = check_hop("https://example.com/a", location);
        assert!(refused.is_err(), "{:?} was allowed", location);
    }
    // Percent-encoded forms are ordinary path bytes and stay legal: they are sent as
    // the characters `%`, `0`, `d`, not as a CR.
    let encoded = check_hop("https://example.com/a", "/a%0d%0aX-Injected:%20yes")
        .expect("a percent-encoded CRLF is a path, not a header");
    assert_eq!(
        encoded.as_str(),
        "https://example.com/a%0d%0aX-Injected:%20yes"
    );
}

#[test]
fn a_response_header_block_over_the_cap_is_refused() {
    // One 20 kB header: over `MAX_HEADER_BYTES`, refused by the transport's cap.
    let server = Server::always(Reply::html("<html><body>never</body></html>").with_header("X-Pad", &"y".repeat(20_000)));
    let client = Client::new();

    let page = client.fetch_gated(&server.token("/big-header"), DEFAULT_MAX_BYTES);

    let error = page.error.clone().expect("a header cap that did not fire");
    assert!(error.contains("16384"), "{}", error);
    assert!(page.text.is_empty());

    // And hundreds of small headers, which the transport refuses on their count
    // before the block reaches the byte cap: a second limit, also a refusal.
    let mut many = Reply::html("<html><body>never</body></html>");
    for index in 0..400 {
        many = many.with_header(&format!("X-Pad-{}", index), &"y".repeat(80));
    }
    let server = Server::always(many);
    let page = client.fetch_gated(&server.token("/many-headers"), DEFAULT_MAX_BYTES);
    let error = page.error.clone().expect("a header count that did not fire");
    assert!(!error.is_empty());
    assert!(page.text.is_empty());
    assert!(page.bytes == 0);
}

#[test]
fn a_loopback_request_never_asks_a_resolver() {
    // The app's own traffic is a literal on this machine: an address, not a name. A
    // literal is its own answer, so there is no lookup to fool and no answer that can
    // change between the verdict and the connection — the whole rebinding class is
    // absent on this path rather than defended on it.
    let server = Server::always(Reply::html("<html><body>local</body></html>"));
    let lookup = Scripted::new(Vec::new());
    let client = scripted(&lookup);
    let page = client.fetch_gated(&server.token("/a"), DEFAULT_MAX_BYTES);

    assert_eq!(page.status, 200, "{:?} {:?}", page.error, page.refused);
    assert!(page.text.contains("local"));
    assert_eq!(server.count(), 1);
    assert!(
        lookup.asked().is_empty(),
        "a literal was looked up: {:?}",
        lookup.asked()
    );
}

#[test]
fn the_gate_and_the_wire_agree_about_which_host_is_being_requested() {
    // Two parsers stand between a URL and a socket: `url` (the gate, `Verdict::url`)
    // and `http::Uri` (the request line ureq writes). A disagreement about the host
    // or the port would be the whole attack — gate `example.com`, request
    // `127.0.0.1` — so every form the gate approves is checked to parse to the same
    // origin in both. The forms that hide a host (`user:pass@`, `\@`, `;@`) are in
    // here too: the gate refuses them, and the second half proves the refusal happens
    // before either parser can disagree.
    let approved = [
        "http://example.com./x",
        "http://example.com:8080/x",
        "http://example.com/a/../b",
        "http://example.com/%2e%2e/x",
        "http://example.com/@evil.com/x",
        "http://example.com/x\\y",
        "http://example.com/%00/x",
        "http://example.com/%c0%af/x",
        "http://example.com/?q=%0d%0aX:%20y",
        "http://example.com/#@evil.com",
        "http://example.com?@evil.com",
        "http://example.com/a%2fb",
        "http://[::ffff:808:808]/x",
        "HTTP://EXAMPLE.COM/X",
        "http://1.2.3.4/x",
        "http://xn--exmple-cua.com/x",
    ];
    for raw in approved {
        let verdict = gate(raw);
        assert!(verdict.ok, "{} was refused: {}", raw, verdict.reason);
        let canonical = verdict.url();
        let via_url = url::Url::parse(canonical).expect("the gate's own canonical form");
        let via_wire: ureq::http::Uri = canonical.parse().expect("a request line");
        let authority = via_wire.authority().expect("an authority");
        assert_eq!(
            via_url.host_str(),
            Some(authority.host()),
            "{}: the gate saw one host and the wire another",
            canonical
        );
        assert_eq!(
            via_url.port_or_known_default(),
            Some(
                authority
                    .port_u16()
                    .or(match via_wire.scheme_str() {
                        Some("https") => Some(443),
                        _ => Some(80),
                    })
                    .expect("a port")
            ),
            "{}: the gate saw one port and the wire another",
            canonical
        );
    }
    // And the forms that would make one parser disagree are refused outright, so
    // there is no request line to disagree about.
    for refused in [
        "http://user:pass@example.com/",
        "http://example.com\\@evil.com/x",
        "http://example.com@127.0.0.1/",
        "http://example.com;@evil.com/",
        "http://127.0.0.1.nip.invalid/x",
    ] {
        assert!(!gate(refused).ok, "{} was approved", refused);
    }
}
