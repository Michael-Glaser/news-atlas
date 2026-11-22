//! What a failed request says to the person — or the model — reading it.
//!
//! [`describe`] turns one transport error into a sentence: which phase ran out of
//! time, that the name does not resolve, that the certificate was not trusted, that
//! the site answered an error status. The caller puts the sentence in
//! [`crate::Page::error`], which the `atlas-net` binary prints and the collector
//! logs, so a log line and a tool result say the same thing and neither has to
//! guess from a generic "request failed".
//!
//! Three rules are decided here. First, nothing in the sentence is page content: no
//! body, no header value beyond a status number or a byte count. Second, a failure
//! never reads like a security decision — a refusal is [`crate::Page::refused`]'s
//! job, and the words here ("timed out", "not trusted", "refused the connection")
//! belong to the transport. Third, **no sentence carries a URL**: a failure names the
//! host and the class of the problem, never the request line. The URL a search tool
//! fetches can hold the query the model wrote and the engine's configured key, and a
//! reason goes into [`crate::Page::error`], the tool result and the log — so a URL in
//! it is a leak with a long tail. Two `ureq` payloads are URLs
//! (`RequireHttpsOnly(String)` is the request URI, `run.rs:156`; `BadUri(String)` can
//! embed it, `util.rs:330–337`), which is why those two arms say the host and the
//! class in this crate's own words, and why every sentence leaves through
//! [`finish`] — a belt for the variants this file has not seen yet.
//!
//! It does not: classify a URL, retry, or decide whether the caller falls back to
//! the rendered browser. It formats one error.

use std::io::ErrorKind;

use ureq::Timeout;

/// One transport error as an English sentence.
///
/// `ureq::Error` is `#[non_exhaustive]`, so the match ends in a catch-all on
/// purpose: a variant added by a future `ureq` must still produce a sentence
/// rather than fail to compile — but it is worth reading this list when the
/// dependency is bumped, because a new variant is a failure this file has not
/// classified yet.
pub(crate) fn describe(error: &ureq::Error) -> String {
    finish(match error {
        ureq::Error::Timeout(timeout) => format!("timed out {}", phase(*timeout)),
        ureq::Error::Io(io) => describe_io(io),
        ureq::Error::HostNotFound => "the name does not resolve: no DNS answer".to_string(),
        ureq::Error::ConnectionFailed => "the connection failed before any answer".to_string(),
        ureq::Error::Protocol(_) => "the answer was not valid HTTP".to_string(),
        // `ureq` builds this payload as `format!("{} is missing scheme|host", uri)`
        // (`util.rs:330–337`): the text is the request URI, query and all. The reason
        // is kept, the URL is not — a sentence that says which part was missing is
        // all a reader can act on, and the URI is the one thing that must not travel.
        ureq::Error::BadUri(_why) => {
            "the url cannot be a request line: it needs a scheme and a host".to_string()
        }
        ureq::Error::StatusCode(status) => format!("the site answered HTTP {}", status),
        ureq::Error::BodyExceedsLimit(limit) => {
            format!("the body is larger than the {} byte cap", limit)
        }
        ureq::Error::LargeResponseHeader(_size, limit) => {
            format!("the response headers are larger than {} bytes", limit)
        }
        ureq::Error::RedirectFailed => "the redirect could not be followed".to_string(),
        ureq::Error::TooManyRedirects => "too many redirects for the transport".to_string(),
        ureq::Error::Tls(why) => format!("TLS failed ({})", why),
        ureq::Error::Rustls(why) => format!("TLS failed ({})", why),
        ureq::Error::Pem(why) => format!("the certificate file could not be read ({})", why),
        // `Decompress` is `cfg`-gated on ureq's `gzip`/`brotli` features, so it is
        // deliberately left to the catch-all: this file must compile whether or not
        // the workspace keeps those features on, and "a compressed body could not be
        // read" arrives through the generic arm with the reason attached.
        // The payload is the request URI (`run.rs:156`), which for a search GET is
        // the substituted query template: host, path, `?q=…`, and the engine's key.
        // Unreachable today (this crate's agents are not built `https_only`), and one
        // setting away from being reachable — so the sentence is written as if it
        // were already.
        ureq::Error::RequireHttpsOnly(url) => only_https(url),
        ureq::Error::ConnectProxyFailed(why) => format!("the proxy failed ({})", why),
        ureq::Error::Other(why) => format!("request failed ({})", why),
        // The arms above are the failures this crate has words for, in the order a
        // request happens in. `ureq`'s `Display` is structural (a variant name and
        // its reason, never a page), so an unclassified variant is still readable
        // here without a second list of variants going stale.
        other => format!("request failed ({})", other),
    })
}

/// The socket and TLS layer, told apart by the `io::ErrorKind` rather than by the
/// text: `rustls` reports a handshake or certificate error as
/// `ErrorKind::InvalidData` (see `rustls::conn`), and by the time an error reaches
/// this function it is past the connection phase, so `InvalidData` is the TLS
/// handshake failing — not a page that could not be parsed.
///
/// Kept next to [`describe`] because it is the same decision: which words a
/// failure gets.
pub(crate) fn describe_io(error: &std::io::Error) -> String {
    finish(match error.kind() {
        ErrorKind::ConnectionRefused => {
            "the site refused the connection: nothing is listening there".to_string()
        }
        ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof => {
            "the connection was closed before the answer was complete".to_string()
        }
        ErrorKind::NotFound => "the site is not there".to_string(),
        ErrorKind::PermissionDenied => {
            // The one error kind this crate produces itself: the address filter in
            // `crate::resolver` refused the name's answer at connect time. The
            // sentence carries the address and the class, so it reads as a decision
            // and not as a network that was unkind.
            format!("refused before the connection was made: {}", error)
        }
        ErrorKind::InvalidData => format!("TLS failed: the certificate or handshake was refused ({})", error),
        ErrorKind::TimedOut => "the socket timed out".to_string(),
        _ => format!("connection failed ({})", error),
    })
}

/// The sentence a body read that failed earns.
///
/// The read happens after the status line and the headers, so a failure here is
/// either the site dribbling a body until the fetch ran out of time (a slow-loris
/// read) or the connection going away mid-page. Both used to read
/// "cannot read the body (timeout: global)", which names neither. The transport wraps
/// its own errors in an `io::Error`, and the wrapper is where the phase is; when it
/// is one of ours ([`std::io::Error::get_ref`]) the sentence is [`describe`]'s, with
/// the same budget sentence a whole-fetch timeout gets.
pub(crate) fn body_failure(error: &std::io::Error) -> String {
    finish(body_failure_inner(error))
}

/// [`body_failure`] before the belt: the phase, then the sentence for the kind.
fn body_failure_inner(error: &std::io::Error) -> String {
    if let Some(inner) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
    {
        return match inner {
            ureq::Error::Timeout(_) => "timed out while reading the page".to_string(),
            other => describe(other),
        };
    }
    describe_io(error)
}

/// The `RequireHttpsOnly` sentence: the host, and the class of the problem.
///
/// The payload is the full request URI. What a reader needs is which site was asked
/// for and that the request was not https; what must never travel is the path and the
/// query, because that is where a search query — and a configured engine key — lives.
/// A URI that will not parse still gets a sentence: the class, and no address.
fn only_https(url: &str) -> String {
    match host_of(url) {
        Some(host) => {
            format!("only https may be requested — the request for {host} was not made over https")
        }
        None => "only https may be requested — this request was not made over https".to_string(),
    }
}

/// The authority of one URL, `host` or `host:port`, and nothing after it.
fn host_of(text: &str) -> Option<String> {
    let parsed = url::Url::parse(text.trim()).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// The last thing every sentence in this file passes through: a token that names a
/// URL keeps its origin and loses its path, query and fragment.
///
/// The two arms above are the shapes `ureq` 3.5 can produce today. This is the belt
/// for the one it produces tomorrow: an error variant whose payload is a URL, or a
/// transport string that happens to quote one, reaches [`crate::Page::error`] and then
/// the tool result — so the scrub is applied to the whole sentence rather than to the
/// arms a reader remembered. It is deliberately dumb: no URL is rewritten into
/// something a reader could mistake for the whole thing, and prose is left alone
/// because a "url-shaped token" has to look like a host to be cut.
fn finish(sentence: String) -> String {
    let mut out = String::with_capacity(sentence.len());
    let mut run = String::new();
    for ch in sentence.chars() {
        if ch.is_whitespace() || matches!(ch, ')' | ']' | '}' | '"' | '\'' | ',' | ';') {
            out.push_str(&origin_only(&run));
            run.clear();
            out.push(ch);
        } else {
            run.push(ch);
        }
    }
    out.push_str(&origin_only(&run));
    out
}

/// One whitespace-delimited token, cut to its origin.
fn origin_only(token: &str) -> String {
    if let Some(scheme_end) = token.find("://") {
        let after = &token[scheme_end + 3..];
        let cut = after
            .find(['/', '?', '#'])
            .unwrap_or(after.len());
        return format!("{}://{}", &token[..scheme_end], &after[..cut]);
    }
    // No scheme: a bare `host/path?query` or `host?query`. Cut only when what
    // precedes the cut really looks like a host, so "and/or" and "GET /rss" (the
    // request line, in a sentence about one) survive intact.
    for cut in [token.find('/'), token.find('?')].into_iter().flatten() {
        if cut > 0 && looks_like_host(&token[..cut]) {
            return token[..cut].to_string();
        }
    }
    token.to_string()
}

/// Would a reader take this for a host? Something with a dot, a port, or brackets —
/// an address or a name, not an English word.
fn looks_like_host(text: &str) -> bool {
    !text.is_empty()
        && (text.contains('.') || text.contains(':') || text.starts_with('['))
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '[' | ']' | '_'))
}

/// Which part of the request ran out of time, in the words a person uses.
fn phase(timeout: Timeout) -> &'static str {
    match timeout {
        Timeout::Resolve => "while looking up the name",
        Timeout::Connect => "while connecting (TCP and TLS)",
        Timeout::SendRequest | Timeout::SendBody => "while sending the request",
        Timeout::Await100 => "while waiting for the site",
        Timeout::RecvResponse => "while waiting for the answer",
        Timeout::RecvBody => "while reading the page",
        Timeout::PerRead | Timeout::PerWrite => "during the transfer",
        Timeout::PerCall | Timeout::Global => "before the page was read",
        // `Timeout` is `#[non_exhaustive]`: an unknown phase is still a timeout.
        _ => "before the page was read",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn a_body_read_failure_says_which_phase_gave_up() {
        // The transport wraps its own failures in an `io::Error`, and the wrapper is
        // where the phase is: a timeout during the body is the whole-fetch clock
        // expiring on a site that dribbles, and it must read as a timeout rather than
        // as "connection failed".
        let timeout = io::Error::other(ureq::Error::Timeout(Timeout::RecvBody));
        assert_eq!(
            body_failure(&timeout),
            "timed out while reading the page"
        );
        // A connection that goes away mid-body is the other shape of the same event.
        let closed = io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed");
        assert_eq!(
            body_failure(&closed),
            "the connection was closed before the answer was complete"
        );
        // And a plain socket error is worded by the socket's own reason.
        let reset = io::Error::new(io::ErrorKind::ConnectionReset, "reset");
        assert_eq!(
            body_failure(&reset),
            "the connection was closed before the answer was complete"
        );
        // Neither sentence carries page content: they are reasons, not text.
        for error in [&timeout, &closed, &reset] {
            let sentence = body_failure(error);
            assert!(!sentence.contains('{'), "{}", sentence);
            assert!(!sentence.contains("Some("), "{}", sentence);
        }
    }

    #[test]
    fn a_timeout_says_which_phase_gave_up() {
        assert_eq!(
            describe(&ureq::Error::Timeout(Timeout::Connect)),
            "timed out while connecting (TCP and TLS)"
        );
        assert_eq!(
            describe(&ureq::Error::Timeout(Timeout::RecvResponse)),
            "timed out while waiting for the answer"
        );
        assert_eq!(
            describe(&ureq::Error::Timeout(Timeout::RecvBody)),
            "timed out while reading the page"
        );
        // Two phases that must never read as the same sentence: "we could not
        // reach it" and "it never answered" send the model to different places.
        assert_ne!(
            describe(&ureq::Error::Timeout(Timeout::Connect)),
            describe(&ureq::Error::Timeout(Timeout::RecvBody))
        );
    }

    #[test]
    fn a_certificate_failure_says_tls_not_connection_failed() {
        // `rustls` reports a handshake or certificate error as an `io::Error`
        // with `InvalidData`; it used to read as the generic "request failed".
        let refused = io::Error::new(io::ErrorKind::InvalidData, "invalid peer certificate: UnknownIssuer");
        let sentence = describe(&ureq::Error::Io(refused));
        assert!(sentence.starts_with("TLS failed"), "{}", sentence);
        assert!(sentence.contains("certificate"), "{}", sentence);
        // The same kind the other way round: a genuine socket refusal is not TLS.
        let closed = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
        assert_eq!(
            describe(&ureq::Error::Io(closed)),
            "the site refused the connection: nothing is listening there"
        );
    }

    /// The leak this rule exists for: the search GET carries the model's query in its
    /// path, and the configured engine may carry a key there too. `ureq`'s
    /// `RequireHttpsOnly` payload is the request URI (`run.rs:156`), so a sentence that
    /// interpolated it would put both into `Page::error`, the tool result and the log.
    #[test]
    fn an_https_only_failure_names_the_host_and_never_the_url() {
        let url = "http://search.example/rss?q=chip+shortage+unverified&key=SECRET-ENGINE-KEY";
        let sentence = describe(&ureq::Error::RequireHttpsOnly(url.to_string()));

        assert!(sentence.contains("search.example"), "{sentence}");
        assert!(sentence.contains("https"), "{sentence}");
        for leak in ["SECRET", "key=", "q=", "chip", "/rss", "?"] {
            assert!(
                !sentence.contains(leak),
                "the sentence carries {leak:?} from the url: {sentence}"
            );
        }
        // A host with a port keeps the port and nothing after it.
        let with_port = describe(&ureq::Error::RequireHttpsOnly(
            "http://search.example:8080/rss?q=SECRET".to_string(),
        ));
        assert!(with_port.contains("search.example:8080"), "{with_port}");
        assert!(!with_port.contains("SECRET"), "{with_port}");
        // And a payload that is not a URL at all still gets a sentence.
        let opaque = describe(&ureq::Error::RequireHttpsOnly("not a url".to_string()));
        assert!(opaque.contains("https"), "{opaque}");
        assert!(!opaque.contains("not a url"), "{opaque}");
    }

    /// `BadUri`'s payload is `format!("{} is missing scheme|host", uri)`
    /// (`util.rs:330–337`), so it embeds the request URI. The sentence says which part
    /// of a request line was wrong and never repeats the line.
    #[test]
    fn a_bad_uri_failure_does_not_quote_the_uri() {
        for payload in [
            "search.example/rss?q=SECRET-ENGINE-KEY is missing scheme",
            "http:/rss?q=SECRET-ENGINE-KEY is missing host",
        ] {
            let sentence = describe(&ureq::Error::BadUri(payload.to_string()));
            assert!(sentence.contains("scheme") || sentence.contains("host"), "{sentence}");
            for leak in ["SECRET", "key=", "q=", "/rss", "search.example"] {
                assert!(
                    !sentence.contains(leak),
                    "the sentence carries {leak:?} from the uri: {sentence}"
                );
            }
        }
    }

    /// The belt: whatever a future `ureq` puts in a payload, a URL-shaped token in a
    /// sentence keeps only its origin. This is the arm a reader cannot get wrong by
    /// forgetting to add a variant.
    #[test]
    fn a_url_in_any_payload_is_cut_to_its_origin() {
        let sentence = describe(&ureq::Error::Other(
            "failed at https://search.example/rss?q=SECRET&key=K while connecting".into(),
        ));
        assert!(sentence.contains("https://search.example"), "{sentence}");
        for leak in ["SECRET", "key=", "q=", "/rss", "?"] {
            assert!(!sentence.contains(leak), "the sentence carries {leak:?}: {sentence}");
        }
        // The same rule without a scheme, and the same rule inside brackets.
        assert_eq!(origin_only("search.example/rss?q=1"), "search.example");
        assert_eq!(origin_only("search.example?q=1"), "search.example");
        // Inside brackets: the `)` is the sentence's, not the url's, and it survives.
        assert_eq!(finish("(see https://a.example/x?y=1)".to_string()), "(see https://a.example)");
        assert_eq!(origin_only("http://[::1]:8080/x?y=1"), "http://[::1]:8080");
        // Prose is not a URL: a slash in a word, or a request line quoted in a
        // sentence, is left exactly as it was.
        for prose in ["and/or", "GET", "/rss", "n/a", "1.2.3.4:80", "the site is not there"] {
            assert_eq!(origin_only(prose), prose, "{prose}");
        }
    }

    /// One sweep over every failure this file can word: not one sentence contains a
    /// query string. The URLs below are the shape a search request has, so a variant
    /// that starts interpolating its payload shows up here as a red test rather than
    /// in a log line.
    #[test]
    fn no_failure_sentence_carries_a_query_string() {
        let url = "https://search.example/rss?q=SECRET-ENGINE-KEY&key=K";
        let cases: Vec<ureq::Error> = vec![
            ureq::Error::RequireHttpsOnly(url.to_string()),
            ureq::Error::BadUri(format!("{url} is missing host")),
            ureq::Error::Other(format!("bespoke failure for {url}").into()),
            ureq::Error::Tls("Rustls invalid dns name error"),
            ureq::Error::StatusCode(503),
            ureq::Error::HostNotFound,
            ureq::Error::ConnectionFailed,
            ureq::Error::BodyExceedsLimit(4_000_000),
            ureq::Error::LargeResponseHeader(20_000, 16_384),
            ureq::Error::Timeout(Timeout::RecvBody),
            ureq::Error::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "refused")),
            ureq::Error::Io(io::Error::other(format!(
                "os error while reading {url}"
            ))),
        ];
        for error in &cases {
            let sentence = describe(error);
            for leak in ["SECRET", "key=K", "q=", "?"] {
                assert!(
                    !sentence.contains(leak),
                    "{error:?} gave {sentence:?}, which carries {leak:?}"
                );
            }
            // The belt also holds for the two functions that wrap `describe`.
            let io_error = io::Error::other(format!("while fetching {url}"));
            for sentence in [describe_io(&io_error), body_failure(&io_error)] {
                assert!(
                    !sentence.contains("SECRET") && !sentence.contains('?'),
                    "a wrapped failure carries the url: {sentence}"
                );
            }
        }
    }

    #[test]
    fn every_failure_is_a_sentence_starting_with_what_happened() {
        let cases = [
            ureq::Error::HostNotFound,
            ureq::Error::ConnectionFailed,
            ureq::Error::StatusCode(503),
            ureq::Error::BodyExceedsLimit(4_000_000),
            ureq::Error::LargeResponseHeader(20_000, 16_384),
            ureq::Error::Tls("Rustls invalid dns name error"),
            ureq::Error::Other("bespoke".into()),
        ];
        for error in cases {
            let sentence = describe(&error);
            assert!(!sentence.is_empty());
            // A sentence, not a fragment: it starts with a letter. `TLS` is the one
            // entry that starts with an acronym, which is how it is written
            // everywhere else in ATLAS.
            assert!(
                sentence.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false),
                "{:?} gave {:?}, which does not start a sentence",
                error,
                sentence
            );
            // And not a debug dump: no struct shape and no page bytes leak in.
            assert!(!sentence.contains("Some("), "{}", sentence);
            assert!(!sentence.contains('{'), "{}", sentence);
        }
        assert_eq!(describe(&ureq::Error::StatusCode(503)), "the site answered HTTP 503");
        assert_eq!(
            describe(&ureq::Error::LargeResponseHeader(20_000, 16_384)),
            "the response headers are larger than 16384 bytes"
        );
    }
}
