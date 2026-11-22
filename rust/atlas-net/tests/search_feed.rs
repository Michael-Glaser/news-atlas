//! The default search request: one plain GET to a public feed, and what the client
//! guarantees about it.
//!
//! `atlas-search` reaches the network through exactly one call —
//! `atlas_net::Client::fetch` (`atlas-search/src/fetch.rs:76`, with `Client::new()` at
//! `:66`) — and the request it makes is a bare `GET` to an RSS/Atom endpoint: no
//! cookie jar, no login, no credentials, no referer. This file pins the five things
//! that request relies on, from outside the crate, against the public API:
//!
//! 1. **Nothing is sent that identifies a session.** [`the_search_get_carries_no_cookie_and_no_credential`]
//!    reads the request off the wire: the first request carries no `Cookie`, no
//!    `Authorization`, no `Proxy-Authorization` and no `Referer`, asks for an
//!    `identity` body, and says it is a desktop browser with no product token naming
//!    this app. `atlas-net/src/*` contains no occurrence of `Cookie`,
//!    `Authorization`, `credential` or `Set-Cookie` at all (grep, 0 hits), so there
//!    is no jar to leak from.
//! 2. **A feed cannot redirect onto this machine.** [`a_feed_that_redirects_to_a_private_address_is_refused`]
//!    asks the redirect rule directly with a public feed origin (no socket), and
//!    [`a_redirect_chain_is_capped_and_every_hop_is_gated`] stages it on the wire: the
//!    hop that points at the metadata service is refused, and the server counts the
//!    requests to show nothing was sent to it.
//! 3. **The chain is capped.** [`a_redirect_chain_is_capped_and_every_hop_is_gated`]
//!    also proves the cap is a *number*: `MAX_REDIRECTS` hops are followed and the
//!    next redirect is a refusal, so a feed that bounces for ever ends the fetch
//!    rather than the window.
//! 4. **The body is capped.** [`the_feed_body_is_capped`] (a 2 MB feed under a 32 KiB
//!    cap) and [`a_hostile_content_length_is_not_trusted`] (a declared 8 GiB, sent
//!    in full in one case and not at all in the other).
//! 5. **There is a clock.** [`the_client_has_a_clock`] pins the two timeouts the
//!    shipped client is built with and shows a silent feed is cut by the budget it
//!    was given, and [`an_rss_answer_is_a_document`] keeps the default cap and the
//!    feed content types honest — a 600 kB cap is no use if the body is never read
//!    because its `Content-Type` was not recognised as a document.
//!
//! The two sockets here are `127.0.0.1:0` listeners bound inside this test process and
//! closed by `Drop`; nothing else is opened, and no test resolves a real name.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atlas_net::{
    check_hop, check_hop_from, is_document_type, read_capped, Client, DEFAULT_MAX_BYTES,
    HARD_MAX_BYTES, MAX_REDIRECTS, TIMEOUT_CONNECT, TIMEOUT_TOTAL,
};

// ---------------------------------------------------------------------------
// A feed server this test owns: scripted replies, and the request heads it saw.
// ---------------------------------------------------------------------------

/// One scripted answer.
#[derive(Clone)]
struct Reply {
    status: u16,
    content_type: &'static str,
    /// Sent as `Location` when set.
    location: Option<String>,
    /// What is actually written after the head.
    body: Vec<u8>,
    /// A `Content-Length` to declare instead of the body's real length — the shape of
    /// a hostile or merely broken answer. `None` declares the truth.
    declared: Option<u64>,
    /// When true the connection is held open and nothing at all is written.
    silent: bool,
}

impl Reply {
    fn feed(body: &[u8]) -> Reply {
        Reply {
            status: 200,
            content_type: "application/rss+xml; charset=utf-8",
            location: None,
            body: body.to_vec(),
            declared: None,
            silent: false,
        }
    }

    fn redirect(to: &str) -> Reply {
        Reply {
            status: 302,
            content_type: "text/html; charset=utf-8",
            location: Some(to.to_string()),
            body: Vec::new(),
            declared: None,
            silent: false,
        }
    }
}

/// A loopback HTTP server that answers from a script and records every request head.
struct Feed {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
    accepted: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Feed {
    fn serve(script: Vec<Reply>) -> Feed {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral loopback port");
        let address = listener.local_addr().expect("the listener address");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let script = Arc::new(Mutex::new(script));
        let thread = {
            let stop = Arc::clone(&stop);
            let seen = Arc::clone(&seen);
            let accepted = Arc::clone(&accepted);
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            accepted.fetch_add(1, Ordering::SeqCst);
                            // BSD/macOS: an accepted socket inherits the listener's
                            // non-blocking flag, so a read would return `WouldBlock`
                            // before the client has written and the recorded head
                            // would be empty. Blocking first, then a timeout: the
                            // request is read whole, or the read gives up.
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let mut head = Vec::new();
                            let mut chunk = [0u8; 2048];
                            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                                match stream.read(&mut chunk) {
                                    Ok(0) | Err(_) => break,
                                    Ok(read) => head.extend_from_slice(&chunk[..read]),
                                }
                            }
                            seen.lock().expect("the request log").push(
                                String::from_utf8_lossy(&head)
                                    .split("\r\n\r\n")
                                    .next()
                                    .unwrap_or("")
                                    .to_string(),
                            );
                            let answer = {
                                let mut script = script.lock().expect("the script");
                                if script.len() > 1 {
                                    script.remove(0)
                                } else {
                                    script.first().cloned().unwrap_or_else(|| Reply::feed(b""))
                                }
                            };
                            if answer.silent {
                                while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                continue;
                            }
                            let location = match &answer.location {
                                Some(to) => format!("Location: {to}\r\n"),
                                None => String::new(),
                            };
                            let length = answer.declared.unwrap_or(answer.body.len() as u64);
                            let head = format!(
                                "HTTP/1.1 {} X\r\nContent-Type: {}\r\n{location}\
                                 Content-Length: {length}\r\nConnection: close\r\n\r\n",
                                answer.status, answer.content_type
                            );
                            let _ = stream.write_all(head.as_bytes());
                            let _ = stream.write_all(&answer.body);
                            let _ = stream.flush();
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Feed {
            address,
            stop,
            seen,
            accepted,
            thread: Some(thread),
        }
    }

    /// The loopback token for this server. The public gate refuses every loopback
    /// address on purpose, so a test that wants a real socket on this machine has to
    /// use the guard's own door — the same token the model-server traffic carries.
    fn token(&self, path: &str) -> atlas_guard::Gated {
        atlas_guard::Gated::loopback(&format!("http://{}{}", self.address, path))
            .expect("a loopback verdict for this server")
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().expect("the request log").clone()
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        // The listener this test opened, closed here and not by luck: the thread owns
        // it, so asking the thread to stop and joining is what gives the port back.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A short feed body, the shape the default search request reads.
fn feed_body() -> Vec<u8> {
    br#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0"><channel><title>Example</title>
<item><title>One</title><link>https://news.example.org/one</link></item>
</channel></rss>"#
        .to_vec()
}


/// One header's value, matched case-insensitively: `ureq` writes field names in lower
/// case on the wire (legal — HTTP/1.1 field names are case-insensitive, RFC 9110 §5.1
/// — and what a browser sends), so a test that matched `Referer:` would be testing
/// its own capitalisation rather than the request.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        if field.trim().eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// 1. No cookie, no credential, no referer.
// ---------------------------------------------------------------------------

/// The first request of a search: a bare `GET`, and nothing in it that carries a
/// session. Read off the wire rather than argued from the config, because a header is
/// only absent when the bytes say so.
#[test]
fn the_search_get_carries_no_cookie_and_no_credential() {
    let feed = Feed::serve(vec![Reply::feed(&feed_body())]);
    let client = Client::new();
    let page = client.fetch_gated(&feed.token("/rss"), DEFAULT_MAX_BYTES);
    assert_eq!(page.status, 200, "{:?}", page.error);
    assert!(page.text.contains("<rss"), "the feed body was not read");
    assert!(!page.truncated, "a 200 byte feed was reported as cut");
    assert_eq!(page.content_type, "application/rss+xml; charset=utf-8");

    let requests = feed.requests();
    assert_eq!(
        requests.len(),
        1,
        "the search request was repeated: {requests:?}"
    );
    let head = &requests[0];
    // What it is: one GET, the exact path the caller asked for, and the literal host.
    assert!(
        head.starts_with("GET /rss HTTP/1.1\r\n"),
        "not a plain GET: {head:?}"
    );
    assert_eq!(
        header(head, "Host"),
        Some(feed.address.to_string().as_str()),
        "the Host header is not the address the gate approved: {head:?}"
    );
    // What it must never be: a session, a credential, a referer.
    for forbidden in [
        "Cookie",
        "Cookie2",
        "Authorization",
        "Proxy-Authorization",
        "Referer",
        "X-Api-Key",
    ] {
        assert_eq!(
            header(head, forbidden),
            None,
            "the search request carries {forbidden}: {head:?}"
        );
    }
    assert_eq!(
        header(head, "Accept-Encoding"),
        Some("identity"),
        "the request asks for a compressed body: {head:?}"
    );
    // The User-Agent is a browser's, and it names no product: a header that said
    // "atlas" would be a fingerprint of this app on every publisher's log.
    let agent = header(head, "User-Agent")
        .unwrap_or_else(|| panic!("no User-Agent in the request: {head:?}"));
    assert!(agent.contains("Mozilla/5.0"), "{agent}");
    assert!(
        !agent.to_ascii_lowercase().contains("atlas"),
        "the User-Agent names this app: {agent}"
    );
    // The whole list, not a sample: five headers and no more. This is the assertion
    // that a sixth one — a jar, a token, an `Accept` for something else — has to be
    // argued for in review, and it is why the cookie/referer claims above cannot rot
    // back in without a red test.
    assert_eq!(
        header_names(head),
        vec![
            "accept",
            "accept-encoding",
            "accept-language",
            "host",
            "user-agent"
        ],
        "the search request sends a header nobody asked for: {head:?}"
    );
}

/// The field names of one request head, lower-cased and sorted.
fn header_names(head: &str) -> Vec<String> {
    let mut names: Vec<String> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim().to_ascii_lowercase()))
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// 2. A feed cannot redirect onto this machine, and 3. the chain is capped.
// ---------------------------------------------------------------------------

/// The redirect rule with a public feed origin: no socket, so this is the half a test
/// can ask without a public server. A `302` from a feed is gated exactly like a link
/// from the feed — the guard's verdict, not the site's.
#[test]
fn a_feed_that_redirects_to_a_private_address_is_refused() {
    let feed = "https://feeds.reuters.com/rss/world";
    let origin = atlas_guard::Gated::verify(feed).expect("a public feed url");
    for (class, location) in [
        ("loopback", "http://127.0.0.1/rss"),
        ("loopback", "http://127.0.0.1:8080/admin"),
        ("loopback v6", "http://[::1]/rss"),
        ("loopback v6", "http://[::ffff:127.0.0.1]/rss"),
        ("metadata", "http://169.254.169.254/latest/meta-data/"),
        ("lan", "http://192.168.1.1/rss"),
        ("lan", "http://10.0.0.5:8080/rss"),
        ("link-local v6", "http://[fe80::1]/rss"),
        ("cgnat", "http://100.64.0.1/rss"),
        ("decimal", "http://2130706433/rss"),
        ("scheme-relative", "//127.0.0.1/rss"),
        ("service port", "https://feeds.reuters.com:22/rss"),
        ("file", "file:///etc/passwd"),
        ("ftp", "ftp://feeds.reuters.com/rss"),
    ] {
        assert!(
            check_hop(feed, location).is_err(),
            "{class}: a feed redirected to {location} and the hop was approved"
        );
        assert!(
            check_hop_from(&origin, location).is_err(),
            "{class}: the transport's hop rule approved {location}"
        );
    }
    // …and a public hop is still followed, or every feed behind a CDN would be dead.
    assert!(check_hop(feed, "https://feeds.reuters.com/rss/world?page=2").is_ok());
}

/// The cap and the re-gate on the wire, in one chain: hop 1 is allowed (same server),
/// hop 2 points at the metadata service and is refused. The server's request count is
/// the measurement — two requests for two hops, and nothing for the third, which was
/// refused before it was requested.
#[test]
fn a_redirect_chain_is_capped_and_every_hop_is_gated() {
    // Hop 2 is private: allowed hop, then a refused one.
    let feed = Feed::serve(vec![
        Reply::redirect("/step-1"),
        Reply::redirect("http://169.254.169.254/latest/meta-data/"),
        Reply::feed(&feed_body()),
    ]);
    let client = Client::with_limits(DEFAULT_MAX_BYTES, Duration::from_secs(5), Duration::from_secs(2));
    let page = client.fetch_gated(&feed.token("/rss"), DEFAULT_MAX_BYTES);
    let refused = page
        .refused
        .clone()
        .unwrap_or_else(|| panic!("a hop to the metadata service was followed: {page:?}"));
    assert!(
        refused.contains("redirect") || refused.contains("hop"),
        "the refusal does not name the redirect: {refused}"
    );
    assert_eq!(
        feed.accepted(),
        2,
        "expected two requests (the feed and the allowed hop), got {}",
        feed.accepted()
    );
    assert_eq!(page.status, 0, "a refused hop produced a status");
    assert!(page.text.is_empty(), "a refused hop produced text");

    // The cap itself: a feed that redirects for ever ends at MAX_REDIRECTS hops.
    let loop_feed = Feed::serve(vec![Reply::redirect("/again")]);
    let started = Instant::now();
    let page = client.fetch_gated(&loop_feed.token("/rss"), DEFAULT_MAX_BYTES);
    let elapsed = started.elapsed();
    let refused = page
        .refused
        .clone()
        .unwrap_or_else(|| panic!("a redirect loop was not refused: {page:?}"));
    assert!(
        refused.contains("redirect"),
        "the refusal does not name the redirects: {refused}"
    );
    assert!(
        refused.contains(&MAX_REDIRECTS.to_string()),
        "the refusal does not say how many redirects were allowed: {refused}"
    );
    // The number, and the constant it comes from: a cap that quietly moved would
    // otherwise agree with itself (`accepted == MAX_REDIRECTS + 1` holds for any cap).
    assert_eq!(MAX_REDIRECTS, 5, "the hop cap moved: {MAX_REDIRECTS}");
    assert_eq!(
        loop_feed.accepted(),
        6,
        "expected the first request plus five hops (6), got {}",
        loop_feed.accepted()
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a redirect loop held the fetch for {elapsed:?}"
    );
}

/// The `Referer` policy, measured: the first request carries none; a hop inside the
/// same origin carries the URL it came from; a hop to a different origin carries
/// nothing at all. (The query string of the feed the model asked for is not a
/// stranger's business.)
#[test]
fn the_referer_is_only_sent_back_to_the_same_site() {
    let other = Feed::serve(vec![Reply::feed(&feed_body())]);
    let same = Feed::serve(vec![Reply::redirect("/step-1"), Reply::feed(&feed_body())]);
    let client = Client::with_limits(DEFAULT_MAX_BYTES, Duration::from_secs(5), Duration::from_secs(2));

    // Same origin: a `Referer` naming the URL the hop came from.
    let page = client.fetch_gated(&same.token("/rss"), DEFAULT_MAX_BYTES);
    assert_eq!(page.status, 200, "{:?}", page.error);
    let requests = same.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        header(&requests[0], "Referer"),
        None,
        "the first request carried a Referer:\n{}",
        requests[0]
    );
    assert_eq!(
        header(&requests[1], "Referer"),
        Some(same.url("/rss").as_str()),
        "a same-origin hop did not carry the page it came from:\n{}",
        requests[1]
    );

    // A different origin (a different port is a different origin): no `Referer`.
    let crossing = Feed::serve(vec![
        Reply::redirect(&other.url("/rss")),
        Reply::feed(&feed_body()),
    ]);
    let page = client.fetch_gated(&crossing.token("/rss"), DEFAULT_MAX_BYTES);
    assert_eq!(page.status, 200, "{:?}", page.error);
    let far = other.requests();
    assert_eq!(far.len(), 1, "{far:?}");
    assert_eq!(
        header(&far[0], "Referer"),
        None,
        "a cross-origin hop carried a Referer:\n{}",
        far[0]
    );
}

// ---------------------------------------------------------------------------
// 4. The body is capped; a hostile Content-Length is not trusted.
// ---------------------------------------------------------------------------

/// A feed that is far larger than the cap is cut at the cap and says so.
#[test]
fn the_feed_body_is_capped() {
    let cap = 32 * 1024;
    let big = vec![b'x'; 2 * 1024 * 1024];
    let feed = Feed::serve(vec![Reply::feed(&big)]);
    let client = Client::with_limits(cap, Duration::from_secs(10), Duration::from_secs(5));
    let started = Instant::now();
    let page = client.fetch_gated(&feed.token("/rss"), cap);
    let elapsed = started.elapsed();

    assert_eq!(page.status, 200, "{:?}", page.error);
    assert_eq!(page.bytes, cap, "a 2 MB feed kept {} bytes", page.bytes);
    assert!(page.truncated, "a cut feed was not reported as truncated");
    assert!(
        elapsed < Duration::from_secs(5),
        "a capped read took {elapsed:?}"
    );
    // The caller cannot raise the cap past the client's ceiling.
    assert_eq!(atlas_net::clamp_bytes(usize::MAX), HARD_MAX_BYTES);
    assert!(DEFAULT_MAX_BYTES <= HARD_MAX_BYTES);
}

/// `Content-Length` is a claim, not a reservation. A feed that declares 8 GiB and
/// sends 512 KiB must be cut at the cap; one that declares 8 GiB and sends *nothing*
/// must not leave the window waiting for a buffer it cannot get — and neither may
/// allocate 8 GiB, which is why this test finishes at all.
#[test]
fn a_hostile_content_length_is_not_trusted() {
    let cap = 64 * 1024;
    let eight_gib: u64 = 8 * 1024 * 1024 * 1024;

    // Declared 8 GiB, 512 KiB actually sent.
    let short = Feed::serve(vec![Reply {
        declared: Some(eight_gib),
        ..Reply::feed(&vec![b'y'; 512 * 1024])
    }]);
    let client = Client::with_limits(cap, Duration::from_secs(10), Duration::from_secs(5));
    let started = Instant::now();
    let page = client.fetch_gated(&short.token("/rss"), cap);
    let elapsed = started.elapsed();
    assert_eq!(page.status, 200, "{:?}", page.error);
    assert_eq!(page.bytes, cap, "the declared 8 GiB was believed: {} bytes kept", page.bytes);
    assert!(page.truncated, "a body cut at the cap was not reported as cut");
    assert!(
        elapsed < Duration::from_secs(5),
        "a hostile Content-Length held the fetch for {elapsed:?}"
    );

    // Declared 8 GiB, nothing sent, connection closed.
    let empty = Feed::serve(vec![Reply {
        declared: Some(eight_gib),
        ..Reply::feed(b"")
    }]);
    let started = Instant::now();
    let page = client.fetch_gated(&empty.token("/rss"), cap);
    let elapsed = started.elapsed();
    assert_eq!(
        page.bytes, 0,
        "bytes arrived from a body that was never sent: {page:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "an empty body with a declared 8 GiB held the fetch for {elapsed:?}"
    );

    // And the reader itself, measured from the other side: how many bytes it was
    // *pulled* through. A `read_capped` that truncates the result but reads the whole
    // stream would return the same `Vec` — and allocate the whole stream getting
    // there — so the length of the answer cannot be the assertion. This reader offers
    // 64 MiB and counts what is taken.
    struct Offered {
        left: usize,
        pulled: Arc<AtomicUsize>,
    }
    impl Read for Offered {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let take = buf.len().min(self.left);
            for byte in buf.iter_mut().take(take) {
                *byte = b'z';
            }
            self.left -= take;
            self.pulled.fetch_add(take, Ordering::SeqCst);
            Ok(take)
        }
    }
    let pulled = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let (bytes, truncated) = read_capped(
        Offered {
            left: 64 * 1024 * 1024,
            pulled: Arc::clone(&pulled),
        },
        1024,
    )
    .expect("a capped read");
    assert_eq!(bytes.len(), 1024, "the answer was not cut at the cap");
    assert!(truncated, "a cut answer was not reported as cut");
    assert!(
        pulled.load(Ordering::SeqCst) <= 1024 + 8192,
        "the cap was not enforced on the stream: {} bytes were read for a 1 KiB cap",
        pulled.load(Ordering::SeqCst)
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a 64 MiB stream was read for {:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// 5. The clock, and the numbers the search path inherits.
// ---------------------------------------------------------------------------

/// The two timeouts and the hop cap the search request inherits, and the clock doing
/// its job on a feed that accepts and then says nothing.
#[test]
fn the_client_has_a_clock() {
    let client = Client::new();
    assert_eq!(
        client.total_timeout(),
        TIMEOUT_TOTAL,
        "the shipped total budget is not the constant"
    );
    assert_eq!(
        client.connect_timeout(),
        TIMEOUT_CONNECT,
        "the shipped connect budget is not the constant"
    );
    assert!(
        client.total_timeout() >= Duration::from_secs(1)
            && client.total_timeout() <= Duration::from_secs(60),
        "a whole fetch is bounded by {:?}, which is either useless or a hang",
        client.total_timeout()
    );
    assert!(
        client.connect_timeout() <= client.total_timeout(),
        "the connect budget outlives the whole fetch"
    );
    assert_eq!(MAX_REDIRECTS, 5, "the hop cap moved");
    assert!(
        client.hard_cap() == HARD_MAX_BYTES,
        "the client's ceiling is {}",
        client.hard_cap()
    );

    // A feed that accepts and says nothing is cut by the budget it was given.
    let budget = Duration::from_millis(400);
    let silent = Feed::serve(vec![Reply {
        silent: true,
        ..Reply::feed(b"")
    }]);
    let client = Client::with_limits(DEFAULT_MAX_BYTES, budget, Duration::from_millis(200));
    let started = Instant::now();
    let page = client.fetch_gated(&silent.token("/rss"), DEFAULT_MAX_BYTES);
    let elapsed = started.elapsed();
    assert!(
        page.error.is_some(),
        "a silent feed produced no error: {page:?}"
    );
    assert!(
        elapsed < budget + Duration::from_millis(1500),
        "a silent feed held the fetch for {elapsed:?}, budget {budget:?}"
    );
    assert_eq!(page.status, 0, "a timeout produced a status");
}

/// A feed's content type has to be recognised as a document, or the cap is beside the
/// point: an unrecognised type means the body is never read at all.
#[test]
fn an_rss_answer_is_a_document() {
    for (content_type, expected) in [
        ("application/rss+xml", true),
        ("application/rss+xml; charset=utf-8", true),
        ("application/atom+xml", true),
        ("application/xml", true),
        ("text/xml; charset=UTF-8", true),
        ("text/html; charset=utf-8", true),
        ("application/json", true),
        ("", true), // a server that sends no type is still a page
        ("application/pdf", false),
        ("image/png", false),
        ("application/octet-stream", false),
        ("video/mp4", false),
    ] {
        assert_eq!(
            is_document_type(content_type),
            expected,
            "is_document_type({content_type:?})"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. A reason never carries the query it was asked with.
// ---------------------------------------------------------------------------

/// The search GET puts the model's query — and, on a configured engine, its key — in
/// the URL. A *page* keeps that URL (`final_url`, `Verdict::url`), because the caller
/// has to know where the answer came from; a **reason** must not, because a reason
/// goes into `Page::error`, the tool result the model reads, and the log. This walks
/// the whole public surface a reason can come out of and asserts the query is gone.
#[test]
fn a_reason_never_carries_the_query_it_was_asked_with() {
    const SECRET: &str = "SECRET-ENGINE-KEY";
    let queried = |url: String| format!("{url}?q=chip+shortage&key={SECRET}");

    // 1. A gate refusal (this machine, a scheme that is not http, a bare name).
    for url in [
        queried("http://127.0.0.1/rss".to_string()),
        queried("http://169.254.169.254/latest/meta-data/".to_string()),
        queried("file:///etc/passwd".to_string()),
        queried("http://localhost/rss".to_string()),
    ] {
        let verdict = atlas_net::gate(&url);
        assert!(!verdict.ok, "{url} was approved");
        assert!(
            !verdict.reason.contains(SECRET) && !verdict.reason.contains("q="),
            "the verdict reason carries the query: {}",
            verdict.reason
        );
        let page = Client::new().fetch(&url, DEFAULT_MAX_BYTES);
        let refused = page.refused.unwrap_or_else(|| panic!("{url} was not refused"));
        assert!(
            !refused.contains(SECRET) && !refused.contains("q="),
            "the refusal carries the query: {refused}"
        );
    }

    // 2. A redirect refused for pointing at this machine, from a feed asked with a
    //    query: the refusal names the hop and never the URL that was requested.
    let feed = Feed::serve(vec![Reply::redirect("http://169.254.169.254/latest/meta-data/")]);
    let client = Client::with_limits(DEFAULT_MAX_BYTES, Duration::from_secs(5), Duration::from_secs(2));
    let page = client.fetch_gated(&feed.token("/rss?q=chip+shortage&key=SECRET-ENGINE-KEY"), DEFAULT_MAX_BYTES);
    let refused = page
        .refused
        .clone()
        .unwrap_or_else(|| panic!("the hop to the metadata service was followed: {page:?}"));
    assert!(
        !refused.contains(SECRET) && !refused.contains("q="),
        "the hop refusal carries the query: {refused}"
    );

    // 3. A transport failure — nothing listening on a port this test just released —
    //    asked with the same query. The sentence is the socket's, and it is clean.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let address = listener.local_addr().expect("the address");
        drop(listener);
        address
    };
    let token = atlas_guard::Gated::loopback(&format!(
        "http://{closed}/rss?q=chip+shortage&key={SECRET}"
    ))
    .expect("a loopback verdict for a closed port");
    let page = client.fetch_gated(&token, DEFAULT_MAX_BYTES);
    let error = page
        .error
        .clone()
        .unwrap_or_else(|| panic!("a closed port produced no error: {page:?}"));
    assert!(
        !error.contains(SECRET) && !error.contains("q="),
        "the transport failure carries the query: {error}"
    );
    assert!(
        error.contains("refused") || error.contains("connection"),
        "the sentence does not say what happened: {error}"
    );

    // And the contrast: a page that *was* read keeps its URL, query and all — the
    // scrub is on reasons, not on the place a caller reads where the answer came from.
    let ok_feed = Feed::serve(vec![Reply::feed(&feed_body())]);
    let page = client.fetch_gated(&ok_feed.token("/rss?q=chip+shortage&key=K"), DEFAULT_MAX_BYTES);
    assert_eq!(page.status, 200, "{:?}", page.error);
    assert!(
        page.final_url.contains("q=chip+shortage"),
        "a successful page lost its own URL: {}",
        page.final_url
    );
}
