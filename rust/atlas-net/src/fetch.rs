//! The request itself: the client, its clock, the transport, and one decode call.
//!
//! What is decided here is the order of one fetch — adopt the URL the gate
//! approved, check that its name still answers with an address the class may reach,
//! request it (with the address filter inside the transport, see [`crate::resolver`]),
//! gate every redirect hop *before* following it, then read at most N bytes of a 2xx
//! document and decode them. Also decided: who a request says it is
//! ([`crate::useragent`]), how long the whole fetch may take ([`crate::budget`]), and
//! which headers a hop is allowed to carry.
//!
//! The sizes — the body cap, the header cap, what counts as a document type — are
//! [`crate::limits`], so a caller who needs the ceiling finds it without reading the
//! transport.
//!
//! It does not: derive a verdict (that is [`crate::gate`]), resolve a hop (that is
//! [`crate::hop`]), turn a name into an address (that is [`crate::resolver`]), decode
//! a charset (that is [`crate::charset`]), word a failure (that is
//! [`crate::reason`]), write a file, keep cookies, choose a proxy or report a page's
//! text in an error message.

use std::sync::Arc;
use std::time::Duration;

use atlas_guard::Gated;
use ureq::unversioned::transport::DefaultConnector;

use crate::budget::Budget;
use crate::charset::{decode, detect_charset};
use crate::gate::{gate, loopback_request, resolve_gate_with, Verdict};
use crate::hop::{check_hop_from, same_origin, MAX_REDIRECTS, REDIRECTS};
use crate::limits::{
    clamp_bytes, is_document_type, read_capped_error, HARD_MAX_BYTES, MAX_HEADER_BYTES,
};
use crate::page::Page;
use crate::reason::{body_failure, describe};
use crate::resolver::{refused_at_connect, Class, FilteredResolver, Lookup, SystemLookup};
use crate::useragent::user_agent;

/// One whole fetch, redirects included — the budget [`Budget`] enforces across
/// hops, so it is the number a caller can believe when it sizes its own deadline.
pub const TIMEOUT_TOTAL: Duration = Duration::from_secs(20);

/// Time to a connected socket (TCP and TLS handshake), per hop.
pub const TIMEOUT_CONNECT: Duration = Duration::from_secs(15);

/// One `Client` per process: the connection pool, the user agent and the caps are
/// built once, so a second request to the same host reuses the TLS session.
///
/// Two agents, not one, and the reason is the address filter: the resolver is built
/// into the transport, so the *class* of request has to be a property of the
/// connection. A public request and the app's own loopback traffic therefore cannot
/// share an agent — one of them would inherit the other's allowed addresses. Which
/// agent a request uses is decided per hop from the token it carries
/// ([`Class::of`]), never by the caller.
pub struct Client {
    /// The public-internet agent: no loopback, private, link-local or reserved
    /// address can be reached through it, whatever a name answers.
    public: ureq::Agent,
    /// The app's own traffic (the local model server): only this machine.
    local: ureq::Agent,
    /// Where a name's answers come from. The system resolver in production, a script
    /// in a test, so the filter can be shown to be load-bearing.
    lookup: Arc<dyn Lookup>,
    user_agent: String,
    hard_cap: usize,
    total: Duration,
    connect: Duration,
}

impl Default for Client {
    fn default() -> Client {
        Client::new()
    }
}

impl Client {
    /// The configured client: [`TIMEOUT_TOTAL`] for the whole fetch,
    /// [`TIMEOUT_CONNECT`] to a connected socket, `identity` bodies, a desktop
    /// Chrome User-Agent, no proxy, no redirect followed by the transport (every
    /// hop is gated here instead), a header cap, a body cap, and the address filter
    /// inside the resolver.
    pub fn new() -> Client {
        Client::with_cap(HARD_MAX_BYTES)
    }

    /// Same client with a different absolute body ceiling (never above
    /// [`HARD_MAX_BYTES`]; anything larger is clamped down to it).
    pub fn with_cap(hard_cap: usize) -> Client {
        Client::with_limits(hard_cap, TIMEOUT_TOTAL, TIMEOUT_CONNECT)
    }

    /// Same client with its own clock, for a caller that cannot wait this long (a
    /// tool call with a shorter deadline) and for the tests, which cannot wait
    /// twenty seconds to see a timeout happen.
    ///
    /// `total` is the budget for the whole fetch, redirects included; `connect` is
    /// the budget to a connected socket, per hop. Both are clamped to at least one
    /// millisecond, because a zero budget is a client that cannot work at all.
    pub fn with_limits(hard_cap: usize, total: Duration, connect: Duration) -> Client {
        Client::with_lookup(Arc::new(SystemLookup), hard_cap, total, connect)
    }

    /// The whole construction, with the names' answers injected.
    ///
    /// The answers are a parameter for one reason: the attack this crate defends
    /// against — a name that answers public for the verdict and `127.0.0.1` for the
    /// connection — cannot be staged against the real resolver, and a filter that is
    /// only argued for is not a filter. Production always passes [`SystemLookup`];
    /// the tests reach this through [`Client::with_limits`]'s neighbour below.
    pub(crate) fn with_lookup(
        lookup: Arc<dyn Lookup>,
        hard_cap: usize,
        total: Duration,
        connect: Duration,
    ) -> Client {
        let user_agent = user_agent();
        let total = total.max(Duration::from_millis(1));
        let connect = connect.max(Duration::from_millis(1));
        let public = agent_for(Class::Public, &lookup, &user_agent, total, connect);
        let local = agent_for(Class::Loopback, &lookup, &user_agent, total, connect);
        Client {
            public,
            local,
            lookup,
            user_agent,
            hard_cap: hard_cap.clamp(1, HARD_MAX_BYTES),
            total,
            connect,
        }
    }

    /// The agent one request of `class` is made with.
    fn agent(&self, class: Class) -> &ureq::Agent {
        match class {
            Class::Public => &self.public,
            Class::Loopback => &self.local,
        }
    }

    /// The User-Agent every request carries.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// The absolute body ceiling this client enforces.
    pub fn hard_cap(&self) -> usize {
        self.hard_cap
    }

    /// The budget one whole fetch gets, redirects included.
    pub fn total_timeout(&self) -> Duration {
        self.total
    }

    /// The budget each hop gets to a connected socket (TCP and TLS handshake).
    pub fn connect_timeout(&self) -> Duration {
        self.connect
    }

    /// True when a server certificate is verified against the Mozilla root store
    /// with SNI and the host name checked — the state this client ships in, and the
    /// one line that would notice if it ever stopped being true. Asked of both
    /// agents: a policy that holds for one class of request and not the other is not
    /// a policy.
    pub fn verifies_tls(&self) -> bool {
        [&self.public, &self.local].iter().all(|agent| {
            let tls = agent.config().tls_config();
            let roots = match tls.root_certs() {
                ureq::tls::RootCerts::Specific(certs) => !certs.is_empty(),
                _ => true,
            };
            !tls.disable_verification() && tls.use_sni() && roots
        })
    }

    /// True when the client would send the request through a proxy. Always false:
    /// see [`agent_for`].
    pub fn uses_proxy(&self) -> bool {
        [&self.public, &self.local]
            .iter()
            .any(|agent| agent.config().proxy().is_some())
    }

    /// [`gate`], pure: no socket, no DNS.
    pub fn gate(&self, url: &str) -> Verdict {
        gate(url)
    }

    /// [`gate`] plus the DNS-answer check — the verdict taken right before egress.
    pub fn gate_resolved(&self, url: &str) -> Verdict {
        resolve_gate_with(&gate(url), self.lookup.as_ref())
    }

    /// Read one page (or feed) behind the gate.
    ///
    /// The order is the whole point: the gate decides, then [`Client::fetch_gated`]
    /// takes the verdict it was handed. A refused URL returns a [`Page`] whose
    /// `refused` field carries the reason, and no request was made for it.
    pub fn fetch(&self, url: &str, max_bytes: usize) -> Page {
        let verdict = self.gate(url);
        if !verdict.ok {
            return Page::refused(verdict.reason);
        }
        match verdict.gated() {
            Some(gated) => self.fetch_gated(gated, max_bytes),
            // Unreachable by construction (`ok` implies a token), and a refusal
            // rather than a guess if it ever stops being true.
            None => Page::refused("the gate approved a URL without a verdict".to_string()),
        }
    }

    /// The same fetch, from an already-approved token.
    ///
    /// The token is *adopted*, not re-decided: a public verdict is re-gated and its
    /// name re-resolved (a name can answer with a private address now, whatever it
    /// answered when the token was minted), and a loopback verdict — the guard's own
    /// door for this app's local traffic — is re-minted from its canonical form
    /// under the port policy instead of being put through the public gate, which
    /// would refuse it as a private address.
    pub fn fetch_gated(&self, gated: &Gated, max_bytes: usize) -> Page {
        let cap = clamp_bytes(max_bytes).min(self.hard_cap);
        match self.adopt(gated) {
            Ok(current) => self.exchange(current, cap),
            Err(reason) => Page::refused(reason),
        }
    }

    /// The verdict taken at egress, for a token this client is about to request.
    ///
    /// This is the one place that decides what may leave the machine, and it is the
    /// only source of the [`Gated`] values [`Client::exchange`] accepts: the first
    /// URL and every hop pass through here.
    ///
    /// It is not the last word on *addresses*: the same filter runs again inside the
    /// transport ([`crate::resolver`]) on the answer that will actually be dialled.
    /// The verdict is early, the resolver is binding.
    fn adopt(&self, token: &Gated) -> Result<Gated, String> {
        if token.is_loopback() {
            // This machine, proven by the guard: there is no public name to resolve
            // and no private address to catch — the address *is* the machine.
            let (_, hop) = loopback_request(token)?;
            return Ok(hop);
        }
        // The public verdict, taken now rather than when the token was minted: a
        // name can answer with a private address today whatever it answered then.
        let verdict = self.gate_resolved(token.as_str());
        if !verdict.ok {
            return Err(verdict.reason);
        }
        match verdict.gated() {
            Some(approved) => Ok(approved.clone()),
            None => Err("the gate approved a URL without a verdict".to_string()),
        }
    }

    /// One fetch: the request, its hops, and the body — the only place a request
    /// line is written, and the signature is why: `first` is a [`Gated`], so the
    /// string that reaches `agent.get` was approved by `atlas-guard` first.
    fn exchange(&self, first: Gated, cap: usize) -> Page {
        // One clock for the whole fetch. Every hop below is given what is left of
        // it, so five redirects cannot buy five fresh timeouts.
        let budget = Budget::new(self.total);
        let mut current = first;
        let mut previous = current.as_str().to_string();
        let mut hops = 0usize;

        loop {
            let left = budget.remaining();
            if budget.is_spent() {
                return Page::failed(out_of_time(&budget), hops);
            }
            // Which agent may make this request is the token's class, not the
            // caller's choice: a public URL is requested by the public agent, whose
            // resolver cannot reach this machine.
            let agent = self.agent(Class::of(&current));
            let mut request = agent
                .get(current.as_str())
                .config()
                .timeout_global(Some(left))
                .build()
                .header("Accept-Language", "en;q=0.9,*;q=0.5");
            // The referrer a browser would send: none on a direct request, the page
            // we came from on a same-site hop, and nothing at all when a hop leaves
            // the site — the query string of the link we were asked to read is not
            // a stranger's business.
            if hops > 0 && same_origin(&previous, current.as_str()) {
                request = request.header("Referer", previous.as_str());
            }
            let response = match request.call() {
                Ok(response) => response,
                Err(error) => {
                    // An address the filter refused at connect time is a security
                    // decision, not a site that was slow: it is reported as a
                    // refusal so nothing retries it or reads it as transient.
                    if let Some(reason) = refused_at_connect(&error) {
                        return Page::refused(if hops == 0 {
                            reason
                        } else {
                            hop_refused(hops, &reason)
                        });
                    }
                    let reason = describe(&error);
                    let reason = if matches!(error, ureq::Error::Timeout(_)) {
                        format!("{} ({})", reason, whole_fetch(budget.total()))
                    } else {
                        reason
                    };
                    return Page::failed(reason, hops);
                }
            };
            let status = response.status().as_u16();

            if REDIRECTS.contains(&status) {
                let location = header_of(&response, "location");
                if location.is_empty() {
                    // A broken server, not a security decision.
                    return Page::failed(
                        format!("the site answered {} without a location", status),
                        hops,
                    );
                }
                if hops >= MAX_REDIRECTS {
                    return Page::refused(format!(
                        "more than {} redirects: the last one was the {}th",
                        MAX_REDIRECTS,
                        hops + 1
                    ));
                }
                let next = match check_hop_from(&current, &location) {
                    Ok(token) => token,
                    Err(reason) => return Page::refused(hop_refused(hops + 1, &reason)),
                };
                // The hop is about to leave the machine: its own verdict is taken
                // here, on the URL that will be requested.
                let approved = match self.adopt(&next) {
                    Ok(token) => token,
                    Err(reason) => return Page::refused(hop_refused(hops + 1, &reason)),
                };
                hops += 1;
                previous = current.as_str().to_string();
                current = approved;
                continue;
            }

            // The URL the request landed on. No second verdict: `current` is a
            // `Gated` minted by `adopt` for exactly this string, and the transport
            // follows no redirect of its own (`max_redirects(0)`), so this *is* where
            // the request went.
            let content_type = header_of(&response, "content-type");
            let mut page = Page {
                status,
                final_url: current.as_str().to_string(),
                content_type,
                redirects: hops,
                ..Page::default()
            };

            // A 2xx document is the only thing worth reading: an error page's body
            // is not an article, and a redirect has already been handled above.
            if !(200..300).contains(&status) {
                return page;
            }
            if !is_document_type(&page.content_type) {
                return page;
            }
            // No compressed bodies, by policy, and the transport cannot undo that
            // policy: `Accept-Encoding: identity` was sent, and the workspace builds
            // `ureq` without its `gzip`/`brotli` features (`rust/Cargo.toml`), so
            // nothing between here and the socket decodes an answer and hides the
            // fact by stripping `Content-Encoding` (RFC 9110 §8.7). That matters
            // beyond tidiness: with the transport decompressing, the byte cap below
            // would bound the *decoded* bytes while the decompressor allocated on
            // its own terms first. An answer that names an encoding anyway is a
            // server ignoring the request, and its bytes are not the text the
            // content type promised. Refused by name, so a log line is actionable.
            let encoding = header_of(&response, "content-encoding");
            if !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity") {
                page.error = Some(format!("compressed response refused ({})", encoding));
                return page;
            }

            let body = response.into_body();
            // Measured cost of the cap: a page that is cut off leaves the rest of the
            // response unread, so ureq cannot pool that connection and the next
            // request to the same host pays a fresh TLS handshake. On one real pair
            // of requests (see `live_news_page_cold_and_warm`): a 600 kB *truncated*
            // page took 66 ms cold and 95 ms warm (no reuse), while a 23 kB feed that
            // fit under the cap took 77 ms cold and 28 ms warm (reused). Reading the
            // remainder would buy the reuse back and cost the bandwidth Python also
            // refuses to spend, so the cap wins.
            //
            // The cap bounds the bytes *after* decoding: a body the transport
            // decompressed is cut here too, which is what keeps a decompression bomb
            // to `cap` bytes of memory (`a_gzip_bomb_is_bounded_by_the_cap`).
            let (bytes, truncated) = match read_capped_error(body.into_reader(), cap) {
                Ok(pair) => pair,
                Err(error) => {
                    page.error = Some(body_failure(&error));
                    return page;
                }
            };
            // The charset of the bytes *this crate* holds. Nothing between the socket
            // and this line decodes them: the workspace builds ureq with
            // `default-features = false` (see `rust/Cargo.toml`), so neither its
            // `charset` feature (which would convert a `text/*` body to UTF-8 behind
            // our back, and then this crate would decode UTF-8 as cp1252 — measured:
            // `caf’` arrived as `cafâ€™`) nor its `gzip` feature (which would strip
            // `Content-Encoding` before the refusal below could see it) is compiled
            // in. `the_transport_hands_over_the_bytes_the_page_sent` fails if that
            // ever stops being true.
            page.charset = detect_charset(&page.content_type, &bytes);
            page.bytes = bytes.len();
            page.truncated = truncated;
            page.text = decode(&bytes, &page.charset);
            return page;
        }
    }
}

/// One agent, built for one class of request.
///
/// Everything except the resolver is the same for both classes, and every line here
/// is a policy that would be invisible until a page arrived from the wrong place:
///
/// * `max_redirects(0)` — ureq never follows a redirect for us, so the only hops
///   that happen are the ones `check_hop_from` allowed;
/// * `http_status_as_error(false)` — a 4xx/5xx comes back as a response we can
///   report, not as an error;
/// * `accept_encoding("identity")` — a compressed body is never asked for;
/// * `proxy(None)` — no proxy, whatever `HTTP_PROXY`/`ALL_PROXY` say. ureq reads
///   those variables by default (`Config::default()` is `Proxy::try_from_env()`),
///   and a proxy would answer for a page this app believes it gated: the address
///   check, the body cap and the certificate check all describe a connection a proxy
///   would replace. An environment variable is not a security decision this app
///   makes (`an_environment_proxy_cannot_take_over_a_gated_request` proves it on the
///   wire, not on the config);
/// * a header cap and the resolver: the transport asks [`FilteredResolver`] for the
///   address, so the address connected to is one the guard approved.
fn agent_for(
    class: Class,
    lookup: &Arc<dyn Lookup>,
    ua: &str,
    total: Duration,
    connect: Duration,
) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(total))
        .timeout_connect(Some(connect))
        .max_redirects(0)
        .http_status_as_error(false)
        .accept_encoding("identity")
        .proxy(None)
        .user_agent(ua)
        .accept("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .max_response_header_size(MAX_HEADER_BYTES)
        .build();
    // `with_parts`, not `into()`: the resolver is the whole point of this module.
    // `DefaultConnector` is the chain the default agent uses (proxy handling, TCP,
    // and the rustls connector that checks the certificate against
    // `uri.authority().host_bare()`), so nothing but the address source changes.
    ureq::Agent::with_parts(
        config,
        DefaultConnector::default(),
        FilteredResolver::new(Arc::clone(lookup), class),
    )
}

/// The `Location`/`Content-Type`/`Content-Encoding` of a response as text, lossily.
fn header_of(response: &ureq::http::Response<ureq::Body>, name: &str) -> String {
    match response.headers().get(name) {
        Some(value) => String::from_utf8_lossy(value.as_bytes()).trim().to_string(),
        None => String::new(),
    }
}

/// Why a hop was not followed, in the words a log line and the model can read.
/// The hop number is in every message: "the link redirected somewhere it may not
/// go" reads very differently from "the link you passed is refused".
fn hop_refused(hop: usize, reason: &str) -> String {
    format!(
        "redirect refused at hop {} of {}: {}",
        hop, MAX_REDIRECTS, reason
    )
}

/// The sentence a fetch that ran out of time gets, saying which budget ran out —
/// a caller reading it must not think the page was slow to answer when the whole
/// fetch was out of time before the hop.
fn out_of_time(budget: &Budget) -> String {
    format!(
        "out of time after {} ms: {}",
        budget.spent().as_millis(),
        whole_fetch(budget.total())
    )
}

/// How the whole-fetch budget reads in a sentence.
fn whole_fetch(total: Duration) -> String {
    format!(
        "one fetch may take {} ms in all, redirects included",
        total.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_is_built_once_with_the_documented_limits() {
        let client = Client::new();
        // The transport refuses to follow a redirect by itself; only `check_hop_from`
        // hops, because every hop has to pass the gate first.
        assert_eq!(client.hard_cap(), HARD_MAX_BYTES);
        assert_eq!(client.total_timeout(), TIMEOUT_TOTAL);
        assert_eq!(client.connect_timeout(), TIMEOUT_CONNECT);
    }

    #[test]
    fn the_shipped_client_verifies_certificates_and_uses_no_proxy() {
        // Two policies that would each be invisible until a page arrived from the
        // wrong place: a client that accepts any certificate, and a client that lets
        // `HTTP_PROXY` decide who answers.
        //
        // Both are asked of both agents: the public one and the app's own loopback
        // one are built by the same `agent_for`, and a policy that holds for one
        // class of request and not the other is not a policy.
        for client in [Client::new(), Client::with_cap(1000)] {
            assert!(client.verifies_tls(), "certificate verification is off");
            assert!(!client.uses_proxy(), "the environment set a proxy");
        }
        // A certificate is checked against the name *in the URL*: ureq's rustls
        // connector builds the `ServerName` from `uri.authority().host_bare()` (see
        // `tls/rustls.rs` in ureq 3.5.2), so a certificate issued for another name
        // cannot stand behind the request. `use_sni` is what sends that name, and
        // `disable_verification` is the one switch that would turn the check off.
        for agent in [&Client::new().public, &Client::new().local] {
            let tls = agent.config().tls_config();
            assert!(tls.use_sni(), "SNI is off, so the name is not sent");
            assert!(!tls.disable_verification(), "verification is off");
            // The Mozilla store (`RootCerts::WebPki`, ureq's default) or an explicit
            // list: either way, something to verify against. The platform verifier is
            // not compiled in (the workspace does not enable `platform-verifier`), so
            // a client that asked for it would verify against nothing.
            match tls.root_certs() {
                ureq::tls::RootCerts::Specific(certs) => {
                    assert!(!certs.is_empty(), "an empty trust store")
                }
                ureq::tls::RootCerts::WebPki => {}
                // `RootCerts` is `#[non_exhaustive]`-shaped in practice: a variant this
                // workspace does not enable is a client that verifies against nothing.
                other => panic!(
                    "the trust store is not one this workspace enables: {:?}",
                    matches!(other, ureq::tls::RootCerts::PlatformVerifier)
                ),
            }
        }
    }

    #[test]
    fn a_shorter_clock_reaches_both_budgets_and_never_becomes_zero() {
        let client = Client::with_limits(1000, Duration::from_millis(300), Duration::from_millis(100));
        assert_eq!(client.total_timeout(), Duration::from_millis(300));
        assert_eq!(client.connect_timeout(), Duration::from_millis(100));
        assert_eq!(client.hard_cap(), 1000);
        // A zero clock is a client that cannot work; it is clamped, not obeyed.
        let zero = Client::with_limits(1000, Duration::ZERO, Duration::ZERO);
        assert!(zero.total_timeout() >= Duration::from_millis(1));
        assert!(zero.connect_timeout() >= Duration::from_millis(1));
    }

    #[test]
    fn a_refused_hop_says_which_hop_and_a_deadline_says_which_budget() {
        // The two messages a log line and a tool result are built from: neither may
        // read as "the link you passed is refused" when the link itself was fine.
        let refused = hop_refused(2, "private, loopback or link-local address is not allowed");
        assert!(refused.starts_with("redirect refused at hop 2 of 5"), "{}", refused);
        assert!(refused.contains("loopback"), "{}", refused);
        let budget = Budget::new(Duration::from_millis(250));
        let spent = out_of_time(&budget);
        assert!(spent.starts_with("out of time after"), "{}", spent);
        assert!(spent.contains("250 ms in all, redirects included"), "{}", spent);
    }
}
