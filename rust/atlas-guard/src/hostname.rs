//! `hostname` — what a host is: an address, a name, or a refusal.
//!
//! Decides the shape of the host part of a URL once the scheme, authority and
//! port are already settled (that is `urlsafe`): the lowercase form, the trailing
//! root dot, the 253/63 character caps, the ASCII requirement, the label
//! alphabet, whether it is an IPv4 or IPv6 literal and which ranges those fall
//! in, whether its labels spell an `inet_aton` number instead of a name, whether
//! it has a dot at all, and — for a name — the punycode decoding that lets a
//! blocklisted name be recognised in its encoded form.
//!
//! It does not decide the scheme, the port or the URL length, it does not resolve
//! anything, and it does not decide whether a URL may be fetched: it answers what
//! the host *is*, and `urlsafe` decides what that means for this caller.
//!
//! Why a module of its own: the rules above are the ones that change when a new
//! address form appears (NAT64, 6to4, a new numeric spelling), and they can be
//! read — and tested — without the URL grammar in the way.

use crate::ipaddr::{is_private_v4, is_private_v6, parse_ipv4, parse_ipv6};
use crate::punycode::punycode_decode;

/// Names that resolve inside a machine or a LAN. Never fetched.
const BLOCKED_HOSTS: &[&str] = &[
    "localhost",
    "local",
    "localdomain",
    "internal",
    "intranet",
    "lan",
    "corp",
    "private",
    "home.arpa",
    "ip6-localhost",
    "ip6-loopback",
    "broadcasthost",
    // The rest of the IANA special-use registry that can name a machine on a
    // home or office network. `home` and `mail` are the two this list was
    // missing: no registry delegates them, every router that hands out
    // `host.home` or `mail.corp` expects its own resolver to answer, and a
    // search result or a model-chosen link that names one is a request into the
    // LAN this app runs in — the same reason `lan`, `corp` and `internal` are
    // here. Vendor-only names (`fritz.box`, `workgroup`) are deliberately *not*
    // added: that list would never be finished, and a name that is not in a
    // registry is a name this gate judges by its shape (a dot, an address form)
    // and by the DNS answer `atlas-net::resolve_gate` checks after it.
    "home",
    "mail",
    // Reserved by RFC 2606 / RFC 6761: they can only ever name something on
    // this machine or in a test harness, never a news publisher.
    "test",
    "example",
    "invalid",
];

/// One host, read and judged.
pub(crate) struct Host {
    /// The lowercase, dot-trimmed ASCII form that would be dialled.
    pub name: String,
    /// True when the host is an address this machine or its LAN can reach.
    pub private: bool,
}

fn blocked_name(host: &str) -> bool {
    for name in BLOCKED_HOSTS {
        if host == *name {
            return true;
        }
        if let Some(prefix) = host.strip_suffix(*name) {
            if prefix.ends_with('.') {
                return true;
            }
        }
    }
    false
}

/// One label that C's `inet_aton` — which every resolver calls before it asks
/// DNS — reads as a number: decimal, octal with a leading zero, or `0x` hex.
///
/// `127.0.0.1` is only one of the spellings of 127.0.0.1. `2130706433`,
/// `127.1`, `0177.0.0.1`, `0x7f.0.0.1`, `0x7f000001` and `127.0x1` all reach the
/// same host, so a host is judged by this rule rather than by a list of literals.
fn c_numeric_label(label: &str) -> bool {
    if label.is_empty() {
        return false;
    }
    let (radix, digits) = match label.strip_prefix("0x").or_else(|| label.strip_prefix("0X")) {
        Some(rest) => (16, rest),
        None if label.len() > 1 && label.starts_with('0') => (8, &label[1..]),
        None => (10, label),
    };
    !digits.is_empty() && digits.chars().all(|ch| ch.is_digit(radix))
}

/// The decoded form of every `xn--` label, so an encoded name cannot hide
/// behind a blocklist entry. A decoded label may hold non-ASCII letters (a
/// legitimate IDN) but never a structural character.
fn decode_labels(host: &str) -> Result<String, String> {
    let mut decoded = String::with_capacity(host.len());
    for (index, label) in host.split('.').enumerate() {
        if index > 0 {
            decoded.push('.');
        }
        match label.strip_prefix("xn--") {
            Some(rest) => match punycode_decode(rest) {
                Some(plain) => {
                    let structural = plain.is_empty()
                        || plain.chars().any(|ch| {
                            ch.is_control()
                                || ch.is_whitespace()
                                || ch == '.'
                                || ch == '/'
                                || ch == '\\'
                                || ch == '@'
                                || ch == ':'
                                || ch == '%'
                        });
                    if structural {
                        return Err("invalid punycode label".to_string());
                    }
                    decoded.push_str(&plain);
                }
                None => return Err("invalid punycode label".to_string()),
            },
            None => decoded.push_str(label),
        }
    }
    Ok(decoded)
}

/// Read one host, as it appears between the authority markers, and judge it.
///
/// `Err` carries the sentence a caller prints; the caller is `urlsafe`, and the
/// only thing it does with a `Host` is decide whether a private one is allowed.
pub(crate) fn assess(raw: &str) -> Result<Host, String> {
    let mut host = raw.to_ascii_lowercase();
    // A trailing dot is legal FQDN syntax; dropping it keeps "foo.local." from
    // walking past the blocklist.
    while host.ends_with('.') {
        host.pop();
    }
    if host.is_empty() {
        return Err("missing host".to_string());
    }
    if host.len() > 253 {
        return Err("host longer than 253 characters".to_string());
    }
    if host.chars().any(|ch| !ch.is_ascii()) {
        return Err("host is not ascii (use its punycode form)".to_string());
    }
    if host.contains('/') || host.contains('\\') || host.contains('%') || host.contains('@') {
        return Err("invalid character in host".to_string());
    }

    if host.contains(':') {
        // An IPv6 literal: the brackets are already stripped and DNS label rules
        // do not apply to it. The answer covers the IPv4 a translator would
        // deliver to as well (`ipaddr::embedded_v4`).
        return match parse_ipv6(&host) {
            Some(seg) => Ok(Host {
                name: host,
                private: is_private_v6(&seg),
            }),
            None => Err("invalid ipv6 literal".to_string()),
        };
    }

    for label in host.split('.') {
        if label.is_empty() {
            return Err("empty label in host".to_string());
        }
        if label.len() > 63 {
            return Err("host label longer than 63 characters".to_string());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("host label starts or ends with a hyphen".to_string());
        }
        if !label.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-') {
            return Err("invalid character in host label".to_string());
        }
    }
    if let Some(ip) = parse_ipv4(&host) {
        return Ok(Host {
            name: host,
            private: is_private_v4(ip),
        });
    }

    let decoded = decode_labels(&host)?;
    if blocked_name(&host) || blocked_name(&decoded) {
        return Err("private hostname is not allowed".to_string());
    }
    // Every label reads as a C number: the resolver would hand this to
    // `inet_aton` and land on an address, whatever the spelling (`2130706433`,
    // `127.1`, `0177.0.0.1`, `0x7f.0.0.1`, `0x7f000001`, `127.0x1`). A real
    // domain has at least one label that is a word.
    if host.split('.').all(c_numeric_label) {
        return Err("ip literal in an unsupported form".to_string());
    }
    // A host whose last label is only digits is not a domain name either; the
    // resolver tries the numeric reading first.
    let last = host.rsplit('.').next().unwrap_or("");
    if !last.is_empty() && last.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("ip literal in an unsupported form".to_string());
    }
    // A single label is not an internet address: it is resolved through the
    // machine's search list, so "nas" or "intranet" names the LAN this app runs
    // in. Nothing a feed can legitimately link to is dotless, and the app's own
    // loopback traffic uses the literal (see `Gated::loopback`).
    if !host.contains('.') {
        return Err(
            "host has no dot (a bare name resolves inside the LAN; use its fully-qualified form)"
                .to_string(),
        );
    }
    Ok(Host {
        name: host,
        private: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(raw: &str) -> String {
        match assess(raw) {
            Ok(host) => panic!("{} was accepted as {:?}", raw, host.name),
            Err(reason) => reason,
        }
    }

    #[test]
    fn a_public_name_is_a_name() {
        let host = assess("www.Reuters.COM").expect("a public name");
        assert_eq!(host.name, "www.reuters.com");
        assert!(!host.private);
        // A numeric *label* inside a name is a normal domain, not an address:
        // www.163.com is a publisher, and 163 is not the whole host.
        for name in ["www.163.com", "123-reg.co.uk", "news.example.com"] {
            assert!(assess(name).is_ok(), "{} should be a name", name);
        }
    }

    #[test]
    fn a_trailing_root_dot_is_the_same_host() {
        assert_eq!(assess("example.com.").expect("a name").name, "example.com");
        assert_eq!(
            assess("example.com...").expect("a name").name,
            "example.com"
        );
    }

    #[test]
    fn every_ipv4_and_ipv6_range_the_gate_refuses() {
        for (raw, private) in [
            ("10.1.2.3", true),
            ("172.16.0.1", true),
            ("192.168.1.1", true),
            ("127.0.0.1", true),
            ("169.254.169.254", true),
            ("100.64.0.1", true),
            ("0.0.0.0", true),
            ("8.8.8.8", false),
            ("172.32.0.1", false),
            ("::1", true),
            ("fc00::1", true),
            ("fe80::1", true),
            ("2606:4700::1111", false),
            // The IPv4 an IPv6 literal carries, judged by the IPv4 rules.
            ("64:ff9b::7f00:1", true),
            ("2002:7f00:1::", true),
            ("::ffff:0:127.0.0.1", true),
            ("64:ff9b::808:808", false),
            // Ranges that are never a publisher.
            ("2001:db8::1", true),
            ("3fff::1", true),
            ("100::1", true),
            ("2001::1", true),
        ] {
            let host = assess(raw).unwrap_or_else(|e| panic!("{}: {}", raw, e));
            assert_eq!(host.private, private, "{} ({})", raw, host.name);
        }
    }

    #[test]
    fn host_shape_refusals_say_what_is_wrong() {
        for (raw, needle) in [
            ("", "missing host"),
            ("ex..ample.com", "empty label"),
            ("-bad.example", "hyphen"),
            ("bad-.example", "hyphen"),
            ("exam_ple.com", "label"),
            ("example.com/x", "host"),
            ("example.com\\x", "host"),
            ("example.com%31", "host"),
            ("example.com@x", "host"),
            ("bücher.de", "ascii"),
            ("[::1", "ipv6"),
            ("::zz", "ipv6"),
        ] {
            let reason = refused(raw);
            assert!(
                reason.contains(needle),
                "{:?} gave {:?}, wanted {:?}",
                raw,
                reason,
                needle
            );
        }
        let long_label = "a".repeat(64);
        assert!(refused(&format!("{}.com", long_label)).contains("63"));
        let long_host = format!("{}.com", "a.".repeat(130));
        assert!(refused(&long_host).contains("253"));
    }

    #[test]
    fn a_numeric_spelling_of_an_address_is_not_a_name() {
        // Every form `inet_aton` accepts. A gate that only refuses the dotted
        // decimal spelling is a gate that only looks closed.
        for raw in [
            "127.1",
            "2130706433",
            "0177.0.0.1",
            "0x7f.0.0.1",
            "0x7f000001",
            "0X7F000001",
            "127.0x1",
            "1.0x7f",
            "017700000001",
            "1.2.3.4.5",
        ] {
            assert!(
                refused(raw).contains("ip literal"),
                "{} must not pass as a name",
                raw
            );
        }
    }

    #[test]
    fn a_dotless_name_is_refused() {
        for raw in ["nas", "wiki", "some-host"] {
            assert!(
                refused(raw).contains("no dot"),
                "{} must not pass as a name",
                raw
            );
        }
        // The blocklisted names keep their specific reason.
        assert!(refused("localhost").contains("private hostname"));
        assert!(refused("intranet").contains("private hostname"));
        assert!(refused("db.internal").contains("private hostname"));
        assert!(refused("nas.lan").contains("private hostname"));
        assert!(refused("a.b.localhost").contains("private hostname"));
        assert!(refused("home.arpa").contains("private hostname"));
    }

    #[test]
    fn the_special_use_tlds_a_router_answers_are_refused() {
        // IANA's special-use registry, as far as it can name a device on the LAN
        // this app runs in. Each of these is a request into the local network if
        // it is fetched, and a publisher can never live under one: no registry
        // delegates them, so the answer is always a local resolver's.
        for raw in [
            "printer.home",
            "nas.home",
            "home.home",
            "mail.home",
            "smtp.mail",
            "mail.mail",
            "box.corp",
            "thing.lan",
            "wiki.internal",
            "printer.home.",      // …and with the root dot a resolver would add
            "a.b.c.home",
        ] {
            let reason = refused(raw);
            assert!(
                reason.contains("private hostname"),
                "{} gave {:?}, wanted a private-hostname refusal",
                raw,
                reason
            );
        }
        // A name that merely *contains* one of the words stays a name: the rule
        // is a suffix on a label boundary, not a substring.
        for raw in ["homepage.example.org", "mailchimp.com", "homes.co.uk", "smail.io"] {
            assert!(
                assess(raw).is_ok(),
                "{} should stay a public name",
                raw
            );
        }
    }

    #[test]
    fn punycode_cannot_hide_a_name_from_the_blocklist() {
        // RFC 3492 samples: these are the plain forms browsers display.
        assert_eq!(punycode_decode("bcher-kva").as_deref(), Some("bücher"));
        assert_eq!(punycode_decode("mnchen-3ya").as_deref(), Some("münchen"));
        assert_eq!(punycode_decode("fiqs8s").as_deref(), Some("中国"));
        assert!(punycode_decode("!!!").is_none());
        // An encoded label that decodes to a structural character is refused
        // rather than mistaken for one label.
        // A label whose payload decodes to a character that is not a letter —
        // here U+0080, a C1 control — is not a name, and is refused before it is
        // compared with anything. (`decode_labels` also refuses a decoded dot,
        // slash, backslash, `@` or `:`; a *decodable* label cannot currently
        // produce one, because punycode copies its ASCII preamble verbatim and
        // the label alphabet has already refused those bytes — that branch is
        // there for the encoder that is not written yet.)
        assert!(refused("xn--a.de").contains("punycode"));
        // A legitimate IDN decodes and stays usable.
        let idn = assess("xn--bcher-kva.de").expect("a legitimate IDN");
        assert_eq!(idn.name, "xn--bcher-kva.de");
        assert!(!idn.private);
    }
}
