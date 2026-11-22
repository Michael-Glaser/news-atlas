//! The window's one outbound address: the loopback model, and nothing else.
//!
//! The native window reads `data/atlas.db` and opens no listener (`src/main.rs`). The
//! one thing it may ever *talk to* is the local model server, and "the local model
//! server" has to be a decision made on the URL string before a socket exists — not a
//! hope about what a name resolves to. This file proves that decision against the code
//! that ships:
//!
//! * [`http::endpoint`] — the check (`src/http.rs:111`), linked from the crate's own
//!   library target (`atlas_desktop::http`) so this test asks the shipped function;
//! * [`http::ask`] — the dial (`src/http.rs:85`), the same call
//!   `src/views/chat.rs:170` makes when a reader sends a message.
//!
//! `src/http.rs`'s own `#[cfg(test)] mod tests` are the author's and run in the library
//! target (`cargo test -p atlas-desktop --lib`), not here: a failure there is that file's,
//! not this one's.
//!
//! What is proven, in the order the file proves it:
//!
//! 1. **The accepted URL is a `127.0.0.1` literal with an explicit port**, and the
//!    value handed on is the address a socket is opened to — not a name, not something
//!    resolved later.
//! 2. **A name is refused with a sentence** — `localhost` included. A name is only
//!    loopback after a resolution this crate does not perform, and a name that answers
//!    with loopback today can answer with something else tomorrow. The sentence names
//!    the address that would work.
//! 3. **A LAN address and a public address are refused with a sentence** — including
//!    the cloud metadata address, which is the target that matters.
//! 4. **No proxy environment variable can change any of that.** A proxy works by
//!    replacing the address a client dials, so two proofs: the decision gives identical
//!    answers with `HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY` and their lower-case twins
//!    pointing at a listener this test owns; and a real request through
//!    [`http::ask`] lands on the model server while that listener is never connected to.
//!    The transport makes this structural rather than lucky: it is
//!    `TcpStream::connect_timeout` to a `SocketAddr` built from the literal, so there is
//!    no resolver and no proxy code on the path to consult an environment variable with.
//! 5. **Nothing in this crate can resolve a name**, so the check cannot be undone
//!    downstream even by accident.
//!
//! Plus the edges the check owes a reader: a refusal sentence never echoes the value it
//! refused (`ATLAS_MODEL_URL` is hostile by construction, and an echo would put a
//! newline or a bidi override on screen); the scheme is matched case-insensitively
//! (RFC 3986 §3.1 — `HTTP://` is the same origin, and only `http` is a scheme this client
//! speaks); a query or a fragment is refused with the `?`/`#` it carried, not with a port
//! or user-name complaint; port 0 is refused by the check rather than by the connect; and
//! the decision is the one on the dial path, in that order.
//!
//! How each test was shown to be load-bearing: the same test file was run against a copy
//! of `src/` in which (a) the host check was deleted, (b) a `ToSocketAddrs` resolver and
//! a `HTTP_PROXY` read were added, (c) the refusal echoed the refused url, (d) the port-0
//! check was deleted. Every one of those mutants reddens the test named for it below.
//!
//! Every socket here is a `127.0.0.1:0` listener bound inside the test process and
//! dropped before the test returns. Nothing is fetched, nothing is started.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

// The check and the dial under test, from the crate that ships them. No `src/` file is
// edited by this file, and nothing is re-implemented here: `atlas_desktop::http` is the
// library target of the same crate the window binary links.
use atlas_desktop::http;

/// The URL the window uses when nobody sets one: `app::DEFAULT_MODEL_URL`.
const DEFAULT_MODEL_URL: &str = "http://127.0.0.1:8080";

/// The process environment is global, so the tests that change it hold this.
static ENV: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// 1. The accepted address: a 127.0.0.1 literal with an explicit port.
// ---------------------------------------------------------------------------

#[test]
fn the_url_the_chat_client_dials_is_a_127_0_0_1_literal() {
    // The shape the window uses by default, and what it turns into: a `Host:` header and
    // a socket address, both of them the literal.
    let (host, address) = http::endpoint(DEFAULT_MODEL_URL).expect("the default model url");
    assert_eq!(host, "127.0.0.1:8080");
    assert_eq!(address, SocketAddr::from(([127, 0, 0, 1], 8080)));
    assert!(address.ip().is_loopback(), "{address} is not loopback");

    // Surrounding whitespace is trimmed, not refused: it arrives from a pasted value.
    let (host, address) = http::endpoint("  http://127.0.0.1:9091  ").expect("a pasted url");
    assert_eq!(host, "127.0.0.1:9091");
    assert_eq!(address.port(), 9091);

    // A path after the authority is ignored by the dial (the request line is always
    // `/v1/chat/completions`), and it cannot smuggle a different authority past the
    // check: the address comes from the literal, not from the path.
    let (host, address) =
        http::endpoint("http://127.0.0.1:8080/anything/at/all").expect("a path after the host");
    assert_eq!(host, "127.0.0.1:8080");
    assert_eq!(address, SocketAddr::from(([127, 0, 0, 1], 8080)));
}

// ---------------------------------------------------------------------------
// 2. A hostname is refused with a sentence — `localhost` included.
// ---------------------------------------------------------------------------

#[test]
fn a_hostname_is_refused_with_a_sentence() {
    for url in [
        "http://localhost:8080",
        "http://LOCALHOST:8080",
        "http://localhost.:8080",
        "http://model.local:8080",
        "http://127.0.0.1.nip.io:8080",
        "http://localhost:8080/v1/chat/completions",
    ] {
        let sentence = refused(url);
        // The refusal says what is wrong and what to write instead, so a reader whose
        // `ATLAS_MODEL_URL` is `http://localhost:8080` (the obvious wrong guess) can fix
        // it without reading the source.
        assert!(
            sentence.contains("127.0.0.1"),
            "{}: the sentence must name the address that would work\ngot: {}",
            why(url),
            sentence
        );
        assert!(
            !sentence.to_lowercase().contains("resolve"),
            "{}: the sentence must not suggest the window resolves names\ngot: {}",
            why(url),
            sentence
        );
    }
    // And `localhost` is refused even though it would resolve to this machine: a name is
    // a resolution this crate does not perform, so it is not a decision about an address.
    assert!(http::endpoint("http://localhost:8080").is_err());
    assert!(http::endpoint("http://127.0.0.1:8080").is_ok());
}

// ---------------------------------------------------------------------------
// 3. A LAN address and a public address are refused with a sentence.
// ---------------------------------------------------------------------------

#[test]
fn a_lan_address_is_refused_with_a_sentence() {
    for url in [
        "http://192.168.1.10:8080",
        "http://192.168.0.1:80",
        "http://10.0.0.5:8080",
        "http://172.16.4.4:8080",
        "http://169.254.169.254:80", // cloud metadata: the target that matters
        "http://[fe80::1]:8080",     // ipv6 link-local
        "http://[fd00::1]:8080",     // ipv6 unique-local
    ] {
        let sentence = refused(url);
        assert!(
            sentence.contains("127.0.0.1"),
            "{}: the sentence must name the address that would work\ngot: {}",
            why(url),
            sentence
        );
    }
}

#[test]
fn a_public_address_is_refused_with_a_sentence() {
    for url in [
        "http://93.184.216.34:8080",
        "http://8.8.8.8:80",
        "http://203.0.113.10:8080",
        "http://[2606:4700:4700::1111]:8080",
    ] {
        let sentence = refused(url);
        assert!(
            sentence.contains("127.0.0.1"),
            "{}: the sentence must name the address that would work\ngot: {}",
            why(url),
            sentence
        );
    }
}

#[test]
fn the_other_spellings_of_loopback_are_refused_with_a_sentence() {
    // `127.0.0.2`, `127.1`, `0x7f000001`, `2130706433` and `[::1]` are all loopback —
    // and all refused, because the rule is the literal the model server is on, not "any
    // address that happens to be this machine". One sentence, the same address to write.
    for url in [
        "http://127.0.0.2:8080",
        "http://127.1:8080",
        "http://0x7f000001:8080",
        "http://2130706433:8080",
        "http://[::1]:8080",
        "http://[0:0:0:0:0:0:0:1]:8080",
    ] {
        let sentence = refused(url);
        assert!(
            sentence.contains("127.0.0.1"),
            "{}: the sentence must name the address that would work\ngot: {}",
            why(url),
            sentence
        );
    }
}

#[test]
fn a_url_that_is_not_an_http_origin_is_refused_with_a_sentence() {
    for (url, needle) in [
        ("https://127.0.0.1:8080", "http"),
        ("ftp://127.0.0.1:8080", "http"),
        ("127.0.0.1:8080", "http"),
        ("//127.0.0.1:8080", "http"),
        ("http://127.0.0.1", "port"),
        ("http://127.0.0.1:", "port"),
        ("http://127.0.0.1:notaport", "port"),
        ("http://user:pass@127.0.0.1:8080", "user"),
        ("http://user@127.0.0.1:8080", "user"),
    ] {
        let sentence = refused(url);
        assert!(
            sentence.to_lowercase().contains(needle),
            "{}\ngot: {}",
            why(url),
            sentence
        );
    }
}

// ---------------------------------------------------------------------------
// 3c. The scheme is case-insensitive; a query or a fragment is named.
// ---------------------------------------------------------------------------

/// RFC 3986 §3.1: a scheme is case-insensitive, so `HTTP://127.0.0.1:8080` names the same
/// origin as `http://127.0.0.1:8080`. A reader who pastes a url a shell, a browser or a
/// chat client upper-cased should not be told their address is wrong. What does *not*
/// change with the case: the literal, the port, and the refusal of every other scheme —
/// a scheme is `http` or a sentence, and no spelling of `https` becomes dialable.
#[test]
fn a_scheme_case_does_not_change_the_address() {
    let same = SocketAddr::from(([127, 0, 0, 1], 8080));
    for spelling in [
        "http://127.0.0.1:8080",
        "HTTP://127.0.0.1:8080",
        "Http://127.0.0.1:8080",
        "hTtP://127.0.0.1:8080",
        "  HTTP://127.0.0.1:8080  ",
    ] {
        let (host, address) = http::endpoint(spelling)
            .unwrap_or_else(|problem| panic!("{}: {problem}", why(spelling)));
        assert_eq!(host, "127.0.0.1:8080", "{}", why(spelling));
        assert_eq!(address, same, "{}", why(spelling));
    }

    // The case rule does not relax the address rule, and it does not make a second scheme
    // acceptable: the refusals keep their sentences, address for address.
    for (url, needle) in [
        ("HTTPS://127.0.0.1:8080", "http"),
        ("HTTPs://127.0.0.1:8080", "http"),
        ("FTP://127.0.0.1:8080", "http"),
        ("HTTP:/127.0.0.1:8080", "http"),
        ("HTTP://localhost:8080", "127.0.0.1"),
        ("HTTP://LOCALHOST:8080", "127.0.0.1"),
        ("HTTP://192.168.1.10:8080", "127.0.0.1"),
        ("HTTP://93.184.216.34:8080", "127.0.0.1"),
        ("HTTP://[::1]:8080", "127.0.0.1"),
        ("HTTP://127.0.0.2:8080", "127.0.0.1"),
        ("HTTP://127.0.0.1:0", "port 0"),
        ("HTTP://127.0.0.1", "port"),
        ("HTTP://user:pass@127.0.0.1:8080", "user"),
    ] {
        let sentence = refused(url);
        assert!(
            sentence.to_lowercase().contains(&needle.to_lowercase()),
            "{}\ngot: {}",
            why(url),
            sentence
        );
    }

    // And a case-varied scheme really dials the model: the check's answer is the address a
    // socket is opened to, so an accepted spelling that did not reach the server would be
    // a check that only looked right.
    let model = FakeModel::start("the local answer");
    let shouted = format!("HTTP://127.0.0.1:{}", model.address().port());
    let answer = http::ask(&shouted, "you are the archive", &[http::Message::user("hello")]);
    assert_eq!(
        answer.expect("an answer").answer,
        "the local answer",
        "an upper-case scheme did not reach the model server"
    );
    let seen = model.requests();
    assert_eq!(seen.len(), 1, "the model server saw {} requests", seen.len());
    assert!(
        seen[0].contains(&format!("Host: 127.0.0.1:{}\r\n", model.address().port())),
        "the request did not carry the literal Host header:\n{}",
        seen[0].lines().take(6).collect::<Vec<_>>().join("\n")
    );
}

/// A query or a fragment is not part of an origin, and this window's request line is a
/// constant (`/v1/chat/completions`), so there is nowhere to put a query and a fragment is
/// never sent at all. Both were refused before this pass — with the wrong sentence: a
/// query came back as "a port that is not a number" and `#@evil` as "carries a user name",
/// which sends a reader looking at the wrong end of their url. The sentence must name the
/// `?` or the `#`, and it must be the check that refuses, not the connect.
#[test]
fn a_query_or_a_fragment_is_refused_with_a_sentence_that_names_it() {
    for (url, needle) in [
        ("http://127.0.0.1:8080?x=1", "query"),
        ("http://127.0.0.1:8080?", "query"),
        ("http://127.0.0.1:8080/v1/chat/completions?x=1", "query"),
        ("HTTP://127.0.0.1:8080?x=1", "query"),
        ("http://127.0.0.1:8080#@evil", "fragment"),
        ("http://127.0.0.1:8080#", "fragment"),
        ("http://127.0.0.1:8080/#frag", "fragment"),
        ("http://127.0.0.1:8080#user:pass@127.0.0.1:8080", "fragment"),
    ] {
        let sentence = refused(url);
        assert!(
            sentence.contains(needle),
            "{} was refused without naming the {needle}: {}",
            why(url),
            sentence
        );
        // The character is reported by position, never by quoting the value: the url is an
        // environment variable and may carry anything.
        assert!(sentence.contains("byte "), "{}: {}", why(url), sentence);
        assert!(
            !sentence.contains("?x=1") && !sentence.contains("#@evil"),
            "{}: the sentence quotes the value it refused: {}",
            why(url),
            sentence
        );
    }

    // Nothing else was relaxed to make room for this: the literal rule, the port-0 refusal
    // and the `..` refusal answer exactly as they did.
    assert!(http::endpoint("http://127.0.0.1:8080").is_ok());
    assert!(refused("http://localhost:8080").contains("127.0.0.1"));
    assert!(refused("http://192.168.1.10:8080").contains("127.0.0.1"));
    assert!(refused("http://127.0.0.1:0").contains("port 0"));
    assert!(refused("http://127.0.0.1:8080/../..").contains(".."));
    // A query or a fragment never reaches a socket, so there is no probe to make here:
    // `endpoint` decides before `exchange` opens one (see `the_check_is_on_the_dial_path`).
}

#[test]
fn junk_is_refused_with_a_sentence_and_never_panics() {
    // The URL comes from `ATLAS_MODEL_URL`, so it is as hostile as an environment
    // variable can be. The only acceptable outcomes are an address and one sentence:
    // never a panic, never an echo of the input into a label.
    let long = format!("http://127.0.0.1:8080/{}", "a".repeat(5_000));
    for url in [
        "",
        "   ",
        "http://",
        "http://:8080",
        "http://127.0.0.1:8080\u{0}",
        "http://127.0.0.1:8080\nX-Injected: 1",
        "http://[::1:8080",
        "\u{202e}http://127.0.0.1:8080",
        long.as_str(),
    ] {
        match http::endpoint(url) {
            Ok((host, address)) => {
                // The one thing a success may be: this machine, on the literal.
                assert_eq!(host, format!("127.0.0.1:{}", address.port()), "{:?}", why(url));
                assert_eq!(address.ip().to_string(), "127.0.0.1", "{:?}", why(url));
            }
            Err(sentence) => {
                assert!(
                    !sentence.trim().is_empty(),
                    "{} was refused with nothing",
                    why(url)
                );
                // A sentence is a sentence: a 5 kB URL must not become a 5 kB label,
                // which is why `endpoint` never echoes the value it refused.
                assert!(
                    sentence.chars().count() <= 400,
                    "{}: {} characters is not a sentence",
                    why(url),
                    sentence.chars().count()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3b. A hostile url cannot choose how its own refusal reads.
// ---------------------------------------------------------------------------

/// `ATLAS_MODEL_URL` is an environment variable, so the string the check refuses is
/// hostile by construction. A refusal that quoted it back would put its `\n` (a second
/// row in a one-line label), its NUL or its right-to-left override (a sentence read in
/// the wrong order) on screen: the value is held as a `String` from `src/app.rs:119`
/// (`model_url_from`) all the way to the chat thread, and the sentence is only built at
/// dial time (`src/views/chat.rs:170`). `endpoint` answers with a sentence about the *address*
/// it wants instead, and never with the value it refused — this test is what keeps that
/// true, because echoing is the natural thing to write and reads fine in review.
#[test]
fn a_hostile_url_cannot_choose_how_its_refusal_reads() {
    for url in [
        "http://127.0.0.1:8080\nX-Injected: 1",
        "http://127.0.0.1:8080\r\nX-Injected: 1",
        "http://127.0.0.1:8080\u{0}",
        "\u{202e}http://127.0.0.1:8080",
        "http://192.168.1.9\n\u{202e}",
        "http://user:secret@127.0.0.1:8080",
    ] {
        let sentence = refused(url);
        assert!(
            !sentence.chars().any(|ch| ch.is_control() || is_bidi(ch)),
            "{}: the refusal carries the input's control characters, so it can be read as \
             two rows or in the wrong order: {:?}. The sentence must describe the address \
             the window wants, never quote the value it refused.",
            why(url),
            sentence
        );
    }
}

/// The bidirectional formatting characters: not `is_control()` in Rust, and the ones that
/// reorder a Latin sentence when a URL supplies them.
fn is_bidi(ch: char) -> bool {
    matches!(
        ch,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

// ---------------------------------------------------------------------------
// 4. No proxy environment variable can change where the request goes.
// ---------------------------------------------------------------------------

#[test]
fn a_proxy_environment_variable_cannot_change_where_the_model_request_goes() {
    // A proxy works by replacing the address a client dials. If the decision or the
    // transport read `HTTP_PROXY`/`ALL_PROXY`, then `http://127.0.0.1:8080` could be
    // answered by somebody else while looking local. Proven on the wire, offline: a real
    // model server on an ephemeral port, a real bait listener for the environment to
    // name, and the shipped `ask` between them.
    let _guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let model = FakeModel::start("the local answer");
    let bait = TcpListener::bind("127.0.0.1:0").expect("an ephemeral loopback port");
    bait.set_nonblocking(true).expect("a non-blocking listener");
    let bait_url = format!("http://{}", bait.local_addr().expect("the bait address"));

    let model_url = model.url();
    // The decision is the same before and after the environment is hostile.
    let before = http::endpoint(&model_url);
    let variables = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];
    let previous: Vec<Option<std::ffi::OsString>> =
        variables.iter().map(|name| std::env::var_os(name)).collect();
    for name in variables {
        std::env::set_var(name, &bait_url);
    }
    let after = http::endpoint(&model_url);
    let answer = http::ask(&model_url, "you are the archive", &[http::Message::user("hello")]);
    for (name, value) in variables.iter().zip(previous.iter()) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }

    // 1. The decision did not move.
    assert_eq!(
        before.as_ref().map(|(host, address)| (host.clone(), *address)),
        after.as_ref().map(|(host, address)| (host.clone(), *address)),
        "a proxy environment variable changed the address the window would dial"
    );
    // 2. The request went to the literal, and the model answered.
    assert_eq!(
        answer.expect("an answer").answer,
        "the local answer",
        "the request did not reach the model server the url named"
    );
    let seen = model.requests();
    assert_eq!(seen.len(), 1, "the model server saw {} requests", seen.len());
    assert!(
        seen[0].starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
        "the model server did not receive a chat-completions request:\n{}",
        first_line(&seen[0])
    );
    assert!(
        seen[0].contains(&format!(
            "Host: 127.0.0.1:{}\r\n",
            model.address().port()
        )),
        "the Host header is not the literal the check approved:\n{}",
        seen[0].lines().take(6).collect::<Vec<_>>().join("\n")
    );
    // 3. The listener the environment named never saw a connection.
    std::thread::sleep(Duration::from_millis(20));
    match bait.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => panic!("the bait listener failed: {error}"),
        Ok((stream, from)) => {
            drop(stream);
            panic!(
                "the window connected to the proxy environment's address ({from}) instead of \
                 deciding on the url — a proxy cannot answer for the model server"
            )
        }
    }
    // Both sockets this test opened are closed here (the bait explicitly, the model by
    // dropping `FakeModel`).
    drop(bait);
    drop(model);
}

#[test]
fn a_hostile_model_url_is_refused_before_any_connection() {
    // The other half of the same claim: a URL that names this machine by *name* is
    // refused even though the name would reach a listener this test owns, so the refusal
    // is a decision about the string and not a failed connection afterwards.
    let _guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let bait = FakeModel::start("should never be reached");
    let named = format!("http://localhost:{}/v1", bait.address().port());

    let outcome = http::ask(&named, "you are the archive", &[http::Message::user("hello")]);

    let sentence = outcome.expect_err("a named model server was dialled");
    assert!(
        sentence.contains("127.0.0.1"),
        "the refusal must name the address that would work: {sentence}"
    );
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        bait.requests().len(),
        0,
        "the name reached a listener anyway: {:?}",
        bait.requests()
    );
}

#[test]
fn no_source_file_in_the_window_reads_a_proxy_variable() {
    // The structural half, and the half that keeps being true as the client grows: the
    // window must never turn the environment into a connection target. A comment may name
    // a proxy; reading one is the defect.
    for (path, source) in sources() {
        for (number, line) in source.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            let reads_env = lower.contains("env::var") || lower.contains("var_os");
            let names_proxy = lower.contains("proxy");
            assert!(
                !(reads_env && names_proxy),
                "{}:{}: the window reads a proxy variable — the environment must not be able to \
                 choose a connection target:\n  {}",
                path.display(),
                number + 1,
                line.trim()
            );
        }
    }
}

#[test]
fn the_window_can_only_reach_an_address_it_never_resolved() {
    // Why the check holds even after the request leaves the check: the connection is
    // `TcpStream::connect_timeout` on a `SocketAddr` that was built from the literal, and
    // there is no name-resolving call anywhere in the crate. A resolver is the one thing
    // that could turn a checked name into an unchecked address, so its absence is
    // asserted rather than assumed.
    let mut connectors = 0usize;
    for (path, source) in sources() {
        for (number, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            for resolver in ["to_socket_addrs", "ToSocketAddrs", "lookup_host", "getaddrinfo"] {
                assert!(
                    !trimmed.contains(resolver),
                    "{}:{}: the window resolves a name ({resolver}), which is the one step that \
                     can turn a checked url into an unchecked address:\n  {}",
                    path.display(),
                    number + 1,
                    trimmed
                );
            }
            if trimmed.contains("TcpStream::connect") {
                connectors += 1;
                assert!(
                    trimmed.contains("connect_timeout"),
                    "{}:{}: a connection without a timeout can hang a frame:\n  {}",
                    path.display(),
                    number + 1,
                    trimmed
                );
            }
        }
    }
    assert_eq!(
        connectors, 1,
        "expected exactly one place that opens the model socket, found {connectors}"
    );
}

#[test]
fn the_window_opens_no_listener() {
    // `src/main.rs` promises "no listener at all". A listener is the one thing that would
    // let something else talk to this process with the archive open, so it is asserted.
    for (path, source) in sources() {
        for (number, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !trimmed.contains("TcpListener") && !trimmed.contains("UdpSocket"),
                "{}:{}: the window opens a socket to listen on:\n  {}",
                path.display(),
                number + 1,
                trimmed
            );
        }
    }
}

/// The check is applied where the socket is opened, not merely available.
///
/// The page streams now (`http::ask_streaming`, which hands each delta to a sink), so the
/// entry point this file holds to the rule is the streaming one; `ask` and `ask_within`
/// are held to it as well — the rule is about every path that dials, not about one name.
#[test]
fn the_check_is_on_the_dial_path() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("http.rs"),
    )
    .expect("src/http.rs");
    for entry in ["pub fn ask(", "pub fn ask_within(", "pub fn ask_streaming("] {
        let body = source
            .split(entry)
            .nth(1)
            .unwrap_or_else(|| panic!("src/http.rs has `{entry}`"));
        let body = &body[..body.find("\n}\n").map(|end| end + 3).unwrap_or(body.len())];
        let checks = body.find("endpoint(").unwrap_or_else(|| {
            panic!(
                "`{entry}` dials without calling `endpoint(...)` — the check exists but nothing \
                 asks it before the socket is opened"
            )
        });
        let dials = body
            .find("exchange(")
            .unwrap_or_else(|| panic!("`{entry}` does not dial"));
        assert!(
            checks < dials,
            "`{entry}` dials before it checks: the url is used at byte {dials} and checked at \
             byte {checks}"
        );
    }
    // And the chat page is the caller, so the check is on the path a message takes. Since
    // the tool loop landed, the page does not dial `http` itself: `start` hands the turn to
    // `agent::run`, which is the one caller of `ask_streaming`. So the chain asserted here
    // is `views/chat.rs → agent::run` and `agent → http::ask_streaming(`, both of which have
    // to hold or a question could reach the socket without a checked url.
    let chat = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("views")
            .join("chat.rs"),
    )
    .expect("src/views/chat.rs");
    assert!(
        chat.contains("crate::agent::run("),
        "src/views/chat.rs no longer sends its turn through `agent::run` — the checked dial is \
         not the one the chat page uses"
    );
    let agent = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("agent.rs"),
    )
    .expect("src/agent.rs");
    assert!(
        agent.contains("http::ask_streaming("),
        "src/agent.rs no longer dials through `http::ask_streaming` — the loop would be a \
         second path to the socket that this file does not check"
    );
}

// ---------------------------------------------------------------------------
// 6. The port is a port: 0 is not an address, and the decision says so.
// ---------------------------------------------------------------------------

/// Port 0 passed the check while `"0".parse::<u16>()` was the only rule, so
/// `http://127.0.0.1:0` reached `TcpStream::connect_timeout` and the reader got a
/// connection error instead of a sentence. Not a way off the machine, but the decision
/// owes the sentence: port 0 is "any port", so the value is not an address at all. This
/// pins that the refusal comes from the check — no socket is opened for it — and that the
/// sentence names the port the server does listen on, like every other refusal.
#[test]
fn port_zero_is_refused_by_the_check_and_not_by_the_connect() {
    let sentence = refused("http://127.0.0.1:0");
    assert!(
        sentence.contains("port"),
        "the sentence must name what is wrong (port 0)\ngot: {sentence}"
    );
    // The boundary: 1 is a port, 65535 is a port, 65536 is not a `u16`.
    assert!(http::endpoint("http://127.0.0.1:1").is_ok());
    assert!(http::endpoint("http://127.0.0.1:65535").is_ok());
    refused("http://127.0.0.1:65536");
    refused("http://127.0.0.1:-1");
}

// ---------------------------------------------------------------------------
// A model server for the wire tests: loopback, ephemeral, closed when dropped.
// ---------------------------------------------------------------------------

/// One `llama-server`-shaped answer, served once per request on `127.0.0.1:0`.
///
/// It speaks just enough HTTP/1.1 for [`http::ask`] — a status line, a `Content-Length`,
/// the chat-completions body — and records the raw request so a test can assert what was
/// sent and where.
struct FakeModel {
    address: SocketAddr,
    seen: std::sync::Arc<Mutex<Vec<String>>>,
    /// Set by `Drop` so the serving thread gives the port back before the test returns.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeModel {
    fn start(answer: &str) -> FakeModel {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral loopback port");
        let address = listener.local_addr().expect("the model address");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let seen: std::sync::Arc<Mutex<Vec<String>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
        let body = format!(
            "{{\"choices\":[{{\"message\":{{\"role\":\"assistant\",\"content\":\"{answer}\"}}}}]}}"
        );
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        // Bind here, serve there: the listener is moved into the thread rather than bound
        // twice, because a second bind of a held ephemeral port is `AddrInUse` and the
        // model would never answer — a silent 30 s timeout in `ask` instead of a test
        // failure that names the reason.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread = {
            let seen = std::sync::Arc::clone(&seen);
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while !stop.load(std::sync::atomic::Ordering::SeqCst)
                    && std::time::Instant::now() < deadline
                {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                            let mut request = String::new();
                            let mut chunk = [0u8; 4096];
                            // One head plus the body its Content-Length declares.
                            let mut needed = usize::MAX;
                            loop {
                                match stream.read(&mut chunk) {
                                    Ok(0) => break,
                                    Ok(read) => {
                                        request.push_str(&String::from_utf8_lossy(&chunk[..read]));
                                        if needed == usize::MAX {
                                            if let Some(split) = request.find("\r\n\r\n") {
                                                let length = request[..split]
                                                    .lines()
                                                    .find_map(|line| {
                                                        let (name, value) = line.split_once(':')?;
                                                        if name.trim().eq_ignore_ascii_case("content-length") {
                                                            value.trim().parse::<usize>().ok()
                                                        } else {
                                                            None
                                                        }
                                                    })
                                                    .unwrap_or(0);
                                                needed = split + 4 + length;
                                            }
                                        }
                                        if request.len() >= needed {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            seen.lock().expect("the request log").push(request);
                            let _ = stream.write_all(reply.as_bytes());
                            let _ = stream.flush();
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            // Keep serving: a test may dial more than once, and the port
                            // stays open until `Drop`.
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => return,
                    }
                }
            })
        };
        FakeModel {
            address,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn address(&self) -> SocketAddr {
        self.address
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().expect("the request log").clone()
    }
}

impl Drop for FakeModel {
    fn drop(&mut self) {
        // The listener this test opened, closed here and not by luck: the serving thread
        // owns it, so asking the thread to stop and joining it is what gives the port
        // back before the test returns.
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// The sentence one URL is refused with, failing loudly (with the URL) if it was accepted.
fn refused(url: &str) -> String {
    match http::endpoint(url) {
        Ok((host, address)) => panic!(
            "{} was accepted as {host} / {address} — the window may only dial 127.0.0.1",
            why(url)
        ),
        Err(sentence) => {
            assert!(
                !sentence.trim().is_empty(),
                "{} was refused with an empty string; a refusal is a sentence",
                why(url)
            );
            assert!(!sentence.contains('\n'), "{}: {:?}", why(url), sentence);
            assert!(
                sentence.chars().count() <= 400,
                "{}: {} characters is not a sentence",
                why(url),
                sentence.chars().count()
            );
            sentence
        }
    }
}

/// A URL as a failure message can quote it, with control characters made visible (a test
/// message is a label like any other).
fn why(url: &str) -> String {
    let visible: String = url
        .chars()
        .take(80)
        .map(|ch| if ch.is_control() { '·' } else { ch })
        .collect();
    format!("{visible:?}")
}

/// The request line of a raw request, for a failure message.
fn first_line(request: &str) -> &str {
    request.lines().next().unwrap_or("")
}

/// Every `.rs` file the window is built from, so a structural claim is a test and not a
/// promise.
fn sources() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    collect(&root, &mut found);
    found.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(
        !found.is_empty(),
        "no sources under {} — the structural checks read nothing",
        root.display()
    );
    found
}

fn collect(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {}: {error}", dir.display()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, found);
        } else if path.extension().map(|ext| ext == "rs").unwrap_or(false) {
            match std::fs::read_to_string(&path) {
                Ok(source) => found.push((path, source)),
                Err(error) => panic!("cannot read {}: {error}", path.display()),
            }
        }
    }
}
