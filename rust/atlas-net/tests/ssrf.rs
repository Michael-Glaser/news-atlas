//! The one outbound act: a page the model asked for by name, and nothing else.
//!
//! `atlas-net` is the only crate in this workspace that opens a socket to a name on
//! the internet (`Client::fetch` → `ureq`); every other `connect` in shipped code is
//! this machine talking to itself (the local model server, the app's own health
//! probe, a DevTools port) and every one of them is registered in
//! `atlas-guard/tests/audit_binds.rs`' `CONNECTIONS` table. This file is the proof of
//! the fetch boundary, asked of the *public* API — the same functions
//! `atlas-ingest` and `atlas-search` call when a tool call names a URL:
//!
//! 1. [`no_address_inside_this_machine_or_its_lan_is_ever_fetched`] — the literal
//!    half: loopback, the LAN ranges, link-local (where the cloud metadata service
//!    lives), the IPv6 unique-local range, and the IPv4-in-IPv6 containers a
//!    translator would deliver to `127.0.0.1` or `169.254.169.254` from an address
//!    that *looks* like IPv6 — plus the numeric spellings a resolver expands. Each is
//!    refused by [`atlas_net::gate`] before a request line exists, and
//!    [`atlas_net::Client::fetch`] turns that verdict into a refusal, not a request.
//! 2. [`a_lan_address_cannot_be_minted_as_this_machines_own_traffic`] — the app's
//!    own door (`Gated::loopback`, the model-server token) proves the address *is
//!    this machine*: it opens for loopback and for nothing else, so the class whose
//!    resolver only checks "is this machine" cannot be reached with anything else.
//! 3. [`a_public_name_that_answers_with_a_local_address_is_refused`] — the name half:
//!    a hostname that resolves to loopback/metadata is refused on its *answer*, not
//!    on its spelling, and a name that answers with both a public and a private
//!    address is reachable only at the public one.
//! 4. [`a_redirect_can_only_land_where_the_fetch_may_go`] and
//!    [`a_redirect_into_this_machine_is_refused_without_a_second_request`] — every
//!    hop is gated before it is requested: a `Location` pointing at this machine, at
//!    the metadata service, at a service port or at a non-http scheme is a refusal,
//!    measured on the wire as exactly one request to the server that answered `302`.
//! 5. [`a_non_http_scheme_is_refused_at_the_entry_point`] — `file://`, `data:`,
//!    `javascript:`, `ftp:`, `gopher:`, `chrome:`, a scheme-relative `//host` and a
//!    bare `www.…` are refused before the transport is reached.
//! 6. [`one_page_cannot_be_bigger_than_the_cap`] and
//!    [`one_page_cannot_be_longer_than_the_budget`] — the response is bounded twice:
//!    a byte cap (a declared 3 MB body, and an endless body with no
//!    `Content-Length` at all) and one clock for the whole fetch (a server that
//!    accepts and then says nothing).
//!
//! What this file cannot stage from outside the crate: a name that answers public
//! for the verdict and private for the connect. `Client::with_lookup` is
//! deliberately crate-private (a caller cannot inject a resolver), so that half is
//! pinned in-crate — `attacks::a_name_that_rebinds_between_the_verdict_and_the_connect_is_refused`,
//! where the filter lives inside the transport. The public half of the same rule is
//! [`atlas_net::approved_addresses_with`], which is the one function the transport's
//! resolver and the browser's pre-flight both call.
//!
//! Offline by construction: no test resolves a real name (the answers are injected
//! through the public [`atlas_net::Lookup`] trait) and the only sockets are
//! `127.0.0.1:0` listeners bound inside this test process and closed by `Drop`.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atlas_net::{
    approved_addresses_with, check_hop, check_hop_from, gate, Client, Lookup, DEFAULT_MAX_BYTES,
    HARD_MAX_BYTES,
};

// ---------------------------------------------------------------------------
// 1. Literal addresses: none of these may ever be fetched.
// ---------------------------------------------------------------------------

/// Every address that is this machine or its neighbourhood, spelled the way a feed,
/// a model or a person's paste might spell it. Grouped so a failure names the range
/// that stopped being refused instead of one url out of thirty.
fn local_urls() -> Vec<(&'static str, &'static str)> {
    vec![
        // 127.0.0.0/8 — the whole block, not only .0.1.
        ("loopback", "http://127.0.0.1:8080/"),
        ("loopback", "http://127.0.0.2/"),
        ("loopback", "http://127.255.255.254/"),
        // ::1 and the IPv6 loopback/unspecified forms.
        ("loopback v6", "http://[::1]/"),
        ("loopback v6", "http://[0:0:0:0:0:0:0:1]:8080/"),
        ("wildcard", "http://0.0.0.0/"),
        ("wildcard v6", "http://[::]/"),
        // 169.254.0.0/16 — link-local, and where every cloud metadata service lives.
        ("link-local", "http://169.254.169.254/latest/meta-data/"),
        ("link-local", "http://169.254.0.1/"),
        ("link-local v6", "http://[fe80::1]/"),
        ("link-local v6", "http://[febf:ffff::1]:80/"),
        // 10/8, 172.16/12, 192.168/16 — the LAN.
        ("lan", "http://10.0.0.1/"),
        ("lan", "http://10.255.255.255/"),
        ("lan", "http://172.16.0.1/"),
        ("lan", "http://172.31.255.255/"),
        ("lan", "http://192.168.0.1/"),
        ("lan", "http://192.168.255.255/"),
        ("lan v6", "http://[fd00::1]/"),
        ("lan v6", "http://[fc00::1]:8080/"),
        // 100.64/10 carrier-grade NAT, 198.18/15 benchmarking, multicast, broadcast.
        ("cgnat", "http://100.64.0.1/"),
        ("benchmark", "http://198.18.0.1/"),
        ("multicast", "http://224.0.0.1/"),
        ("broadcast", "http://255.255.255.255/"),
        // The same addresses again, carried inside IPv6: a v4-mapped literal, a
        // NAT64 literal and a 6to4 literal are all delivered to the v4 address they
        // contain, so the range that is refused above has to be refused here too.
        ("v4-mapped", "http://[::ffff:127.0.0.1]/"),
        ("v4-mapped", "http://[::ffff:169.254.169.254]/"),
        ("v4-mapped", "http://[::ffff:10.0.0.5]:8080/"),
        ("nat64", "http://[64:ff9b::7f00:1]/"),
        ("nat64", "http://[64:ff9b::a9fe:a9fe]/"),
        ("6to4", "http://[2002:7f00:1::]/"),
        ("6to4", "http://[2002:a9fe:a9fe::]/"),
        // A resolver would expand these into loopback; the gate refuses the spelling a
        // resolver would expand rather than guessing what it means.
        ("decimal", "http://2130706433/"),
        ("hex", "http://0x7f000001/"),
        ("hex octets", "http://0x7f.0.0.1/"),
        ("octal", "http://0177.0.0.1/"),
        ("short", "http://127.1/"),
        // A name is not an address this crate resolves, so it cannot be one it dials.
        ("name", "http://localhost:8080/"),
        ("name", "http://metadata.google.internal/"),
    ]
}

/// The literal matrix, against the two functions a caller actually uses: the verdict
/// and the fetch. `gate` is pure (no socket, no DNS), so this test is offline; the
/// two `Client::fetch` probes then show the verdict is what the entry point obeys —
/// a refusal that `fetch` ignored would be a verdict nobody read.
#[test]
fn no_address_inside_this_machine_or_its_lan_is_ever_fetched() {
    let client = Client::new();
    for (class, url) in local_urls() {
        let verdict = gate(url);
        assert!(
            !verdict.ok,
            "{class}: {url} was approved (host {:?} port {})",
            verdict.host, verdict.port
        );
        assert!(
            verdict.gated().is_none(),
            "{class}: {url} was approved and handed a request token"
        );
        assert!(
            !verdict.reason.trim().is_empty(),
            "{class}: {url} was refused with nothing a reader can act on"
        );
        // The entry point agrees, and it agrees without opening a socket: a refused
        // verdict returns before any connection, so `status` is still 0.
        let page = client.fetch(url, 1024);
        assert!(
            page.refused.is_some(),
            "{class}: {url} reached the transport instead of the gate"
        );
        assert_eq!(
            page.status, 0,
            "{class}: {url} was refused but a response was read"
        );
        assert!(
            page.text.is_empty(),
            "{class}: {url} put text on a refusal"
        );
    }
}

/// The app's own door — `Gated::loopback`, the token the model-server client uses —
/// is not a general "private network" door: it proves the address *is this machine*,
/// so a LAN address, the metadata service or a public address cannot be minted
/// through it. This matters because that token is the one class of request whose
/// resolver only checks "is this machine": if a LAN address could be minted, the
/// filter would have nothing left to refuse.
#[test]
fn a_lan_address_cannot_be_minted_as_this_machines_own_traffic() {
    // Genuinely this machine: the door must open, or the app's model traffic would
    // have no way to reach 127.0.0.1 at all.
    for url in [
        "http://127.0.0.1:8081/",
        "http://127.0.0.2/",
        "http://[::1]:8081/",
        "http://[::ffff:127.0.0.1]/",
    ] {
        assert!(
            atlas_guard::Gated::loopback(url).is_ok(),
            "{url} is this machine and the loopback door refused it"
        );
    }
    // Everything else: refused, including the ranges a "allow private" check would
    // have waved through.
    for (class, url) in [
        ("metadata", "http://169.254.169.254/latest/meta-data/"),
        ("lan", "http://10.0.0.5/"),
        ("lan", "http://172.16.4.4/"),
        ("lan", "http://192.168.1.1/"),
        ("link-local v6", "http://[fe80::1]/"),
        ("lan v6", "http://[fd00::1]/"),
        ("cgnat", "http://100.64.0.1/"),
        ("wildcard", "http://0.0.0.0/"),
        ("wildcard v6", "http://[::]/"),
        ("multicast", "http://224.0.0.1/"),
        ("broadcast", "http://255.255.255.255/"),
        ("nat64", "http://[64:ff9b::7f00:1]/"),
        ("6to4", "http://[2002:7f00:1::]/"),
        ("decimal", "http://2130706433/"),
        ("hex", "http://0x7f000001/"),
        ("octal", "http://0177.0.0.1/"),
        ("short", "http://127.1/"),
        ("name", "http://localhost/"),
        ("name", "http://metadata.google.internal/"),
        ("name", "http://127.0.0.1.nip.io/"),
        ("public", "http://93.184.216.34/"),
    ] {
        assert!(
            atlas_guard::Gated::loopback(url).is_err(),
            "{class}: {url} was minted as this machine's own traffic"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Names: the answer decides, not the spelling.
// ---------------------------------------------------------------------------

/// A resolver whose answers are this test's, so a name that points at this machine
/// can be staged without a DNS query. It also records what it was asked, because
/// "the name was never even resolved" is a stronger statement than "it was refused".
#[derive(Debug, Default)]
struct Answers {
    addresses: Vec<&'static str>,
    asked: Mutex<Vec<String>>,
}

impl Answers {
    fn answering(addresses: &[&'static str]) -> Arc<Answers> {
        Arc::new(Answers {
            addresses: addresses.to_vec(),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("the question log").clone()
    }
}

impl Lookup for Answers {
    fn addresses(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        self.asked
            .lock()
            .expect("the question log")
            .push(host.to_string());
        Ok(self
            .addresses
            .iter()
            .map(|text| {
                let ip: IpAddr = text.parse().expect("a literal address in the table");
                SocketAddr::new(ip, port)
            })
            .collect())
    }
}

/// A public name is judged on what it answers. `127.0.0.1.nip.io` and a rebinding
/// name are the same attack with different letterheads: the first answer is public
/// or the name is refused, and the answer that is *connected to* is the filtered one
/// (the resolver in the transport asks this same question again, which is what makes
/// the verdict early rather than merely advisory).
#[test]
fn a_public_name_that_answers_with_a_local_address_is_refused() {
    for answer in [
        "127.0.0.1",
        "127.0.0.2",
        "::1",
        "169.254.169.254",
        "10.0.0.5",
        "172.16.4.4",
        "192.168.1.1",
        "fe80::1",
        "fd00::1",
        "::ffff:127.0.0.1",
        "64:ff9b::a9fe:a9fe",
    ] {
        let lookup = Answers::answering(&[answer]);
        let refused = approved_addresses_with("http://www.reuters.com/story", lookup.as_ref());
        assert!(
            refused.is_err(),
            "a name answering {answer} was approved for a page: {:?}",
            refused.map(|a| a.addresses)
        );
        assert!(
            !lookup.asked().is_empty(),
            "the name was never resolved, so the refusal was not about its answer"
        );
    }

    // A multi-homed site stays readable on its public address, and the private answer
    // is dropped rather than averaged: one address in, one address out.
    let lookup = Answers::answering(&["93.184.216.34", "127.0.0.1", "169.254.169.254"]);
    let approved = approved_addresses_with("http://www.reuters.com/story", lookup.as_ref())
        .expect("a name with one public answer is a page");
    assert_eq!(
        approved.addresses,
        vec!["93.184.216.34:80".parse::<SocketAddr>().expect("a literal")],
        "the private answers were not dropped: {:?}",
        approved.addresses
    );
    assert_eq!(approved.port, 80);
    assert_eq!(lookup.asked(), vec!["www.reuters.com".to_string()]);
}

// ---------------------------------------------------------------------------
// 3. Redirects: a hop is gated before it is requested.
// ---------------------------------------------------------------------------

/// A `Location` is attacker-controlled text. The hop it describes is gated exactly
/// like a link from a feed — so a redirect cannot walk a public page onto this
/// machine, onto the metadata service, or into a protocol this client does not
/// speak. Every case here is the *cheap* half of the defence: a refusal means no
/// request line was written for it.
#[test]
fn a_redirect_can_only_land_where_the_fetch_may_go() {
    let origin = "https://www.reuters.com/world/story";
    let token = atlas_guard::Gated::verify(origin).expect("a public origin");

    for (class, location) in [
        ("loopback", "http://127.0.0.1/"),
        ("loopback", "http://127.0.0.1:8080/admin"),
        ("loopback", "//127.0.0.1/"),
        ("loopback", "HTTP://127.0.0.1/"),
        ("loopback v6", "http://[::1]/"),
        ("loopback v6", "http://[::ffff:127.0.0.1]/"),
        ("metadata", "http://169.254.169.254/latest/meta-data/iam/security-credentials/"),
        ("lan", "http://192.168.1.1/"),
        ("lan", "http://10.0.0.5:8080/"),
        ("cgnat", "http://100.64.0.1/"),
        ("decimal", "http://2130706433/"),
        ("userinfo", "http://www.reuters.com@127.0.0.1/"),
        ("service port", "https://www.reuters.com:22/"),
        ("file", "file:///etc/passwd"),
        ("file", "file://localhost/etc/passwd"),
        ("ftp", "ftp://www.reuters.com/a"),
        ("gopher", "gopher://127.0.0.1:70/"),
        ("javascript", "javascript:alert(document.cookie)"),
        ("data", "data:text/html,<script>fetch('http://127.0.0.1')</script>"),
        ("chrome", "chrome://settings/"),
    ] {
        assert!(
            check_hop(origin, location).is_err(),
            "{class}: a redirect to {location:?} was approved"
        );
        assert!(
            check_hop_from(&token, location).is_err(),
            "{class}: the transport's hop rule approved {location:?}"
        );
    }

    // A relative `Location` — including one that climbs — resolves *inside its own
    // origin*: `../../../../etc/passwd` is `https://www.reuters.com/etc/passwd`, not a
    // file and not another host. So the assertion is not "refused" but "same origin":
    // a climb cannot move the request to a host the gate did not approve.
    for location in ["../../../../etc/passwd", "/../../x", "?q=1", ""] {
        match check_hop(origin, location) {
            Ok(hop) => {
                let approved = hop.as_str();
                assert!(
                    approved.starts_with("https://www.reuters.com/"),
                    "a relative location {location:?} left the origin: {approved}"
                );
            }
            Err(reason) => assert!(
                location.is_empty(),
                "a relative location {location:?} was refused: {reason}"
            ),
        }
    }

    // The same rule for the app's own door: a loopback origin may stay on this
    // machine and may never leave it.
    let local = atlas_guard::Gated::loopback("http://127.0.0.1:8081/").expect("a loopback origin");
    for location in [
        "http://127.0.0.1:8082/",
        "http://127.0.0.1:8081/next",
    ] {
        assert!(
            check_hop_from(&local, location).is_ok(),
            "a loopback hop to {location} was refused: the local model server would be unusable"
        );
    }
    for (class, location) in [
        ("metadata", "http://169.254.169.254/latest/meta-data/"),
        ("lan", "http://10.0.0.5/"),
        ("service port", "http://127.0.0.1:22/"),
        ("file", "file:///etc/passwd"),
    ] {
        assert!(
            check_hop_from(&local, location).is_err(),
            "{class}: a loopback origin was walked to {location}"
        );
    }
    // Leaving this machine is allowed — a local service may redirect to the real
    // site — but only as a *public* request: the returned token is no longer the
    // loopback one, so the resolver that dials it switches from "only this machine"
    // to "never this machine". A token whose class did not change would be dialled
    // by the loopback agent, and the URL's own address would never be checked.
    let out = check_hop_from(&local, "https://www.reuters.com/world/story").expect("a public hop");
    assert!(
        !out.is_loopback(),
        "a hop off this machine kept the loopback class: {}",
        out.as_str()
    );
    assert_eq!(out.as_str(), "https://www.reuters.com/world/story");

    // And the public door is still open: a refusal for *everything* would also pass
    // the assertions above.
    assert!(check_hop(origin, "https://www.reuters.com/world/next").is_ok());
    assert!(check_hop(origin, "/next").is_ok());
}

// ---------------------------------------------------------------------------
// 4. Bounded: one page, one cap, one clock.
// ---------------------------------------------------------------------------

/// A one-connection-at-a-time loopback server, bound to `127.0.0.1:0` inside this
/// test process and closed by `Drop`. It is not a listener the app opens: nothing
/// outside this process can be reached through it, and it exists only so a hostile
/// response can be staged.
struct Reply {
    /// The status line it answers with.
    status: u16,
    /// The `Location` it sends, for the redirect probes.
    location: Option<&'static str>,
    /// The body it promises in `Content-Length` and then sends.
    body: Vec<u8>,
    /// When true the server accepts the request and then says nothing at all.
    silent: bool,
    /// When true the answer carries no `Content-Length` and the bytes keep coming:
    /// the shape a cap has to stop, because there is no number to trust.
    stream: bool,
}

struct Page {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    /// Connections accepted: the proof of what the client did *not* request.
    requests: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Page {
    fn serve(reply: Reply) -> Page {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral loopback port");
        let address = listener.local_addr().expect("the listener address");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let thread = {
            let stop = Arc::clone(&stop);
            let requests = Arc::clone(&requests);
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            requests.fetch_add(1, Ordering::SeqCst);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let mut request = [0u8; 2048];
                            let _ = stream.read(&mut request);
                            if reply.silent {
                                // Say nothing, hold the socket open, and let the
                                // client's clock give up: a slow-loris with no bytes.
                                while !stop.load(Ordering::SeqCst)
                                    && Instant::now() < deadline
                                {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                continue;
                            }
                            let location = match reply.location {
                                Some(where_to) => format!("Location: {where_to}\r\n"),
                                None => String::new(),
                            };
                            let length = if reply.stream {
                                String::new()
                            } else {
                                format!("Content-Length: {}\r\n", reply.body.len())
                            };
                            let head = format!(
                                "HTTP/1.1 {} {}\r\nContent-Type: text/html; charset=utf-8\r\n\
                                 {location}{length}Connection: close\r\n\r\n",
                                reply.status,
                                if (300..400).contains(&reply.status) {
                                    "Redirect"
                                } else {
                                    "OK"
                                },
                            );
                            let _ = stream.write_all(head.as_bytes());
                            let _ = stream.write_all(&reply.body);
                            let _ = stream.flush();
                            if reply.stream {
                                // No length, no end: the only thing that stops this is
                                // the client's cap and then the socket going away.
                                let chunk = [b'b'; 8 * 1024];
                                while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                                    if stream.write_all(&chunk).is_err() {
                                        break;
                                    }
                                }
                            }
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
        Page {
            address,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    /// The loopback token for this server: the guard's own door, which proves the
    /// address is this machine. The public gate refuses loopback on purpose, so this
    /// is the only way a test can reach a local server through the shipped client —
    /// and it is the same path the app's model traffic takes.
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn token(&self) -> atlas_guard::Gated {
        atlas_guard::Gated::loopback(&format!("http://{}/", self.address))
            .expect("a loopback verdict for this server")
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // The port this test opened, given back before the test returns: the serving
        // thread owns the listener, so asking it to stop and joining is the close.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The byte cap: a page that keeps sending is cut at the cap, and the cut is
/// *reported* rather than passed off as a whole page. This is what keeps one hostile
/// response from exhausting the window's memory.
#[test]
fn one_page_cannot_be_bigger_than_the_cap() {
    let cap = 64 * 1024;
    let page = Page::serve(Reply {
        status: 200,
        location: None,
        body: vec![b'a'; 3 * 1024 * 1024],
        silent: false,
        stream: false,
    });
    let client = Client::with_limits(cap, Duration::from_secs(10), Duration::from_secs(5));
    let started = Instant::now();
    let read = client.fetch_gated(&page.token(), cap);
    let elapsed = started.elapsed();

    assert_eq!(read.status, 200, "{:?}", read.error);
    assert_eq!(
        read.bytes, cap,
        "the cap is {} bytes but {} were kept",
        cap, read.bytes
    );
    assert!(read.text.len() >= cap, "the text is shorter than the bytes");
    assert!(
        read.truncated,
        "a 3 MB body under a 64 kB cap was not reported as truncated"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a capped read took {elapsed:?}"
    );

    // The caller cannot ask for more than the client allows: `max_bytes` is clamped
    // (0 means the default, and anything above the client's ceiling is that ceiling),
    // so a hostile tool call cannot raise its own cap.
    assert_eq!(
        atlas_net::clamp_bytes(usize::MAX),
        HARD_MAX_BYTES,
        "a caller's cap is not clamped to the hard ceiling"
    );
    assert!(DEFAULT_MAX_BYTES <= HARD_MAX_BYTES);
    // The same answer with no `Content-Length` at all: the bytes never end, so there
    // is no number to trust and the cap is the only thing that stops the read.
    let endless = Page::serve(Reply {
        status: 200,
        location: None,
        body: Vec::new(),
        silent: false,
        stream: true,
    });
    let started = Instant::now();
    let streamed = client.fetch_gated(&endless.token(), cap);
    let streamed_elapsed = started.elapsed();
    assert_eq!(streamed.status, 200, "{:?}", streamed.error);
    assert_eq!(
        streamed.bytes, cap,
        "an endless body kept {} bytes under a {cap} byte cap",
        streamed.bytes
    );
    assert!(streamed.truncated, "an endless body was not reported as cut");
    assert!(
        streamed_elapsed < Duration::from_secs(5),
        "an endless body held the fetch for {streamed_elapsed:?}"
    );

    let via_default = client.fetch_gated(&page.token(), 0);
    assert_eq!(
        via_default.bytes,
        DEFAULT_MAX_BYTES.min(cap),
        "a zero cap did not mean the default (clamped by the client's ceiling)"
    );
}

/// The clock: a server that accepts the request and then says nothing must not hang
/// the window. One budget covers the whole fetch, so the refusal arrives inside it —
/// measured, not assumed.
#[test]
fn one_page_cannot_be_longer_than_the_budget() {
    let budget = Duration::from_millis(400);
    let page = Page::serve(Reply {
        status: 200,
        location: None,
        body: Vec::new(),
        silent: true,
        stream: false,
    });
    let client = Client::with_limits(DEFAULT_MAX_BYTES, budget, Duration::from_millis(200));
    let started = Instant::now();
    let read = client.fetch_gated(&page.token(), DEFAULT_MAX_BYTES);
    let elapsed = started.elapsed();

    let why = read
        .error
        .clone()
        .unwrap_or_else(|| panic!("a silent server produced {read:?}"));
    assert!(
        elapsed < budget + Duration::from_millis(1500),
        "a silent server held the fetch for {elapsed:?}, budget {budget:?}"
    );
    assert!(
        why.to_lowercase().contains("time") || why.to_lowercase().contains("took"),
        "the sentence does not say the fetch ran out of time: {why}"
    );
    assert!(read.text.is_empty(), "a timeout produced page text");
    assert_eq!(read.status, 0, "a timeout produced a status");
}

/// The redirect attack, end to end and on the wire: a server this test owns answers
/// `302` with a `Location` pointing at the metadata service, and the shipped client
/// must refuse it *without a second request*. `requests() == 1` is the measurement:
/// the hop was gated before it was requested, so nothing was sent to the address the
/// redirect named (and nothing could have been — the test never opens it).
#[test]
fn a_redirect_into_this_machine_is_refused_without_a_second_request() {
    for (class, location) in [
        ("metadata", "http://169.254.169.254/latest/meta-data/"),
        // A loopback origin may stay on this machine (that is the model server's
        // path), so the case that matters here is a *service port* on it: the port
        // policy is what refuses that, and it is refused before a request too.
        ("service port", "http://127.0.0.1:22/"),
        ("lan", "http://192.168.1.1/"),
        ("file", "file:///etc/passwd"),
    ] {
        let server = Page::serve(Reply {
            status: 302,
            location: Some(location),
            body: Vec::new(),
            silent: false,
            stream: false,
        });
        let client = Client::with_limits(DEFAULT_MAX_BYTES, Duration::from_secs(5), Duration::from_secs(2));
        let page = client.fetch_gated(&server.token(), DEFAULT_MAX_BYTES);

        let refused = page
            .refused
            .clone()
            .unwrap_or_else(|| panic!("{class}: a redirect to {location} was followed: {page:?}"));
        assert!(
            refused.to_lowercase().contains("redirect") || refused.contains("hop"),
            "{class}: the refusal does not name the redirect: {refused}"
        );
        assert_eq!(
            server.requests(),
            1,
            "{class}: the client made {} requests for a refused hop",
            server.requests()
        );
        assert_eq!(page.status, 0, "{class}: a refused hop produced a status");
        assert!(page.text.is_empty(), "{class}: a refused hop produced text");

        // The local server's own body is never read either: the 302 was the answer,
        // and the page the model asked for does not exist.
        assert_eq!(page.redirects, 0, "{class}: a refused hop was counted as followed");
    }
}

/// A scheme this client does not speak is not a fetch: `file://` would read the
/// window's own disk, and `data:`/`javascript:` would make the "page" a program. The
/// check is the entry point's, not only the hop's — a URL from a feed, a search
/// result or a model-chosen link arrives here first.
#[test]
fn a_non_http_scheme_is_refused_at_the_entry_point() {
    let client = Client::new();
    for (class, url) in [
        ("file", "file:///etc/passwd"),
        ("file", "file://localhost/etc/passwd"),
        ("file", "file:///Users/atlas/data/atlas.db"),
        ("javascript", "javascript:fetch('http://127.0.0.1')"),
        ("data", "data:text/html,<h1>x</h1>"),
        ("gopher", "gopher://127.0.0.1:70/_x"),
        ("ftp", "ftp://ftp.example.org/a"),
        ("chrome", "chrome://settings/"),
        ("about", "about:blank"),
        ("scheme-relative", "//127.0.0.1/"),
        ("no scheme", "www.reuters.com/story"),
        ("empty", ""),
    ] {
        let verdict = gate(url);
        assert!(!verdict.ok, "{class}: {url:?} was approved");
        assert!(
            verdict.gated().is_none(),
            "{class}: {url:?} was approved and handed a token"
        );
        let page = client.fetch(url, 1024);
        assert!(
            page.refused.is_some(),
            "{class}: {url:?} reached the transport"
        );
        assert_eq!(page.status, 0, "{class}: {url:?} produced a status");
    }
}

/// Measurements taken while writing this file, pinned so a change of mind is visible.
///
/// * a **port 0** URL is a *port* question, not an address question: the guard's
///   grammar allows `:0`, so it is approved as a URL and the connect fails. Nothing
///   is reachable that way (the kernel refuses port 0), so this is recorded, not
///   fixed here — the sentence a caller gets is the transport's, not the gate's.
/// * an **https → http** downgrade of the same host is followed: the guard judges
///   the address, not the transport's confidentiality. That is the same choice a
///   browser makes for most sites, and refusing it here would break real publishers
///   that redirect their own secure URL to a CDN over http.
#[test]
fn the_two_edges_that_are_not_address_questions_are_measured() {
    // Port 0 is refused by the *gate*, not left to a failed connect: "any port" is
    // not a page, and the sentence has to come from the decision.
    for url in [
        "http://www.reuters.com:0/",
        "https://www.reuters.com:0/story",
        "http://127.0.0.1:0/",
    ] {
        let verdict = gate(url);
        assert!(!verdict.ok, "{url} was approved on port 0");
        assert!(
            verdict.reason.to_lowercase().contains("port"),
            "{url} was refused without naming the port: {}",
            verdict.reason
        );
    }

    let origin = "https://www.reuters.com/world/story";
    let downgrade = check_hop(origin, "http://www.reuters.com/world/story");
    assert!(
        downgrade.is_ok(),
        "https -> http is now refused; that is a policy change worth its own test"
    );
    assert_eq!(
        downgrade.expect("measured above").as_str(),
        "http://www.reuters.com/world/story"
    );
    // The downgrade is still the *same address*: the guard re-judges the host, so a
    // downgrade to a private host is refused like any other hop.
    assert!(check_hop(origin, "http://127.0.0.1/world/story").is_err());
}
