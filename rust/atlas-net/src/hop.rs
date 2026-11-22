//! The redirect decision: may this hop be followed, and where does it land?
//!
//! A `Location` is untrusted input, so it is resolved against the URL that issued
//! it (as `urllib` and every browser do) and then handed to [`crate::gate::gate`]
//! exactly like a link from a feed. That is what turns a `302` to `10.0.0.1`, to
//! `file:///etc/passwd` or to a service port into a refusal *before* any request.
//!
//! One rule, stated once: **a hop inherits the origin's class and never escalates
//! it**. A public page may redirect to another public page and nowhere else; the
//! app's own loopback traffic may also stay on this machine
//! ([`check_hop_from`]). A public origin therefore cannot be walked onto
//! `127.0.0.1` by a redirect, which is the whole point of gating every hop.
//!
//! It decides the hop only. It does not perform the request, count hops against
//! [`MAX_REDIRECTS`] (the client does that), resolve DNS, or read a body.

use atlas_guard::Gated;
use url::Url;

use crate::gate::{gate, loopback_request};

/// `collector.MAX_REDIRECTS` — hops per fetch, every one of them re-gated.
pub const MAX_REDIRECTS: usize = 5;

/// Statuses this client follows by hand, because every hop must pass the gate.
pub(crate) const REDIRECTS: &[u16] = &[301, 302, 303, 307, 308];

/// One redirect hop, decided without touching the network.
///
/// `from` is the URL that issued the `Location`, `to` is the raw header value.
/// Relative locations are resolved against `from` (as `urllib` and every browser
/// do); the absolute result is then gated exactly like a link from a feed, which
/// is what makes a 302 to `10.0.0.1` — or to `file:///etc/passwd` — a refusal.
///
/// Returns the gated absolute URL as the guard's token — the only value the
/// transport will request — or the reason the hop must not be followed.
///
/// This is the public-only form, for a caller holding a URL rather than a verdict.
/// The transport uses [`check_hop_from`], which knows that a loopback origin may
/// stay on this machine.
pub fn check_hop(from: &str, to: &str) -> Result<Gated, String> {
    let joined = resolve_location(from, to)?;
    follow(&joined, false)
}

/// The same hop, from a URL the transport is already allowed to request.
///
/// The verdict's *class* travels with the hop: a loopback origin (the guard proved
/// the address is this machine) may redirect to another loopback literal, and to
/// nothing else; a public origin gets the public door only. Neither can escalate,
/// so a page fetched from the internet can never be walked onto this machine.
pub fn check_hop_from(from: &Gated, to: &str) -> Result<Gated, String> {
    let joined = resolve_location(from.as_str(), to)?;
    follow(&joined, from.is_loopback())
}

/// Resolve one `Location` header against the URL that issued it.
///
/// A header value is untrusted input, and a control character in it is not a URL: a
/// raw CR or LF means the header did not even arrive intact, and a NUL or a DEL means
/// something upstream is smuggling bytes. Both parsers involved here would *silently
/// rewrite* such a value — the `url` crate drops CR/LF/tab rather than refusing —
/// which turns `Location: /a\r\nX-Injected: 1` into a request for
/// `/aX-Injected:%201`: no header injection (the canonical form can never carry the
/// CR), but a different resource on the same host, chosen by nobody. A hop that says
/// it is one thing and requests another is refused, not repaired.
///
/// Percent-encoded forms (`%0d%0a`) are ordinary path bytes and stay legal: they
/// arrive as three ASCII characters and are sent as three ASCII characters.
fn resolve_location(from: &str, to: &str) -> Result<String, String> {
    let location = to.trim();
    if location.is_empty() {
        return Err("redirect without a location".to_string());
    }
    if location.chars().any(|ch| ch.is_control()) {
        return Err(
            "redirect location contains a control character (a header is not a url)".to_string(),
        );
    }
    let base = Url::parse(from).map_err(|e| format!("cannot resolve redirect ({})", e))?;
    let joined = base
        .join(location)
        .map_err(|e| format!("cannot resolve redirect ({})", e))?;
    Ok(joined.as_str().to_string())
}

/// The two doors, in the order they are tried: public first, and the loopback one
/// only for a hop whose origin already had it.
fn follow(joined: &str, from_loopback: bool) -> Result<Gated, String> {
    let verdict = gate(joined);
    if verdict.ok {
        // The canonical form is put in front of the guard again by `gate` itself, so
        // the string this returns is the string that was approved.
        if let Some(token) = verdict.gated() {
            // Belt: the canonical form must also be free of control characters, or
            // the request line it becomes is not the URL the guard saw.
            if joined.chars().any(|ch| ch.is_control()) {
                return Err("redirect location contains a control character".to_string());
            }
            return Ok(token.clone());
        }
    }
    if from_loopback {
        // `Gated::loopback` needs a token to start from, and the origin already is
        // one; re-minting from the absolute hop means the guard has seen the exact
        // string that will be requested.
        if let Ok(token) = Gated::loopback(joined) {
            match loopback_request(&token) {
                Ok((_, hop)) => return Ok(hop),
                // The port policy is this crate's own refusal, and the more useful
                // sentence of the two: "port 22 is a service port" says what happened
                // to a hop that never leaves the machine, where "address is not
                // publicly routable" only says what the address class is.
                Err(reason) => return Err(reason),
            }
        }
    }
    Err(if verdict.reason.is_empty() {
        "refused by the URL gate".to_string()
    } else {
        verdict.reason
    })
}

/// True when two URLs share scheme, host and port — the browser definition of one
/// site, which is what decides whether a hop may carry a `Referer`.
pub fn same_origin(a: &str, b: &str) -> bool {
    let (Ok(left), Ok(right)) = (Url::parse(a), Url::parse(b)) else {
        return false;
    };
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::gate;

#[test]
fn relative_hops_resolve_against_the_url_that_issued_them() {
    assert_eq!(
        check_hop("https://example.com/a/b", "/c").unwrap().as_str(),
        "https://example.com/c"
    );
    assert_eq!(
        check_hop("https://example.com/a/b", "c").unwrap().as_str(),
        "https://example.com/a/c"
    );
    assert_eq!(
        check_hop("https://example.com/a/b", "//other.example.org/x")
            .unwrap()
            .as_str(),
        "https://other.example.org/x"
    );
    assert_eq!(
        check_hop("https://example.com/a", "https://elsewhere.example.net/z?q=1")
            .unwrap()
            .as_str(),
        "https://elsewhere.example.net/z?q=1"
    );
}


#[test]
fn a_hop_to_a_refused_address_is_refused_before_any_request() {
    let cases = [
        ("http://127.0.0.1/", "redirect to loopback"),
        ("http://169.254.169.254/latest/meta-data/", "cloud metadata"),
        ("http://10.1.2.3/x", "private range"),
        ("http://[fe80::1]/", "ipv6 link-local"),
        ("file:///etc/passwd", "file scheme"),
        ("gopher://example.com/", "gopher scheme"),
        ("data:text/html,<b>x", "data scheme"),
        ("javascript:alert(1)", "javascript scheme"),
        ("http://user:pass@example.com/", "userinfo trick"),
        ("https://example.com:6379/", "service port"),
    ];
    for (location, label) in cases {
        let refusal = check_hop("https://example.com/a", location);
        assert!(refusal.is_err(), "{}: the hop was allowed", label);
        assert!(
            !refusal.unwrap_err().is_empty(),
            "{}: a refusal without a reason",
            label
        );
    }
}


#[test]
fn a_hop_without_a_location_is_refused() {
    assert!(check_hop("https://example.com/a", "").is_err());
    assert!(check_hop("https://example.com/a", "   ").is_err());
}


#[test]
fn a_garbage_base_url_cannot_be_used_to_smuggle_a_hop() {
    assert!(check_hop("not a url", "http://example.com/").is_err());
    assert!(check_hop("javascript:alert(1)", "/x").is_err());
}


#[test]
fn a_redirect_chain_of_public_hops_stays_public() {
    let mut url = "https://example.com/a".to_string();
    for hop in ["/b", "https://cdn.example.org/c", "?q=1"] {
        url = check_hop(&url, hop)
            .expect("a public hop must be allowed")
            .as_str()
            .to_string();
        assert!(gate(&url).ok, "{}", url);
    }
}


#[test]
fn a_hop_inherits_the_origin_class_and_never_escalates_it() {
    // A loopback origin (the app's own traffic) may stay on this machine.
    let local = Gated::loopback("http://127.0.0.1:8080/page").unwrap();
    let same = check_hop_from(&local, "/other").expect("loopback may stay loopback");
    assert!(same.is_loopback(), "{}", same.as_str());
    assert_eq!(same.as_str(), "http://127.0.0.1:8080/other");
    // It may also leave to a public page, which is gated publicly like any link.
    let out = check_hop_from(&local, "https://example.com/story").expect("a public hop");
    assert!(!out.is_loopback());
    assert_eq!(out.as_str(), "https://example.com/story");
    // But it may not walk onto a metadata address or a service port on its own
    // machine: privilege does not extend to "any local port".
    for location in [
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.5/x",
        "http://127.0.0.1:22/",
        "file:///etc/passwd",
    ] {
        assert!(
            check_hop_from(&local, location).is_err(),
            "{} was allowed from a loopback origin",
            location
        );
    }
    // The public origin gets the public door only: this is the rule that stops a
    // fetched page from pointing the app at its own machine.
    let public = Gated::verify("https://example.com/a").unwrap();
    for location in ["http://127.0.0.1:8080/x", "http://[::1]:8080/x", "//127.0.0.1/x"] {
        let refused = check_hop_from(&public, location);
        assert!(refused.is_err(), "{} was allowed from a public origin", location);
    }
    // And the public-only form agrees: it is the same decision for a public URL.
    assert!(check_hop("https://example.com/a", "http://127.0.0.1:8080/x").is_err());
}


#[test]
fn a_location_that_hides_an_address_behind_an_escape_is_refused() {
    // The spellings a `Location` can use to say "127.0.0.1" without looking like it.
    // Each is refused by the guard, and each is refused here before any request.
    for location in [
        "http://127%2e0%2e0%2e1/",              // percent-encoded dots
        "http://[::ffff:7f00:1]/",               // the same address in an IPv6 literal
        "http://[::ffff:127.0.0.1]/",            // and in its mapped form
        "http://0x7f000001/",                    // integer spelling
        "http://2130706433/",                    // decimal spelling
        "http://127.1/",                         // short spelling
        "//127.0.0.1:8080/x",                    // scheme-relative, this machine
        "https://example.com:6379/",             // a service port on a public host
        "http://user:pass@example.com/",         // userinfo, refused by the guard
        "http://example.com@127.0.0.1/",         // userinfo that hides an address
    ] {
        assert!(
            check_hop("https://example.com/a", location).is_err(),
            "{} was allowed",
            location
        );
    }
    // A zone index cannot be resolved into a host at all: refused as unusable rather
    // than turned into a name.
    assert!(check_hop("https://example.com/a", "http://[fe80::1%25en0]/").is_err());
}

#[test]
fn a_unicode_location_is_judged_as_the_name_it_becomes() {
    // A non-ASCII host is punycoded before it is judged, so the gate sees the name
    // that would be dialled; a non-ASCII path is percent-encoded, so the request line
    // stays ASCII. Neither is an address trick — the homograph a reader sees is a
    // different problem — but both must survive the hop as the exact string that will
    // be requested.
    let host = check_hop("https://example.com/a", "https://exämple.com/x")
        .expect("a public name in its punycode form");
    assert_eq!(host.as_str(), "https://xn--exmple-cua.com/x");
    let path = check_hop("https://example.com/a", "/résumé").expect("a non-ascii path");
    assert_eq!(path.as_str(), "https://example.com/r%C3%A9sum%C3%A9");
    assert!(path.as_str().is_ascii());
}

#[test]
fn only_one_site_may_receive_a_referer() {
    assert!(same_origin("https://example.com/a", "https://example.com/b?x=1"));
    assert!(same_origin("https://example.com/a", "https://EXAMPLE.com/b"));
    // A port is part of the origin: two local servers are two sites.
    assert!(!same_origin("http://127.0.0.1:8080/a", "http://127.0.0.1:9090/b"));
    // Scheme is part of it too, and so is the host.
    assert!(!same_origin("http://example.com/a", "https://example.com/b"));
    assert!(!same_origin("https://example.com/a", "https://cdn.example.org/b"));
    // Nonsense is never "the same site": a caller cannot leak by accident.
    assert!(!same_origin("not a url", "https://example.com/b"));
}

}
