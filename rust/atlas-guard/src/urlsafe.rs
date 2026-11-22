//! `urlsafe` — the SSRF gate: may this URL be fetched?
//!
//! Decides scheme (http/https only), the length cap, userinfo and whitespace,
//! invisible characters ([`crate::cleantext::is_invisible`]: a bidi override or a
//! tag character has no place in a URL this app stores and prompts with), the
//! authority split, the port, and — through [`crate::hostname`] — what the host
//! is: an address in one of the spellings a resolver expands, a name, or a
//! refusal. The answer is a verdict with `ok` and `reason`: an unfetchable URL is
//! a verdict, not an error.
//!
//! It does not resolve DNS — a public name that resolves to a private address is
//! caught by `atlas-net` after this gate — it fetches nothing, follows nothing
//! and retries nothing. `allow_private` exists for the app's own loopback calls;
//! a URL that came from a feed or a fetched page never sets it.

use crate::cleantext;
use crate::field;
use crate::hostname;
use crate::json::{self, jbool, jnum, jobj, jstr, Json};

// The literal parsers are reachable as `urlsafe::parse_ipv4` / `urlsafe::parse_ipv6`
// exactly as before: the gate is the module callers know, `ipaddr` is where the
// arithmetic lives.
pub use crate::ipaddr::{parse_ipv4, parse_ipv6};

/// Longest URL `url` will look at, per the fetch policy.
const MAX_URL_CHARS: usize = 2048;

pub struct Check {
    pub ok: bool,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub reason: String,
    pub is_private_ip: bool,
}

impl Check {
    fn new() -> Check {
        Check {
            ok: false,
            scheme: String::new(),
            host: String::new(),
            port: 0,
            reason: String::new(),
            is_private_ip: false,
        }
    }

    fn reject(mut self, reason: &str) -> Check {
        self.ok = false;
        self.reason = reason.to_string();
        self
    }
}

fn scheme_token(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    for (i, ch) in s.chars().enumerate() {
        if i == 0 {
            if !ch.is_ascii_alphabetic() {
                return false;
            }
        } else if !(ch.is_ascii_alphanumeric() || ch == '+' || ch == '-' || ch == '.') {
            return false;
        }
    }
    true
}

/// The decision. `allow_private` exists for the app's own loopback calls; a
/// URL that came from a feed or a fetched page never sets it.
pub fn check(raw: &str, allow_private: bool) -> Check {
    let mut out = Check::new();
    let url = raw.trim();
    if url.is_empty() {
        return out.reject("empty url");
    }
    if url.chars().count() > MAX_URL_CHARS {
        return out.reject("url longer than 2048 characters");
    }
    if url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return out.reject("url contains whitespace or a control character");
    }
    // A URL is text this app stores, shows and prompts a model with, and
    // `cleantext::is_invisible` is the crate's rule for characters that carry no
    // width but do carry meaning: a bidi override makes the path read backwards,
    // and a tag character (`U+E0000..`) is an invisible ASCII alphabet a page can
    // spell instructions in. `atlas-ingest` cleans its own inputs, but the
    // browser's landing check and `atlas-search` arrive here directly, so the
    // gate is where the rule has to hold. A percent-encoded `%E2%80%AE` is
    // unaffected (it is ASCII), which is how a real feed spells such a URL.
    if url.chars().any(cleantext::is_invisible) {
        return out.reject(
            "url contains an invisible character (zero-width, bidi override or tag)",
        );
    }
    let colon = match url.find(':') {
        Some(i) => i,
        None => return out.reject("missing scheme"),
    };
    let scheme_raw = match url.get(..colon) {
        Some(s) => s,
        None => return out.reject("invalid scheme"),
    };
    if !scheme_token(scheme_raw) {
        return out.reject("invalid scheme");
    }
    let scheme = scheme_raw.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return out.reject("scheme must be http or https");
    }
    out.scheme = scheme.clone();
    out.port = if scheme == "https" { 443 } else { 80 };

    let rest = match url.get(colon + 1..) {
        Some(r) => r,
        None => return out.reject("invalid url"),
    };
    let rest = match rest.strip_prefix("//") {
        Some(r) => r,
        None => return out.reject("expected // after the scheme"),
    };
    let auth_end = match rest.find(|c: char| c == '/' || c == '?' || c == '#') {
        Some(i) => i,
        None => rest.len(),
    };
    let authority = match rest.get(..auth_end) {
        Some(a) => a,
        None => return out.reject("invalid authority"),
    };
    if authority.contains('@') {
        return out.reject("userinfo (user:pass@) is not allowed");
    }

    let (host_raw, port_raw) = if authority.starts_with('[') {
        let close = match authority.find(']') {
            Some(i) => i,
            None => return out.reject("unterminated ipv6 literal"),
        };
        let host = match authority.get(1..close) {
            Some(h) => h,
            None => return out.reject("invalid ipv6 literal"),
        };
        let after = match authority.get(close + 1..) {
            Some(a) => a,
            None => return out.reject("invalid ipv6 literal"),
        };
        let port = if after.is_empty() {
            None
        } else {
            match after.strip_prefix(':') {
                Some(p) => Some(p),
                None => return out.reject("unexpected characters after the ipv6 literal"),
            }
        };
        (host, port)
    } else {
        match authority.rfind(':') {
            Some(i) => {
                let host = match authority.get(..i) {
                    Some(h) => h,
                    None => return out.reject("invalid authority"),
                };
                let port = match authority.get(i + 1..) {
                    Some(p) => Some(p),
                    None => return out.reject("invalid authority"),
                };
                // A second colon before the port ("example.com:80:90") used to
                // travel on as if it were an IPv6 literal and come back as
                // "invalid ipv6 literal", which sends the reader looking for
                // brackets that are not there.
                if host.contains(':') {
                    return out.reject(
                        "the authority has more than one colon (an ipv6 literal needs [brackets])",
                    );
                }
                (host, port)
            }
            None => (authority, None),
        }
    };

    if let Some(p) = port_raw {
        if p.is_empty() {
            return out.reject("empty port");
        }
        if !p.chars().all(|c| c.is_ascii_digit()) {
            return out.reject("port is not a number");
        }
        let n: u32 = match p.parse::<u32>() {
            Ok(v) => v,
            Err(_) => return out.reject("port is not a number"),
        };
        if n == 0 || n > 65535 {
            return out.reject("port out of range (1-65535)");
        }
        out.port = n as u16;
    }

    // The host is judged by `hostname`, which owns the address forms, the
    // numeric spellings a resolver expands, the blocklist and the punycode
    // decoding. Everything above this line is the URL grammar.
    let host = match hostname::assess(host_raw) {
        Ok(host) => host,
        Err(reason) => return out.reject(&reason),
    };
    out.host = host.name;
    out.is_private_ip = host.private;
    if host.private && !allow_private {
        return out.reject(
            "address is not publicly routable (private, loopback, link-local, documentation or reserved)",
        );
    }
    out.ok = true;
    out.reason = String::new();
    out
}

pub fn run(root: &Json) -> Result<String, String> {
    let url = field::required(root, "url")?;
    let allow_private = root
        .get("allow_private")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let verdict = check(url, allow_private);
    Ok(json::to_string(&jobj(vec![
        ("ok", jbool(verdict.ok)),
        ("scheme", jstr(&verdict.scheme)),
        ("host", jstr(&verdict.host)),
        ("port", jnum(verdict.port as f64)),
        ("reason", jstr(&verdict.reason)),
        ("is_private_ip", jbool(verdict.is_private_ip)),
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::json_of;

    #[test]
    fn url_allows_a_public_https_url() {
        let c = check("https://example.com/a/b?x=1", false);
        assert!(c.ok, "{}", c.reason);
        assert_eq!(c.scheme, "https");
        assert_eq!(c.host, "example.com");
        assert_eq!(c.port, 443);
        assert!(c.reason.is_empty());
        assert!(!c.is_private_ip);
    }

    #[test]
    fn url_keeps_an_explicit_port_and_defaults_the_rest() {
        assert_eq!(check("http://example.com:8080/x", false).port, 8080);
        assert_eq!(check("http://example.com", false).port, 80);
        assert_eq!(check("HTTPS://Example.COM/Path", false).host, "example.com");
        assert!(check("HTTPS://Example.COM/Path", false).ok);
    }

    #[test]
    fn url_rejects_schemes_that_are_not_http() {
        for raw in ["javascript:alert(1)", "file:///etc/passwd", "ftp://e.com/x", "data:text/html,<b>"] {
            let c = check(raw, false);
            assert!(!c.ok, "{} should be rejected", raw);
            assert!(
                c.reason.contains("scheme"),
                "{} gave {:?}",
                raw,
                c.reason
            );
        }
    }

    #[test]
    fn url_rejects_userinfo_and_whitespace() {
        let c = check("https://user:pass@example.com/", false);
        assert!(!c.ok);
        assert!(c.reason.contains("userinfo"), "{}", c.reason);
        let w = check("https://exa mple.com/", false);
        assert!(!w.ok);
        assert!(w.reason.contains("whitespace"), "{}", w.reason);
        let n = check("https://example.com/\u{0}extra", false);
        assert!(!n.ok, "NUL in a url must be refused");
    }

    #[test]
    fn url_blocks_private_and_reserved_ip_literals() {
        let cases = [
            "http://10.1.2.3/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://127.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fc00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:127.0.0.1]/",
            // The same addresses, carried inside an IPv6 literal. A NAT64
            // gateway, a SIIT box or a 6to4 relay expands each of these into the
            // IPv4 the gate refuses by name, so accepting the long spelling
            // would be a gate that only looks closed.
            "http://[64:ff9b::7f00:1]/",
            "http://[64:ff9b::a00:1]/",
            "http://[64:ff9b:1::c0a8:101]/",
            "http://[2002:7f00:1::]/",
            "http://[2002:1:2::]/",
            "http://[::ffff:0:127.0.0.1]/",
            "http://[0:0:0:0:0:ffff:7f00:1]/",
            // Ranges that are never a publisher: discard-only, tunnel and the
            // documentation block.
            "http://[100::1]/",
            "http://[2001::1]/",
            "http://[2001:db8::1]/",
            "http://[3fff::1]/",
        ];
        for raw in cases {
            let c = check(raw, false);
            assert!(!c.ok, "{} should be blocked", raw);
            assert!(c.is_private_ip, "{} should be flagged private", raw);
            assert!(
                c.reason.contains("not publicly routable"),
                "{} gave {:?}",
                raw,
                c.reason
            );
        }
        // Public addresses stay usable, including a public address carried by a
        // NAT64 literal (8.8.8.8 through 64:ff9b::/96).
        for raw in [
            "http://8.8.8.8/",
            "http://172.32.0.1/",
            "http://[2606:4700::1111]/",
            "http://[64:ff9b::808:808]/",
            "http://[2002:808:808::]/",
        ] {
            let c = check(raw, false);
            assert!(c.ok, "{} should be allowed, got {:?}", raw, c.reason);
            assert!(!c.is_private_ip, "{} is public", raw);
        }
    }

    #[test]
    fn url_allow_private_admits_loopback_for_the_app_itself() {
        let c = check("http://127.0.0.1:8787/api/status", true);
        assert!(c.ok, "{}", c.reason);
        assert!(c.is_private_ip);
        assert_eq!(c.port, 8787);
        assert_eq!(c.host, "127.0.0.1");
    }

    #[test]
    fn url_blocks_private_names_and_the_trailing_dot_trick() {
        for raw in [
            "http://localhost/",
            "http://localhost./",
            "http://LOCALHOST:8080/",
            "http://foo.local/",
            "http://api.internal/",
            "http://a.b.localhost/",
            "http://db.localdomain/",
            "http://nas.lan/",
        ] {
            let c = check(raw, false);
            assert!(!c.ok, "{} should be blocked", raw);
            assert!(
                c.reason.contains("private hostname"),
                "{} gave {:?}",
                raw,
                c.reason
            );
        }
    }

    #[test]
    fn url_rejects_numeric_shorthand_the_resolver_would_expand() {
        // Every spelling `inet_aton` accepts: one integer, one dot, octal, hex,
        // and a hex octet hidden among decimal ones. The last two used to pass:
        // `0x7f000001` has no dot, so no label check fired, and `127.0x1` ends
        // in a hex label, not a digit.
        for raw in [
            "http://127.1/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://0x7f.0.0.1/",
            "http://0x7f000001/",
            "http://0X7F000001/",
            "http://127.0x1/",
            "http://1.0x7f/",
            "http://017700000001/",
            "http://1.2.3.4.5/",
        ] {
            let c = check(raw, false);
            assert!(!c.ok, "{} should be refused", raw);
            assert!(c.reason.contains("ip literal"), "{} gave {:?}", raw, c.reason);
        }
        // A numeric *label* inside a name is a normal domain, not an address:
        // www.163.com is a publisher, and 163 is not the whole host.
        for raw in ["http://www.163.com/", "http://123-reg.co.uk/"] {
            let c = check(raw, false);
            assert!(c.ok, "{} should be allowed, got {:?}", raw, c.reason);
        }
    }

    #[test]
    fn url_rejects_a_dotless_host_that_would_resolve_inside_the_lan() {
        // A bare name goes through the resolver's search list, so "nas" or
        // "wiki" names a machine on this LAN and "localhost" names this one.
        for raw in ["http://nas/", "http://wiki/x", "https://some-host:8080/"] {
            let c = check(raw, false);
            assert!(!c.ok, "{} should be refused", raw);
            assert!(c.reason.contains("no dot"), "{} gave {:?}", raw, c.reason);
        }
        // The private-name answer wins for a name the blocklist knows, so the
        // reason stays the specific one ("intranet" is refused as a name, not
        // as a shape).
        for raw in ["http://localhost/", "http://intranet/"] {
            assert!(
                check(raw, false).reason.contains("private hostname"),
                "{}",
                raw
            );
        }
    }

    #[test]
    fn url_rejects_bad_ports_and_empty_hosts() {
        for (raw, needle) in [
            ("http://example.com:0/", "port"),
            ("http://example.com:99999/", "port"),
            ("http://example.com:abc/", "port"),
            ("http://example.com:/x", "port"),
            ("", "empty url"),
            ("https://", "host"),
            ("https:///path", "host"),
            ("https://ex..ample.com/", "label"),
            ("https://-bad.example/", "hyphen"),
        ] {
            let c = check(raw, false);
            assert!(!c.ok, "{} should be refused", raw);
            assert!(
                c.reason.contains(needle),
                "{:?} gave {:?}, wanted {:?}",
                raw,
                c.reason,
                needle
            );
        }
    }

    #[test]
    fn url_enforces_the_length_cap() {
        let long = format!("https://e.co/{}", "a".repeat(2100));
        let c = check(&long, false);
        assert!(!c.ok);
        assert!(c.reason.contains("2048"), "{}", c.reason);
    }

    #[test]
    fn url_rejects_a_non_ascii_host_but_accepts_its_punycode() {
        let raw = check("https://bücher.de/", false);
        assert!(!raw.ok);
        assert!(raw.reason.contains("ascii"), "{}", raw.reason);
        let encoded = check("https://xn--bcher-kva.de/", false);
        assert!(encoded.ok, "{}", encoded.reason);
        assert_eq!(encoded.host, "xn--bcher-kva.de");
    }

    #[test]
    fn url_refuses_the_confusable_forms_that_hide_a_host() {
        // Each string is a URL one parser reads as "a page on example.com" and
        // another reads as "a page on 127.0.0.1". The gate's answer must be the
        // same for all of them: no.
        for (raw, needle) in [
            // A backslash is a path separator to a browser, part of a host here.
            ("http://example.com\\@127.0.0.1/", "userinfo"),
            ("http://127.0.0.1\\@example.com/", "userinfo"),
            ("http://example.com\\127.0.0.1/", "host"),
            ("http://127.0.0.1:8787\\@example.com/api", "userinfo"),
            // Unicode dots and full-width separators: refused as non-ascii shape
            // rather than normalised into a dot.
            ("https://exa\u{3002}mple.com/", "ascii"),
            ("https://127\u{ff0e}0\u{ff0e}0\u{ff0e}1/", "ascii"),
            ("https://example.com.。/", "ascii"),
            ("https://example.com\u{ff0f}@x/", "userinfo"),
            ("https://user\u{ff20}example.com@127.0.0.1/", "userinfo"),
            // An encoded NUL or an encoded path separator never becomes one.
            ("https://example.com%00.attacker.com/", "host"),
            // Two ports, and trailing junk after an IPv6 literal.
            ("https://example.com:80:90/", "more than one colon"),
            ("http://[::1]extra:80/", "after the ipv6 literal"),
        ] {
            let c = check(raw, false);
            assert!(!c.ok, "{} should be refused", raw);
            assert!(
                c.reason.contains(needle),
                "{} gave {:?}, wanted {:?}",
                raw,
                c.reason,
                needle
            );
            assert!(c.host.is_empty(), "a refusal carried the host {:?}", c.host);
        }
        // A trailing root dot is legal FQDN syntax and is dropped, so the form a
        // feed writes and the form a resolver reads are the same host.
        let dotted = check("https://example.com./x", false);
        assert!(dotted.ok, "{}", dotted.reason);
        assert_eq!(dotted.host, "example.com");
    }

    #[test]
    fn url_run_emits_the_documented_fields() {
        let out = run(&json_of("{\"url\":\"https://example.com/x\",\"allow_private\":false}"))
            .expect("ok");
        assert!(out.starts_with("{\"ok\":true,\"scheme\":\"https\",\"host\":\"example.com\",\"port\":443,"), "{}", out);
        assert!(out.contains("\"reason\":\"\""), "{}", out);
        assert!(out.contains("\"is_private_ip\":false"), "{}", out);
        let blocked = run(&json_of("{\"url\":\"http://127.0.0.1/\",\"allow_private\":false}"))
            .expect("ok");
        assert!(blocked.contains("\"is_private_ip\":true"), "{}", blocked);
    }
}
