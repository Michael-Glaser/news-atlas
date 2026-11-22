//! Loopback only — proved on the source, so a new listener cannot appear quietly.
//!
//! The claim: no crate in this workspace can open a socket that another machine
//! can reach. That is five rules, and each is checked here against the shipped
//! code rather than against a running process:
//!
//! 1. **Every listener bind** names a loopback literal, and the only one left is
//!    `atlas-config`'s port probe (`the_only_bind_left_is_the_port_probe`). A bind
//!    in any other file fails this test.
//! 2. **Every outbound connection** targets a loopback address: the model client
//!    (`atlas-ai`), the window's model exchange (`atlas-desktop`), the launcher's
//!    health and port probes. The browser's debug port and the end-to-end harness
//!    that dialled it were deleted with the web face (this task).
//! 3. **The address at a listener is a loopback literal** — checked on the call
//!    itself, so being inside an allowlisted function is not enough
//!    ([`every_listener_binds_a_loopback_literal`]). A helper that takes a host name
//!    and binds it is a finding. The one exception (`atlas-web/src/boot.rs::listen`,
//!    which bound a vetted parameter) went with its crate: the app has no listener.
//! 4. **The wildcard is never an address** — not as `"0.0.0.0"`, not as `"::"`,
//!    not as [`Ipv4Addr::UNSPECIFIED`] and not as a `[0, 0, 0, 0]` byte array,
//!    which is the same address spelled so the string scan cannot see it.
//! 5. **The allowlist is live.** An entry that no longer matches a hit is a
//!    finding, and so is an entry whose function no longer exists: an entry is a
//!    review decision, and a decision about code that has moved is a licence for
//!    whoever writes there next.
//!
//! The allowlist is explicit and one line each, with the reason. Adding a listener
//! therefore needs a decision in review, which is the point.
//!
//! # How a hit is found
//!
//! `atlas_guard::source` answers "what is code", and its wrapped matcher ignores
//! whitespace and up to one line break, so `TcpListener ::\n bind(…)`, `L::bind(…)`
//! through an alias import, and a bare `::bind(` under any type are all the same
//! hit. Comment handling lives there too: a line that starts with `*` is a
//! dereference, not a doc-comment continuation, so `*slot = TcpListener::bind(…)`
//! can no longer hide.
//!
//! Every rule below has a planted regression next to it: a synthetic
//! [`SourceFile`] holding the attack, and the assertion that the rule reports it.
//! Reverting the rule makes those tests fail — that is how they were checked.

use atlas_guard::source::{dead_copy, find_spanned, shipped_code, SourceFile};
use std::path::Path;

fn workspace() -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let files = shipped_code(&root);
    assert!(files.len() > 50, "the scanner read only {} files", files.len());
    files
}

/// A hit this test accepts, with the reason it is accepted.
///
/// `function` pins the entry to the function that opens the socket. An empty
/// `function` pins it to the whole file, which is weaker and says so in `why`.
struct Allowed {
    file: &'static str,
    function: &'static str,
    why: &'static str,
}

/// Every spelling that opens a socket to listen. `::bind(` rather than
/// `TcpListener::bind` on purpose: an alias (`use … as L; L::bind`) is the same
/// call, and the type name is not what makes it a listener.
const LISTEN: &[&str] = &["::bind(", "::try_bind(", "TcpListener::", "UdpSocket::", "axum::serve"];

/// Every spelling that opens a connection out.
const CONNECT: &[&str] = &["::connect(", "::connect_timeout("];

/// `::bind` / `::try_bind` / `axum::serve`: where a socket is opened to listen.
/// Each of these is loopback, and each says how.
///
/// The entries for `atlas-web/src/boot.rs` (`listen`, `serve`), `atlas-bench/src/startup.rs`,
/// `atlas-e2e/src/ports.rs` and `atlas-e2e/src/fake_model.rs` were removed with those
/// files: the web face, the browser harness and the startup measurement were deleted
/// (no HTTP face in the shipped tree). An entry pointing at a file that is gone is the
/// start of a bypass — rule 5 below reports exactly that — so the count here is now one
/// probe, and `the_only_bind_left_is_the_port_probe` pins it.
const LISTENERS: &[Allowed] = &[
    Allowed {
        file: "atlas-config/src/ports.rs",
        function: "port_free",
        why: "port_free(): TcpListener::bind((\"127.0.0.1\", port)).is_ok() and dropped at once — a probe, not a listener; the address is the loopback literal",
    },
];

/// `::connect` / `::connect_timeout`: where a connection is opened. Every one of
/// these must be a loopback address or a gated URL.
///
/// Removed with their crates: `atlas-browser/src/status.rs`, `atlas-browser/src/cdp.rs`,
/// `atlas-bench/src/startup.rs` and `atlas-e2e/src/{server,fake_model,http,cdp}.rs`.
/// The CDP reader and the end-to-end harness are gone; the four dials left are the
/// model client, the window's own model exchange and the launcher's two probes.
const CONNECTIONS: &[Allowed] = &[
    Allowed {
        file: "atlas-ai/src/http1.rs",
        function: "",
        why: "the model client: the address comes from Endpoint, which only exists after atlas_guard::Gated::loopback approved the literal",
    },
    Allowed {
        file: "atlas-desktop/src/http.rs",
        function: "exchange",
        why: "the window's one outbound socket: `endpoint()` accepts an `http://` URL whose host is the literal `127.0.0.1` and whose port is a number, and turns it into `SocketAddr::from(([127, 0, 0, 1], port))` — no name is ever resolved (`localhost` and `[::1]` are refused by its own tests), so the only address it can dial is this machine's loopback",
    },
    Allowed {
        file: "atlas-launcher/src/health.rs",
        function: "",
        why: "the launcher's health probe against its own 127.0.0.1 listener",
    },
    Allowed {
        file: "atlas-launcher/src/ports.rs",
        function: "state",
        why: "the launcher's port-availability probe: `TcpStream::connect_timeout(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))`, dropped at once. It was a bind until the launcher stopped serving anything — a probe that binds the port it asks about can be wrong about the answer, so it now asks by connecting, and never serves",
    },
];

/// One hit: the file, the statement, and the function it sits in.
struct Hit {
    path: String,
    line: usize,
    /// The last line the match covers (a call split over two lines covers two).
    end: usize,
    function: String,
    /// The code of the statement the hit sits in (a split call is one statement).
    text: String,
}

fn hits(files: &[SourceFile], needles: &'static [&'static str]) -> Vec<Hit> {
    let mut raw: Vec<Hit> = Vec::new();
    for needle in needles {
        for (path, first, last, _own, text) in find_spanned(files, needle, 2) {
            // A copy no compiler can reach (a sync conflict's `x 2.rs`) cannot bind
            // anything: its text is in no build. `audit_logs.rs` proves every copy
            // stays unreachable, so this is a fact, not an exemption.
            if dead_copy(files, &path) {
                continue;
            }
            let function = files
                .iter()
                .find(|file| file.path == path)
                .map(|file| file.function_at(first))
                .unwrap_or_default();
            raw.push(Hit {
                path,
                line: first,
                end: last,
                function,
                text,
            });
        }
    }
    // Two needles describe one call (`::bind(` and `TcpListener::`), and a call
    // split over two lines can be found starting on either of them: a statement is
    // one hit, merged when the line ranges *overlap*. Two binds on neighbouring
    // lines have disjoint ranges and stay two findings.
    raw.sort_by(|a, b| {
        (&a.path, a.line, std::cmp::Reverse(a.end)).cmp(&(&b.path, b.line, std::cmp::Reverse(b.end)))
    });
    let mut out: Vec<Hit> = Vec::new();
    for hit in raw {
        if let Some(seen) = out.iter_mut().find(|seen| {
            seen.path == hit.path && seen.line <= hit.end && hit.line <= seen.end
        }) {
            // One statement, found twice: keep the widest line range and both
            // windows, so the shape rule sees the whole call — the window that
            // holds `TcpListener::` and the one that holds `::bind(("0.0.0.0"…)`
            // are two halves of the same statement.
            seen.end = seen.end.max(hit.end);
            seen.line = seen.line.min(hit.line);
            if !seen.text.contains(&hit.text) && !hit.text.contains(seen.text.as_str()) {
                seen.text = format!("{} {}", seen.text, hit.text);
            }
            continue;
        }
        out.push(hit);
    }
    out
}

fn allow<'a>(allowed: &'a [Allowed], path: &str, function: &str) -> Option<&'a str> {
    allowed
        .iter()
        .find(|entry| entry.file == path && (entry.function.is_empty() || entry.function == function))
        .map(|entry| entry.why)
}

/// The hits no entry explains, as sentences a reviewer can act on.
fn unexplained(hits: &[Hit], allowed: &[Allowed], list: &str) -> Vec<String> {
    hits.iter()
        .filter(|hit| allow(allowed, &hit.path, &hit.function).is_none())
        .map(|hit| {
            format!(
                "{}:{} in {}() — {}\n    add it to {} in this test with the reason it is loopback",
                hit.path,
                hit.line,
                if hit.function.is_empty() { "?" } else { &hit.function },
                hit.text,
                list
            )
        })
        .collect()
}

/// Entries that excuse nothing any more: a file that no longer holds the
/// primitive, or a function name that no longer exists there.
fn stale(allowed: &[Allowed], hits: &[Hit], files: &[SourceFile]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in allowed {
        let file = match files.iter().find(|file| file.path == entry.file) {
            Some(file) => file,
            None => {
                out.push(format!("{}: no such file", entry.file));
                continue;
            }
        };
        let used = if entry.function.is_empty() {
            hits.iter().any(|hit| hit.path == entry.file)
        } else {
            hits.iter()
                .any(|hit| hit.path == entry.file && hit.function == entry.function)
        };
        if !used {
            out.push(format!(
                "{}:{} — the entry excuses nothing any more ({}); remove it or name the function that opens the socket now",
                entry.file, entry.function, entry.why
            ));
        }
        if !entry.function.is_empty() && !defines(file, entry.function) {
            out.push(format!(
                "{}: no function {} in that file — the entry points at code that is not there",
                entry.file, entry.function
            ));
        }
    }
    out
}

fn defines(file: &SourceFile, function: &str) -> bool {
    file.code.contains(&format!("fn {}(", function))
}

// ---------------------------------------------------------------------------
// the address at a listener, read at the call
// ---------------------------------------------------------------------------

/// Tokens that make an address provably loopback at the call site.
const LOOPBACK: &[&str] = &[
    "127.0.0.1",
    "Ipv4Addr::LOCALHOST",
    "Ipv6Addr::LOCALHOST",
    "[127, 0, 0, 1]",
    "\"::1\"",
];

/// Tokens that spell the wildcard, including the typed and byte-array forms that
/// a scan for the quoted literal cannot see.
const WILDCARD: &[&str] = &[
    "0.0.0.0",
    "UNSPECIFIED",
    "[0, 0, 0, 0]",
    "\"::\"",
    "Ipv6Addr::ANY",
    "Ipv4Addr::ANY",
];

// The `GUARDED_BIND` exception that used to sit here was
// `atlas-web/src/boot.rs::listen`: the app's own listener, which bound what
// `bind_address()` returned. That crate was deleted with the web face (this task).
// The shipped tree now has no listener at all, only the loopback *probe* in
// `atlas-config/src/ports.rs`, which names its address as a literal. An exception
// for a function nobody can write again is a licence for whoever creates that file
// next, so the exception went with the code it excused:
// `the_only_bind_left_is_the_port_probe` pins the new state.
fn is_loopback_literal(text: &str) -> bool {
    LOOPBACK.iter().any(|token| text.contains(token))
}

fn is_wildcard(text: &str) -> bool {
    WILDCARD.iter().any(|token| text.contains(token))
}

/// A listener whose call does not name a loopback address.
///
/// Only the bind-shaped hits: `axum::serve(listener, …)` is handed a socket that
/// some bind above already produced and cannot open one of its own, so the address
/// is judged where it is chosen.
fn shape_misses(hits: &[Hit]) -> Vec<String> {
    let mut out = Vec::new();
    // Judged on the *statement*, not on the needle that found it: a call split
    // over two lines is reported by whichever needle matched first, and the
    // question "does this bind name a loopback address?" is about the call.
    for hit in hits.iter().filter(|hit| hit.text.contains("bind")) {
        if is_wildcard(&hit.text) {
            out.push(format!(
                "{}:{} — the wildcard is a listener address: {}\n    bind a loopback address; the wildcard accepts every interface",
                hit.path, hit.line, hit.text
            ));
            continue;
        }
        if is_loopback_literal(&hit.text) {
            continue;
        }
        out.push(format!(
            "{}:{} in {}() — the address is not a loopback literal: {}\n    \
             bind 127.0.0.1 (or ::1) as a literal; a host name, a settings value or a function \
             parameter is not a decision this test can check, and since the app's own listener \
             was deleted there is no function allowed to forward an address",
            hit.path,
            hit.line,
            if hit.function.is_empty() { "?" } else { &hit.function },
            hit.text
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// the workspace rules
// ---------------------------------------------------------------------------

#[test]
fn every_listener_in_this_workspace_is_a_loopback_listener() {
    let files = workspace();
    let hits = hits(&files, LISTEN);
    assert!(!hits.is_empty(), "no listener was found at all — the scan is blind");
    let misses = unexplained(&hits, LISTENERS, "LISTENERS");
    assert!(
        misses.is_empty(),
        "a bind without the loopback guard:\n{}",
        misses.join("\n")
    );
}

#[test]
fn every_outbound_connection_goes_to_a_loopback_address() {
    let files = workspace();
    let hits = hits(&files, CONNECT);
    assert!(!hits.is_empty(), "no connection was found at all — the scan is blind");
    let misses = unexplained(&hits, CONNECTIONS, "CONNECTIONS");
    assert!(
        misses.is_empty(),
        "a connection that is not provably loopback:\n{}",
        misses.join("\n")
    );
}

#[test]
fn every_listener_binds_a_loopback_literal() {
    // Rule 3: the address is checked on the call, not on the file that contains
    // it, so an allowlisted function cannot grow a second, non-loopback bind and
    // a helper that forwards a host name cannot hide behind its caller.
    let files = workspace();
    let misses = shape_misses(&hits(&files, LISTEN));
    assert!(
        misses.is_empty(),
        "a listener whose address is not provably loopback:\n{}",
        misses.join("\n")
    );
}

#[test]
fn no_wildcard_literal_is_ever_an_address() {
    // Rule 4, second half: the wildcard is not a value any crate may hold — a
    // settings default, a constant, a help string that a later edit turns into a
    // bind. `"::"` and `"0.0.0.0"` are the spellings a reader sees; the typed
    // forms are the spellings a scan misses.
    //
    // Matched exactly, not whitespace-insensitively: the needles *are* quoted
    // literals, and `" :: "` is the title separator in `atlas-text`, not an
    // address. Collapsing whitespace there would report two innocent lines and
    // train a reviewer to add an entry without reading it.
    const REFUSALS: &[&str] = &[
        // the list of hosts the config refuses to bind
        "atlas-config/src/spec.rs",
        // the address verdict: the wildcard answers `loopback: false`
        "atlas-guard/src/address.rs",
        // `::` here is the IPv6 group separator, not an address
        "atlas-guard/src/urlsafe.rs",
        // the same, in the parser
        "atlas-guard/src/ipaddr.rs",
        // Removed with `atlas-web` (this task): its `boot.rs` refused the wildcard
        // before a socket and its `gate.rs` compared it in the loopback-authority
        // check. Both files are gone, and an entry for a file that does not exist
        // would excuse whatever is written at that path next.
    ];
    let files = workspace();
    let mut misses = Vec::new();
    let mut seen = 0usize;
    for file in &files {
        // A copy no compiler can reach carries no address into a build.
        if dead_copy(&files, &file.path) {
            continue;
        }
        for needle in ["\"0.0.0.0\"", "\"::\""] {
            for (line, text) in file.hits(needle) {
                if file.is_comment(line) {
                    continue; // prose cannot bind
                }
                seen += 1;
                if !REFUSALS.contains(&file.path.as_str()) {
                    misses.push(format!("{}:{} — {}", file.path, line, text));
                }
            }
        }
        for (line, text) in file.hits_wrapped("UNSPECIFIED", 2) {
            seen += 1;
            if !REFUSALS.contains(&file.path.as_str()) {
                misses.push(format!("{}:{} — {}", file.path, line, text));
            }
        }
    }
    assert!(seen > 0, "the wildcard scan read nothing — the needle is wrong");
    assert!(
        misses.is_empty(),
        "a wildcard address outside a refusal:\n{}",
        misses.join("\n")
    );
}

#[test]
fn every_allowlist_entry_still_describes_the_code() {
    // Rule 5. An entry that outlives the code it excused is the start of a
    // bypass: the next bind in that file is already allowed. Both lists, so the
    // reasons stay true and nobody has to grep for dead entries by hand.
    let files = workspace();
    let mut dead = stale(LISTENERS, &hits(&files, LISTEN), &files);
    dead.extend(stale(CONNECTIONS, &hits(&files, CONNECT), &files));
    assert!(
        dead.is_empty(),
        "a stale allowlist entry:\n{}",
        dead.join("\n")
    );
    for entry in LISTENERS.iter().chain(CONNECTIONS.iter()) {
        assert!(
            entry.why.len() > 20,
            "{}: the reason must be a sentence, not a word",
            entry.file
        );
    }
}

#[test]
fn the_only_bind_left_is_the_port_probe() {
    // This replaces `the_bind_guard_is_the_one_reachable_through_the_crate_root`,
    // which pinned that `atlas-web` re-exported `bind_address`/`listen`/`BIND_HOST`
    // and that its `main.rs` never named `TcpListener`. `atlas-web` was deleted with
    // the web face (this task), so the rule it encoded — "the app's one listener is
    // reached through the crate root" — has no subject. What matters now is stronger
    // and worth pinning on its own: the shipped tree has no listener at all. The
    // single bind-shaped hit is `atlas-config`'s port probe, which binds a loopback
    // literal, drops the socket at once and serves nothing. A second file here, or a
    // second function in this file, is a listener appearing without a review.
    let files = workspace();
    let hits = hits(&files, LISTEN);
    assert!(!hits.is_empty(), "no bind at all was found — the scan is blind");
    let files_with_binds: Vec<&str> = {
        let mut names: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names
    };
    assert_eq!(
        files_with_binds,
        vec!["atlas-config/src/ports.rs"],
        "a crate other than atlas-config binds a socket — the window, the launcher and the \
         collector must not listen at all, and the app's own listener was deleted with atlas-web"
    );
    let functions: Vec<&str> = hits.iter().map(|hit| hit.function.as_str()).collect();
    assert_eq!(
        functions,
        vec!["port_free"],
        "a new function in atlas-config/src/ports.rs binds a socket; the probe is the only \
         allowed one and it may not grow a listener beside it"
    );
}

#[test]
fn a_test_only_module_is_not_a_shipped_listener() {
    // `atlas-net/src/server.rs` is a `#[cfg(test)] mod server;` fixture: it binds
    // 127.0.0.1:0 inside the test process and is never compiled into the app. The
    // scanner has to be able to say so from the declaration, because the
    // alternative — an allowlist entry — would excuse a listener in a file that
    // nothing stops from being made shippable later.
    let files = workspace();
    assert!(
        !files.iter().any(|file| file.path == "atlas-net/src/server.rs"),
        "a `#[cfg(test)] mod` file is in the shipped scan"
    );
    // …and the module is still declared, test-only, in the crate root.
    let net = files
        .iter()
        .find(|file| file.path == "atlas-net/src/lib.rs")
        .expect("atlas-net/src/lib.rs");
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("atlas-net/src/lib.rs"),
    )
    .expect("atlas-net/src/lib.rs on disk");
    let declaration = net.code.contains("mod server")
        || raw.contains("#[cfg(test)]")
        || net.code.contains("mod server;");
    assert!(
        declaration && raw.contains("#[cfg(test)]\nmod server;"),
        "the test-only declaration changed shape: this rule no longer tracks it"
    );
}

// ---------------------------------------------------------------------------
// planted regressions: each one fails if its rule is reverted
// ---------------------------------------------------------------------------

fn planted(path: &str, code: &str) -> Vec<SourceFile> {
    vec![SourceFile {
        path: path.to_string(),
        code: code.to_string(),
    }]
}

#[test]
fn a_dereference_line_cannot_hide_a_listener() {
    // Before: the audit skipped any line whose trimmed text began with `*`, which
    // is how a doc-comment continuation starts. An assignment through a
    // dereference is not a comment, and this is the cheapest bypass there was.
    let files = planted(
        "planted/src/bind.rs",
        "fn serve_status() {\n    *listener = TcpListener::bind(\"0.0.0.0:8787\").unwrap();\n}\n",
    );
    let hits = hits(&files, LISTEN);
    assert_eq!(hits.len(), 1, "the dereference line hid the bind: {:?}", hits.len());
    assert!(!unexplained(&hits, LISTENERS, "LISTENERS").is_empty());
    assert!(!shape_misses(&hits).is_empty(), "the wildcard was not named");
}

#[test]
fn a_listener_split_across_lines_is_still_a_listener() {
    // Before: a needle was one exact string on one line, so pressing Enter was
    // enough. The split here is *inside* the primitive — `::` ends one line and
    // `bind(` starts the next — so no single line holds a needle and only a
    // window can see the call.
    let files = planted(
        "planted/src/bind.rs",
        "fn serve_status() {\n    let l = std::net::TcpListener\n        ::\n        bind((\"0.0.0.0\", 8787))\n        .unwrap();\n    l\n}\n",
    );
    let hits = hits(&files, LISTEN);
    assert_eq!(hits.len(), 1, "a split bind was not a hit: {:?}", hits.len());
    assert!(!shape_misses(&hits).is_empty());
    // …and the softer split, between the type and the call, which the alias-proof
    // needle already covers on its own line.
    let files = planted(
        "planted/src/bind.rs",
        "fn serve_status() {\n    let l = std::net::TcpListener\n        ::bind((\"0.0.0.0\", 8787))\n        .unwrap();\n    l\n}\n",
    );
    assert_eq!(self::hits(&files, LISTEN).len(), 1);
}

#[test]
fn a_listener_named_through_an_alias_is_still_a_listener() {
    // Before: `TcpListener::bind` was the needle, so importing the type under
    // another name moved the call out of the scan.
    let files = planted(
        "planted/src/bind.rs",
        "use std::net::TcpListener as L;\nfn serve_status() {\n    L::bind(\"127.0.0.1:8787\").unwrap()\n}\n",
    );
    let hits = hits(&files, LISTEN);
    assert_eq!(hits.len(), 1, "the alias hid the bind: {}", hits.len());
}

#[test]
fn a_helper_that_binds_a_parameter_is_a_finding() {
    // The shape rule: a function that takes the address and binds it can be given
    // any address, so the *call* must carry a literal. This is the "listener via a
    // helper function" attack — the helper is new, so no entry covers it, and the
    // address it binds is not provable.
    let files = planted(
        "planted/src/bind.rs",
        "pub fn open(host: &str, port: u16) -> std::net::TcpListener {\n    std::net::TcpListener::bind((host, port)).expect(\"bind\")\n}\n",
    );
    let hits = hits(&files, LISTEN);
    assert_eq!(hits.len(), 1);
    let misses = shape_misses(&hits);
    assert_eq!(misses.len(), 1, "a forwarded address passed the shape rule");
    assert!(misses[0].contains("not a loopback literal"), "{}", misses[0]);
}

#[test]
fn a_bind_that_forwards_its_address_is_a_finding_everywhere() {
    // This is `the_guarded_bind_may_only_bind_what_the_guard_returned` with the
    // exception removed. That test let `atlas-web/src/boot.rs::listen` bind a bare
    // `addr` parameter, because `bind_address()` had vetted it. `atlas-web` was
    // deleted with the web face (this task), so there is no vetted parameter left:
    // a bind whose address is a parameter, a settings value or a typed wildcard is
    // now a finding in every file, including the one the exception used to cover.
    let parameter = planted(
        "atlas-config/src/ports.rs",
        "pub fn listen(addr: SocketAddr) -> io::Result<Listener> {\n    let inner = TcpListener::bind(addr)\n        .map_err(|e| e)?;\n    Ok(Listener { inner })\n}\n",
    );
    let misses = shape_misses(&hits(&parameter, LISTEN));
    assert_eq!(
        misses.len(),
        1,
        "a bind that forwards an address was accepted — no function is allowed to do that any more"
    );
    assert!(misses[0].contains("not a loopback literal"), "{}", misses[0]);
    let wildcard = planted(
        "atlas-config/src/ports.rs",
        "pub fn listen(addr: SocketAddr) -> io::Result<Listener> {\n    let inner = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 8787))?;\n    Ok(Listener { inner })\n}\n",
    );
    let misses = shape_misses(&hits(&wildcard, LISTEN));
    assert_eq!(misses.len(), 1, "the typed wildcard slipped through");
    // …and a second bind, with a settings value, a few lines below a literal one
    // (further apart than the two-line match window, so the two statements cannot
    // merge into one hit): a loopback literal on another line excuses nothing.
    let second = planted(
        "atlas-config/src/ports.rs",
        "fn port_free(port: u16) -> bool {\n    let _ = TcpListener::bind((\"127.0.0.1\", port)).is_ok();\n\n    let settings = Settings::load();\n    let _ = TcpListener::bind((settings.host.as_str(), 8787));\n    true\n}\n",
    );
    assert_eq!(
        shape_misses(&hits(&second, LISTEN)).len(),
        1,
        "a second, non-literal bind beside a loopback one was accepted"
    );
}

#[test]
fn a_connection_through_an_alias_is_still_a_connection() {
    let files = planted(
        "atlas-launcher/src/ports.rs",
        "use std::net::TcpStream as S;\nfn dial(addr: SocketAddr) {\n    let _ = S::connect(addr);\n}\n",
    );
    let hits = hits(&files, CONNECT);
    assert_eq!(hits.len(), 1, "the alias hid the connection");
    assert!(!unexplained(&hits, CONNECTIONS, "CONNECTIONS").is_empty());
}

/// The in-process web engine adds no listener to **our** code.
///
/// `atlas-desktop` now carries `wry` — a binding to the web engine the operating system
/// owns (WKWebView on macOS, WebView2 on Windows). The engine's own process may hold
/// sockets; that is the OS's code and not this product's. What this rule pins is the half
/// this tree writes: no `::bind(`, no `TcpListener`, no `axum::serve` in the window's
/// shipped source, and none in the file that will own the engine (`src/webview.rs`) once
/// it lands. It fails if the engine is moved to another crate (the scan would then be
/// pointed at the wrong source) as well as if a bind appears.
#[test]
fn the_web_engine_adds_no_listener_to_our_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let manifest = std::fs::read_to_string(root.join("atlas-desktop/Cargo.toml"))
        .expect("atlas-desktop/Cargo.toml");
    assert!(
        manifest.contains("wry"),
        "atlas-desktop no longer carries `wry`: this rule exists because the window links \
         an in-process web engine, and it must be pointed at the crate that does"
    );
    let files = shipped_code(&root);
    let desktop: Vec<SourceFile> = files
        .iter()
        .filter(|file| file.path.starts_with("atlas-desktop/src/"))
        .cloned()
        .collect();
    assert!(
        desktop.len() > 20,
        "the scan read only {} files under atlas-desktop/src (measured 26 when this rule \
         was written) — it is pointed at the wrong directory and would pass on an empty set",
        desktop.len()
    );
    let mut hits: Vec<String> = Vec::new();
    for needle in LISTEN {
        for (path, first, _last, _own, text) in find_spanned(&desktop, needle, 2) {
            if dead_copy(&desktop, &path) {
                continue;
            }
            hits.push(format!("{path}:{first} `{needle}` — {text}"));
        }
    }
    assert!(
        hits.is_empty(),
        "the window opens a listening socket — the engine is in-process on purpose, a port \
         is the thing the product must not have:\n  {}",
        hits.join("\n  ")
    );
}
