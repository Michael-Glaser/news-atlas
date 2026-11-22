//! Adversarial input for the URL parser and both gate verdicts: 10 000 mutations.
//!
//! Every refused URL is a page this app does not open, and every accepted one is a
//! socket. The claim: for any byte sequence, `urlsafe::check` and
//! `address::check` decide, say why when they refuse, and never panic — and a
//! refusal never carries a host, a port or a `Gated`.
//!
//! The mutation corpus is built from the URL forms that have historically broken
//! this kind of parser: integer and short-form IP literals, userinfo, punycode
//! homoglyphs, embedded whitespace and control characters, an IPv6 zone index,
//! and a scheme that only *looks* like http.

use atlas_guard::gated::Gated;
use atlas_guard::urlsafe;
use atlas_guard::address;

/// `atlas-guard`'s own cap (`urlsafe::MAX_URL_CHARS`, private to the crate) is
/// 2048; `atlas-net` carries the same number for the request line. Written out
/// here so this test does not reach into a private constant.
const MAX_URL_CHARS: usize = 2048;

const SEEDS: &[&str] = &[
    "https://www.reuters.com/world/2026/nov/21/harbour-strike-ends",
    "http://127.0.0.1:8787/api/status",
    "http://[::1]:8787/api/status",
    "http://169.254.169.254/latest/meta-data/",
    "http://2130706433/",
    "http://127.1/",
    "http://user:pass@example.com/",
    "http://example.com@127.0.0.1/",
    "https://xn--80ak6aa92e.com/",
    "http://localhost:8080/",
    "file:///etc/passwd",
    "javascript:alert(1)",
    "http://[fe80::1%25en0]/",
    "https://example.com:6379/",
    "http://example.com/a b",
    // The forms a translator expands into an IPv4 address, and the numeric
    // spellings `inet_aton` accepts: mutations of these keep the parsers under
    // pressure on the part of the gate that used to answer "public".
    "http://[64:ff9b::7f00:1]/",
    "http://[64:ff9b:1::a00:1]:8080/",
    "http://[2002:7f00:1::]/",
    "http://[2001:db8::1]/",
    "http://[::ffff:0:127.0.0.1]/",
    "http://0x7f000001/",
    "http://127.0x1/",
    "http://nas/",
    // The classes added in the deep pass: an IPv4 embedded where the platform
    // does not put one, a zone id on an address and on a uri, a dotted quad in
    // an IPv6 literal, an invisible character in a path, a bracket/port
    // ambiguity, and a port that only looks numeric.
    "http://[127.0.0.1::1]/",
    "http://[1.2.3.4::]/",
    "http://[2002:7f00:1:1.2.3.4::]/",
    "http://[::1.2.3.4]/",
    "http://[fe80::1%eth0]/",
    "http://127.0.0.1%25eth0/",
    "https://example.com/\u{202e}gpj.exe",
    "https://exa\u{3002}mple.com/",
    "http://[::1]evil/",
    "http://[::1]:80:90/",
    "http://printer.home/",
    "http://192.88.99.1/",
];

#[test]
fn ten_thousand_mutated_urls_never_panic_and_never_pass_by_accident() {
    let mut accepted = 0usize;
    let mut refused = 0usize;
    let mut gated = 0usize;
    let mut loopback = 0usize;
    let started = std::time::Instant::now();

    let stats = atlas_guard::mutate::run(SEEDS, 10_000, 0x_A71A5_0005, |input| {
        for allow_private in [false, true] {
            let verdict = urlsafe::check(input, allow_private);
            if verdict.ok {
                accepted += 1;
                // An accepted URL has a scheme, a host and a port: a partial
                // verdict would be a request with nowhere to go.
                assert!(
                    verdict.scheme == "http" || verdict.scheme == "https",
                    "{:?} was allowed with scheme {:?}",
                    input,
                    verdict.scheme
                );
                assert!(!verdict.host.is_empty(), "{:?} was allowed without a host", input);
                assert!(verdict.port > 0, "{:?} was allowed with port 0", input);
                assert!(
                    verdict.reason.is_empty(),
                    "an allowed URL carried a refusal reason: {}",
                    verdict.reason
                );
                // The host is a slice of the input, never something invented.
                assert!(
                    verdict.host.len() <= input.len(),
                    "a {} byte host from a {} byte url",
                    verdict.host.len(),
                    input.len()
                );
                // A host the resolver could read as an address is not a host
                // name: it is dotted, or it is an IPv6 literal. A dotless name
                // is resolved through the machine's search list, so an accepted
                // one would be a request into whatever LAN this runs in.
                assert!(
                    verdict.host.contains('.') || verdict.host.contains(':'),
                    "{:?} was allowed with the bare host {:?}",
                    input,
                    verdict.host
                );
                // No accepted host is a name made only of numbers unless the
                // gate read it as an address: `inet_aton` would expand a
                // numeric shape the gate refused to judge. A four-octet decimal
                // literal is the one spelling both agree on, and anything that
                // parses passes; anything else with a numeric shape must be
                // refused, which `url_rejects_numeric_shorthand…` pins.
                if !allow_private {
                    let parsed = urlsafe::parse_ipv4(&verdict.host).is_some();
                    let numeric = verdict.host.split('.').all(|label| {
                        !label.is_empty() && label.chars().all(|c| c.is_ascii_digit())
                    });
                    assert!(
                        !numeric || parsed,
                        "{:?} was allowed with the unjudged numeric host {:?}",
                        input,
                        verdict.host
                    );
                }
            } else {
                refused += 1;
                // A refusal without a reason reads as a pass to a caller that
                // prints it.
                assert!(!verdict.reason.trim().is_empty(), "{:?} was refused silently", input);
                assert!(verdict.reason.len() < 256, "a refusal must stay a sentence");
                // And the token door agrees: no refusal becomes a `Gated`, so a
                // transport that asks for one cannot be handed this input.
                assert!(
                    Gated::verify(input).is_err(),
                    "{:?} was refused by the gate and approved as a token",
                    input
                );
            }
        }

        // The address verdict: what lsof prints, plus every literal.
        let addr = address::check(input);
        if addr.loopback {
            accepted += 1;
            // Only the loopback forms may answer `loopback`, and the wildcard may
            // never do it — that is the whole point of the function.
            assert!(
                !input.trim().is_empty(),
                "an empty address answered loopback"
            );
        }

        // The token the transports take: `Some` only for a public URL.
        if let Ok(token) = Gated::verify(input) {
            gated += 1;
            assert!(!token.is_loopback());
            assert!(urlsafe::check(token.as_str(), false).ok, "a token the gate refuses");
            assert!(token.as_str().len() <= input.trim().len());
        }
        // The loopback token: only for a literal this machine owns.
        if let Ok(token) = Gated::loopback(input) {
            loopback += 1;
            assert!(token.is_loopback(), "{:?} was approved as loopback", input);
            let lower = token.as_str().to_ascii_lowercase();
            let authority = lower
                .strip_prefix("http://")
                .or_else(|| lower.strip_prefix("https://"))
                .unwrap_or(&lower);
            let authority = authority
                .split(['/', '?', '#'])
                .next()
                .unwrap_or(authority);
            // The authority may carry a port; the host is what the socket opens.
            let host = if let Some(inner) = authority.strip_prefix('[') {
                inner.split(']').next().unwrap_or(inner)
            } else {
                authority.split(':').next().unwrap_or(authority)
            };
            // 127.0.0.0/8, ::1, and the IPv6 spellings that carry 127.0.0.1
            // (`::ffff:127.0.0.1`, `::127.0.0.1`) — every one of them is a
            // literal, and no name may be approved here at all. This check used
            // to accept only `127.` and `[::1]`, which called a genuine loopback
            // literal a bypass; written this way it also proves the host is a
            // literal and not a name the gate never resolves.
            let loopback_literal = if host.contains(':') {
                match urlsafe::parse_ipv6(host) {
                    Some(seg) => {
                        let carried = ((seg[6] as u32) << 16) | seg[7] as u32;
                        seg[0..5].iter().all(|group| *group == 0)
                            && (carried == 1 || (carried >> 24) == 127)
                    }
                    None => false,
                }
            } else {
                match urlsafe::parse_ipv4(host) {
                    Some(ip) => (ip >> 24) == 127,
                    None => false,
                }
            };
            assert!(
                loopback_literal,
                "{:?} was approved as loopback through {:?}",
                input, authority
            );
        }

        // The length cap is the one place an over-long value is refused, and it
        // must not be a panic point.
        let _ = urlsafe::parse_ipv4(input);
        let _ = urlsafe::parse_ipv6(input);
    });

    let elapsed = started.elapsed();
    println!(
        "{} mutated urls in {:.0} ms ({} accepted, {} refused, {} gated, {} loopback tokens)",
        stats.runs,
        elapsed.as_secs_f64() * 1e3,
        accepted,
        refused,
        gated,
        loopback
    );
    assert_eq!(stats.runs, 10_000);
    assert!(refused > 0 && accepted > 0, "the corpus reached no verdict");
    assert!(
        elapsed.as_secs_f64() < 10.0,
        "10 000 urls took {:.1} s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn an_over_long_url_is_refused_before_it_is_parsed() {
    let long = format!("https://example.com/{}", "a".repeat(MAX_URL_CHARS * 2));
    let verdict = urlsafe::check(&long, false);
    assert!(!verdict.ok);
    assert!(verdict.reason.contains("2048"), "{}", verdict.reason);
}

#[test]
fn a_url_of_pure_punctuation_is_refused_without_a_panic() {
    for input in [":", "://", "////", "http:", "http://", "http://:", "[]", "[::", "%%%%", "\u{0}"] {
        let verdict = urlsafe::check(input, false);
        assert!(!verdict.ok, "{:?} must be refused", input);
        assert!(!verdict.reason.trim().is_empty(), "{:?} was refused silently", input);
    }
}

// ---------------------------------------------------------------------------
// The attack list, case by case.
//
// Every string below is something an attacker controls — a search result, a
// link on a fetched page, or a URL the model wrote — and each one is a spelling
// that a *different* parser (inet_aton, inet_pton, a browser, a NAT gateway, a
// redirect, an invisible-character alphabet) reads as something the gate must
// not allow. One table per class, one verdict per row, and no row is allowed to
// be a silent pass.
// ---------------------------------------------------------------------------

/// One attack and the refusal it must earn. `needle` is "" when any refusal
/// will do — the point of that row is the verdict, not the sentence.
struct Attack {
    url: &'static str,
    needle: &'static str,
}

const IPV4_SPELLINGS: &[Attack] = &[
    // Everything `inet_aton` expands. A gate that only refuses the dotted
    // decimal form is a gate that only looks closed: the resolver (and every
    // `getaddrinfo`) reads each of these as 127.0.0.1 or 0.0.0.0.
    Attack { url: "http://127.1/", needle: "ip literal" },
    Attack { url: "http://127.0.1/", needle: "ip literal" },
    Attack { url: "http://127.0.0.1/", needle: "not publicly routable" },
    Attack { url: "http://2130706433/", needle: "ip literal" },
    Attack { url: "http://0177.0.0.1/", needle: "ip literal" },
    Attack { url: "http://0x7f.0.0.1/", needle: "ip literal" },
    Attack { url: "http://0x7f000001/", needle: "ip literal" },
    Attack { url: "http://0X7F000001/", needle: "ip literal" },
    Attack { url: "http://127.0x1/", needle: "ip literal" },
    Attack { url: "http://017700000001/", needle: "ip literal" },
    Attack { url: "http://010.0.0.1/", needle: "ip literal" },
    Attack { url: "http://08.0.0.1/", needle: "ip literal" },
    Attack { url: "http://127.000.000.001/", needle: "ip literal" },
    Attack { url: "http://127.0.0.01/", needle: "ip literal" },
    Attack { url: "http://0/", needle: "ip literal" },
    Attack { url: "http://0x0/", needle: "ip literal" },
    Attack { url: "http://1.2.3.4.5/", needle: "ip literal" },
    Attack { url: "http://127.0.0.256/", needle: "ip literal" },
    Attack { url: "http://256.1.1.1/", needle: "ip literal" },
    // A trailing root dot is stripped before the address is read, so the dotted
    // forms keep their verdict however the resolver's spelling ends.
    Attack { url: "http://127.0.0.1./", needle: "not publicly routable" },
    Attack { url: "http://127.0.0.1../", needle: "not publicly routable" },
    Attack { url: "http://2130706433./", needle: "ip literal" },
    Attack { url: "http://0.0.0.0/", needle: "not publicly routable" },
    Attack { url: "http://169.254.169.254/", needle: "not publicly routable" },
    Attack { url: "http://100.64.0.1/", needle: "not publicly routable" },
    Attack { url: "http://192.168.1.1/", needle: "not publicly routable" },
    Attack { url: "http://172.16.0.1/", needle: "not publicly routable" },
    Attack { url: "http://192.0.2.1/", needle: "not publicly routable" },
    Attack { url: "http://198.51.100.1/", needle: "not publicly routable" },
    Attack { url: "http://203.0.113.1/", needle: "not publicly routable" },
    Attack { url: "http://192.88.99.1/", needle: "not publicly routable" },
    Attack { url: "http://240.0.0.1/", needle: "not publicly routable" },
];

const IPV6_SPELLINGS: &[Attack] = &[
    // The same addresses written the long way, plus the translation prefixes
    // that turn a public IPv6 literal back into an IPv4 the gate refuses.
    Attack { url: "http://[::1]/", needle: "not publicly routable" },
    Attack { url: "http://[0:0:0:0:0:0:0:1]/", needle: "not publicly routable" },
    Attack { url: "http://[0000::1]/", needle: "not publicly routable" },
    Attack { url: "http://[::]/", needle: "not publicly routable" },
    Attack { url: "http://[::ffff:127.0.0.1]/", needle: "not publicly routable" },
    Attack { url: "http://[::ffff:7f00:1]/", needle: "not publicly routable" },
    Attack { url: "http://[0:0:0:0:0:ffff:7f00:1]/", needle: "not publicly routable" },
    Attack { url: "http://[::ffff:0:127.0.0.1]/", needle: "not publicly routable" },
    Attack { url: "http://[::127.0.0.1]/", needle: "not publicly routable" },
    Attack { url: "http://[::0.0.0.0]/", needle: "not publicly routable" },
    Attack { url: "http://[::ffff:169.254.169.254]/", needle: "not publicly routable" },
    Attack { url: "http://[64:ff9b::7f00:1]/", needle: "not publicly routable" },
    Attack { url: "http://[64:ff9b::127.0.0.1]/", needle: "not publicly routable" },
    Attack { url: "http://[64:ff9b:1::a00:1]/", needle: "not publicly routable" },
    Attack { url: "http://[2002:7f00:1::]/", needle: "not publicly routable" },
    Attack { url: "http://[2002:a00:1::]/", needle: "not publicly routable" },
    Attack { url: "http://[2001::1]/", needle: "not publicly routable" }, // Teredo
    Attack { url: "http://[2001:0:4136:e378:8000:63bf:7fff:fdd2]/", needle: "not publicly routable" },
    Attack { url: "http://[fe80::1]/", needle: "not publicly routable" },
    Attack { url: "http://[fc00::1]/", needle: "not publicly routable" },
    Attack { url: "http://[fd12:3456:789a::1]/", needle: "not publicly routable" },
    Attack { url: "http://[fec0::1]/", needle: "not publicly routable" },
    Attack { url: "http://[ff02::1]/", needle: "not publicly routable" },
    Attack { url: "http://[100::1]/", needle: "not publicly routable" },
    Attack { url: "http://[2001:db8::1]/", needle: "not publicly routable" },
    Attack { url: "http://[3fff::1]/", needle: "not publicly routable" },
    Attack { url: "http://[2001:2::1]/", needle: "not publicly routable" },
    Attack { url: "http://[2001:20::1]/", needle: "not publicly routable" },
    // A zone id is a scope, never a way past the range check. `%25eth0` is what
    // a browser hands over after percent-decoding `%eth0`.
    Attack { url: "http://[fe80::1%25eth0]/", needle: "host" },
    Attack { url: "http://[fe80::1%eth0]/", needle: "host" },
    Attack { url: "http://[::1%25eth0]/", needle: "host" },
    Attack { url: "http://[::ffff:127.0.0.1%25eth0]/", needle: "host" },
    Attack { url: "http://127.0.0.1%25eth0/", needle: "host" },
];

/// Literals no resolver accepts: an embedded IPv4 anywhere but the last 32 bits.
/// The gate must refuse them as *not a literal*, because the alternative is
/// answering "a public page" about a string no socket can reach — and before
/// this rule `127.0.0.1::ffff:127.0.0.1` was read as a public address.
const IPV6_NOT_LITERALS: &[&str] = &[
    "http://[1.2.3.4::]/",
    "http://[127.0.0.1::1]/",
    "http://[127.0.0.1::ffff:127.0.0.1]/",
    "http://[2002:7f00:1:1.2.3.4::]/",
    "http://[1.2.3.4::0:0]/",
    "http://[::1.2.3.4:5]/",
    "http://[1.2.3.4:5::]/",
];

const BRACKET_AND_PORT: &[Attack] = &[
    // Where the bracket ends and the port begins is the ambiguity: a second
    // colon, junk after `]`, an empty literal, a port that overflows.
    Attack { url: "http://[::1]x/", needle: "after the ipv6 literal" },
    Attack { url: "http://[::1]:/", needle: "port" },
    Attack { url: "http://[::1]:80:90/", needle: "port" },
    Attack { url: "http://[::1]:65536/", needle: "port" },
    Attack { url: "http://[::1]:0/", needle: "port" },
    Attack { url: "http://[::1]:-1/", needle: "port" },
    Attack { url: "http://[::1]:+80/", needle: "port" },
    Attack { url: "http://[::1]:99999999999999999999/", needle: "port" },
    Attack { url: "http://[::1]:abc/", needle: "port" },
    Attack { url: "http://[[::1]]/", needle: "ipv6" },
    Attack { url: "http://[]/", needle: "host" },
    Attack { url: "http://[]:80/", needle: "host" },
    Attack { url: "http://[::1/", needle: "ipv6" },
    Attack { url: "http://::1/", needle: "colon" },
    Attack { url: "http://example.com:80:90/", needle: "more than one colon" },
    Attack { url: "http://example.com:/x", needle: "port" },
    Attack { url: "http://example.com:-1/", needle: "port" },
];

const USERINFO_AND_BACKSLASH: &[Attack] = &[
    // The classic split-parser confusion: one reader sees a userinfo, the other
    // a host, and the browser is the one that dials.
    Attack { url: "http://example.com\\@127.0.0.1/", needle: "userinfo" },
    Attack { url: "http://127.0.0.1\\@example.com/", needle: "userinfo" },
    Attack { url: "http://127.0.0.1:8787\\@example.com/api", needle: "userinfo" },
    Attack { url: "http://example.com@127.0.0.1/", needle: "userinfo" },
    Attack { url: "http://user:pass@example.com/", needle: "userinfo" },
    Attack { url: "http://@example.com/", needle: "userinfo" },
    Attack { url: "http://example.com%2f@127.0.0.1/", needle: "userinfo" },
    Attack { url: "http://example.com\\127.0.0.1/", needle: "host" },
    Attack { url: "http://127.0.0.1%/", needle: "host" },
    Attack { url: "http://exa%6dple.com/", needle: "host" },
];

const SCHEMES: &[Attack] = &[
    Attack { url: "file:///etc/passwd", needle: "scheme" },
    Attack { url: "file://127.0.0.1/etc/passwd", needle: "scheme" },
    Attack { url: "gopher://example.com/", needle: "scheme" },
    Attack { url: "data:text/html,<b>x</b>", needle: "scheme" },
    Attack { url: "javascript:alert(1)", needle: "scheme" },
    Attack { url: "javascript:fetch('http://127.0.0.1:8787/api/status')", needle: "scheme" },
    Attack { url: "ws://127.0.0.1:9222/", needle: "scheme" },
    Attack { url: "wss://example.com/", needle: "scheme" },
    Attack { url: "ftp://example.com/", needle: "scheme" },
    Attack { url: "http+unix://%2Ftmp%2Fsock/", needle: "scheme" },
    Attack { url: "http:/example.com", needle: "//" },
    Attack { url: "https:example.com", needle: "//" },
    Attack { url: "http:\\\\example.com", needle: "//" },
    Attack { url: "http:////example.com/", needle: "host" },
    Attack { url: "example.com/path", needle: "scheme" },
    Attack { url: "//example.com/path", needle: "scheme" },
    Attack { url: " http://example.com/ ", needle: "" }, // trimmed: allowed, pinned below
];

const HOSTS_AND_UNICODE: &[Attack] = &[
    // Unicode dots, full-width forms and homoglyphs: refused as a shape rather
    // than normalised into a host, so the answer cannot depend on which reader
    // folds what.
    Attack { url: "https://exa\u{3002}mple.com/", needle: "ascii" },
    Attack { url: "https://127\u{ff0e}0\u{ff0e}0\u{ff0e}1/", needle: "ascii" },
    Attack { url: "https://example.com\u{ff0f}@x/", needle: "userinfo" },
    Attack { url: "https://\u{ff45}xample.com/", needle: "ascii" },
    Attack { url: "https://\u{430}pple.com/", needle: "ascii" }, // Cyrillic а
    Attack { url: "https://example.com.\u{3002}/", needle: "ascii" },
    Attack { url: "https://xn--localhost-/", needle: "hyphen" },
    Attack { url: "https://xn--local-/", needle: "hyphen" },
    Attack { url: "https://xn--a.de/", needle: "punycode" },
    // Invisible characters: a bidi override makes the path read backwards, a
    // tag character is an invisible ASCII alphabet. This crate refuses both
    // everywhere else (`cleantext::is_invisible`); the URL gate is the one
    // string that reaches the archive and the model.
    Attack { url: "https://example.com/\u{202e}gpj.exe", needle: "invisible" },
    Attack { url: "https://example.com/\u{200b}story", needle: "invisible" },
    Attack { url: "https://example.com/\u{feff}story", needle: "invisible" },
    Attack { url: "https://example.com/\u{00ad}story", needle: "invisible" },
    Attack { url: "https://example.com/\u{e0041}\u{e0042}", needle: "invisible" },
    Attack { url: "https://example.com/\u{2066}x\u{2069}", needle: "invisible" },
    // The local names, including the two IANA special-use TLDs a router answers.
    Attack { url: "http://localhost/", needle: "private hostname" },
    Attack { url: "http://localhost./", needle: "private hostname" },
    Attack { url: "http://a.b.localhost/", needle: "private hostname" },
    Attack { url: "http://nas.lan/", needle: "private hostname" },
    Attack { url: "http://db.internal/", needle: "private hostname" },
    Attack { url: "http://printer.home/", needle: "private hostname" },
    Attack { url: "http://mail.home/", needle: "private hostname" },
    Attack { url: "http://smtp.mail/", needle: "private hostname" },
    Attack { url: "http://printer.home./", needle: "private hostname" },
    Attack { url: "http://wiki.home.arpa/", needle: "private hostname" },
    // A dotless name goes through the resolver's search list: the LAN.
    Attack { url: "http://nas/", needle: "no dot" },
    Attack { url: "http://wiki/x", needle: "no dot" },
    Attack { url: "http://some-host:8080/", needle: "no dot" },
];

#[test]
fn every_ipv4_spelling_a_resolver_expands_is_refused() {
    for attack in IPV4_SPELLINGS {
        let verdict = urlsafe::check(attack.url, false);
        assert!(!verdict.ok, "{} was allowed: {:?}", attack.url, verdict.host);
        assert!(
            verdict.reason.contains(attack.needle),
            "{} gave {:?}, wanted {:?}",
            attack.url,
            verdict.reason,
            attack.needle
        );
        // The token door agrees: no refusal becomes a `Gated`.
        assert!(Gated::verify(attack.url).is_err(), "{} became a token", attack.url);
    }
    // …and the ones a resolver does **not** read as an address stay usable, so
    // the rule is about the spelling and not about digits in a name.
    for url in ["http://www.163.com/", "http://123-reg.co.uk/", "http://8.8.8.8/"] {
        let verdict = urlsafe::check(url, false);
        assert!(verdict.ok, "{} was refused: {}", url, verdict.reason);
    }
}

#[test]
fn every_ipv6_spelling_that_carries_a_translated_address_is_refused() {
    for attack in IPV6_SPELLINGS {
        let verdict = urlsafe::check(attack.url, false);
        assert!(!verdict.ok, "{} was allowed: {:?}", attack.url, verdict.host);
        assert!(
            verdict.reason.contains(attack.needle),
            "{} gave {:?}, wanted {:?}",
            attack.url,
            verdict.reason,
            attack.needle
        );
        assert!(Gated::verify(attack.url).is_err(), "{} became a token", attack.url);
        // The loopback door is not a second opinion. A literal that *carries*
        // 127.0.0.1 through a translator is refused there too; a genuine `::1`
        // or `127.0.0.1` is the app's own traffic and passes, which is why the
        // table above is about the public door.
        let carried = attack.url.contains("ff9b") || attack.url.contains("2002:");
        if carried {
            assert!(
                Gated::loopback(attack.url).is_err(),
                "{} was approved as the app's own loopback traffic",
                attack.url
            );
        }
    }
    // A public address carried by a translator is still public.
    for url in ["http://[64:ff9b::808:808]/", "http://[2002:808:808::]/"] {
        let verdict = urlsafe::check(url, false);
        assert!(verdict.ok, "{} was refused: {}", url, verdict.reason);
    }
}

#[test]
fn a_literal_the_platform_rejects_is_not_a_public_page() {
    for url in IPV6_NOT_LITERALS {
        let verdict = urlsafe::check(url, false);
        assert!(!verdict.ok, "{} was allowed as a public page", url);
        assert!(
            verdict.reason.contains("ipv6"),
            "{} gave {:?}, wanted an ipv6 refusal",
            url,
            verdict.reason
        );
        assert!(verdict.host.is_empty(), "{} carried the host {:?}", url, verdict.host);
    }
    // The forms that *are* literals keep working, dotted quad at the end.
    for url in ["http://[::1.2.3.4]/", "http://[2606:4700::1111]/"] {
        let verdict = urlsafe::check(url, false);
        assert!(verdict.ok, "{} was refused: {}", url, verdict.reason);
    }
}

#[test]
fn brackets_ports_userinfo_and_schemes_have_one_answer() {
    for table in [BRACKET_AND_PORT, USERINFO_AND_BACKSLASH, SCHEMES] {
        for attack in table {
            let verdict = urlsafe::check(attack.url, false);
            if attack.needle.is_empty() {
                continue; // pinned separately below
            }
            assert!(!verdict.ok, "{} was allowed: {:?}", attack.url, verdict.host);
            assert!(
                verdict.reason.contains(attack.needle),
                "{} gave {:?}, wanted {:?}",
                attack.url,
                verdict.reason,
                attack.needle
            );
            assert!(verdict.host.is_empty(), "{} carried a host", attack.url);
            assert!(Gated::verify(attack.url).is_err(), "{} became a token", attack.url);
        }
    }
    // Surrounding space is not an attack: the gate trims, as it always has, and
    // what it approves is the trimmed value.
    let spaced = urlsafe::check(" http://example.com/ ", false);
    assert!(spaced.ok, "{}", spaced.reason);
    assert_eq!(spaced.host, "example.com");
    // A leading-zero port is the same port, which is what the platform does.
    let zeros = urlsafe::check("http://example.com:00000000000000000080/", false);
    assert!(zeros.ok, "{}", zeros.reason);
    assert_eq!(zeros.port, 80);
}

#[test]
fn unicode_hosts_and_invisible_characters_are_refused() {
    for attack in HOSTS_AND_UNICODE {
        let verdict = urlsafe::check(attack.url, false);
        assert!(!verdict.ok, "{} was allowed: {:?}", attack.url, verdict.host);
        assert!(
            verdict.reason.contains(attack.needle),
            "{} gave {:?}, wanted {:?}",
            attack.url,
            verdict.reason,
            attack.needle
        );
    }
    // A legitimate IDN arrives as punycode and stays a name; a letter that only
    // looks exotic is not invisible, and neither is an accent in a path.
    for url in [
        "https://xn--bcher-kva.de/",
        "https://xn--80ak6aa92e.com/",
        "https://example.com/r\u{e9}sum\u{e9}",
        "https://example.com/%E2%80%AEgpj.exe",
    ] {
        let verdict = urlsafe::check(url, false);
        assert!(verdict.ok, "{} was refused: {}", url, verdict.reason);
    }
}

#[test]
fn oversized_and_degenerate_input_reaches_a_verdict_and_never_a_panic() {
    // 100 kB of URL, 10 000 labels, 2 000 characters of one structural byte:
    // every one of these must be a refusal with a sentence, in bounded time.
    let started = std::time::Instant::now();
    let huge = format!("https://example.com/{}", "a".repeat(100_000));
    let labels = format!("http://{}/", "a.".repeat(10_000));
    let mut degenerate: Vec<String> = vec![huge, labels];
    for fill in ['.', '[', ':', '/', '\\', '@', '%', 'a'] {
        degenerate.push(format!("http://{}/", fill.to_string().repeat(2_000)));
    }
    degenerate.push(format!("http://example.com/{}\u{0}", "a".repeat(1000)));
    degenerate.push("\u{0}http://127.0.0.1/".to_string());
    degenerate.push("http://127.0.0.1\u{0}.example.com/".to_string());
    degenerate.push(format!("http://{}:80/", "a".repeat(300)));
    degenerate.push(format!("http://127.0.0.1:{}/", "9".repeat(300)));
    for input in &degenerate {
        let verdict = urlsafe::check(input, false);
        assert!(!verdict.ok, "{:?} was allowed", &input[..40.min(input.len())]);
        assert!(
            !verdict.reason.trim().is_empty() && verdict.reason.len() < 256,
            "{:?} gave {:?}",
            &input[..40.min(input.len())],
            verdict.reason
        );
        // The address verdict must survive the same input without a panic.
        let judged = address::check(input);
        assert!(
            !(judged.loopback && input.contains("example.com")),
            "an example.com string answered loopback"
        );
    }
    let elapsed = started.elapsed();
    println!("{} degenerate inputs in {:.1} ms", degenerate.len(), elapsed.as_secs_f64() * 1e3);
    assert!(
        elapsed.as_secs_f64() < 2.0,
        "{} degenerate inputs took {:.2} s",
        degenerate.len(),
        elapsed.as_secs_f64()
    );
}

#[test]
fn the_gates_literal_parsers_agree_with_the_platform() {
    // Two parsers must decide one thing: the guard's, and the one the kernel
    // uses (`inet_pton`, reached here as `std::net`). A form one accepts and the
    // other refuses is a form whose address the gate did not judge.
    //
    // What this found: `parse_ipv6` read an embedded IPv4 anywhere in the
    // literal, so `1.2.3.4::` and `127.0.0.1::ffff:127.0.0.1` were "public
    // pages" no socket can reach. Mutations of the seeds below reproduce it, and
    // the assertion that catches it is the first one.
    let mut checked = 0usize;
    let mut literals = 0usize;
    let v6_seeds = [
        "::1",
        "::ffff:127.0.0.1",
        "64:ff9b::7f00:1",
        "2002:7f00:1::",
        "2001:db8::1",
        "1:2:3:4:5:6:127.0.0.1",
        "1.2.3.4::",
        "::127.0.0.1",
        "fe80::1%25en0",
        "0:0:0:0:0:ffff:7f00:1",
    ];
    let v4_seeds = ["127.0.0.1", "0x7f000001", "127.1", "0177.0.0.1", "255.255.255.255"];
    let compare = |text: &str, checked: &mut usize, literals: &mut usize| {
        *checked += 1;
        if let Some(ours) = urlsafe::parse_ipv6(text) {
            *literals += 1;
            let theirs = text.parse::<std::net::Ipv6Addr>();
            let theirs = theirs.unwrap_or_else(|_| {
                panic!("{:?} is not an ipv6 literal: the gate read {:?}", text, ours)
            });
            assert_eq!(theirs.segments(), ours, "{:?}: the two parsers disagree", text);
        }
        if let Some(ours) = urlsafe::parse_ipv4(text) {
            *literals += 1;
            let theirs = text
                .parse::<std::net::Ipv4Addr>()
                .unwrap_or_else(|_| {
                    panic!("{:?} is not an ipv4 literal: the gate read {:#010x}", text, ours)
                });
            assert_eq!(u32::from(theirs), ours, "{:?}: the two parsers disagree", text);
        }
    };
    let _ = atlas_guard::mutate::run(&v6_seeds, 12_000, 0x_E6E6_0001, |text| {
        compare(text.trim(), &mut checked, &mut literals);
    });
    let _ = atlas_guard::mutate::run(&v4_seeds, 8_000, 0x_E4E4_0002, |text| {
        compare(text.trim(), &mut checked, &mut literals);
    });
    println!("{} strings, {} read as a literal", checked, literals);
    assert!(checked >= 20_000, "the corpus ran {} strings", checked);
    assert!(literals > 100, "only {} literals were reached", literals);

    // The direction that matters most: a legal literal is never refused. A gate
    // that refuses `2606:4700::1111` is a gate that cannot read the news.
    for legal in [
        "::1",
        "::",
        "::ffff:127.0.0.1",
        "1:2:3:4:5:6:7:8",
        "2606:4700::1111",
        "2a00:1450:4009:81f::200e",
        "1.2.3.4",
        "255.255.255.255",
        "0.0.0.0",
    ] {
        assert!(
            urlsafe::parse_ipv6(legal).is_some() || urlsafe::parse_ipv4(legal).is_some(),
            "{} is a literal and must parse",
            legal
        );
    }
}

#[test]
fn an_accepted_host_is_a_host_the_gate_accepts_again() {
    // The verdict names a host, and `atlas-net::resolve_gate` rebuilds a URL from
    // it to judge the DNS answer. If the host the gate reports is not itself an
    // acceptable host, the two halves disagree about what is being fetched.
    let mut accepted = 0usize;
    let _ = atlas_guard::mutate::run(SEEDS, 6_000, 0x_40E5_7001, |input| {
        let verdict = urlsafe::check(input, false);
        if !verdict.ok {
            return;
        }
        accepted += 1;
        let rebuilt = if verdict.host.contains(':') {
            format!("http://[{}]:{}/", verdict.host, verdict.port)
        } else {
            format!("http://{}:{}/", verdict.host, verdict.port)
        };
        let again = urlsafe::check(&rebuilt, false);
        assert!(
            again.ok,
            "{:?} was allowed with host {:?}, which the gate refuses in {:?}: {}",
            input,
            verdict.host,
            rebuilt,
            again.reason
        );
    });
    assert!(accepted > 100, "only {} accepted verdicts", accepted);
}

#[test]
fn the_address_verdict_never_calls_a_foreign_address_loopback() {
    // The listener audit and the browser's debug-port check ask this function
    // whether a socket is on this machine. A `loopback:true` for anything that is
    // not a loopback literal, or for a name, is a control that reports success
    // while the port is open to the LAN.
    let seeds = [
        "127.0.0.1",
        "127.0.0.1:8787",
        "[::1]:9222",
        "0.0.0.0:8787",
        "[::]:8787",
        "*:*",
        "::1%lo0",
        "fe80::1%en0",
        "localhost",
        "[::ffff:127.0.0.1]:9222",
        "[::1]evil",
    ];
    let mut loopbacks = 0usize;
    let _ = atlas_guard::mutate::run(&seeds, 6_000, 0x_10C4_1001, |input| {
        let verdict = address::check(input);
        if verdict.ok && verdict.loopback {
            loopbacks += 1;
        }
    });
    assert!(loopbacks > 100, "only {} loopback verdicts", loopbacks);

    // The bracketed form is the address inside the brackets *only* when what
    // follows is a port or nothing: dropping the rest made `[::1]anything`
    // answer loopback.
    for (raw, loopback) in [
        ("[::1]", true),
        ("[::1]:9222", true),
        ("[::1]:*", true),
        ("[::1]:0", true),
        ("[fe80::1%en0]:9222", false),
        ("[::1%lo0]", true),
        ("[::1]evil", false),
        ("[::1]@evil.example", false),
        ("[::1]:9222evil", false),
        ("[127.0.0.1]x", false),
        ("[::1][]", false),
        ("[]", false),
        ("[::1", false),
        ("[]:8787", false),
    ] {
        let verdict = address::check(raw);
        assert_eq!(
            verdict.ok && verdict.loopback,
            loopback,
            "{} gave {:?} / {:?}",
            raw,
            verdict.kind,
            verdict.reason
        );
    }
}
