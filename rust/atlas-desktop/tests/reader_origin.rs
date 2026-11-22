//! The page engine is a view, not a second network client — and never the app's.
//!
//! The window has exactly two things that can reach a network: `http.rs`, which dials the
//! model server on the literal `127.0.0.1`, and `webview.rs`, which hands one address to the
//! operating system's web engine (WKWebView, WebView2). The engine is the dangerous one:
//! it renders a page written by strangers, it can follow redirects, and it resolves names —
//! everything `atlas-net` exists to gate. So the claim this file pins is not "the engine is
//! safe"; it is narrower and checkable:
//!
//! 1. **`atlas-net` is never named here.** The gated client is not a dependency of this
//!    crate, so `atlas_net::…` cannot compile, and no source file spells it either. A
//!    window that cannot name the client cannot borrow its verdicts and pretend it gated
//!    something.
//! 2. **No address reaches the engine from the model's answer.** The engine's entry points
//!    may be touched by the Reader view (a story row's `url`) and by the shell in `app.rs`
//!    (the click that opened that row), and by nothing else — the chat view draws the
//!    model's text and cannot hand it to a page. Addresses are never assembled from text
//!    in the reader path: a stored row, or nothing.
//! 3. **Loopback is a literal, and a name is refused.** `http::endpoint` accepts
//!    `http://127.0.0.1:<port>` and refuses every name — `localhost`, `127.0.0.1.nip.io`,
//!    `[::1]` — with a sentence that says the literal rule. The engine's own rule is
//!    different on purpose (`webview::allowed`: scheme, authority, no credentials), and it
//!    is pinned below together with the sentence that says the engine never resolves a
//!    name in this crate.
//! 4. **The reader path calls none of `atlas-net`'s DNS-pinning helpers.** Not
//!    `approved_addresses`, not `resolve_gate`, not `check_hop`, not `SystemLookup`.
//! 5. **The engine has no bridge into this process.** No IPC handler, no custom protocol, no
//!    `file://` allowance, no download, no permission granted: the archive (`data/atlas.db`,
//!    read-only) and the queue (`requests::queue_path`, a SQLite file) are reached by Rust
//!    calls, and a page has no call to make. The gate also runs *before* the builder — an
//!    address checked after `build_as_child` is an address already fetched — and the
//!    navigation handler is the same predicate, so a link the page follows obeys the rule the
//!    constructor did.
//!
//! What this file deliberately does **not** claim: that a page cannot reach loopback. It
//! can — that is what a browser is, and `allowed` says so in one sentence. The property
//! that keeps it from being an SSRF shim is that the app's *own* traffic never goes through
//! it (test 3) and the model client never accepts a name (test 8): the engine cannot be
//! asked to fetch what `http.rs` refused, because nothing connects the two.

use std::path::{Path, PathBuf};

/// The crate this test belongs to (`rust/atlas-desktop`).
fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Every `.rs` under `src/`, as `(relative path with '/', text)`.
///
/// A walk rather than a list: a new module is exactly the thing these tests exist to see,
/// and a list would have to be edited by the same change that adds the module.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, base, out);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, text));
        }
    }
    let root = crate_root();
    let mut out = Vec::new();
    walk(&root.join("src"), &root.join("src"), &mut out);
    assert!(
        out.len() > 10,
        "the scan read only {} source files — it is not looking at the crate",
        out.len()
    );
    out
}

/// One source file by its path under `src/`, with a sentence when it is gone.
fn text_of(relative: &str) -> String {
    sources()
        .into_iter()
        .find(|(path, _)| path == relative)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("src/{relative} is not in this crate any more"))
}

/// Every occurrence of `needle` in the crate, as `file:line — text`.
fn hits(needle: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (path, text) in sources() {
        for (index, line) in text.lines().enumerate() {
            if line.contains(needle) {
                out.push(format!("{}:{} — {}", path, index + 1, line.trim()));
            }
        }
    }
    out
}

/// The files the Reader page is made of. The engine, the view that owns it, and the chat
/// that must never reach it.
const READER_PATH: &[&str] = &["webview.rs", "views/reader.rs", "views/chat.rs"];

#[test]
fn the_window_cannot_name_the_gated_client() {
    // Rule 1, the hard half: the dependency. A crate that does not list `atlas-net` cannot
    // call it, whatever a future file says — and after `cargo build`, "cannot" is the
    // compiler's, not this test's.
    let manifest = std::fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("atlas-desktop/Cargo.toml is readable");
    let mut offenders = Vec::new();
    for (index, line) in manifest.lines().enumerate() {
        let code = line.split('#').next().unwrap_or("");
        if code.contains("atlas-net") {
            offenders.push(format!("Cargo.toml:{} — {}", index + 1, line.trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "the window names the gated client as a dependency:\n{}\n\
         fix: delete the edge. The window's only network client is `src/http.rs`, which \
         dials the literal 127.0.0.1 and gated nothing because it needs nothing: one \
         address, no names, no redirect to follow.",
        offenders.join("\n")
    );

    // Rule 1, the visible half: no file in the crate spells the name at all — not an
    // import, not a doc sentence telling a reader it is used. The second is how a claim
    // survives a deleted line.
    for needle in ["atlas_net", "atlas-net"] {
        let found = hits(needle);
        assert!(
            found.is_empty(),
            "`{}` appears in the window:\n{}",
            needle,
            found.join("\n")
        );
    }

    // Non-vacuity: the scan really read the two modules this test is about.
    for file in ["webview.rs", "views/reader.rs"] {
        let _ = text_of(file);
    }
}

#[test]
fn the_reader_path_calls_no_dns_pinning_helper() {
    // Rule 4. These are `atlas-net`'s names — the pinning that makes "the address the gate
    // judged is the address dialled" true in that crate. Copying one of them into the
    // window would look like diligence and be the opposite: a verdict about a *name*
    // borrowed by the one place that has no resolver and does not want one.
    for needle in [
        "approved_addresses",
        "resolve_gate",
        "check_hop",
        "SystemLookup",
        "resolver::",
    ] {
        let found = hits(needle);
        assert!(
            found.is_empty(),
            "the window calls atlas-net's DNS pinning (`{}`):\n{}\n\
             fix: the model client refuses names instead (see \
             `loopback_is_a_literal_and_a_name_is_refused`); a pin needs a resolver, and \
             this crate has none on purpose.",
            needle,
            found.join("\n")
        );
    }
}

#[test]
fn the_engine_and_the_model_client_do_not_reach_each_other() {
    // The SSRF-shim question in one sentence: can the app's own request be routed through
    // a page, or a page's fetch be answered by the app's client? No file may name both.
    // `http.rs` is the only code that writes a request to the model; `webview.rs` is the
    // only code that hands an address to the engine.
    let http = text_of("http.rs");
    let named: Vec<String> = http
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("webview"))
        .map(|(index, line)| format!("http.rs:{} — {}", index + 1, line.trim()))
        .collect();
    assert!(
        named.is_empty(),
        "the model client mentions the page engine:\n{}\n\
         fix: the model request is hand-written on a TcpStream to 127.0.0.1 (`http.rs`); a \
         web view in that path would send it wherever a page decided.",
        named.join("\n")
    );

    let engine = text_of("webview.rs");
    for needle in ["crate::http", "http::ask", "endpoint("] {
        let found: Vec<String> = engine
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .map(|(index, line)| format!("webview.rs:{} — {}", index + 1, line.trim()))
            .collect();
        assert!(
            found.is_empty(),
            "the page engine reaches the model client (`{}`):\n{}",
            needle,
            found.join("\n")
        );
    }

    // The gate is applied at every way in, not at one: the address asked for, the
    // navigation a page attempts, and the constructor that builds the view at all.
    let gated = engine.matches("allowed(").count();
    assert!(
        gated >= 3,
        "webview.rs applies `allowed` {} time(s); it is applied to the initial address, to \
         every navigation the page attempts, and to `Page::go` — three places, not one",
        gated
    );
    assert!(
        engine.contains("if !allowed(url)"),
        "the engine's constructors no longer start with `if !allowed(url)`: a page could \
         be built for an address the gate refuses"
    );
    assert!(
        engine.contains("with_navigation_handler"),
        "the navigation handler is gone: a redirect, a `location.href` or a meta refresh \
         would no longer be re-gated after the first load"
    );
}

#[test]
fn the_engine_opens_no_port_and_resolves_no_name() {
    // Rule 3's engine half, said as absence: this crate never listens and never resolves.
    // `wry` builds a child view of *this* window; a view that served a socket, or that
    // looked a name up itself, would be the browser this product refuses to be.
    let engine = text_of("webview.rs");
    let mut found = Vec::new();
    for (index, line) in engine.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        for needle in [
            "TcpListener",
            "UdpSocket",
            "bind(",
            "Command::new",
            "to_socket_addrs",
            "ToSocketAddrs",
            "getaddrinfo",
        ] {
            if code.contains(needle) {
                found.push(format!("webview.rs:{} — {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "the page engine listens, spawns or resolves:\n{}",
        found.join("\n")
    );

    // The engine's decision is a compile-time fact, not a runtime probe for a server.
    let _ = atlas_desktop::webview::engine();
}

#[test]
fn only_the_reader_and_the_shell_may_touch_the_engine() {
    // Rule 2, the call-site half. `webview` is named by: the module declaration, the engine
    // itself, the Reader view (which owns the rectangle), and `app.rs` (the shell that
    // decides which row is open). Any other file naming it is a new way into a page, and a
    // new way needs a decision, not a default.
    let named: Vec<String> = sources()
        .into_iter()
        .filter(|(_, text)| text.contains("webview"))
        .map(|(path, _)| path)
        .collect();
    assert!(
        named.iter().any(|path| path == "views/reader.rs"),
        "the Reader no longer names the engine: the scan is looking at the wrong tree"
    );
    let allowed: &[&str] = &["lib.rs", "webview.rs", "views/reader.rs", "app.rs"];
    let extra: Vec<&String> = named
        .iter()
        .filter(|path| !allowed.contains(&path.as_str()))
        .collect();
    assert!(
        extra.is_empty(),
        "a file outside the Reader and the shell names the page engine: {:?}\n\
         fix: the engine is opened by the shell from a story row (`views::reader` hands the \
         shell the address in the row it already read) and by nothing else. A view that can \
         call `webview::open` is a view that can put a string of its own into a page.",
        extra
    );
}

#[test]
fn a_model_answer_is_text_not_an_address() {
    // Rule 2, the model half. The chat view draws what the model said. It must not own a
    // link, an opener, or a page: the model's answer is untrusted text, and the only way a
    // URL becomes an action in this window is `link::openable` on a *stored* address, or
    // the shell opening the row a reader clicked.
    let chat = text_of("views/chat.rs");
    let mut found = Vec::new();
    for (index, line) in chat.lines().enumerate() {
        for needle in ["webview", "link::openable", "hyperlink", "Page::"] {
            if line.contains(needle) && !line.trim_start().starts_with("//") {
                found.push(format!(
                    "views/chat.rs:{} — {} (`{}`)",
                    index + 1,
                    line.trim(),
                    needle
                ));
            }
        }
    }
    assert!(
        found.is_empty(),
        "the model's answer can become a link or a page:\n{}",
        found.join("\n")
    );

    // Non-vacuity: the link gate exists and is used where a *stored* address is shown, so
    // the absence above is a boundary and not an unused function.
    let users: Vec<String> = sources()
        .into_iter()
        .filter(|(_, text)| text.contains("link::openable"))
        .map(|(path, _)| path)
        .collect();
    assert!(
        users.len() >= 2,
        "`link::openable` is used in {} file(s): {:?} — the stored-address path is gone or \
         the scan is blind",
        users.len(),
        users
    );
}

#[test]
fn no_page_address_is_assembled_in_the_reader_path() {
    // Rule 2, the shape half. A page's address is a stored row's `url` or the model
    // server's constant; it is never glued together from pieces of text, because the piece
    // that comes from a page or a model is the piece an attacker wrote.
    //
    // Measured allowlist for the whole crate, both outside the reader path on purpose:
    // `app.rs` builds the model server's address from a port setting (`http://127.0.0.1:
    // {port}` — one literal host, no name), and every hit *below a file's first
    // `#[cfg(test)]`* is a fixture: `link.rs` names `example.com`, and `app.rs`'s chip tests
    // name `example.invalid`. That cut is sound because this workspace writes its tests last —
    // a top-level item after a test module is itself a finding
    // (`tests/disclosure.rs::the_scan_is_not_blind` and `no_top_level_item_is_hidden`), so
    // "below the first `#[cfg(test)]`" is "inside a test". A hit in shipped code in any other
    // file fails — including in `reader.rs`, `webview.rs` and `chat.rs`.
    let mut offenders = Vec::new();
    for (path, text) in sources() {
        let tests_begin = text
            .lines()
            .position(|line| line.trim() == "#[cfg(test)]")
            .unwrap_or(usize::MAX);
        for (index, line) in text.lines().enumerate() {
            if !line.contains("format!(\"http") {
                continue;
            }
            let model_address = path == "app.rs" && line.contains("127.0.0.1:");
            let fixture = index > tests_begin;
            if !model_address && !fixture {
                offenders.push(format!("{}:{} — {}", path, index + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "an address is assembled from pieces:\n{}\n\
         fix: a page's address is the row's `url`, exactly as stored. A `format!` is where \
         a model's sentence or a page's title becomes part of a destination.",
        offenders.join("\n")
    );

    // The Reader takes the address from the row it was handed, not from anywhere else.
    let reader = text_of("views/reader.rs");
    assert!(
        reader.contains("article.url"),
        "the Reader no longer copies the story's `url` from the archive row"
    );

    // The three files the page is made of, named once: none of them builds an address, so
    // the address a page gets is always one that was already in the archive (or the model
    // server's constant in `app.rs`).
    for file in READER_PATH {
        let text = text_of(file);
        assert!(
            !text.contains("format!(\"http"),
            "src/{file} assembles a page address"
        );
    }
}

#[test]
fn the_scan_reads_the_crate() {
    // The four scan-based rules above pass by *finding nothing*, so the scanner itself is
    // pinned: it reads the tree, it reports line numbers, and a needle that is certainly
    // present is certainly found. Without this, a walk that silently stopped matching
    // would turn every rule into a comment.
    let files = sources();
    assert!(
        files.len() > 10,
        "the walk returned {} file(s)",
        files.len()
    );
    assert!(
        files.iter().any(|(path, _)| path == "webview.rs"),
        "src/webview.rs is missing from the walk"
    );
    let present = hits("pub mod webview");
    assert!(
        present.iter().any(|hit| hit.starts_with("lib.rs:")),
        "a needle that is in the tree was not reported: {:?}",
        present
    );
    let numbered = hits("pub fn allowed");
    assert!(
        numbered
            .iter()
            .any(|hit| hit.starts_with("webview.rs:") && hit.contains(':') && hit.contains('(')),
        "the engine's gate is not visible to the scan: {:?}",
        numbered
    );
}

#[test]
fn loopback_is_a_literal_and_a_name_is_refused() {
    // Rule 3, the client half. This is the rule the *model server* address has, and it is
    // an address rule, not a host list: one literal, one port, and every name refused —
    // including names that resolve to loopback today (`localhost`, `127.0.0.1.nip.io`).
    // A name is a second resolver between the check and the connect, which is exactly the
    // window an SSRF needs.
    let (host, address) = atlas_desktop::http::endpoint("http://127.0.0.1:8081")
        .expect("the literal model address is accepted");
    assert_eq!(host, "127.0.0.1:8081", "the Host header");
    assert_eq!(address.port(), 8081);
    assert!(address.ip().is_loopback(), "the dial is loopback: {address}");

    for (address, why) in [
        ("http://localhost:8081", "a name"),
        ("http://LOCALHOST:8081", "a name, upper-cased"),
        ("http://127.0.0.1.nip.io:8081", "a name that resolves to loopback"),
        ("http://[::1]:8081", "the other loopback spelling"),
        ("http://127.0.0.2:8081", "another address in 127/8"),
        ("https://127.0.0.1:8081", "TLS to a plaintext server"),
    ] {
        let error = atlas_desktop::http::endpoint(address)
            .err()
            .unwrap_or_else(|| panic!("{why} was accepted as a model address: {address}"));
        assert!(
            error.contains("not 127.0.0.1") || error.contains("not an http:// url"),
            "{address} was refused with a sentence about something else: {error}"
        );
        // The sentence never repeats what it refused: the url can hold a user name or a
        // control character, and a refusal that echoed it would print it.
        let value = address.split("//").nth(1).unwrap_or(address);
        assert!(
            !error.contains(value),
            "the refusal echoes the address it refused: {error}"
        );
    }

    // The sentence says the rule, so a reader knows what to write instead.
    let refusal = atlas_desktop::http::endpoint("http://localhost:8081").unwrap_err();
    assert!(
        refusal.contains("literal") && refusal.contains("the model on this machine"),
        "the name refusal does not say the literal rule: {refusal}"
    );
}

#[test]
fn the_engines_rule_is_the_address_shape_and_it_says_so() {
    // Rule 3, the engine half — *stated*, because it is not the client's rule.
    // `webview::allowed` is about the address: `http`/`https` (scheme compared without
    // case), an authority, no credentials, printable ASCII, one line. It does **not**
    // resolve and does not judge the host: loopback is allowed on purpose (the model
    // server is there), and so is `localhost`, because the engine that will do the
    // resolving is the operating system's web view.
    //
    // That is safe *here* for one reason, and it is test
    // `the_engine_and_the_model_client_do_not_reach_each_other`: the app's own requests
    // never go through this engine, so "a page may reach loopback" is a browser doing what
    // a browser does, not the window's client being steered.
    use atlas_desktop::webview::allowed;

    for address in [
        "http://127.0.0.1:8081/",
        "http://127.0.0.1:8081/v1/models",
        "https://example.com/story",
        "HTTP://127.0.0.1:8081/upper",
        "HtTpS://example.com/upper",
        "http://localhost:8081/",
    ] {
        assert!(allowed(address), "the engine refuses an ordinary address: {address}");
    }

    for address in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,<b>x</b>",
        "about:blank",
        "chrome://settings",
        "ftp://example.com/a",
        "http://user:pw@example.com/",
        "http:///no-host",
        "http://example.com/a b",
        "http://example.com/a\nb",
        "",
        "example.com/a",
    ] {
        assert!(
            !allowed(address),
            "the engine would load an address the rule refuses: {address:?}"
        );
    }

    // The length cap — `link::MAX_LENGTH`, 512 bytes, now the first thing `allowed` checks.
    //
    // A stored `url` is text out of `data/atlas.db`, and both gates read the same column: the
    // widget's (`link::openable`, which refuses over 512) and the engine's. Before this, only
    // the widget had a bound, so an address too long to be a link could still be *loaded* —
    // the tighter gate was decorative, and a multi-megabyte string was handed to a parse that
    // is linear in its input. The engine gate must not be the looser reader of the two.
    //
    // The boundary is written as 512 rather than read from the constant (it is `pub(crate)`),
    // and the `openable` assertions are what bind the literal to the crate's own number: move
    // the cap and the boundary fixture stops being the boundary, so this fails instead of
    // passing by luck.
    let prefix = "https://example.com/";
    let longest = format!("{prefix}{}", "a".repeat(512 - prefix.len()));
    assert_eq!(longest.len(), 512, "the fixture is not the boundary: {} bytes", longest.len());
    assert!(allowed(&longest), "512 bytes is still a well-formed address: the cap is short");
    assert_eq!(
        atlas_desktop::link::openable(&longest),
        Some(longest.as_str()),
        "the two gates disagree at the cap: the widget refuses what the engine would load"
    );

    let one_over = format!("{longest}a");
    assert_eq!(one_over.len(), 513, "the second fixture is not one byte over the cap");
    assert!(!allowed(&one_over), "513 bytes is over the cap and must be refused");
    assert_eq!(
        atlas_desktop::link::openable(&one_over),
        None,
        "the two gates disagree one byte over the cap"
    );

    // The residual itself: a "url" far longer than any address. Written as two megabytes so
    // the case is the one that was open, not a near miss, and so a refusal that only checked a
    // small prefix would be caught by the second megabyte rather than by luck.
    let huge = format!("{prefix}{}", "a".repeat(2 * 1024 * 1024));
    assert!(huge.len() > 2_000_000, "the multi-megabyte fixture is {} bytes", huge.len());
    assert!(!allowed(&huge), "the engine would load a multi-megabyte \"url\"");
    assert_eq!(
        atlas_desktop::link::openable(&huge),
        None,
        "the widget would offer a multi-megabyte \"url\""
    );

    // And the cap is structural, not incidental: it is the first thing the function does, so
    // the character scan under it never walks a megabyte. Ordering is pinned by reading the
    // function — a cap applied after the parse answers identically on well-formed input, so no
    // input can tell the two apart; only the cost differs, and the cost is the point. This is
    // the same way `the_page_gate_runs_before_the_engine_is_built` pins gate-before-builder.
    let engine = text_of("webview.rs");
    let body = engine
        .split_once("pub fn allowed(url: &str) -> bool {")
        .map(|(_, rest)| rest)
        .expect("`allowed` is gone: this test pins a function it does not know");
    let cap = body
        .find("url.len() > crate::link::MAX_LENGTH")
        .expect("the length cap is gone from `allowed`");
    let scan = body
        .find("is_ascii_control")
        .expect("the character scan is gone from `allowed`");
    assert!(
        cap < scan,
        "the length cap runs after the character scan (cap at byte {cap}, scan at byte \
         {scan}): an over-long stored \"url\" would be walked before it is refused"
    );
}

#[test]
fn the_page_gate_runs_before_the_engine_is_built() {
    // The gate is only a gate if it comes first. On a build with an engine, a page is a live
    // view that starts loading the moment `build_as_child` returns — so an address checked
    // *after* the builder is an address that has already been fetched. Behavioural half: no
    // view is built for a refused address (`open` answers `None`). Textual half: the `if
    // !allowed(url)` sits above `wry::WebViewBuilder::new()`, and there is one such guard at
    // each way in — the constructor and `Page::go`.
    let engine = text_of("webview.rs");
    let gate = engine
        .find("if !allowed(url)")
        .expect("`open` no longer starts with `if !allowed(url)`");
    let builder = engine
        .find("wry::WebViewBuilder::new()")
        .expect("the builder is gone: this file is pinning a module it does not know");
    assert!(
        gate < builder,
        "the engine is built before the address is checked: the builder is at byte {builder}, \
         the gate at {gate}. A refused address would already be loading."
    );
    assert_eq!(
        engine.matches("if !allowed(url)").count(),
        2,
        "expected exactly two gates in webview.rs — the constructor and `Page::go`; a third \
         is a new way in, and one is a missing gate"
    );

    // The four shapes this window must never load, refused by the gate and by `open`, which
    // is the only function that can make a view.
    for address in [
        "file:///Users/example/news-atlas/data/atlas.db",
        "javascript:alert(document.cookie)",
        "data:text/html,<script>fetch('http://127.0.0.1:8081/')</script>",
        "http://reader:password@example.com/",
    ] {
        assert!(
            !atlas_desktop::webview::allowed(address),
            "the gate would allow {address}"
        );
        assert!(
            atlas_desktop::webview::open(address, egui::Rect::NOTHING).is_none(),
            "a view was built for {address}"
        );
    }
}

#[test]
fn every_navigation_passes_the_same_gate() {
    // A page can navigate without the app: a link, a redirect, a meta refresh, a
    // `location.href` in a script. Each of those reaches the navigation handler, and the
    // handler is `allowed` — the same function the constructor used, so a page can move
    // anywhere inside the rule and nowhere outside it. The three "not a page" surfaces are
    // closed beside it: `window.open`, downloads (wry's default handler answers *yes*), and
    // permissions (macOS grants when no handler is installed).
    let engine = text_of("webview.rs");
    assert!(
        engine.contains(".with_navigation_handler(|next: String| allowed(&next))"),
        "the navigation handler no longer calls `allowed` on the address it is handed"
    );
    assert!(
        engine.contains("NewWindowResponse::Deny"),
        "`window.open` is no longer denied: a second surface with no bounds and no rule"
    );
    assert!(
        engine.contains("with_download_started_handler(deny_download)"),
        "downloads are no longer denied: wry's own handler answers yes to every request"
    );
    assert!(
        engine.contains("with_permission_handler(deny_permission)"),
        "permissions are no longer denied: macOS grants the camera, microphone and location \
         when no handler is installed"
    );

    // What the handler would do with an off-rule link: the same predicate, on the same
    // shapes. `allowed` is the function the closure calls, so this is the handler's answer.
    for link in [
        "file:///etc/passwd",
        "javascript:fetch('http://127.0.0.1:8081/')",
        "data:text/html,x",
        "http://user:pass@example.com/",
        "ftp://example.com/",
        "http://[::1]:8081/",
    ] {
        assert!(
            !atlas_desktop::webview::allowed(link),
            "the navigation handler would follow {link}"
        );
    }
    // …and an ordinary move inside the web is still allowed: the rule is a shape, not a list.
    assert!(atlas_desktop::webview::allowed("https://example.com/next"));
    assert!(atlas_desktop::webview::allowed("http://127.0.0.1:8081/v1/models"));
}

#[test]
fn a_page_has_no_bridge_into_this_process_and_the_window_has_one_socket() {
    // Rule 4 of this file, from the engine's side. The archive (`data/atlas.db`, opened
    // read-only through rusqlite) and the queue (`requests::queue_path`, a SQLite file) are
    // reached by Rust function calls in this process. A page can reach neither unless the
    // app hands it a channel: an IPC handler, a custom protocol, a file:// allowance or a
    // download. There is none of those here — measured absence, crate-wide.
    for needle in [
        "with_ipc",
        "ipc_handler",
        "custom_protocol",
        "register_uri_scheme",
        "with_custom",
        "allow_file",
        "with_file",
        "with_https",
        "with_web_context",
    ] {
        let found = hits(needle);
        assert!(
            found.is_empty(),
            "the engine gained a channel into this process (`{}`):\n{}",
            needle,
            found.join("\n")
        );
    }
    // The download handler is a named function precisely so this can be checked: wry's
    // default answers *yes*, and a page that could write a file has a channel even without
    // IPC.
    let engine = text_of("webview.rs");
    assert!(
        engine.contains("fn deny_download") && engine.matches("deny_download").count() >= 2,
        "the download denial is gone (definition plus the builder that installs it)"
    );

    // And the window's own sockets, over the whole crate: one connect, in `http.rs`; no
    // listener, no name resolution anywhere.
    let mut offenders = Vec::new();
    for (path, text) in sources() {
        for (index, line) in text.lines().enumerate() {
            for needle in [
                "TcpListener",
                "UdpSocket",
                "to_socket_addrs",
                "ToSocketAddrs",
                "getaddrinfo",
                "lookup_host",
            ] {
                if line.contains(needle) {
                    offenders.push(format!("{}:{} — {}", path, index + 1, line.trim()));
                }
            }
            if line.contains("std::net") && path != "http.rs" {
                offenders.push(format!(
                    "{}:{} — {} (the socket API belongs to http.rs alone)",
                    path,
                    index + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a socket or a resolver outside the model client:\n{}",
        offenders.join("\n")
    );
    let connects = hits("TcpStream::connect");
    assert_eq!(
        connects.len(),
        1,
        "the window must have exactly one connect site, not {}:\n{}",
        connects.len(),
        connects.join("\n")
    );
    assert!(
        connects[0].starts_with("http.rs:"),
        "the one connect left the model client: {}",
        connects[0]
    );
}
