//! `ipaddr` — IP literals, and which of them live inside a machine or a LAN.
//!
//! Decides how an IPv4 literal is read (four decimal octets, no shorthand) and
//! which IPv4 ranges are loopback, private, link-local, carrier-grade NAT,
//! documentation or otherwise unroutable; how an IPv6 literal is read (groups,
//! `::` compression, a trailing embedded IPv4) and which IPv6 ranges are
//! `::`-wildcard, loopback, unique-local, link-local, or a container for an
//! IPv4 address that a translator would deliver to (`::ffff:a.b.c.d`, NAT64,
//! 6to4) — a literal is only as safe as the address it carries.
//!
//! Both parsers are strict on purpose: a form a resolver would expand and this
//! parser would not is refused by the gate, not guessed at. It does not resolve
//! names — a hostname never reaches this module — and it does not decide whether
//! a URL may be fetched (that is `urlsafe`). `address` shares these parsers, so
//! the listener audit and the fetch gate cannot disagree about what `127.0.0.1`
//! is.

pub fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut value: u32 = 0;
    for part in parts {
        if part.is_empty() || part.len() > 3 {
            return None;
        }
        // "010" is octal to inet_aton and decimal to a reader: refuse it.
        if part.len() > 1 && part.starts_with('0') {
            return None;
        }
        let mut octet: u32 = 0;
        for ch in part.chars() {
            if !ch.is_ascii_digit() {
                return None;
            }
            octet = octet * 10 + (ch as u32 - '0' as u32);
        }
        if octet > 255 {
            return None;
        }
        value = (value << 8) | octet;
    }
    Some(value)
}

pub(crate) fn is_private_v4(ip: u32) -> bool {
    let a = ((ip >> 24) & 0xff) as u8;
    let b = ((ip >> 16) & 0xff) as u8;
    let c = ((ip >> 8) & 0xff) as u8;
    if a == 0 || a == 10 || a == 127 {
        return true;
    }
    if a == 172 && (16..=31).contains(&b) {
        return true;
    }
    if a == 192 && b == 168 {
        return true;
    }
    if a == 169 && b == 254 {
        return true;
    }
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    if a == 192 && b == 0 && (c == 0 || c == 2) {
        return true;
    }
    if a == 198 && (b == 18 || b == 19) {
        return true;
    }
    if a == 198 && b == 51 && c == 100 {
        return true;
    }
    if a == 192 && b == 88 && c == 99 {
        return true; // 6to4 relay anycast (RFC 7526, deprecated)
    }
    if a == 203 && b == 0 && c == 113 {
        return true;
    }
    if a >= 224 {
        return true; // multicast, reserved, broadcast
    }
    false
}

/// One half of a compressed literal, as groups.
///
/// `v4_allowed` is true only for the half that ends up at the *end* of the
/// address. RFC 4291 §2.2 puts an embedded IPv4 in the last 32 bits and
/// nowhere else — `1.2.3.4::`, `127.0.0.1::1` and `127.0.0.1::ffff:127.0.0.1`
/// are not literals any resolver, kernel or `inet_pton` accepts, and reading
/// them here would mean answering "a public page" about a string that is not a
/// page at all. `tests/fuzz_url.rs` pins the whole parser against the
/// platform's: for every string either parser accepts, the two must agree on
/// the address, and this is the one disagreement that measurement found.
fn parse_v6_groups(part: &str, v4_allowed: bool) -> Option<Vec<u16>> {
    let mut out: Vec<u16> = Vec::new();
    if part.is_empty() {
        return Some(out);
    }
    let pieces: Vec<&str> = part.split(':').collect();
    let count = pieces.len();
    for (i, piece) in pieces.iter().enumerate() {
        if piece.is_empty() {
            return None;
        }
        if piece.contains('.') {
            // The embedded address must be the last thing in the literal: last
            // in this half, and this half must be the one that ends the address.
            if !v4_allowed || i + 1 != count {
                return None;
            }
            let v4 = parse_ipv4(piece)?;
            out.push(((v4 >> 16) & 0xffff) as u16);
            out.push((v4 & 0xffff) as u16);
        } else {
            if piece.len() > 4 {
                return None;
            }
            let mut value: u16 = 0;
            for ch in piece.chars() {
                let digit = match ch.to_digit(16) {
                    Some(d) => d,
                    None => return None,
                };
                value = value * 16 + digit as u16;
            }
            out.push(value);
        }
    }
    Some(out)
}

pub fn parse_ipv6(s: &str) -> Option<[u16; 8]> {
    if s.is_empty() || s.len() > 45 {
        return None;
    }
    let mut halves = s.splitn(2, "::");
    let head_s = match halves.next() {
        Some(h) => h,
        None => return None,
    };
    let tail_s = halves.next();
    // A `::` in the middle means the head is *not* where the address ends, so an
    // embedded IPv4 belongs in the tail. Without `::` the head is the whole
    // literal and the address may end there.
    let head = parse_v6_groups(head_s, tail_s.is_none())?;
    let mut seg = [0u16; 8];
    match tail_s {
        None => {
            if head.len() != 8 {
                return None;
            }
            for (i, value) in head.iter().enumerate() {
                seg[i] = *value;
            }
        }
        Some(tail_raw) => {
            let tail = parse_v6_groups(tail_raw, true)?;
            if head.len() + tail.len() > 7 {
                return None;
            }
            for (i, value) in head.iter().enumerate() {
                seg[i] = *value;
            }
            let offset = 8 - tail.len();
            for (i, value) in tail.iter().enumerate() {
                seg[offset + i] = *value;
            }
        }
    }
    Some(seg)
}

/// The IPv4 address an IPv6 literal *carries*, when it carries one.
///
/// Every form below is a way to spell `127.0.0.1` that a resolver, a kernel or
/// a NAT gateway will connect to for real. Refusing the decimal spelling and
/// accepting `64:ff9b::7f00:1` would be a gate that only looks closed, so the
/// embedded address is extracted and judged by the IPv4 rules:
///
/// | form | range | who expands it |
/// | --- | --- | --- |
/// | `::a.b.c.d` | `::/96` | deprecated IPv4-compatible |
/// | `::ffff:a.b.c.d` | `::ffff:0:0/96` | IPv4-mapped, every kernel |
/// | `::ffff:0:a.b.c.d` | `::ffff:0:0:0/96` | IPv4-translated (SIIT) |
/// | `64:ff9b::a.b.c.d` | `64:ff9b::/96` | NAT64 well-known prefix |
/// | `64:ff9b:1::a.b.c.d` | `64:ff9b:1::/48` | local-use NAT64 (RFC 8215) |
/// | `2002:aabb:ccdd::` | `2002::/16` | 6to4, the v4 is in groups 1-2 |
pub(crate) fn embedded_v4(seg: &[u16; 8]) -> Option<u32> {
    let v4 = |high: u16, low: u16| ((high as u32) << 16) | (low as u32);
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 {
        if seg[5] == 0xffff || seg[5] == 0 {
            return Some(v4(seg[6], seg[7]));
        }
    }
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0xffff && seg[5] == 0 {
        return Some(v4(seg[6], seg[7]));
    }
    // 64:ff9b::/96 and 64:ff9b:1::/48 both keep the IPv4 in the last 32 bits.
    // A literal inside 64:ff9b::/32 but outside both prefixes is judged the
    // same way: the answer is then conservative, which is the right direction.
    if seg[0] == 0x0064 && seg[1] == 0xff9b && (seg[2] == 0 || seg[2] == 1) {
        return Some(v4(seg[6], seg[7]));
    }
    if seg[0] == 0x2002 {
        return Some(v4(seg[1], seg[2]));
    }
    None
}

pub(crate) fn is_private_v6(seg: &[u16; 8]) -> bool {
    if seg.iter().all(|v| *v == 0) {
        return true; // ::
    }
    if (seg[0] & 0xfe00) == 0xfc00 {
        return true; // fc00::/7 unique local
    }
    if (seg[0] & 0xffc0) == 0xfe80 {
        return true; // fe80::/10 link local
    }
    if (seg[0] & 0xffc0) == 0xfec0 {
        return true; // fec0::/10 site local (deprecated)
    }
    if (seg[0] & 0xff00) == 0xff00 {
        return true; // ff00::/8 multicast
    }
    // Ranges that exist on paper only: a discarded packet, a tunnel endpoint, a
    // documentation example. None of them can be a publisher, and a fetch to one
    // is at best a wasted request and at worst a probe of an internal tunnel.
    if seg[0] == 0x0100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 {
        return true; // 100::/64 discard-only
    }
    if seg[0] == 0x2001 && seg[1] == 0 {
        return true; // 2001::/32 Teredo
    }
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return true; // 2001:db8::/32 documentation
    }
    if seg[0] == 0x2001 && seg[1] == 0x0002 && seg[2] == 0 {
        return true; // 2001:2::/48 benchmarking (RFC 5180)
    }
    if seg[0] == 0x2001 && (seg[1] & 0xfff0) == 0x0010 {
        return true; // 2001:10::/28 ORCHID (RFC 4843): a crypto identifier
    }
    if seg[0] == 0x2001 && (seg[1] & 0xfff0) == 0x0020 {
        return true; // 2001:20::/28 ORCHIDv2 (RFC 7343)
    }
    if seg[0] == 0x3fff && (seg[1] & 0xf000) == 0 {
        return true; // 3fff::/20 documentation (RFC 9637)
    }
    match embedded_v4(seg) {
        Some(ip) => is_private_v4(ip),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v6(text: &str) -> [u16; 8] {
        parse_ipv6(text).expect("a valid literal")
    }

    fn v4(text: &str) -> u32 {
        parse_ipv4(text).expect("a valid literal")
    }

    #[test]
    fn ipv4_ranges_are_read_from_the_octets() {
        for private in [
            "0.0.0.0", "10.1.2.3", "127.0.0.1", "169.254.169.254", "172.16.0.1", "172.31.255.255",
            "192.168.1.1", "100.64.0.1", "100.127.255.255", "192.0.0.8", "192.0.2.1",
            "198.18.0.1", "198.19.255.255", "198.51.100.7", "203.0.113.9", "224.0.0.1",
            "255.255.255.255", "192.88.99.1",
        ] {
            assert!(is_private_v4(v4(private)), "{} must be unroutable", private);
        }
        for public in ["1.1.1.1", "8.8.8.8", "172.32.0.1", "100.128.0.1", "192.0.1.1", "198.20.0.1", "192.88.100.1"] {
            assert!(!is_private_v4(v4(public)), "{} is public", public);
        }
    }

    #[test]
    fn an_ipv6_literal_carries_an_ipv4_address_in_six_forms() {
        for (literal, carried) in [
            ("::127.0.0.1", "127.0.0.1"),
            ("::ffff:127.0.0.1", "127.0.0.1"),
            ("::ffff:7f00:1", "127.0.0.1"),
            ("::ffff:0:10.0.0.5", "10.0.0.5"),
            ("64:ff9b::7f00:1", "127.0.0.1"),
            ("64:ff9b::a00:1", "10.0.0.1"),
            ("64:ff9b:1::c0a8:101", "192.168.1.1"),
            ("2002:7f00:1::", "127.0.0.1"),
            ("2002:a00:1::", "10.0.0.1"),
        ] {
            let seg = v6(literal);
            assert_eq!(
                embedded_v4(&seg),
                Some(v4(carried)),
                "{} should carry {}",
                literal,
                carried
            );
            assert!(
                is_private_v6(&seg),
                "{} carries an address the gate refuses in its v4 spelling",
                literal
            );
        }
        // A plain public literal carries nothing.
        for literal in ["2606:4700::1111", "2001:4860:4860::8888", "64:ff9b::808:808"] {
            let seg = v6(literal);
            if literal.starts_with("64:ff9b") {
                assert_eq!(embedded_v4(&seg), Some(v4("8.8.8.8")));
                assert!(!is_private_v6(&seg), "{} carries a public address", literal);
            } else {
                assert_eq!(embedded_v4(&seg), None, "{} carries no v4", literal);
                assert!(!is_private_v6(&seg), "{} is public", literal);
            }
        }
    }

    #[test]
    fn ipv6_ranges_that_are_never_a_publisher_are_refused() {
        for private in [
            "::",
            "::1",
            "fc00::1",
            "fd12:3456:789a::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "3fff::1",
            // Paper-only ranges: benchmarking and the two ORCHID blocks. None of
            // them can be a publisher, and the module already refuses the other
            // ranges of the same kind (documentation, discard, Teredo).
            "2001:2::1",
            "2001:10::1",
            "2001:20::1",
            "2001:2f::1",
        ] {
            assert!(is_private_v6(&v6(private)), "{} must be unroutable", private);
        }
        for public in [
            "2606:4700::1111",
            "2001:4860:4860::8888",
            "2a00:1450:4009:81f::200e",
            "2001:200::1",
            "3f00::1",
        ] {
            assert!(!is_private_v6(&v6(public)), "{} is public", public);
        }
        // 3fff::/20 is the documentation block; 3fff:1000:: is outside it and
        // must not be swept in with it.
        assert!(!is_private_v6(&v6("3fff:1000::1")));
        // …and the same for the ranges added above: the neighbours of a /28 and
        // the block after a /48 stay public.
        assert!(!is_private_v6(&v6("2001:30::1")));
        assert!(!is_private_v6(&v6("2001:2:1::1")));
    }

    #[test]
    fn an_embedded_ipv4_is_only_where_the_platform_puts_one() {
        // RFC 4291 §2.2: the dotted quad is the last 32 bits and nothing else.
        // Every string below is refused by `inet_pton`, `std::net::Ipv6Addr` and
        // the url crate. Reading them as an address would answer "a public page"
        // about a string that no resolver can dial — and, before this rule,
        // `parse_ipv6("127.0.0.1::1")` answered a public address for a literal
        // that mentions 127.0.0.1 twice.
        for illegal in [
            "1.2.3.4::",
            "127.0.0.1::1",
            "127.0.0.1::ffff:127.0.0.1",
            "2002:7f00:1:1.2.3.4::",
            "ffff:127.0.0.1::",
            "1.2.3.4::0:0",
            "::1.2.3.4:5",
            "1.2.3.4:5::",
        ] {
            assert!(
                parse_ipv6(illegal).is_none(),
                "{} is not an ipv6 literal",
                illegal
            );
        }
        // The forms that are: the quad ends the address, compressed or not.
        for (legal, carried) in [
            ("::1.2.3.4", "1.2.3.4"),
            ("::ffff:1.2.3.4", "1.2.3.4"),
            ("2002:7f00:1:0:0:0:1.2.3.4", "127.0.0.1"),
            ("1:2:3:4:5:6:1.2.3.4", ""),
            ("64:ff9b::7f00:1", "127.0.0.1"),
        ] {
            let seg = v6(legal);
            assert!(
                parse_ipv6(legal).is_some(),
                "{} is an ipv6 literal",
                legal
            );
            if !carried.is_empty() {
                // Only a prefix that *carries* an IPv4 has one to extract: a
                // plain `x:x:x:x:x:x:d.d.d.d` is just 128 bits.
                assert_eq!(embedded_v4(&seg), Some(v4(carried)), "{}", legal);
            }
        }
    }
}
