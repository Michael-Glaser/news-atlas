//! `address` — is this listening address reachable from another machine?
//!
//! Decides the `loopback` verdict for the forms lsof, netstat and
//! `/proc/net/tcp` print (`127.0.0.1:8080`, `[::1]:80`, `0.0.0.0:3000`, `*:*`,
//! `0.0.0.0:*`), leaning on `urlsafe`'s literal parsers instead of parsing
//! IPv4/IPv6 twice. Wildcards and foreign addresses are exposed; anything
//! unparseable answers `loopback: false`, so an unreadable address can never
//! pass as safe.
//!
//! The port, the wildcard port and a `%zone` are stripped because a *socket
//! tool* prints them and none of them changes the address; what follows a
//! bracketed literal is read strictly ([`bracketed_suffix`]), because a string
//! that says `[::1]@elsewhere` is not proof of anything.
//!
//! It does not check whether anything is actually listening, it does not scan a
//! port, and it does not decide which address the app should bind.

use crate::json::{self, jbool, jobj, jstr, Json};
use crate::field;
use crate::urlsafe;

pub struct Verdict {
    pub ok: bool,
    pub loopback: bool,
    pub kind: String,
    pub reason: String,
}

/// True only for addresses that cannot be reached from another machine.
/// `::` and `0.0.0.0` are wildcards and therefore exposed, not loopback.
fn loopback_v6(seg: &[u16; 8]) -> bool {
    let mut zero_prefix = true;
    for word in 0..7 {
        if seg[word] != 0 {
            zero_prefix = false;
            break;
        }
    }
    if zero_prefix && seg[7] == 1 {
        return true; // ::1
    }
    // ::ffff:a.b.c.d (v4-mapped) and the deprecated ::a.b.c.d
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 {
        if seg[5] == 0xffff || seg[5] == 0 {
            let high = (seg[6] >> 8) as u8;
            return high == 127;
        }
    }
    false
}

/// What may follow `]` in a bracketed address, as lsof, netstat, ss and
/// `DevToolsActivePort` print it: nothing, `:port`, `:*`, or a zone id
/// (`%en0`, `%25`, `%{a-guid}`) with the same options after it.
///
/// Anything else means the string is *not* the address inside the brackets, so
/// the caller must not be told it is. A zone id is opaque — an interface name or
/// a Windows interface GUID — so it is accepted up to the `:` that ends it.
fn bracketed_suffix(after: &str) -> bool {
    let rest = match after.strip_prefix('%') {
        Some(zone) => match zone.find(':') {
            Some(pos) => match zone.get(pos..) {
                Some(tail) => tail,
                None => return false,
            },
            None => return !zone.is_empty(),
        },
        None => after,
    };
    match rest.strip_prefix(':') {
        Some(port) => {
            !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit() || ch == '*')
        }
        None => rest.is_empty(),
    }
}

/// Classify one listening address, as printed by lsof / netstat /
/// /proc/net/tcp. Anything that is not provably loopback answers
/// `loopback: false`, so an unreadable address can never pass as safe.
pub fn check(raw: &str) -> Verdict {
    let mut out = Verdict {
        ok: false,
        loopback: false,
        kind: String::new(),
        reason: String::new(),
    };
    let mut text = raw.trim().to_ascii_lowercase();
    if text.is_empty() {
        out.kind = "empty".to_string();
        out.reason = "empty address".to_string();
        return out;
    }
    // lsof and netstat print "address:port" and "[address]:port". The port
    // is not part of the address and must not change the verdict.
    //
    // A bracketed address is the address *only* when what follows the bracket is
    // what a socket tool prints: nothing, `:port`, `:*`, or a zone id with the
    // same options (`[fe80::1%en0]:9222`). Dropping whatever came after `]` made
    // `[::1]anything`, `[::1]@evil.example` and `[127.0.0.1]x` answer
    // `loopback:true`: the decision was about a prefix of the string, not about
    // the string, which is the shape of a fail-open in any caller that later
    // dials the value it asked about.
    if text.starts_with('[') {
        let close = match text.find(']') {
            Some(close) => close,
            None => {
                out.reason = "unterminated bracketed address".to_string();
                return out;
            }
        };
        let inner = match text.get(1..close) {
            Some(inner) if !inner.is_empty() => inner.to_string(),
            _ => {
                out.reason = "empty bracketed address".to_string();
                return out;
            }
        };
        let after = match text.get(close + 1..) {
            Some(after) => after,
            None => {
                out.reason = "unterminated bracketed address".to_string();
                return out;
            }
        };
        if !bracketed_suffix(after) {
            out.reason = "unexpected characters after the bracketed address".to_string();
            return out;
        }
        text = inner;
    } else if text.matches(':').count() == 1 {
        if let Some(pos) = text.find(':') {
            let numeric_port = match text.get(pos + 1..) {
                Some(port) => !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()),
                None => false,
            };
            if numeric_port {
                let inner = match text.get(..pos) {
                    Some(head) => head.to_string(),
                    None => text.clone(),
                };
                text = inner;
            }
        }
    }
    // netstat prints a socket bound to every interface with a wildcard port
    // (`*:*`, `0.0.0.0:*`, `:::*`). The port is never part of the address, and
    // no group of an IPv6 literal is `*`, so the suffix can go whatever shape
    // the address has.
    if let Some(head) = text.strip_suffix(":*") {
        text = head.to_string();
    }
    // A zone index ("fe80::1%en0") says nothing about scope safety.
    if let Some(pos) = text.find('%') {
        let inner = match text.get(..pos) {
            Some(head) => head.to_string(),
            None => text.clone(),
        };
        text = inner;
    }
    out.ok = true;
    if text == "*" || text == "0.0.0.0" || text == "::" {
        out.kind = "wildcard".to_string();
        out.loopback = false;
        out.reason = "wildcard address listens on every interface".to_string();
        return out;
    }
    if text == "localhost" || text == "localhost.localdomain" || text == "ip6-localhost" {
        out.kind = "name".to_string();
        out.loopback = true;
        return out;
    }
    if text.contains(':') {
        out.kind = "ipv6".to_string();
        match urlsafe::parse_ipv6(&text) {
            Some(seg) => {
                out.loopback = loopback_v6(&seg);
                if !out.loopback {
                    out.reason = "not a loopback address".to_string();
                }
            }
            None => {
                out.ok = false;
                out.reason = "unparsable ipv6 address".to_string();
            }
        }
        return out;
    }
    match urlsafe::parse_ipv4(&text) {
        Some(ip) => {
            out.kind = "ipv4".to_string();
            out.loopback = (ip >> 24) == 127;
            if !out.loopback {
                out.reason = "not a loopback address".to_string();
            }
        }
        None => {
            // A host name is not a literal: it cannot be proven loopback
            // here, so the caller must treat it as exposed.
            out.kind = "name".to_string();
            out.loopback = false;
            out.reason = "host name is not a loopback literal".to_string();
        }
    }
    out
}

pub fn run(root: &Json) -> Result<String, String> {
    let value = field::required(root, "value")?;
    let verdict = check(value);
    Ok(json::to_string(&jobj(vec![
        ("ok", jbool(verdict.ok)),
        ("loopback", jbool(verdict.loopback)),
        ("kind", jstr(&verdict.kind)),
        ("reason", jstr(&verdict.reason)),
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::json_of;

    #[test]
    fn address_recognises_loopback_literals() {
        for raw in ["127.0.0.1", "127.255.255.254", "::1", "[::1]", "::ffff:127.0.0.1", "localhost"] {
            let v = check(raw);
            assert!(v.ok, "{} should parse", raw);
            assert!(v.loopback, "{} should be loopback", raw);
        }
    }

    #[test]
    fn address_accepts_the_address_port_form_the_os_prints() {
        for (raw, loopback) in [
            ("127.0.0.1:8787", true),
            ("[::1]:9222", true),
            ("[::]:9222", false),
            ("0.0.0.0:9222", false),
            ("192.168.1.5:80", false),
        ] {
            let v = check(raw);
            assert!(v.ok, "{} should parse", raw);
            assert_eq!(v.loopback, loopback, "{} gave {:?}", raw, v.reason);
        }
        assert_eq!(check("127.0.0.1:8787").kind, "ipv4");
        assert_eq!(check("[::]:9222").kind, "wildcard");
        assert_eq!(check("::1").kind, "ipv6");
    }

    #[test]
    fn address_reads_the_wildcard_port_netstat_prints() {
        // `*:*` is what netstat shows for a socket bound to every interface;
        // it used to reach the IPv6 parser ('two colons') and come back as an
        // unparsable error, which the caller then has to interpret instead of
        // reading a verdict.
        for raw in ["*:*", "*:0", "0.0.0.0:*", ":::*"] {
            let v = check(raw);
            assert!(v.ok, "{} should parse", raw);
            assert!(!v.loopback, "{} must not pass as loopback", raw);
            assert_eq!(v.kind, "wildcard", "{} gave {:?}", raw, v.kind);
            assert!(!v.reason.is_empty(), "{} needs a reason", raw);
        }
        // The same form on a loopback literal keeps the port out of the verdict.
        for raw in ["127.0.0.1:*", "127.0.0.1:0", "[::1]:*"] {
            let v = check(raw);
            assert!(v.ok, "{} should parse", raw);
            assert!(v.loopback, "{} should be loopback", raw);
        }
    }

    #[test]
    fn address_treats_wildcards_and_foreign_addresses_as_exposed() {
        for raw in ["0.0.0.0", "::", "*", "192.168.1.5", "10.0.0.1", "8.8.8.8", "fe80::1%en0", "example.com", "[::]"] {
            let v = check(raw);
            assert!(v.ok, "{} should parse", raw);
            assert!(!v.loopback, "{} must not pass as loopback", raw);
            assert!(!v.reason.is_empty(), "{} needs a reason", raw);
        }
        let wildcard = check("0.0.0.0");
        assert_eq!(wildcard.kind, "wildcard");
    }

    #[test]
    fn address_rejects_an_empty_or_broken_value() {
        let empty = check("   ");
        assert!(!empty.ok);
        assert_eq!(empty.reason, "empty address");
        let broken = check("[::zz]");
        assert!(!broken.ok, "unparsable ipv6 must not answer ok");
        assert!(!broken.reason.is_empty(), "a refusal needs a reason");
    }

    #[test]
    fn address_run_emits_ok_loopback_kind_reason() {
        let out = run(&json_of("{\"value\":\"127.0.0.1\"}")).expect("ok");
        assert_eq!(out, "{\"ok\":true,\"loopback\":true,\"kind\":\"ipv4\",\"reason\":\"\"}");
        let wild = run(&json_of("{\"value\":\"0.0.0.0\"}")).expect("ok");
        assert!(wild.contains("\"loopback\":false"), "{}", wild);
        assert!(run(&json_of("{}")).is_err(), "missing value");
    }
}
