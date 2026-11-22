//! The verdict: may this URL leave the machine, and in which exact form?
//!
//! This file decides *nothing* about what is safe. `atlas_guard::urlsafe::check`
//! makes that call; what is decided here is the plumbing around it: the canonical
//! request line (percent-encoded, fragment dropped, default port normalised), the
//! port policy the guard deliberately does not carry, and — in [`resolve_gate`] —
//! whether a public *name* answers with an address a page may reach.
//!
//! The address half is a verdict *early*, not the verdict that holds: the answer can
//! change before the socket is opened, so the same filter runs again inside the
//! transport ([`crate::resolver`]). One rule, two places it is asked, and the second
//! one is the one a rebinding name cannot get past.
//!
//! It does not: open a socket, follow a redirect (that is [`crate::hop`]), read a
//! body or keep any state. A refusal never carries a URL, so nothing downstream can
//! fetch one by accident.

use std::net::{IpAddr, SocketAddr};

use atlas_guard::Gated;
use serde::Serialize;
use url::Url;

use crate::resolver::{addresses_for, approved, nothing_allowed, Class, Lookup, SystemLookup};

/// `collector.MAX_URL_CHARS` (also enforced by `atlas_guard::MAX_URL_CHARS`).
pub const MAX_URL_CHARS: usize = 2048;

/// `collector.BLOCKED_PORTS` — service ports, never a news site. Port numbers are
/// transport policy, not an address decision, so they live here rather than in
/// `atlas-guard` (whose gate deliberately allows a custom port for the app's own
/// loopback server).
pub const BLOCKED_PORTS: &[u16] = &[
    22, 23, 25, 110, 143, 389, 445, 465, 587, 636, 873, 993, 995, 1433, 1521, 2049, 2375, 2376,
    3306, 5432, 5900, 6379, 7001, 9200, 9300, 11211, 27017,
];

/// One URL verdict. The decision itself is `atlas-guard`'s; the canonical URL is
/// this crate's (a request line has to be ASCII and percent-encoded).
///
/// The canonical form is not a `pub` field any more, and the only way to get the
/// string a request is built from is [`Verdict::gated`], which hands back the
/// `atlas_guard::Gated` the gate produced. A request line therefore cannot come
/// from a `String` someone assembled: it comes from the verdict.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    /// May this URL be requested?
    pub ok: bool,
    /// The canonical form actually requested (empty when refused).
    url: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Why it was refused, in words a log line may carry.
    pub reason: String,
    /// The URL is a literal private/loopback/link-local address.
    pub is_private_ip: bool,
    /// The token `atlas-net`'s transport takes instead of a `&str`.
    #[serde(skip)]
    gated: Option<Gated>,
}

impl Verdict {
    fn refused(reason: impl Into<String>) -> Verdict {
        Verdict {
            ok: false,
            url: String::new(),
            scheme: String::new(),
            host: String::new(),
            port: 0,
            reason: reason.into(),
            is_private_ip: false,
            gated: None,
        }
    }

    /// The canonical URL this verdict is about (empty when refused).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The approved value a request may be built from; `None` when refused.
    ///
    /// `Some` implies `ok`, so a caller that needs a `&str` for a log line has
    /// no reason to reach for [`Verdict::url`] at all.
    pub fn gated(&self) -> Option<&Gated> {
        self.gated.as_ref()
    }

    /// The wire shape: the guard's fields, plus `error`/`source` so
    /// `collector.url_gate()` can read it unchanged.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": self.ok,
            "url": self.url,
            "scheme": self.scheme,
            "host": self.host,
            "port": self.port,
            "reason": self.reason,
            "error": self.reason,
            "is_private_ip": self.is_private_ip,
            "source": "rust",
        })
    }
}

/// The refusal a service port earns, or `None` when the port may be requested.
///
/// One function, two doors: [`gate`] (a public page) and the app's own loopback
/// traffic both refuse [`BLOCKED_PORTS`], so a redirect cannot reach ssh either.
pub(crate) fn service_port(port: u16) -> Option<String> {
    if BLOCKED_PORTS.contains(&port) {
        Some(format!("port {} is a service port, not a web page", port))
    } else {
        None
    }
}

/// One canonical request line: the exact bytes that may be sent, and the port they
/// go to.
///
/// A request line has to be ASCII (the path a feed hands us may carry raw UTF-8),
/// so this percent-encodes it, normalises the origin and drops the fragment — the
/// fragment is never sent, and keeping it would make two request lines out of one
/// page. It is the only place a URL becomes a request line, so a `Verdict` and the
/// transport's loopback door cannot disagree about what is being asked for.
#[derive(Debug, Clone)]
pub(crate) struct RequestLine {
    /// The canonical form.
    pub url: String,
    /// The port it asks for, with the scheme's default filled in.
    pub port: u16,
}

/// [`RequestLine`] for one URL, or why it cannot be one.
pub(crate) fn request_line(raw: &str) -> Result<RequestLine, String> {
    let mut parsed = Url::parse(raw.trim()).map_err(|e| format!("unusable url ({})", e))?;
    parsed.set_fragment(None);
    let port = parsed.port_or_known_default().unwrap_or(0);
    Ok(RequestLine {
        url: parsed.as_str().to_string(),
        port,
    })
}

/// Verdict for one outbound URL — `collector.url_gate(url)` without the DNS step.
///
/// The gate is not re-implemented here: `atlas_guard::urlsafe::check` decides.
/// What this function adds is (a) the port policy and (b) the canonical form,
/// which is gated again so the guard has seen the exact request line.
///
/// No socket is opened: this is the pure decision, which is why the bridge can
/// call it under a 3 s timeout and the tests can call it offline.
pub fn gate(raw: &str) -> Verdict {
    let check = atlas_guard::urlsafe::check(raw, false);
    if !check.ok {
        return Verdict::refused(check.reason);
    }
    if let Some(reason) = service_port(check.port) {
        return Verdict::refused(reason);
    }
    // The canonical form is put in front of the guard again rather than trusted by
    // construction: percent-encoding a path is exactly where a second opinion is
    // cheap and worth having.
    let line = match request_line(raw) {
        Ok(line) => line,
        Err(reason) => return Verdict::refused(reason),
    };
    let again = atlas_guard::urlsafe::check(&line.url, false);
    if !again.ok {
        return Verdict::refused(again.reason);
    }
    if let Some(reason) = service_port(again.port) {
        return Verdict::refused(reason);
    }
    // The token the transport takes. Built by the guard from the canonical
    // string, so the URL that is requested is the URL that was approved — three
    // verdicts (raw, canonical, token) and not one of them this crate's rule.
    let gated = match Gated::verify(&line.url) {
        Ok(gated) => gated,
        Err(reason) => return Verdict::refused(reason),
    };
    Verdict {
        ok: true,
        url: line.url,
        scheme: again.scheme,
        host: again.host,
        port: again.port,
        reason: String::new(),
        is_private_ip: again.is_private_ip,
        gated: Some(gated),
    }
}

/// The gate's own door for the app's local traffic, as a request line.
///
/// [`Gated::loopback`] proves the *address* is this machine; this adds the two
/// things the transport needs before it may write a request line: the canonical
/// form and the port policy. A loopback hop to `127.0.0.1:22` is still a refusal.
///
/// It is deliberately not `pub`: the only caller is [`crate::fetch`], which arrives
/// here holding a token the guard already minted. Nothing in this crate can turn a
/// `&str` into loopback traffic.
pub(crate) fn loopback_request(token: &Gated) -> Result<(RequestLine, Gated), String> {
    if !token.is_loopback() {
        return Err("not a loopback verdict".to_string());
    }
    let line = request_line(token.as_str())?;
    if let Some(reason) = service_port(line.port) {
        return Err(reason);
    }
    let gated = Gated::loopback(&line.url)?;
    Ok((line, gated))
}

/// Does one resolved address belong in a request of this class? The classifier is
/// the guard's: the address is handed back to it as a URL (public) or to its address
/// verdict (loopback), and that verdict is the answer. This crate does not carry a
/// range table of its own — a second copy of the guard's answer is a bug.
pub(crate) fn address_verdict(ip: IpAddr, port: u16, class: Class) -> Option<String> {
    match class {
        Class::Public => address_refused(ip, port),
        Class::Loopback => {
            if address_is_loopback(ip) {
                None
            } else {
                Some("not this machine: a loopback request may not leave it".to_string())
            }
        }
    }
}

/// The public-address verdict for one resolved address.
fn address_refused(ip: IpAddr, port: u16) -> Option<String> {
    let probe = match ip {
        IpAddr::V4(v4) => format!("http://{}:{}/", v4, port),
        IpAddr::V6(v6) => format!("http://[{}]:{}/", v6, port),
    };
    let check = atlas_guard::urlsafe::check(&probe, false);
    if check.ok {
        None
    } else {
        Some(check.reason)
    }
}

/// True only for an address the guard's own `address` verdict calls loopback.
fn address_is_loopback(ip: IpAddr) -> bool {
    atlas_guard::address::check(&ip.to_string()).loopback
}

/// The addresses a public page URL may dial, and the name they answer for.
///
/// For the one caller that does not open the socket itself: the browser hands the
/// URL to Chromium, which resolves names in its own process — so the gate's answer
/// about the *address* has to be asked for separately, before the URL is handed
/// over, or a name that rebinds to `127.0.0.1` is fetched by Chrome having been
/// judged on its first answer ([`crate::resolver`] closes that window for this
/// crate's own transport; a browser child has to be told).
///
/// It is not a permission: the URL is judged by `atlas_guard` before anyone gets
/// here, and what this returns is the guard's classification of *addresses*. The
/// filter is the one function the transport uses ([`approved`]), so the browser and
/// the HTTP client cannot disagree about which answer is reachable.
pub fn approved_addresses(raw: &str) -> Result<Addresses, String> {
    approved_addresses_with(raw, &SystemLookup)
}

/// [`approved_addresses`] with the answers coming from `lookup`.
///
/// One implementation for the system resolver and for a test's scripted one, so the
/// case that matters — a name that answers with `127.0.0.1` — is staged against the
/// shipped path instead of a copy of it.
pub fn approved_addresses_with(raw: &str, lookup: &dyn Lookup) -> Result<Addresses, String> {
    let line = request_line(raw)?;
    let parsed = Url::parse(&line.url).map_err(|e| format!("unusable url ({})", e))?;
    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "the url names no host".to_string())?
        .to_string();
    let addresses = addresses_for(&host, line.port, lookup)?;
    let (kept, refusal) = approved(&addresses, Class::Public, line.port);
    if kept.is_empty() {
        return Err(nothing_allowed(
            &host,
            Class::Public,
            refusal.as_deref().unwrap_or("no address"),
            addresses.len(),
        ));
    }
    Ok(Addresses {
        host,
        port: line.port,
        addresses: kept,
    })
}

/// One name's answers, as the guard approved them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addresses {
    /// The name that was asked (a literal address is answered as itself).
    pub host: String,
    /// The port the answers are for — the URL's port, which is part of the address
    /// verdict, and never the port of a re-written request.
    pub port: u16,
    /// Every answer a public page may reach, in the resolver's order. Never empty:
    /// an answer the guard refuses every part of is an `Err`, not an empty `Vec`.
    pub addresses: Vec<SocketAddr>,
}


/// `collector.url_gate(..., resolve=True)` — also refuse a public *name* whose DNS
/// answer is an address a page may not reach.
///
/// This runs at egress (inside [`crate::Client::fetch`] and on every hop), never in
/// [`gate`], so nothing socket-shaped happens in a test. A name that does not
/// resolve is allowed here: the request itself will fail, exactly as in Python.
///
/// It is an early answer, not the load-bearing one: the resolver in
/// [`crate::resolver`] asks the same question again where the socket is made, with
/// the same filter. What this buys is a refusal *before* a connection is attempted
/// and a sentence that names the address.
pub fn resolve_gate(verdict: &Verdict) -> Verdict {
    resolve_gate_with(verdict, &SystemLookup)
}

/// [`resolve_gate`] with the answers coming from `lookup`.
///
/// One implementation for the system resolver and for a test's scripted one, so the
/// test that stages a rebinding name exercises the shipped path rather than a copy of
/// it.
pub(crate) fn resolve_gate_with(verdict: &Verdict, lookup: &dyn Lookup) -> Verdict {
    if !verdict.ok || verdict.host.is_empty() {
        return verdict.clone();
    }
    // A private literal was already refused by `gate`; the guard's answer is not
    // re-derived here.
    if verdict.is_private_ip {
        return verdict.clone();
    }
    let addresses = match addresses_for(&verdict.host, verdict.port, lookup) {
        Ok(addresses) => addresses,
        // It does not resolve: the request will say so, in the transport's words.
        Err(_) => return verdict.clone(),
    };
    let (kept, refusal) = approved(&addresses, Class::Public, verdict.port);
    if kept.is_empty() {
        return Verdict::refused(format!(
            "host {} resolves to the private address {} ({})",
            verdict.host,
            addresses
                .first()
                .map(|address| address.ip().to_string())
                .unwrap_or_default(),
            refusal.unwrap_or_else(|| "not publicly routable".to_string())
        ));
    }
    verdict.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hop::check_hop;

/// Every address class the gate must refuse. The first assertion in the test is
/// always "atlas-guard refuses it too": if one of these ever passes the guard,
/// the failure belongs to the guard, not to this crate — but it fails loudly here
/// either way, because a URL the guard refuses must never be fetched.
const REFUSED: &[(&str, &str)] = &[
    ("ipv4 loopback", "http://127.0.0.1/"),
    ("ipv4 loopback with port", "http://127.0.0.1:8787/api/status"),
    ("loopback name", "http://localhost:8080/"),
    ("loopback name, trailing dot", "http://localhost./"),
    ("ipv6 loopback", "http://[::1]/"),
    ("ipv6 loopback, port", "http://[::1]:8787/"),
    ("ipv4 link-local", "http://169.254.169.254/latest/meta-data/"),
    ("ipv6 link-local", "http://[fe80::1]/"),
    ("ipv4 private /8", "http://10.0.0.5/"),
    ("ipv4 private /12", "http://172.16.0.1/"),
    ("ipv4 private /16", "http://192.168.1.1/"),
    ("ipv6 unique local", "http://[fc00::1]/"),
    ("ipv6 unique local fd", "http://[fd12:3456:789a::1]/"),
    ("unspecified ipv4", "http://0.0.0.0/"),
    ("unspecified ipv6", "http://[::]/"),
    ("ipv4-mapped loopback", "http://[::ffff:127.0.0.1]/"),
    ("integer ip literal", "http://2130706433/"),
    ("short ip literal", "http://127.1/"),
    ("userinfo trick", "http://user:pass@example.com/"),
    ("userinfo after host", "http://example.com@127.0.0.1/"),
    ("private name", "http://db.internal/"),
    ("lan name", "http://nas.lan/"),
    ("file scheme", "file:///etc/passwd"),
    ("gopher scheme", "gopher://example.com/"),
    ("data scheme", "data:text/html,<b>hi</b>"),
    ("javascript scheme", "javascript:alert(1)"),
    ("ftp scheme", "ftp://example.com/x"),
    ("no scheme", "example.com/path"),
    ("empty url", ""),
    ("whitespace in url", "http://example.com/a b"),
    ("port out of range", "http://example.com:99999/"),
    ("missing host", "http:///path"),
];

#[test]
fn every_refused_class_is_refused_by_the_guard_and_here() {
    for (label, url) in REFUSED {
        let native = atlas_guard::urlsafe::check(url, false);
        assert!(!native.ok, "{}: atlas-guard allowed {:?}", label, url);
        let verdict = gate(url);
        assert!(!verdict.ok, "{}: atlas-net allowed {:?}", label, url);
        assert!(
            !verdict.reason.is_empty(),
            "{}: a refusal without a reason",
            label
        );
        assert!(
            verdict.url().is_empty(),
            "{}: a refused verdict carried a url",
            label
        );
    }
}


#[test]
fn service_ports_are_refused_even_on_a_public_host() {
    for port in [22u16, 25, 3306, 6379, 9200, 11211, 27017] {
        let url = format!("https://news.example.com:{}/x", port);
        let native = atlas_guard::urlsafe::check(&url, false);
        assert!(native.ok, "port {}: the guard is expected to allow it", port);
        let verdict = gate(&url);
        assert!(!verdict.ok, "port {}: atlas-net allowed it", port);
        assert!(verdict.reason.contains("service port"), "{}", verdict.reason);
    }
}


#[test]
fn public_urls_pass_and_are_canonicalised() {
    for url in [
        "https://example.com",
        "https://www.theguardian.com/world/2026/nov/21/example?x=1#frag",
        "http://example.com/a/b/",
    ] {
        let verdict = gate(url);
        assert!(verdict.ok, "{:?} was refused: {}", url, verdict.reason);
        assert!(verdict.url().starts_with("http"), "{}", verdict.url());
        assert!(!verdict.host.is_empty());
        assert!(verdict.port == 80 || verdict.port == 443, "{}", verdict.port);
        assert!(verdict.reason.is_empty());
    }
    // A bare origin gains its path, and the canonical form is what gets fetched.
    assert_eq!(
        gate("https://example.com").url(),
        "https://example.com/".to_string()
    );
    // A default port is normalised away, and the port survives on a custom one.
    assert_eq!(
        gate("https://example.com:443/a").url(),
        "https://example.com/a".to_string()
    );
    assert_eq!(gate("http://example.com:8080/a").port, 8080);
}


#[test]
fn a_fragment_is_never_part_of_the_request() {
    // `#section` is client-side only: it is not sent, so it is not requested and
    // two links to the same page with different anchors are one request line.
    assert_eq!(
        gate("https://example.com/a#section-2").url(),
        "https://example.com/a"
    );
    assert_eq!(
        check_hop("https://example.com/a", "/b#top").unwrap().as_str(),
        "https://example.com/b"
    );
    // The query string is part of the request and survives.
    assert_eq!(
        gate("https://example.com/a?x=1&y=2#z").url(),
        "https://example.com/a?x=1&y=2"
    );
}


#[test]
fn a_non_ascii_path_is_percent_encoded_before_it_is_requested() {
    let verdict = gate("https://example.com/résumé");
    assert!(verdict.ok, "{}", verdict.reason);
    assert!(verdict.url().is_ascii(), "{}", verdict.url());
    assert!(verdict.url().contains("%C3%A9"), "{}", verdict.url());
    // The guard has seen the canonical form too, so this is not a bypass.
    assert!(atlas_guard::urlsafe::check(verdict.url(), false).ok);
}


#[test]
fn a_refusal_never_carries_a_url_to_request() {
    let verdict = gate("http://169.254.169.254/latest/meta-data/");
    let json = verdict.to_json();
    assert_eq!(json["ok"], serde_json::json!(false));
    assert_eq!(json["url"], serde_json::json!(""));
    assert_eq!(json["source"], serde_json::json!("rust"));
    assert_eq!(json["error"], json["reason"]);
}


/// The verdict is pure: no socket, no DNS, so the bridge can ask per URL. This
/// measures what that costs instead of assuming it is free.
#[test]
fn a_verdict_costs_far_less_than_the_request_it_protects() {
    let urls = [
        "https://www.theguardian.com/world/2026/nov/21/example-article-slug?ref=rss",
        "http://127.0.0.1:8787/api/status",
        "https://example.com/a/b/c",
        "file:///etc/passwd",
    ];
    let rounds = 2_000;
    let start = std::time::Instant::now();
    let mut allowed = 0usize;
    for _ in 0..rounds {
        for url in urls {
            if gate(url).ok {
                allowed += 1;
            }
        }
    }
    let elapsed = start.elapsed();
    let calls = rounds * urls.len();
    let per_call_us = elapsed.as_secs_f64() * 1e6 / calls as f64;
    println!(
        "{} verdicts in {:.2} ms -> {:.2} µs each",
        calls,
        elapsed.as_secs_f64() * 1e3,
        per_call_us
    );
    assert_eq!(allowed, rounds * 2);
    // A verdict is microseconds; a page is tens of milliseconds. Anything near a
    // millisecond would mean the guard is being re-implemented somewhere.
    assert!(per_call_us < 200.0, "{:.1} µs per verdict", per_call_us);
}


#[test]
fn a_request_line_is_ascii_and_normalised() {
    // The same canonicalisation `gate` uses, reachable on its own so the
    // transport's loopback door cannot build a different request line.
    let line = request_line("https://example.com/résumé?x=1#frag").unwrap();
    assert!(line.url.is_ascii(), "{}", line.url);
    assert_eq!(line.url, "https://example.com/r%C3%A9sum%C3%A9?x=1");
    assert_eq!(line.port, 443);
    assert_eq!(request_line("http://example.com/").unwrap().port, 80);
    assert_eq!(request_line("http://example.com:8080/x").unwrap().port, 8080);
    assert!(request_line("not a url").is_err());
}


#[test]
fn the_loopback_door_needs_a_loopback_verdict_and_keeps_the_port_policy() {
    // The app's own traffic: canonical, and a service port is still refused.
    let loopback = atlas_guard::Gated::loopback("http://127.0.0.1:8080/a#f").unwrap();
    let (line, token) = loopback_request(&loopback).unwrap();
    assert_eq!(line.url, "http://127.0.0.1:8080/a");
    assert_eq!(line.port, 8080);
    assert!(token.is_loopback());
    // Port 22 on this machine is still ssh, not a page.
    let ssh = atlas_guard::Gated::loopback("http://127.0.0.1:22/").unwrap();
    let reason = loopback_request(&ssh).expect_err("port 22 must be refused");
    assert!(reason.contains("service port"), "{}", reason);
    // A public verdict is not loopback traffic and cannot borrow the door.
    let public = atlas_guard::Gated::verify("https://example.com/a").unwrap();
    assert!(loopback_request(&public).is_err());
}

/// A `Lookup` whose answer is written down, so a name can be made to answer the
/// way a rebinding name does: publicly when the verdict is taken, privately when
/// the connection is made. `resolver.rs`'s own `Scripted` is private to that
/// module; this is the same idea, one file over, and it exists because
/// [`resolve_gate_with`] documents "the test that stages a rebinding name" — this
/// is that test.
#[derive(Debug)]
struct Answers(Result<Vec<std::net::SocketAddr>, String>);

impl Lookup for Answers {
    fn addresses(&self, _host: &str, _port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        self.0.clone()
    }
}

fn answered(addresses: &[&str], port: u16) -> Answers {
    Answers(Ok(addresses
        .iter()
        .map(|text| {
            std::net::SocketAddr::new(
                text.parse::<std::net::IpAddr>().expect("a test address"),
                port,
            )
        })
        .collect()))
}

#[test]
fn a_public_name_that_answers_with_a_private_address_is_refused_at_egress() {
    // THREAT-MODEL §4.1, the DNS half: "`resolve_gate` refuses a public name whose
    // DNS answer is private". Nothing pinned it before: the static list above is
    // about *literals and names* the guard can judge without asking anyone, and the
    // control that needs a resolver answer — attacker.example resolving to
    // 127.0.0.1, or to the cloud metadata address — had no test at all. The attack
    // is the one the document names: a name that is public at verdict time and
    // private at connect time.
    let public = gate("https://news.example.com/story");
    assert!(public.ok, "{}", public.reason);
    for address in ["127.0.0.1", "10.1.2.3", "192.168.1.1", "172.16.0.1", "169.254.169.254", "::1", "fe80::1"] {
        let verdict = resolve_gate_with(&public, &answered(&[address], 443));
        assert!(!verdict.ok, "{} resolved to {} and was still allowed", public.host, address);
        assert!(
            verdict.url().is_empty(),
            "{} carried a url to request: {:?}",
            address,
            verdict.url()
        );
        assert!(
            verdict.reason.contains("resolves to the private address"),
            "{}: the refusal does not say what happened: {}",
            address,
            verdict.reason
        );
        assert!(verdict.reason.contains(address), "{} missing from: {}", address, verdict.reason);
    }

    // A multi-homed name stays readable: one public answer is enough, because the
    // resolver repeats this filter where the socket is made. Refusing the whole
    // name would make a site behind a private mirror unreadable — a feature lost to
    // make a check pass.
    let mixed = answered(&["10.1.2.3", "93.184.216.34"], 443);
    assert!(
        resolve_gate_with(&public, &mixed).ok,
        "a name with one public answer must stay readable"
    );

    // A name that does not resolve is not refused here: the request itself says so,
    // in the transport's own words (the Python did the same, and a DNS hiccup must
    // not read as "this address is forbidden").
    let nothing = Answers(Err("the name does not resolve".to_string()));
    assert!(resolve_gate_with(&public, &nothing).ok);

    // The egress check is not a second gate: a verdict that was already refused, and
    // a private literal the guard already judged, are handed back untouched so the
    // reason the reader sees is the guard's.
    let refused = gate("http://127.0.0.1/");
    assert!(!refused.ok);
    let again = resolve_gate_with(&refused, &answered(&["93.184.216.34"], 80));
    assert!(!again.ok);
    assert_eq!(again.reason, refused.reason, "the guard's reason was replaced");
}

/// The same filter, asked for by the caller that does not open the socket: a
/// browser child resolves names in its own process, so
/// [`approved_addresses_with`] is what stands between a rebinding name and the page
/// Chrome would fetch. Nothing pinned that answer before — the browser had no
/// address check at all, because the verdict it applied was about the URL.
#[test]
fn the_answer_the_browser_gets_is_the_answer_the_transport_would_dial() {
    // A name that answers with this machine is refused, in the transport's words.
    for address in ["127.0.0.1", "10.1.2.3", "169.254.169.254", "::1"] {
        let reason = approved_addresses_with("https://news.example.com/story", &answered(&[address], 443))
            .expect_err("an address a public page may not reach must be refused");
        assert!(reason.contains(address), "{}: {}", address, reason);
        assert!(reason.contains("may not reach"), "{}", reason);
    }

    // One public answer: the name, the port and the address come back.
    let one = approved_addresses_with(
        "https://news.example.com/story",
        &answered(&["93.184.216.34"], 443),
    )
    .expect("a public answer");
    assert_eq!(one.host, "news.example.com");
    assert_eq!(one.port, 443);
    assert_eq!(one.addresses.len(), 1);
    assert_eq!(one.addresses[0].ip().to_string(), "93.184.216.34");

    // Multi-homed stays readable: the private answer is dropped, never dialled.
    let mixed = approved_addresses_with(
        "https://news.example.com/story",
        &answered(&["10.1.2.3", "93.184.216.34"], 443),
    )
    .expect("one public answer is enough");
    assert_eq!(mixed.addresses.len(), 1);
    assert_eq!(mixed.addresses[0].ip().to_string(), "93.184.216.34");

    // A name with no answer is an error here, because the caller's next step is to
    // hand the name to something else that will resolve it again.
    assert!(approved_addresses_with(
        "https://news.example.com/story",
        &Answers(Err("the name does not resolve".to_string()))
    )
    .is_err());

    // A literal is its own answer and no lookup happens: the URL the gate approved
    // is the address the browser may reach.
    let literal = approved_addresses_with(
        "https://93.184.216.34/story",
        &Answers(Err("a literal must not be looked up".to_string())),
    )
    .expect("a public literal");
    assert_eq!(literal.addresses.len(), 1);
}

}
