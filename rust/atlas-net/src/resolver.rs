//! The address decision, made where the connection is made.
//!
//! The verdict ([`crate::gate`]) resolves a name and stops. Between that answer and
//! the connector's own lookup there was a window: the name could answer once with a
//! public address and once with `127.0.0.1`, and the socket would go to the second
//! answer having been judged on the first. That window is what this module closes —
//! not by resolving twice and hoping, but by putting the filter *inside* the
//! transport: [`FilteredResolver`] is the resolver every request is given, and the
//! addresses it hands the connector are the only ones that can be connected to.
//!
//! Three things are decided here:
//!
//! * what a name answers ([`Lookup`], [`SystemLookup`]) — and, for an address
//!   literal, that the answer is the literal in the URL and no lookup at all;
//! * which of those answers a request of a given [`Class`] may reach
//!   ([`approved`]) — the guard's classification, asked once per address, dropped
//!   rather than averaged: an answer the guard refuses is never connected to, and a
//!   name whose every answer is refused does not connect at all;
//! * how much of the fetch's budget the lookup may spend ([`Lookup::addresses`] is
//!   called under the same [`NextTimeout`] the connector was given), so a hostile
//!   resolver cannot buy a fresh timeout per hop.
//!
//! It does not: decide whether a *URL* may be fetched (that is `atlas-guard`'s
//! verdict, carried by [`crate::gate`]), open a socket, or read a body.

use std::fmt::Debug;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{mpsc, Arc};

use atlas_guard::Gated;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;
use ureq::Error;

/// What a request is allowed to reach, as the verdict said.
///
/// The class travels with the [`Gated`] token: `verify` (a URL from a feed, a page
/// or the model) can only ever produce [`Class::Public`], and only the guard's own
/// loopback door produces [`Class::Loopback`]. A request therefore cannot choose its
/// own class — it inherits the one it was approved with, exactly as a redirect hop
/// inherits its origin's class ([`crate::hop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A page on the public internet: no loopback, private, link-local or reserved
    /// address may be connected to, whatever the name answers.
    Public,
    /// The app's own traffic (the local model server): the address *is* this
    /// machine, so the check is that it really is.
    Loopback,
}

impl Class {
    /// The class of one approved token.
    pub fn of(token: &Gated) -> Class {
        if token.is_loopback() {
            Class::Loopback
        } else {
            Class::Public
        }
    }

    /// The class in a word, for a refusal a person or the model reads.
    pub fn name(self) -> &'static str {
        match self {
            Class::Public => "public",
            Class::Loopback => "loopback",
        }
    }
}

/// What one name answers, as addresses.
///
/// A trait rather than a function so the transport can be asked the same question a
/// test asks: the interesting failure (an answer that changes between the verdict
/// and the connect) is impossible to stage against the real resolver, and staging it
/// is the only way to prove the filter is load-bearing.
pub trait Lookup: Debug + Send + Sync {
    /// Every address `host` answers with. `host` may carry IPv6 brackets.
    fn addresses(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
}

/// The system resolver — the same `getaddrinfo` path `atlas-guard`'s neighbours and
/// every other program on this machine use.
#[derive(Debug, Default)]
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn addresses(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        let wanted = format!("{}:{}", host, port);
        match wanted.to_socket_addrs() {
            Ok(found) => Ok(found.collect()),
            Err(error) => Err(format!("the name does not resolve ({})", error)),
        }
    }
}

/// Every address one URL resolves to, with an address literal answered locally.
///
/// A literal is not a lookup: the connector is going to dial exactly what the URL
/// says, so asking a resolver what `127.0.0.1` means would be a second opinion about
/// a fact. It is also the reason the loopback door needs no resolver answer at all.
pub(crate) fn addresses_for(
    host: &str,
    port: u16,
    lookup: &dyn Lookup,
) -> Result<Vec<SocketAddr>, String> {
    let bare = host.trim();
    let bare = bare.strip_prefix('[').unwrap_or(bare);
    let bare = bare.strip_suffix(']').unwrap_or(bare);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    lookup.addresses(bare, port)
}

/// The answers a request of `class` may connect to, and why the rest were dropped.
///
/// This is the one filter: the egress check in [`crate::gate::resolve_gate_with`] and
/// the resolver below both call it, so a page cannot be approved by one rule and
/// connected by another. Dropping rather than refusing outright is what keeps a
/// multi-homed site readable: a name that answers with one public and one private
/// address is connected to on the public one, and never on the private one. When
/// nothing is left, the caller refuses — which is the rebinding case.
pub(crate) fn approved(
    addresses: &[SocketAddr],
    class: Class,
    port: u16,
) -> (Vec<SocketAddr>, Option<String>) {
    let mut kept: Vec<SocketAddr> = Vec::with_capacity(addresses.len());
    let mut first_refusal: Option<String> = None;
    for address in addresses {
        match crate::gate::address_verdict(address.ip(), port, class) {
            None => kept.push(*address),
            Some(reason) => {
                if first_refusal.is_none() {
                    first_refusal = Some(format!("{} ({})", address.ip(), reason));
                }
            }
        }
    }
    (kept, first_refusal)
}

/// The sentence a request whose every answer was refused earns.
///
/// It names the address and the class, because that is the difference between "this
/// site is down" and "this name was pointed at this machine". Two callers ask for
/// it — the transport's resolver and [`crate::gate::approved_addresses`], which a
/// browser asks before it hands a name to something else that resolves — so the
/// sentence has one implementation.
pub(crate) fn nothing_allowed(host: &str, class: Class, refusal: &str, refused_count: usize) -> String {
    let plural = if refused_count == 1 { "" } else { "s" };
    format!(
        "host {} answered with {} address{} this {} request may not reach: {}",
        host,
        refused_count,
        plural,
        class.name(),
        refusal
    )
}

/// The transport's resolver: the one place a name becomes an address the connector
/// may dial.
///
/// `resolve` is called by ureq for every connection, including every redirect hop,
/// with the URI that is about to be requested — so the filter runs on the address
/// that will actually be connected to, not on one an earlier verdict remembered.
#[derive(Debug)]
pub(crate) struct FilteredResolver {
    lookup: Arc<dyn Lookup>,
    class: Class,
}

impl FilteredResolver {
    /// A resolver that answers under `lookup` for requests of `class`.
    pub(crate) fn new(lookup: Arc<dyn Lookup>, class: Class) -> FilteredResolver {
        FilteredResolver { lookup, class }
    }

    /// Every address `host` answers with, inside `timeout`.
    ///
    /// The lookup is what a hostile name can make slow, so it runs under the same
    /// deadline the connector was given: when the budget is spent the fetch says so
    /// instead of waiting for `getaddrinfo`. A lookup that never returns leaves its
    /// worker thread behind, which is the price ureq's own resolver pays too — a
    /// thread that is not answering cannot be recalled.
    fn lookup_within(
        &self,
        host: &str,
        port: u16,
        timeout: &NextTimeout,
    ) -> Result<Vec<SocketAddr>, Error> {
        if timeout.after.is_not_happening() {
            return self
                .lookup
                .addresses(host, port)
                .map_err(|_| Error::HostNotFound);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let lookup = Arc::clone(&self.lookup);
        let wanted = host.to_string();
        std::thread::spawn(move || {
            let _ = sender.send(lookup.addresses(&wanted, port));
        });
        match receiver.recv_timeout(*timeout.after) {
            Ok(Ok(addresses)) => Ok(addresses),
            // The name has no answer: the same sentence an unreachable host earns.
            Ok(Err(_)) => Err(Error::HostNotFound),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout(timeout.reason)),
            // The worker thread died without answering: that is a lookup that failed.
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::HostNotFound),
        }
    }
}

impl Resolver for FilteredResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let authority = match uri.authority() {
            Some(authority) => authority,
            // No authority cannot happen for an http(s) URI, and a resolver is not
            // the place to invent one.
            None => return Err(Error::BadUri("the request line has no host".to_string())),
        };
        let host = authority.host();
        let port = authority
            .port_u16()
            .or_else(|| default_port(uri.scheme_str()))
            // `gate`/`hop` only ever approve http(s), so this is the belt, not the
            // braces: an unknown scheme gets no address.
            .ok_or_else(|| Error::BadUri("the request line has no port".to_string()))?;

        let addresses = if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
            // An address literal: nothing to look up, and nothing to be misled about.
            let bare = host.trim_start_matches('[').trim_end_matches(']');
            match bare.parse::<IpAddr>() {
                Ok(ip) => vec![SocketAddr::new(ip, port)],
                // Not reachable for an http(s) request line ureq accepted; a refusal
                // is still better than a guess.
                Err(_) => return Err(Error::HostNotFound),
            }
        } else {
            self.lookup_within(host, port, &timeout)?
        };

        let (kept, refusal) = approved(&addresses, self.class, port);
        if kept.is_empty() {
            // Not `HostNotFound`: the name answered, and the answer is the reason.
            // `PermissionDenied` is the kind that says "a rule refused this", which
            // is how `crate::fetch` turns the failure into a refusal rather than a
            // transport error the caller might retry.
            let reason = nothing_allowed(
                host,
                self.class,
                refusal.as_deref().unwrap_or("no address"),
                addresses.len(),
            );
            return Err(Error::Io(std::io::Error::new(ErrorKind::PermissionDenied, reason)));
        }

        // The family the caller asked for (ureq's own `IpFamily`): filtering here
        // keeps a v6-only or v4-only client from dialling the other family, which is
        // what the default resolver does. ureq's list holds sixteen addresses and
        // `push` panics when it is full, so the seventeenth answer is simply not
        // offered — a limit on how many candidates one host may cost us, not a
        // security decision.
        let mut out = self.empty();
        for address in config.ip_family().keep_wanted(kept.into_iter()) {
            if out.try_push(address).is_err() {
                break;
            }
        }
        if out.is_empty() {
            // The filtering above cannot empty a non-empty list with `IpFamily::Any`
            // (the only value this client sets), but a client configured for one
            // family and given answers from the other has no address to dial.
            return Err(Error::HostNotFound);
        }
        Ok(out)
    }
}

/// The default port of a scheme `gate`/`hop` may approve.
fn default_port(scheme: Option<&str>) -> Option<u16> {
    match scheme {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    }
}

/// The reason a connect-time refusal carries, when the transport reports one.
///
/// The resolver above is the only thing in this client that fails a lookup with
/// `PermissionDenied`, so this test is structural rather than a string match: it asks
/// the error what happened, and a refusal is never retried by the caller.
pub(crate) fn refused_at_connect(error: &Error) -> Option<String> {
    match error {
        Error::Io(io) if io.kind() == ErrorKind::PermissionDenied => Some(io.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A resolver that answers from a script, so a name can answer differently on
    /// the second lookup — the rebinding this module exists for.
    #[derive(Debug)]
    struct Scripted {
        answers: Mutex<Vec<Vec<SocketAddr>>>,
        asked: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(answers: Vec<Vec<SocketAddr>>) -> Arc<Scripted> {
            Arc::new(Scripted {
                answers: Mutex::new(answers),
                asked: Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().expect("the lookup log").clone()
        }
    }

    impl Lookup for Scripted {
        fn addresses(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
            self.asked
                .lock()
                .expect("the lookup log")
                .push(format!("{}:{}", host, port));
            let mut answers = self.answers.lock().expect("the script");
            if answers.is_empty() {
                return Err("no more answers".to_string());
            }
            let next = answers.remove(0);
            Ok(next)
        }
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().expect("a socket address")
    }

    #[test]
    fn one_filter_drops_the_addresses_a_class_may_not_reach() {
        let private = address("127.0.0.1:8080");
        let lan = address("192.168.1.1:80");
        let metadata = address("169.254.169.254:80");
        let public = address("93.184.216.34:80");
        let answers = vec![private, public, lan, metadata];

        // Public: only the public answer survives, and the reason is the guard's.
        let (kept, refusal) = approved(&answers, Class::Public, 80);
        assert_eq!(kept, vec![public]);
        let refusal = refusal.expect("a refusal with a reason");
        assert!(refusal.contains("loopback"), "{}", refusal);

        // Loopback: the opposite — this machine is the only address allowed, so the
        // one loopback answer survives and the other three do not.
        let (kept, refusal) = approved(&answers, Class::Loopback, 80);
        assert_eq!(kept, vec![private]);
        assert!(refusal.is_some());
        let (kept, _) = approved(&[private], Class::Loopback, 8080);
        assert_eq!(kept, vec![private]);
    }

    #[test]
    fn a_literal_is_its_own_answer_and_a_name_is_asked() {
        let answers = Scripted::new(vec![vec![address("93.184.216.34:443")]]);
        let literal = addresses_for("[::1]", 8080, answers.as_ref()).unwrap();
        assert_eq!(literal, vec![address("[::1]:8080")]);
        // No lookup was made for a literal: what is dialled is what the URL says.
        assert!(answers.asked().is_empty(), "{:?}", answers.asked());

        let resolved = addresses_for("news.example", 443, answers.as_ref()).unwrap();
        assert_eq!(resolved, vec![address("93.184.216.34:443")]);
        assert_eq!(answers.asked(), vec!["news.example:443".to_string()]);
    }

    #[test]
    fn the_class_comes_from_the_token_and_not_from_the_caller() {
        let public = Gated::verify("https://example.com/a").unwrap();
        let local = Gated::loopback("http://127.0.0.1:8080/a").unwrap();
        assert_eq!(Class::of(&public), Class::Public);
        assert_eq!(Class::of(&local), Class::Loopback);
        // And the words a refusal uses are the class's own.
        assert_eq!(Class::Public.name(), "public");
        assert_eq!(Class::Loopback.name(), "loopback");
    }

    #[test]
    fn a_connect_time_refusal_is_told_apart_from_a_transport_failure() {
        // The kind is what the fetch turns into a refusal; anything else stays a
        // transport failure the caller may retry.
        let refused = Error::Io(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "host evil.example answered with 1 address this public request may not reach",
        ));
        assert!(refused_at_connect(&refused).is_some());
        let closed = Error::Io(std::io::Error::new(ErrorKind::ConnectionRefused, "no"));
        assert!(refused_at_connect(&closed).is_none());
        assert!(refused_at_connect(&Error::HostNotFound).is_none());
        assert!(refused_at_connect(&Error::Timeout(ureq::Timeout::Resolve)).is_none());
    }

    #[test]
    fn the_default_ports_are_the_two_schemes_the_gate_approves() {
        assert_eq!(default_port(Some("https")), Some(443));
        assert_eq!(default_port(Some("http")), Some(80));
        assert_eq!(default_port(Some("ftp")), None);
        assert_eq!(default_port(None), None);
    }

    #[test]
    fn a_name_with_no_answer_is_a_name_that_does_not_resolve() {
        let empty = Scripted::new(Vec::new());
        let resolver = FilteredResolver::new(Arc::clone(&empty) as Arc<dyn Lookup>, Class::Public);
        let uri: Uri = "http://nowhere.example/x".parse().unwrap();
        let config = Config::default();
        let error = resolver
            .resolve(
                &uri,
                &config,
                NextTimeout {
                    after: ureq::unversioned::transport::time::Duration::from_millis(500),
                    reason: ureq::Timeout::Resolve,
                    ..
                    NextTimeout::default()
                },
            )
            .expect_err("a name with no answer");
        assert!(matches!(error, Error::HostNotFound), "{:?}", error);
    }
}
