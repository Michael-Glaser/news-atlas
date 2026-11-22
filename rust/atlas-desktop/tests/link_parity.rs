//! One rule for "a web address", two callers — pinned so they cannot drift apart.
//!
//! A stored address reaches the window by two roads, and each road has its own gate:
//!
//! * [`atlas_desktop::link::openable`] — *what a page offers to open*: the gate in front of
//!   the "Copy <host>" / "Read the source" affordances. It answers `Some(&str)` with the
//!   borrowed address, or `None` (the caller then shows the address as plain text).
//! * [`atlas_desktop::webview::allowed`] — *what the engine may load*: applied to the
//!   address the reader asks for, to every navigation the page attempts (a redirect, a
//!   `location.href`, a meta refresh), and to `window.open`, which is refused outright.
//!
//! Two functions, two authors, one question. If they disagree about which strings are web
//! addresses, then either the window offers something it will not load, or it loads
//! something it will not show — and the second is how a check and a consumer come to
//! disagree about where a string ends. So the table below states, for 30 addresses, both
//! answers, and the parity test says of every disagreement either *which line of which
//! file explains it* or that it is **unexplained** (printed, pinned, and to be closed).
//!
//! The three documented asymmetries (four rows: the bracketed-IPv6 one has two spellings),
//! each deliberate, all in the same direction — copyable but not loadable, never the reverse:
//!
//! 1. **Uppercase scheme.** `link.rs:44` — "A scheme is only acceptable in lower case:
//!    `HTTP://` is not what a browser would treat as the same thing, and normalising it
//!    here would be inventing a URL." `webview.rs:95` — the scheme is lowered before it is
//!    compared, so `HtTpS:` is `https:`. The widget is strict about the spelling it puts on
//!    a button; the engine follows the engine's parser.
//! 2. **Length is no longer a split.** `link.rs:34` caps an address at 512 bytes so "a
//!    megabyte of `url` cannot become a layout problem or a launcher argument"; the engine
//!    gate now applies the same constant as its first statement (`webview.rs:78`), so a
//!    3 000-char address is neither copyable nor loadable. It *was* a documented split until
//!    the cap moved: the engine accepted any length, which was the one place where the
//!    looser gate was the engine's — the direction this file forbids. The table keeps the
//!    address as a `Split::Agree` row.
//! 3. **Bracketed IPv6.** `webview.rs:55` — "`http://[::1]/` is a legitimate address, but
//!    this parse splits the port on the last colon and would have to grow a bracket rule to
//!    keep the literal whole; refusing is honest here." `link.rs` has no IPv6 rule: copying
//!    an IPv6 address to the clipboard costs nothing.
//! 4. **Surrounding whitespace.** `link.rs:40` trims, so `" https://x/ "` copies as
//!    `https://x/`; `webview.rs:88` **refuses** whitespace rather than trimming it, and says
//!    so: the string there is about to become a navigation, and a byte that is not part of the
//!    address is a byte the engine would have to interpret. Both sides state their rule, so
//!    this split is documented rather than papered over.
//!
//! Two splits that used to be listed here as **unexplained** are now closed, and the rows below
//! say how: a backslash (`webview.rs:88` refuses one anywhere in the address, because browsers
//! read `\` as `/` — so `https://evil.test\.example.com/` would load a host the check did not
//! see — which is the same answer `link.rs` already gave), and the surrounding-whitespace case,
//! which is now a documented split instead of an accidental one. The `Split::Agree` row is the
//! backslash: two gates, one answer.
//!
//! Run it, with the table printed:
//!
//! ```text
//! cargo test --offline --locked -p atlas-desktop --test link_parity -- --nocapture
//! ```
//!
//! No window, no socket, no archive: both gates are pure functions over a string.

use std::collections::HashSet;

use atlas_desktop::link::openable;
use atlas_desktop::webview::allowed;

/// How the two gates are expected to relate on one address.
#[derive(Clone, Copy)]
enum Split {
    /// The two rules must give the same answer. A split here is drift, and a failure.
    Agree,
    /// A split with its reason written down in one of the two files; the string cites it.
    Documented(&'static str),
    /// A split with no rule behind it. Pinned to today's answer and printed as a finding, so
    /// it cannot move unnoticed; the fix is the line named in `why`.
    Unexplained(&'static str),
}

/// One address, both answers, and the relationship between them.
struct Row {
    address: String,
    /// `link::openable` — may the window offer to copy this?
    copy: bool,
    /// `webview::allowed` — may the engine load this?
    load: bool,
    split: Split,
}

fn row(address: impl Into<String>, copy: bool, load: bool, split: Split) -> Row {
    Row { address: address.into(), copy, load, split }
}

/// The thirty addresses: the twenty shapes in the brief (https, http, uppercase
/// scheme, a port, path+query+fragment, IDN, bare host, IP, `file:`, `javascript:`, `data:`,
/// `about:`, `mailto:`, `ftp:`, `user:pass`, empty, 3 000 chars) plus the probes that make
/// the two rules' edges visible.
fn rows() -> Vec<Row> {
    // 3 000 printable ASCII characters: the length cap's case, built rather than written.
    let long = format!("https://example.com/{}", "a".repeat(2980));
    assert_eq!(long.len(), 3000, "the long row is exactly 3 000 bytes");

    vec![
        // -- the ordinary shapes: both gates open ------------------------------------
        row("https://www.example.com/politics/story-1", true, true, Split::Agree),
        row("http://example.org/a", true, true, Split::Agree),
        row("https://example.com:8443/story", true, true, Split::Agree),
        row("https://example.com/path/seg?q=1&r=2#frag", true, true, Split::Agree),
        // IDNA: the punycode form is the one a scraper can store as ASCII.
        row("https://xn--bcher-kva.example/pfad", true, true, Split::Agree),
        // The same name in Unicode is refused by both: no non-ASCII in an address.
        row("https://bücher.example/pfad", false, false, Split::Agree),
        row("https://example.com", true, true, Split::Agree),
        row("https://example.com/", true, true, Split::Agree),
        row("http://192.0.2.10:8081/status", true, true, Split::Agree),
        row("http://127.0.0.1:8081/", true, true, Split::Agree),
        row("https://example.com/#frag?not-a-query", true, true, Split::Agree),
        // Out of range for a TCP port, and refused by neither parser: both answer "digits",
        // and the connect is what fails. Pinned so neither side starts guessing ranges.
        row("http://example.com:99999/x", true, true, Split::Agree),
        // -- every other scheme: both gates shut --------------------------------------
        row("file:///Users/x/.ssh/id_ed25519", false, false, Split::Agree),
        row("javascript:alert(1)", false, false, Split::Agree),
        row("data:text/html,<script>alert(1)</script>", false, false, Split::Agree),
        row("about:blank", false, false, Split::Agree),
        row("mailto:someone@example.com", false, false, Split::Agree),
        row("ftp://example.com/f", false, false, Split::Agree),
        // -- degenerate spellings: both gates shut ------------------------------------
        row("https://user:pass@example.com/", false, false, Split::Agree),
        row("", false, false, Split::Agree),
        // A scheme with one slash, or none: no authority, and no base to resolve against.
        row("http:/x", false, false, Split::Agree),
        row("https:///x", false, false, Split::Agree),
        row("https:/example.com", false, false, Split::Agree),
        // A NUL byte is a control character: not printable, not a URL.
        row("https://example.com/\u{0}x", false, false, Split::Agree),
        // -- the documented asymmetries ------------------------------------------------
        row(
            "HTTPS://example.com/story",
            false,
            true,
            Split::Documented(
                "link.rs:37 refuses a scheme that is not lower case (\"normalising it here \
                 would be inventing a URL\"); webview.rs:95 compares the scheme without case",
            ),
        ),
        row(
            long,
            false,
            false,
            // Expectation changed on purpose: this row used to be a `Documented` split with
            // `load = true`, because only `link::openable` had the 512-byte cap and the engine
            // gate had none. The cap now sits at the top of the engine's gate as well — the
            // first statement of `webview::allowed`, reading `link::MAX_LENGTH` — so a
            // 3 000-byte "url" is refused by both. That is the direction this file's own rule
            // requires (a split may only ever be "copyable but not loadable", never the
            // reverse), and it is why the row is `Agree` now rather than removed: the address
            // is still the case, its second answer is what moved.
            Split::Agree,
        ),
        row(
            "https://[::1]/x",
            true,
            false,
            Split::Documented(
                "webview.rs:55 refuses a bracketed IPv6 literal (the port split on the last \
                 colon would need a bracket rule); link.rs has no IPv6 rule, and a copy costs \
                 nothing",
            ),
        ),
        row(
            "https://[::1]:8081/x",
            true,
            false,
            Split::Documented(
                "same bullet: the brackets are refused before the port is looked at, so the \
                 spelling with a port is refused too",
            ),
        ),
        // -- the two that used to be findings, now closed -------------------------------
        row(
            "https://example.com\\path",
            false,
            false,
            // Both gates refuse it now, so the row is `Agree` — this is the strictest kind, and
            // the reason is written on the engine's side: `webview.rs:88` refuses a backslash
            // anywhere in the address because a browser reads it as `/`, so
            // `https://evil.test\.example.com/` would load a host the check never saw.
            Split::Agree,
        ),
        row(
            " https://example.com/ ",
            true,
            false,
            Split::Documented(
                "link.rs:33 trims, because it is deciding what to put on a button and a button \
                 can show the trimmed spelling; webview.rs:88 refuses whitespace rather than \
                 trimming it, and says so, because its string is about to become a navigation \
                 and a byte that is not part of the address is a byte the engine would have to \
                 interpret",
            ),
        ),
    ]
}

/// A debug-escaped, width-limited cell, so a NUL byte or a 3 000-char address stays on one
/// line (and stays visible: `\u{0}` prints as an escape, not as nothing).
fn cell(address: &str, width: usize) -> String {
    let quoted = format!("{address:?}");
    if quoted.chars().count() <= width {
        return quoted;
    }
    // Clip the address itself and keep the closing quote, so a 3 000-char address reads as
    // one string rather than as an unterminated line. (A clipped address is ASCII in
    // practice, so nothing that needed an escape is lost here.)
    let kept: String = address.chars().take(width.saturating_sub(4)).collect();
    format!("\"{kept}…\"")
}

fn yes_no(answer: bool) -> &'static str {
    if answer { "yes" } else { "no" }
}

/// Rule 1. Every address in the table gets exactly the answer written down for it, in both
/// columns. This is the pin: change either gate and the row that moved fails here.
#[test]
fn the_table_pins_both_columns_for_thirty_addresses() {
    let rows = rows();
    assert_eq!(rows.len(), 30, "the table is thirty addresses");

    let mut seen = HashSet::new();
    for row in &rows {
        assert!(
            seen.insert(row.address.as_str()),
            "the table holds the same address twice: {:?}",
            row.address
        );
        let copy = openable(&row.address);
        assert_eq!(
            copy.is_some(),
            row.copy,
            "openable({:?}) answered {copy:?}, the table says copy={}",
            row.address,
            row.copy
        );
        // `openable` returns the address borrowed and trimmed — never a rebuilt copy
        // ("Returned borrowed, so the caller cannot accidentally keep a modified copy").
        if let Some(got) = copy {
            assert_eq!(got, row.address.trim(), "openable returned a changed address");
            assert_eq!(
                got.as_ptr(),
                row.address.trim().as_ptr(),
                "openable allocated: the answer is not a slice of its input"
            );
        }
        assert_eq!(
            allowed(&row.address),
            row.load,
            "allowed({:?}) disagreed with the table (load={})",
            row.address,
            row.load
        );
    }

    // The table, as the report prints it.
    eprintln!();
    eprintln!("  #  address                                                          copy  load  split");
    eprintln!("  -  ---------------------------------------------------------------  ----  ----  -----");
    for (index, row) in rows.iter().enumerate() {
        let split = match row.split {
            Split::Agree => "—",
            Split::Documented(_) => "documented",
            Split::Unexplained(_) => "UNEXPLAINED",
        };
        eprintln!(
            "  {:>2}  {:<65}  {:<4}  {:<4}  {}",
            index + 1,
            cell(&row.address, 65),
            yes_no(row.copy),
            yes_no(row.load),
            split
        );
    }
    eprintln!(
        "  {} addresses: {} copy, {} load, {} split",
        rows.len(),
        rows.iter().filter(|row| row.copy).count(),
        rows.iter().filter(|row| row.load).count(),
        rows.iter().filter(|row| !matches!(row.split, Split::Agree)).count()
    );
    eprintln!();
}

/// Rule 2. The point of the file: every disagreement is either explained by a line of code
/// or an open finding — and the bookkeeping is live in both directions, so an entry that
/// stops splitting fails here instead of quietly becoming an excuse.
#[test]
fn every_split_is_either_documented_or_named_as_open() {
    let rows = rows();
    let mut documented = 0usize;
    let mut unexplained = Vec::new();
    let mut agreed = 0usize;

    for row in &rows {
        let splits = openable(&row.address).is_some() != allowed(&row.address);
        match (row.split, splits) {
            (Split::Agree, false) => agreed += 1,
            (Split::Agree, true) => panic!(
                "drift: openable and allowed disagree about {:?} (copy={}, load={}) and the \
                 table has no entry for it",
                row.address,
                openable(&row.address).is_some(),
                allowed(&row.address)
            ),
            (Split::Documented(why), true) => {
                documented += 1;
                assert!(!why.is_empty(), "a documented split with no citation");
            }
            (Split::Unexplained(why), true) => {
                assert!(!why.is_empty(), "an open split with no finding");
                unexplained.push(format!("{:?} — {why}", row.address));
            }
            (Split::Documented(_) | Split::Unexplained(_), false) => panic!(
                "the table says the two gates answer differently for {:?}, but they agree \
                 now: remove the entry (openable={:?}, allowed={})",
                row.address,
                openable(&row.address),
                allowed(&row.address)
            ),
        }
    }

    assert_eq!(
        (documented, unexplained.len(), agreed),
        // 4 documented, 0 open, 26 agreed: the length row became an agreement when the engine
        // gate took `link::MAX_LENGTH` (it was the one split where the engine was the looser
        // reader of the two, and this file's rule forbids that direction). The backslash row
        // and the surrounding-whitespace row are as they were. An open finding is a debt; the
        // assertion counts them so the debt cannot grow quietly.
        (4, 0, 26),
        "the table's bookkeeping moved: {documented} documented, {} open, {agreed} agreed",
        unexplained.len()
    );

    if !unexplained.is_empty() {
        eprintln!("\nunexplained splits (reported, not blessed):");
        for finding in &unexplained {
            eprintln!("  - {finding}");
        }
    }
}

/// Rule 3, beyond the table: for every scheme that is not `http`/`https`, both gates shut —
/// whatever the spelling after the colon. This is the rule the two functions exist to share,
/// so it is checked as a rule, not as twenty strings.
#[test]
fn the_scheme_rule_is_one_rule() {
    let schemes = [
        "file", "javascript", "data", "about", "mailto", "ftp", "ws", "wss", "chrome",
        "vbscript", "blob", "tel", "gopher", "smb",
    ];
    let mut checked = 0usize;
    for scheme in schemes {
        for address in [
            format!("{scheme}://example.com/x"),
            format!("{scheme}:payload"),
            format!("{scheme}://user:pw@example.com/x"),
        ] {
            assert_eq!(
                openable(&address),
                None,
                "the widget would offer {address:?}"
            );
            assert!(
                !allowed(&address),
                "the engine would load {address:?}"
            );
            checked += 1;
        }
    }
    eprintln!("\n  {checked} non-http(s) addresses: 0 offered, 0 loadable");
}

/// Rule 4, beyond the table: credentials and empty authorities are refused by both, in every
/// spelling either gate tries to parse. The `host@` form is the interesting one — it is how
/// a reader is made to believe the host is the part after the `@`.
#[test]
fn credentials_and_empty_authorities_are_refused_by_both() {
    let hostile = [
        "https://user:pass@example.com/",
        "https://user@example.com/",
        "https://@example.com/",
        "https://example.com@127.0.0.1/",
        "https://user:pw@example.com:8443/x",
        "https://",
        "http://",
        "https:///x",
        "http:///x",
        "https:///",
        "//example.com/x",
        "http:",
        "https:",
        "https:/example.com",
        "https:example.com",
        "http:\\example.com",
    ];
    for address in hostile {
        assert_eq!(openable(address), None, "the widget would offer {address:?}");
        assert!(!allowed(address), "the engine would load {address:?}");
    }
    eprintln!(
        "\n  {} credential/empty-authority spellings: 0 offered, 0 loadable",
        hostile.len()
    );
}

/// Rule 5. Degenerate input is input. Neither gate may panic (a panic in the window is not
/// a refusal), and `openable` may only ever answer with a slice of the string it was given —
/// including for 100 kB, 10 000 labels, an RTL override and a NUL.
#[test]
fn neither_rule_panics_on_a_hostile_string() {
    let hostile = vec![
        "a".repeat(100_000),
        format!("https://{}", "/".repeat(10_000)),
        "https://example.com/".to_string() + &"\u{0}".repeat(64),
        format!("https://{}.example/x", "a".repeat(253)),
        format!("https://{}.example/x", "a".repeat(254)),
        "https://example.com/%00".to_string(),
        format!("https://{}", "é".repeat(1_000)),
        "https://example.com/\u{202E}gnp.exe".to_string(),
        "http://example.com:999999999999999999999999999999/x".to_string(),
        "https://example.com/\u{200B}".to_string(),
        format!("https://{}@example.com/", "u".repeat(500)),
        "https://\u{FEFF}example.com/".to_string(),
    ];
    for address in &hostile {
        let copy = openable(address);
        if let Some(got) = copy {
            // A slice of `address`, or of `address.trim()` — never a fresh allocation and
            // never a longer string than the input.
            let base = address.as_ptr() as usize;
            let inside = (got.as_ptr() as usize) >= base
                && (got.as_ptr() as usize) + got.len() <= base + address.len();
            assert!(inside, "openable returned memory outside its input for {address:?}");
        }
        let load = allowed(address);
        // Both answers are just answers; the assertion is that we got here at all.
        let _ = (copy.is_some(), load);
    }
    eprintln!("\n  {} hostile strings: no panic from either gate", hostile.len());
}

/// Rule 6. Breadth, not just the table: every ordinary combination of the shapes a scraped
/// article URL actually takes gets the same answer from both gates.
#[test]
fn ordinary_addresses_agree_in_bulk() {
    let schemes = ["http", "https"];
    let hosts = [
        "example.com",
        "www.example.org",
        "news.example.net",
        "127.0.0.1",
        "192.0.2.10",
        "localhost",
        "a.b.c.example",
    ];
    let ports = ["", ":80", ":8080"];
    let paths = ["", "/", "/a/b?c=d#e", "/story/2026/11?id=7"];

    let mut checked = 0usize;
    for scheme in schemes {
        for host in hosts {
            for port in ports {
                for path in paths {
                    let address = format!("{scheme}://{host}{port}{path}");
                    assert_eq!(
                        openable(&address),
                        Some(address.as_str()),
                        "the widget refuses an ordinary address {address:?}"
                    );
                    assert!(
                        allowed(&address),
                        "the engine refuses an ordinary address {address:?}"
                    );
                    checked += 1;
                }
            }
        }
    }
    eprintln!(
        "\n  {checked} ordinary addresses: both gates open on every one"
    );
}
