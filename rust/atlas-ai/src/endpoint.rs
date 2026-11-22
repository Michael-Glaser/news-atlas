//! The caller's base URL, validated once: `http://<loopback>:<port>[/prefix]`.
//!
//! Decisions: only `http`; only a loopback literal or the name `localhost`
//! (mapped to `127.0.0.1` *without* asking the resolver — this crate never
//! resolves a name); a port must be named, because defaulting one would be this
//! crate inventing one; no credentials, query or fragment. The prefix kept here is
//! what `/v1/chat/completions` is appended to.
//!
//! The validated endpoint also holds the gate's token: `atlas_guard::Gated::loopback`
//! on the literal the socket is opened to. That is the structural half — the
//! transport carries a verdict, not a string this crate assembled — and it means
//! "loopback only" is one decision, made in `atlas-guard`, asked twice (the address
//! verdict above and the token below).
//!
//! It does not: open a socket, retry, or read a body — [`crate::http1`] does. A
//! non-loopback base URL is a [`AiError::Config`] before anything else happens.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::error::{clip, AiError};

/// The only names a model endpoint may use. `localhost` is mapped to
/// `127.0.0.1` without asking the resolver.
const HOST_NAME: &str = "localhost";

/// crate has no default port of its own: the base URL must name one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    ip: IpAddr,
    port: u16,
    /// `host:port` exactly as it goes into the `Host` header.
    pub(crate) authority: String,
    /// Path prefix from the base URL, without a trailing slash (may be empty).
    pub(crate) prefix: String,
    /// The base URL as given, for a status line.
    base: String,
    /// The gate's token for this endpoint, built from the literal that is dialled.
    gated: atlas_guard::Gated,
}

impl Endpoint {
    /// Parse `http://127.0.0.1:PORT[/prefix]`.
    ///
    /// Refused, loudly, with [`AiError::Config`]: anything but `http`; a host this
    /// crate does not recognise as a loopback literal; a missing port (defaulting
    /// one would be this crate inventing a port); credentials, a query, a fragment.
    pub fn parse(base_url: &str) -> Result<Endpoint, AiError> {
        let raw = base_url.trim();
        if raw.is_empty() {
            return Err(AiError::Config("the model base URL is empty".into()));
        }
        let lower = raw.to_ascii_lowercase();
        let rest = if let Some(tail) = lower.strip_prefix("http://") {
            // Slice the original, so the case of a path survives.
            &raw[7..7 + tail.len()]
        } else if lower.starts_with("https://") {
            return Err(AiError::Config(
                "the model base URL must be http:// — this crate does no TLS to a loopback server"
                    .into(),
            ));
        } else {
            return Err(AiError::Config(format!(
                "the model base URL must start with http:// (got {:?})",
                clip(raw, 60)
            )));
        };
        if rest.contains('?') || rest.contains('#') {
            return Err(AiError::Config(
                "the model base URL must not carry a query or a fragment".into(),
            ));
        }
        if rest.contains('@') {
            return Err(AiError::Config(
                "the model base URL must not carry credentials".into(),
            ));
        }
        let (authority, prefix) = match rest.find('/') {
            Some(pos) => (&rest[..pos], rest[pos..].trim_end_matches('/').to_string()),
            None => (rest, String::new()),
        };
        if authority.is_empty() {
            return Err(AiError::Config("the model base URL names no host".into()));
        }

        let (host, port_text) = split_authority(authority)?;
        let port_text = port_text.ok_or_else(|| {
            AiError::Config(
                "the model base URL must name its port explicitly (http://127.0.0.1:PORT)".into(),
            )
        })?;
        let port: u16 = port_text
            .parse()
            .map_err(|_| AiError::Config(format!("{:?} is not a port number", clip(&port_text, 20))))?;
        if port == 0 {
            return Err(AiError::Config("port 0 is not a model server".into()));
        }

        // The one address decision: asked of the guard, never made here.
        let verdict = atlas_guard::address::check(&host);
        if !verdict.ok || !verdict.loopback {
            return Err(AiError::Config(format!(
                "the model server must be a loopback address; {:?} is not ({})",
                clip(&host, 60),
                verdict.reason
            )));
        }
        let ip: IpAddr = if let Ok(v4) = host.parse::<Ipv4Addr>() {
            IpAddr::V4(v4)
        } else if let Ok(v6) = host.parse::<Ipv6Addr>() {
            IpAddr::V6(v6)
        } else {
            // A loopback *name*: mapped to the v4 literal, never resolved.
            debug_assert_eq!(host, HOST_NAME);
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let shown_host = match ip {
            IpAddr::V6(_) => format!("[{}]", host),
            IpAddr::V4(_) => host.clone(),
        };
        // The token the transport carries: the URL gate plus the address verdict,
        // on the literal the socket is opened to (a name was mapped above).
        let literal = match ip {
            IpAddr::V4(v4) => format!("http://{}:{}", v4, port),
            IpAddr::V6(v6) => format!("http://[{}]:{}", v6, port),
        };
        let gated = atlas_guard::Gated::loopback(&literal).map_err(|reason| {
            AiError::Config(format!(
                "the model server address was refused by the gate: {}",
                clip(&reason, 120)
            ))
        })?;
        Ok(Endpoint {
            ip,
            port,
            authority: format!("{}:{}", shown_host, port),
            prefix,
            base: raw.to_string(),
            gated,
        })
    }

    /// The address the socket is opened to — always a loopback literal.
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.ip, self.port)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The base URL as the caller gave it.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The gate's token for this endpoint: the loopback literal `atlas-guard`
    /// approved, which is the same address [`Endpoint::addr`] dials.
    pub fn gated(&self) -> &atlas_guard::Gated {
        &self.gated
    }

    /// A path on this endpoint, with the base URL's prefix applied.
    pub fn path(&self, suffix: &str) -> String {
        if self.prefix.is_empty() {
            suffix.to_string()
        } else {
            format!("{}{}", self.prefix, suffix)
        }
    }
}

/// Split `host[:port]`, honouring `[::1]:8080`. A bare `::1` has no port.
fn split_authority(authority: &str) -> Result<(String, Option<String>), AiError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let close = rest
            .find(']')
            .ok_or_else(|| AiError::Config("the model base URL has an unterminated IPv6 literal".into()))?;
        let host = rest[..close].to_string();
        let after = &rest[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.to_string()),
            None if after.is_empty() => None,
            None => {
                return Err(AiError::Config(
                    "the model base URL has characters after the IPv6 literal".into(),
                ))
            }
        };
        return Ok((host, port));
    }
    match authority.matches(':').count() {
        0 => Ok((authority.to_string(), None)),
        1 => {
            let pos = authority.find(':').unwrap_or(0);
            let (host, tail) = authority.split_at(pos);
            Ok((host.to_string(), Some(tail[1..].to_string())))
        }
        // A bare IPv6 literal, no port.
        _ => Ok((authority.to_string(), None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn endpoint_accepts_a_loopback_literal() {
        let endpoint = Endpoint::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(endpoint.port(), 8080);
        assert_eq!(endpoint.addr().to_string(), "127.0.0.1:8080");
        assert_eq!(endpoint.path("/v1/models"), "/v1/models");
    }

    #[test]
    fn endpoint_accepts_localhost_without_resolving_it() {
        let endpoint = Endpoint::parse("http://localhost:8090").unwrap();
        assert_eq!(endpoint.addr().to_string(), "127.0.0.1:8090");
    }

    #[test]
    fn endpoint_keeps_a_path_prefix() {
        let endpoint = Endpoint::parse("http://127.0.0.1:8080/llm/").unwrap();
        assert_eq!(endpoint.path("/v1/models"), "/llm/v1/models");
    }

    #[test]
    fn a_non_loopback_endpoint_is_refused_before_a_socket_exists() {
        for bad in [
            "http://10.0.0.5:8080",
            "http://example.com:8080",
            "https://127.0.0.1:8080",
            "http://127.0.0.1",
            "http://127.0.0.1:0",
            "http://127.0.0.1:8080/?q=1",
            "http://user:pw@127.0.0.1:8080",
            "127.0.0.1:8080",
            "",
        ] {
            let error = Endpoint::parse(bad).unwrap_err();
            assert_eq!(error.kind(), "config", "{:?} was accepted", bad);
        }
    }

}
