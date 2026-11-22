//! `gated` — a URL that already carries the gate's answer.
//!
//! This is the structural half of "the gate is never bypassed": the functions
//! that open a socket or write a request line ask for a `Gated`, not for a
//! `&str`, so a URL cannot reach the transport without having been through
//! [`crate::urlsafe::check`] first. A `Gated` has a private field and exactly two
//! constructors, both of which call this crate's gate:
//!
//! * [`Gated::verify`] — a public URL: `urlsafe::check(url, false)` said yes.
//! * [`Gated::loopback`] — the app's own local traffic (the model server): the
//!   URL gate accepts it *and* [`crate::address::check`] proves the host is a
//!   loopback literal. `allow_private` alone would also accept `10.0.0.5`, so the
//!   second verdict is what makes this loopback and not "private network".
//!
//! A loopback *name* (`localhost`) is refused by the URL gate on purpose — a
//! name is only loopback after a decision, and this crate does not resolve. A
//! caller that accepts the name maps it to the `127.0.0.1` literal first (as
//! `atlas-ai` does), so the value that reaches a socket is always the literal the
//! gate saw.
//!
//! It does not decide anything itself, it is not a parsed URL (no host, no path —
//! that is `atlas-net`'s `Verdict`), and it does not expire: a `Gated` is the
//! answer for the string it was built from, which is why the string is kept and
//! never re-derived from a redirect.
//!
//! A caller outside this crate cannot write `Gated { .. }` (the field is
//! private), cannot `Default` it, and there is no `From<String>`/`From<&str>`
//! impl — so the only way to hold one is to have asked the gate.

use std::fmt;

use crate::address;
use crate::urlsafe;

/// A URL this crate's gate approved. Unforgeable outside `atlas-guard`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Gated {
    /// Kept private on purpose: see the module comment.
    url: String,
    /// True when the address, not only the URL, was proven loopback.
    loopback: bool,
}

impl Gated {
    /// Approve a public URL: the one gate, `allow_private: false`.
    ///
    /// The returned value is the trimmed input, not a canonical form — turning a
    /// URL into a request line is `atlas-net`'s job, and it re-gates the result.
    pub fn verify(raw: &str) -> Result<Gated, String> {
        let check = urlsafe::check(raw, false);
        if !check.ok {
            return Err(refusal(check.reason));
        }
        Ok(Gated {
            url: raw.trim().to_string(),
            loopback: false,
        })
    }

    /// Approve the app's own loopback traffic (the local model server).
    ///
    /// Two verdicts, both from this crate: the URL gate with `allow_private: true`
    /// (scheme, userinfo, port syntax) and [`address::check`] on the host, which
    /// is the only thing that says "loopback". A LAN address passes the first and
    /// fails the second.
    pub fn loopback(raw: &str) -> Result<Gated, String> {
        let check = urlsafe::check(raw, true);
        if !check.ok {
            return Err(refusal(check.reason));
        }
        let verdict = address::check(&check.host);
        if !verdict.ok || !verdict.loopback {
            let why = if verdict.reason.is_empty() {
                "not a loopback address".to_string()
            } else {
                verdict.reason
            };
            return Err(refusal(why));
        }
        Ok(Gated {
            url: raw.trim().to_string(),
            loopback: true,
        })
    }

    /// The approved text, exactly as it was approved.
    pub fn as_str(&self) -> &str {
        &self.url
    }

    /// True when the address was proven loopback, not only the URL.
    pub fn is_loopback(&self) -> bool {
        self.loopback
    }
}

impl fmt::Display for Gated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url)
    }
}

/// A refusal always says something; an empty reason would read as a pass.
fn refusal(reason: String) -> String {
    if reason.trim().is_empty() {
        "refused by the URL gate".to_string()
    } else {
        reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_url_is_gated_and_keeps_its_text() {
        let gated = Gated::verify("https://example.com/a/b?x=1").expect("public url");
        assert_eq!(gated.as_str(), "https://example.com/a/b?x=1");
        assert!(!gated.is_loopback());
        // leading and trailing space is trimmed by the gate, as everywhere else
        assert_eq!(
            Gated::verify("  https://example.com/  ").unwrap().as_str(),
            "https://example.com/"
        );
    }

    #[test]
    fn every_class_the_gate_refuses_produces_no_gated_value() {
        for url in [
            "http://127.0.0.1/",
            "http://localhost:8080/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "http://user:pass@example.com/",
            "example.com/no-scheme",
            "",
            "   ",
        ] {
            let error = Gated::verify(url).expect_err("must not be approved");
            assert!(!error.trim().is_empty(), "{}: a refusal needs a reason", url);
        }
    }

    #[test]
    fn the_loopback_gate_accepts_only_loopback() {
        for url in [
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/llm/",
            "http://127.0.0.1:1",
            "http://[::1]:8080",
            "http://[::1]:8080/llm/",
        ] {
            let gated = Gated::loopback(url).expect("the app's own loopback traffic");
            assert!(gated.is_loopback(), "{}", url);
        }
        for url in [
            "http://10.0.0.5:8080",      // private, but not this machine
            "http://192.168.1.10:8080",  // the LAN
            "http://example.com:8080",   // a name on the internet
            "http://169.254.169.254:80", // link-local metadata
            "http://[fe80::1]:8080",     // ipv6 link-local
            "http://localhost:8080",     // a name: map it to the literal first
            "file:///etc/passwd",
        ] {
            let error = Gated::loopback(url).expect_err("must not be approved");
            assert!(!error.trim().is_empty(), "{}: a refusal needs a reason", url);
        }
    }

    #[test]
    fn a_public_url_cannot_be_passed_off_as_loopback() {
        // `verify` never sets the flag, whatever the URL is.
        assert!(!Gated::verify("https://example.com/").unwrap().is_loopback());
    }

    #[test]
    fn a_loopback_address_in_an_ipv6_disguise_reaches_neither_door() {
        // Every literal here is 127.0.0.1 (or 10.0.0.1) written the long way:
        // a NAT64 gateway, a SIIT translator or a 6to4 relay expands it back to
        // the IPv4 the gate refuses by name. The token is the thing the
        // transports take, so the disguise has to fail both doors — and it used
        // to open the public one.
        for url in [
            "http://[64:ff9b::7f00:1]:8080/",
            "http://[64:ff9b:1::a00:1]:8080/",
            "http://[2002:7f00:1::]:8080/",
            "http://[::ffff:0:127.0.0.1]:8080/",
        ] {
            assert!(
                Gated::verify(url).is_err(),
                "{} was approved as a public url",
                url
            );
            assert!(
                Gated::loopback(url).is_err(),
                "{} was approved as the app's own loopback traffic",
                url
            );
        }
    }

    #[test]
    fn the_field_is_private_and_there_are_exactly_two_doors() {
        // A source-level guard for the property the type system gives us: this
        // test fails the moment someone adds a third constructor or makes the
        // field `pub`, which is exactly the bypass that would break the claim.
        let source = include_str!("gated.rs");
        let struct_at = source.find("pub struct Gated {").expect("the struct");
        let body = &source[struct_at..];
        let end = body.find("\n}").expect("the struct's end");
        let fields = &body[..end];
        for line in fields.lines().skip(1) {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !trimmed.starts_with("pub "),
                "a `Gated` field must stay private: {}",
                trimmed
            );
        }
        let code = &source[..source.find("#[cfg(test)]").unwrap_or(source.len())];
        let doors = code.matches("-> Result<Gated, String>").count();
        assert_eq!(doors, 2, "exactly two constructors: verify and loopback");
        for bypass in [
            "impl From<&str> for Gated",
            "impl From<String> for Gated",
            "impl From<&String> for Gated",
            "impl Default for Gated",
        ] {
            assert!(!code.contains(bypass), "a bypass constructor: {}", bypass);
        }
    }
}
