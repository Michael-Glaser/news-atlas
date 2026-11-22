//! The click-through unwrap, against hostile feeds — SSRF review fixtures.
//!
//! `atlas-ingest/src/search.rs` unwraps the address an engine puts inside a
//! click-through link (`unwrap_click_through`), so the model is handed the publisher's
//! URL instead of a redirect shell. That unwrap turns a **feed field** into the address
//! the model will ask `read_article` to open, so it is an SSRF surface: whoever can get
//! an item into the feed (the engine, or anyone who gets an item into it) chooses what
//! comes out of `url=`.
//!
//! This file asks four questions, each with the fixture that would answer "no":
//!
//! 1. **[`a_wrapped_private_target_is_kept_inside_its_wrapper`]** — can the unwrapped
//!    value be forced to a loopback / private / link-local / metadata address, or to a
//!    non-`http(s)` scheme? Every hostile payload listed there must keep its wrapper.
//! 2. **[`the_fallback_when_the_gate_refuses_is_the_wrapper_and_it_is_public`]** — when
//!    the value is refused, what does the model get instead? The fallback is the
//!    wrapper URL, which is the engine's own public host, so the refused address is
//!    never handed out and the item is not silently dropped.
//! 3. **[`a_second_url_parameter_cannot_win_the_unwrap`]** — a feed item that carries
//!    more than one `url=` (before, after, upper-case, entity-escaped) must not be able
//!    to make a private value win.
//! 4. **[`read_article_gates_before_any_request_and_never_dials_a_private_target`]** and
//!    [`web_open_gates_before_the_browser_sees_the_url`]** — the read path re-gates the
//!    final URL, so an unwrap that went wrong still cannot become a request. Measured
//!    with a fetcher and a browser that record every call: zero calls for a refused URL.
//!
//! Two pipeline tests pin what the *model* is actually handed. Both tools now judge a
//! link with **one** predicate (`search::acceptable`): a link the gate refuses — plainly,
//! or hidden behind a click-through wrapper — is dropped and counted, never handed out
//! and never silently vanishing. When this file was written `web_search` had no filter
//! at all and returned a plain `http://127.0.0.1:8081/…` item as a result; the test that
//! pinned that asymmetry was rewritten to the stronger rule when the filter landed (see
//! [`the_plain_links_a_feed_can_carry_are_refused_by_both_tools`]).
//!
//! Everything runs offline: the fetcher serves one canned feed and records the URL it
//! was asked for, the browser records and refuses, the store is a temporary file, and
//! the rules engines answer in this process.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use atlas_ingest::fetcher::{BrowserPage, BrowserReader, Fetched, Fetcher};
use atlas_ingest::model::OfflineModel;
use atlas_ingest::rules::NativeRules;
use atlas_ingest::search::{encode_component, parse_feed, unwrap_click_through, Engine};
use atlas_ingest::{gate_url, host_of, Config, Pipeline};
use atlas_store::Store;

// ------------------------------------------------------------------------ fixtures

/// The shape Bing News puts in `<link>`, measured: a query string with the publisher
/// URL percent-encoded inside `url=`.
fn wrapper(payload: &str) -> String {
    format!(
        "https://www.bing.com/news/apiclick.aspx?ref=FexRss&aid=&url={}&c=1",
        encode_component(payload)
    )
}

/// A wrapper built from a raw (already-encoded) query string, for the smuggling cases.
fn wrapper_raw(query: &str) -> String {
    format!("https://www.bing.com/news/apiclick.aspx?{query}")
}

/// The addresses and schemes a search hit must never turn into.
///
/// Each is a real SSRF target: the app's own server and model port, the loopback and
/// metadata services, the RFC 1918 ranges, and the schemes a browser would treat as a
/// file or a script.
const PRIVATE_PAYLOADS: &[&str] = &[
    "http://127.0.0.1:8081/api/status",
    "http://127.0.0.1/",
    "http://[::1]:8081/api/status",
    "http://0.0.0.0:8787/",
    "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
    "http://[fe80::1]/",
    "http://10.0.0.5/admin",
    "http://192.168.1.10/",
    "http://172.16.4.4/",
    "http://2130706433/",
    "http://0x7f000001/",
    "http://017700000001/",
    "http://127.1/",
    "http://localhost:8081/api/status",
    "http://LOCALHOST/",
    "http://user:secret@127.0.0.1:8081/",
    "http://127.0.0.1:8081@evil.example/",
    // Not a web address at all: the unwrap must not hand these out either.
    "file:///etc/passwd",
    "file://localhost/etc/passwd",
    "//169.254.169.254/latest/meta-data/",
    "javascript:fetch('http://127.0.0.1:8081/')",
    "data:text/html,<script>alert(1)</script>",
    "gopher://127.0.0.1:6379/_INFO",
    "ftp://127.0.0.1/",
    // IPv6 spellings of loopback and of a mapped IPv4, and a host that tries to hide
    // digits in percent-encoding (the gate sees the literal `%` and must refuse it).
    "http://[::ffff:127.0.0.1]:8081/",
    "http://[0:0:0:0:0:0:0:1]:8081/",
    "http://%31%32%37.0.0.1:8081/",
];

/// Targets that carry a control character, where the *cleaning* is the behaviour under
/// test rather than the refusal.
///
/// Measured: `gate_url` strips control characters before it judges a URL
/// (`crate::gate::clean_value`), so `http://127.0.0.1\0.evil.com/` is judged as the
/// public *name* `127.0.0.1.evil.com` — not as this machine, because a name's address is
/// whatever DNS answers and `atlas-net`'s resolve gate refuses a private answer at dial
/// time. The two things that must hold are narrower and checkable: what reaches the
/// transport is the string the gate judged (no NUL, no ESC), and it is not a private
/// address. That is why these are not in [`PRIVATE_PAYLOADS`].
const CONTROL_PAYLOADS: &[&str] = &[
    "http://127.0.0.1\u{0}.evil.com/",
    "http://evil.com\u{0}@127.0.0.1/",
    "http://127.0.0.1:8081/\u{1b}[2J",
    "http://127.0.0.1\u{0}/api/status",
];

/// True when `url`'s host is this machine, a private range, or a metadata address.
///
/// Written without a DNS lookup: it parses the host as an IP literal and recognises the
/// loopback names and the decimal/hex/octal spellings of 127.0.0.1 that a URL parser
/// lets through as *names*. A public hostname that merely starts with a loopback literal
/// (`127.0.0.1.evil.com`) is not private — it is a name, its address is the one DNS
/// answers, and `atlas-net`'s resolve gate is what refuses a private answer at dial time
/// (`atlas-net/src/fetch.rs`, `adopt`/`resolve_gate_with`).
fn private_host(url: &str) -> bool {
    let host = host_of(url);
    let host = host.trim_matches(|ch| ch == '[' || ch == ']');
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if let Ok(v4) = Ipv4Addr::from_str(host) {
        return v4.is_loopback()
            || v4.is_private()
            || v4.is_link_local()
            || v4.is_unspecified()
            || v4.is_broadcast();
    }
    if let Ok(v6) = Ipv6Addr::from_str(host) {
        return v6.is_loopback()
            || v6.is_unspecified()
            || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local
            || (v6.segments()[0] & 0xfe00) == 0xfc00; // unique-local
    }
    // The spellings a resolver turns into 127.0.0.1 but `Ipv4Addr` refuses.
    matches!(
        host,
        "2130706433" | "0x7f000001" | "0x7f.0.0.1" | "017700000001" | "127.1" | "127.0.1"
    )
}

/// A fetcher that answers with one canned body and remembers what it was asked for.
///
/// `calls` is the list of URLs, so a test can assert that a refused URL never became a
/// request rather than merely that the result says "refused".
struct FeedFetcher {
    body: String,
    calls: Mutex<Vec<String>>,
}

impl FeedFetcher {
    fn new(body: &str) -> FeedFetcher {
        FeedFetcher {
            body: body.to_string(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().map(|held| held.clone()).unwrap_or_default()
    }
}

impl Fetcher for FeedFetcher {
    fn fetch(&self, url: &str, _max_bytes: usize) -> Fetched {
        if let Ok(mut held) = self.calls.lock() {
            held.push(url.to_string());
        }
        Fetched {
            status: 200,
            final_url: url.to_string(),
            content_type: "application/rss+xml".to_string(),
            body: self.body.clone(),
            ..Fetched::default()
        }
    }
}

/// A browser that records and refuses: anything it is asked to open is a finding.
struct RecordingBrowser {
    calls: Mutex<Vec<String>>,
}

impl RecordingBrowser {
    fn new() -> RecordingBrowser {
        RecordingBrowser {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().map(|held| held.clone()).unwrap_or_default()
    }
}

impl BrowserReader for RecordingBrowser {
    fn read(&self, url: &str, _max_chars: usize) -> BrowserPage {
        if let Ok(mut held) = self.calls.lock() {
            held.push(url.to_string());
        }
        BrowserPage {
            url: url.to_string(),
            error: "the recording browser opens nothing".to_string(),
            ..BrowserPage::default()
        }
    }
}

/// One temporary archive, one pipeline, removed on drop.
struct Fixture {
    pipeline: Pipeline,
    fetcher: Arc<FeedFetcher>,
    browser: Arc<RecordingBrowser>,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn temp_root(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-ingest-unwrap-{}-{}-{}",
        label,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    root
}

/// A pipeline whose engine is the built-in default (nothing configured) and whose
/// transport is `body` for every URL. `ATLAS_SEARCH_ENGINE` is cleared for the life of
/// the fixture so a developer's shell cannot change what these tests measure.
fn fixture(label: &str, body: &str) -> Fixture {
    let root = temp_root(label);
    let store = Arc::new(Store::open(root.join("data").join("atlas.db")).expect("open"));
    store.init().expect("schema");
    store.init_monitors().expect("monitor tables, as the app makes them");
    let mut config = Config::for_root(&root);
    config.search_engine = None;
    let rules = NativeRules::discover(&config).expect("the engines answer in this process");
    let fetcher = Arc::new(FeedFetcher::new(body));
    let browser = Arc::new(RecordingBrowser::new());
    let pipeline = Pipeline::new(
        store,
        config,
        Arc::new(rules),
        Arc::new(OfflineModel::new("no model in this test")),
        fetcher.clone(),
        browser.clone(),
        None,
    );
    Fixture {
        pipeline,
        fetcher,
        browser,
        root,
    }
}

/// An RSS document with one item per `(title, link)`, in order.
fn feed(items: &[(&str, String)]) -> String {
    let mut out = String::from("<?xml version=\"1.0\"?>\n<rss version=\"2.0\"><channel>\n");
    out.push_str("<title>Bing: news</title>\n");
    for (title, link) in items {
        out.push_str(&format!(
            "<item><title>{title}</title><link>{link}</link>\
             <description>a result</description></item>\n"
        ));
    }
    out.push_str("</channel></rss>\n");
    out
}

/// Every `url` in a tool result's `results`, as the model receives it.
fn result_urls(payload: &serde_json::Value) -> Vec<String> {
    payload["results"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["url"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// -------------------------------------------------------- (a) the payload is gated

#[test]
fn a_wrapped_private_target_is_kept_inside_its_wrapper() {
    // The unwrap's contract: a value that decodes to an http(s) URL is the hit's
    // address, *if* the gate accepts it and it is not a blocked host. A private target
    // must therefore keep its wrapper — handing it out would be handing the model a
    // target with this module's blessing.
    for payload in PRIVATE_PAYLOADS {
        let link = wrapper(payload);
        let out = unwrap_click_through(&link);
        assert!(
            !private_host(&out),
            "the unwrap turned a wrapper into a private target: payload {payload:?} \
             became {out:?} (raw: {link})\n\
             Rule: `unwrap_click_through` — the gate judges the URL the model will read, \
             and a refused value keeps its wrapper"
        );
        // A control character (NUL, ESC) is the classic "the string the gate judged is not
        // the string the next layer reads" trick: nothing handed to the model may carry
        // one, whatever the gate made of it.
        assert!(
            !out.chars().any(|ch| ch.is_control()),
            "the unwrap produced a URL with a control character: payload {payload:?} \
             became {out:?}"
        );
        assert_eq!(
            out, link,
            "payload {payload:?} was rewritten instead of kept: {out:?} (raw: {link})"
        );
        // And what is left is an address the gate accepts: the wrapper is the engine's.
        assert!(
            !gate_url(&out).is_empty(),
            "the fallback for {payload:?} is not a URL the gate accepts: {out:?}"
        );
        assert_eq!(
            host_of(&out),
            "www.bing.com",
            "the fallback for {payload:?} is not the wrapper's host: {out:?}"
        );
    }
    // Control characters: either the value is refused like the ones above, or it is
    // handed over *clean* — the same string the gate judged, with nothing a transport
    // would truncate at.
    for payload in CONTROL_PAYLOADS {
        let link = wrapper(payload);
        let out = unwrap_click_through(&link);
        if out != link {
            assert!(
                !out.chars().any(|ch| ch.is_control()),
                "the unwrap handed over a URL with a control character: payload \
                 {payload:?} became {out:?}"
            );
            assert!(
                !private_host(&out) && !gate_url(&out).is_empty(),
                "the unwrap handed over a URL the gate refuses: payload {payload:?} became \
                 {out:?}"
            );
        }
    }
}

#[test]
fn a_publisher_address_inside_the_wrapper_is_the_hit() {
    // The positive half: a public publisher URL *is* unwrapped, or the feature does not
    // exist and the test above would pass by doing nothing.
    for (payload, expect) in [
        ("https://example.com/story", "https://example.com/story"),
        ("https://example.com/a?b=1&c=2", "https://example.com/a?b=1&c=2"),
        ("http://example.com/plain", "http://example.com/plain"),
        (
            "https://www.reuters.com/technology/chips",
            "https://www.reuters.com/technology/chips",
        ),
        // A *name* that begins with a loopback literal is a public name: the gate judges
        // addresses, and the address is the one DNS answers — refused at dial time by
        // `atlas-net`'s resolve gate if it is private (`fetch.rs`, `adopt`).
        ("https://127.0.0.1.evil.com/x", "https://127.0.0.1.evil.com/x"),
    ] {
        assert_eq!(
            unwrap_click_through(&wrapper(payload)),
            expect,
            "a public publisher URL was not unwrapped (payload {payload:?})"
        );
    }
    // A link with no query string is not a wrapper and passes through untouched.
    for plain in ["https://example.com/a", "http://127.0.0.1:8081/api/status"] {
        assert_eq!(unwrap_click_through(plain), plain, "{plain}");
    }
}

#[test]
fn the_fallback_when_the_gate_refuses_is_the_wrapper_and_it_is_public() {
    // Question (b), the second half: what replaces a refused value. Measured: the
    // wrapper. That is the safe fallback because it is the engine's own public host —
    // the refused address is never handed out, and the item is not silently dropped
    // (a drop would let a feed hide a story by wrapping it in a private target).
    let hits = parse_feed(&feed(&[
        ("private", wrapper("http://127.0.0.1:8081/api/status")),
        ("metadata", wrapper("http://169.254.169.254/latest/meta-data/")),
        ("public", wrapper("https://example.com/story")),
    ]));
    assert_eq!(hits.len(), 3, "the reader dropped items: {hits:?}");
    assert_eq!(host_of(&hits[0].url), "www.bing.com", "{}", hits[0].url);
    assert_eq!(host_of(&hits[1].url), "www.bing.com", "{}", hits[1].url);
    assert_eq!(hits[2].url, "https://example.com/story");
    for hit in &hits {
        assert!(!private_host(&hit.url), "{}", hit.url);
        assert!(!gate_url(&hit.url).is_empty(), "{}", hit.url);
    }
}

// ------------------------------------------- (c) a second `url=` cannot win the unwrap

#[test]
fn a_second_url_parameter_cannot_win_the_unwrap() {
    // A feed item chooses its own query string, so it can repeat `url=`. Whatever the
    // reader does with the duplicate — take the first, take the last, take none — the
    // value handed to the model must not be private. Measured: the first parameter that
    // decodes to an http(s) URL is judged, and a refused one returns the wrapper, so a
    // good first value wins and a private first value blocks the rest.
    let private = encode_component("http://127.0.0.1:8081/api/status");
    let public = encode_component("https://example.com/story");
    let cases = [
        format!("url={private}&url={public}"),
        format!("url={public}&url={private}"),
        format!("a=1&url={private}&url={public}"),
        format!("url={private}&a=1&url={public}"),
        // entity-escaped separator, as an XML feed writes it
        format!("x=1&amp;url={private}&amp;url={public}"),
        // upper-case parameter name: a different parameter, so it is not the wrapper's
        format!("URL={private}&url={public}"),
        // a private value behind a non-URL value
        format!("url=not-a-url&url={private}"),
        // empty value first
        format!("url=&url={private}"),
        // a value that decodes to a private target only after one more decode
        format!("url={}", encode_component(&format!("%38%30%38%31"))),
    ];
    for query in &cases {
        let link = wrapper_raw(query);
        let out = unwrap_click_through(&link);
        assert!(
            !private_host(&out),
            "a duplicate parameter smuggled a private target out: query {query:?} \
             became {out:?}"
        );
        assert!(
            !gate_url(&out).is_empty(),
            "the unwrap produced a URL the gate refuses: {query:?} -> {out:?}"
        );
    }
    // The documented direction: the first acceptable value wins, and only that one.
    assert_eq!(
        unwrap_click_through(&wrapper_raw(&format!("url={public}&url={private}"))),
        "https://example.com/story"
    );
    // A private value first is not skipped in favour of the good one behind it: the
    // wrapper is kept, which is the conservative answer (the good value is not lost —
    // the model can still open the wrapper).
    assert_eq!(
        unwrap_click_through(&wrapper_raw(&format!("url={private}&url={public}"))),
        wrapper_raw(&format!("url={private}&url={public}"))
    );
}

#[test]
fn a_wrapper_that_is_not_a_wrapper_is_left_alone() {
    // A link whose query has no `url=` (or whose `url=` is not a web address) is the
    // engine's own link and must not be rewritten — otherwise every search result with
    // a `?` would be replaced by something the feed chose.
    for link in [
        "https://news.example/a?utm_source=bing&utm_medium=rss",
        "https://news.example/a?url=",
        "https://news.example/a?url=not-a-url",
        "https://news.example/a?url=%2F%2F169.254.169.254%2F",
        "https://news.example/a#fragment",
    ] {
        assert_eq!(unwrap_click_through(link), link, "rewritten: {link}");
    }
}

// ------------------------------------------------- (d) the read path re-gates the URL

#[test]
fn read_article_gates_before_any_request_and_never_dials_a_private_target() {
    // The last line of defence, and the one that does not depend on the unwrap being
    // right: `read_article` gates the URL the model passed and returns a sentence
    // instead of fetching. Measured with a fetcher that records every call — a refused
    // URL must produce *zero* calls, or "refused" would only mean the answer was
    // rewritten after the request went out.
    let fixture = fixture("read", &feed(&[]));
    for url in PRIVATE_PAYLOADS
        .iter()
        .copied()
        .chain(["", "   ", "not a url", "//127.0.0.1:8081/"])
    {
        let result = fixture.pipeline.read_article(url, "a title");
        assert!(!result.ok, "read_article opened {url:?}");
        assert_eq!(
            result.reason, "refused",
            "read_article refused {url:?} with reason {:?} instead of a gate refusal",
            result.reason
        );
        assert!(
            !result.error.trim().is_empty(),
            "read_article refused {url:?} with no sentence for the model"
        );
    }
    assert_eq!(
        fixture.fetcher.calls().len(),
        0,
        "a refused URL was fetched anyway: {:?}",
        fixture.fetcher.calls()
    );
    // The control-character payloads may be *cleaned* rather than refused: what reaches
    // the transport must then be the string the gate judged, with no control character
    // left in it. Measured: `gate_url` strips them (`crate::gate::clean_value`), so
    // `http://127.0.0.1\0.evil.com/` becomes the public *name* `127.0.0.1.evil.com` and
    // the read goes out to it — which is why the assertion is "clean and public", not
    // "refused". When nothing was fetched, the reason must be the gate's refusal.
    for url in CONTROL_PAYLOADS {
        let before = fixture.fetcher.calls().len();
        let result = fixture.pipeline.read_article(url, "a title");
        let calls = fixture.fetcher.calls();
        let new_calls = &calls[before..];
        for call in new_calls {
            assert!(
                !call.chars().any(|ch| ch.is_control()),
                "a URL with a control character reached the transport: {call:?} (asked {url:?})"
            );
            assert!(
                !private_host(call),
                "a private address reached the transport: {call:?} (asked {url:?})"
            );
        }
        if new_calls.is_empty() {
            assert!(!result.ok, "{url:?} read ok without a request");
            assert_eq!(result.reason, "refused", "{url:?}: {:?}", result.reason);
        }
    }
}

#[test]
fn web_open_gates_before_the_browser_sees_the_url() {
    // The other transport (`web_open`): the browser is the widest surface in the app —
    // it has a profile, cookies and a debug port — so a refused URL must not reach it.
    let fixture = fixture("open", &feed(&[]));
    for url in PRIVATE_PAYLOADS.iter().copied() {
        let page = fixture.pipeline.open_page(url, 2_000);
        assert!(
            !page.error.is_empty(),
            "the browser was asked to open {url:?}: {page:?}"
        );
        assert!(
            page.text.is_empty(),
            "the browser answered for {url:?}: {page:?}"
        );
    }
    assert_eq!(
        fixture.browser.calls().len(),
        0,
        "a refused URL reached the browser: {:?}",
        fixture.browser.calls()
    );
    // The control-character payloads may be cleaned instead of refused; then what the
    // browser is asked to open must be clean and public, never the NUL-bearing string.
    for url in CONTROL_PAYLOADS {
        let before = fixture.browser.calls().len();
        let page = fixture.pipeline.open_page(url, 2_000);
        let calls = fixture.browser.calls();
        for call in &calls[before..] {
            assert!(
                !call.chars().any(|ch| ch.is_control()),
                "a URL with a control character reached the browser: {call:?} (asked {url:?})"
            );
            assert!(
                !private_host(call),
                "a private address reached the browser: {call:?} (asked {url:?})"
            );
        }
        if calls.len() == before {
            assert!(
                !page.error.is_empty(),
                "the browser was not called for {url:?} and nothing said why"
            );
        }
    }
}

// ---------------------------------------------------------- what the model is handed

#[test]
fn the_model_is_never_handed_a_private_address_by_either_tool() {
    // End to end: one hostile feed, both tools. Every URL in the result is judged with
    // the same rule the read path uses, and the search engine is the only URL fetched.
    //
    // The three *wrapped* targets are what this test is about: they must keep their
    // wrapper (the engine's public host) rather than becoming the private address inside
    // them. The two *plain* private links are links the feed carries directly; both tools
    // refuse them now (one predicate, `search::acceptable`), which is what
    // [`the_plain_links_a_feed_can_carry_are_refused_by_both_tools`] pins.
    let plain_loopback = "http://127.0.0.1:8081/api/status".to_string();
    let plain_private = "http://10.0.0.5/admin".to_string();
    let body = feed(&[
        ("loopback", wrapper("http://127.0.0.1:8081/api/status")),
        ("metadata", wrapper("http://169.254.169.254/latest/meta-data/")),
        ("file", wrapper("file:///etc/passwd")),
        ("scheme-relative", wrapper("//169.254.169.254/latest/meta-data/")),
        ("plain loopback", plain_loopback.clone()),
        ("plain private", plain_private.clone()),
        ("good", wrapper("https://example.com/story")),
    ]);
    let fixture = fixture("model", &body);

    let searched = fixture.pipeline.web_search("chips", 20);
    let found = fixture.pipeline.find_news("chips", 72, 20);

    // `find_news` judges every link, wrapped or plain: nothing private may appear.
    for url in result_urls(&found) {
        assert!(
            !private_host(&url),
            "find_news handed the model a private address: {url:?}\n{found}"
        );
        assert!(
            !gate_url(&url).is_empty(),
            "find_news returned a URL its own gate refuses: {url:?}\n{found}"
        );
    }
    // `web_search` judges every link too: nothing it returns — wrapped, unwrapped or
    // plain out of the feed — may be an address the gate refuses. No exemption for a
    // link that "came straight out of the feed": measured before the filter landed, a
    // plain `http://127.0.0.1:8081/api/status` item was handed to the model as a search
    // result, and the model's next move is `read_article` on it.
    for url in result_urls(&searched) {
        assert!(
            !private_host(&url),
            "web_search handed the model a private address: {url:?}\n{searched}"
        );
        assert!(
            !gate_url(&url).is_empty(),
            "web_search returned a URL its own gate refuses: {url:?}\n{searched}"
        );
    }
    // `find_news` drops a refused link and says how many. At least the two plain ones are
    // counted; the wrapped hostile items are counted too now (their hidden destination is
    // refused), which is why this is a lower bound rather than an exact number — pinning
    // the exact total here would break the day the predicate learns one more wrapper
    // shape, which is the predicate's business, not this test's.
    let refused = found["refused_links"].as_u64().unwrap_or(0);
    assert!(
        refused >= 2,
        "find_news did not report the two plain refused links: {found}"
    );
    // The good result survived both tools, unwrapped: the filter is not a blanket drop.
    assert!(
        result_urls(&found).iter().any(|url| url == "https://example.com/story"),
        "find_news lost the public result: {found}"
    );
    // The only URL fetched was the engine's own: the feed is served, nothing else is.
    assert_eq!(
        fixture.fetcher.calls().len(),
        2,
        "the tools fetched more than the engine URL: {:?}",
        fixture.fetcher.calls()
    );
    for call in fixture.fetcher.calls() {
        assert!(
            !private_host(&call),
            "a tool fetched a private address: {call:?}"
        );
    }
}

#[test]
fn the_plain_links_a_feed_can_carry_are_refused_by_both_tools() {
    // This test used to pin the opposite, and the change is deliberate — do not "restore"
    // it. Measured before `search::acceptable` existed: a feed item whose `<link>` is
    // *already* a private address (no wrapper to unwrap) reached `web_search`'s result
    // list unchanged, because `web_search` did not gate (it returned
    // `{"count":1,"results":[{"url":"http://127.0.0.1:8081/api/status",…}]}`), while
    // `find_news` dropped and counted it. That asymmetry was this review's one open
    // finding: not an SSRF (the read path refuses the URL anyway) but the model was shown
    // this machine's address as a search result and told to open its 2–4 most relevant
    // hits. `web_search` now applies the same predicate as `find_news`, so the rule is
    // pinned for both tools in its strong form.
    let body = feed(&[(
        "plain loopback",
        "http://127.0.0.1:8081/api/status".to_string(),
    )]);
    let fixture = fixture("plain", &body);
    let searched = fixture.pipeline.web_search("chips", 20);
    let found = fixture.pipeline.find_news("chips", 72, 20);

    assert!(
        result_urls(&searched).is_empty(),
        "web_search returned a link the gate refuses: {searched}"
    );
    assert_eq!(
        searched["refused_links"].as_u64(),
        Some(1),
        "web_search dropped the refused link without counting it: {searched}"
    );
    assert!(
        result_urls(&found).is_empty(),
        "find_news returned a refused link: {found}"
    );
    assert_eq!(
        found["refused_links"].as_u64(),
        Some(1),
        "find_news dropped the refused link without counting it: {found}"
    );
    // The empty result is not a silent nothing: both tools say why there is nothing.
    assert!(
        searched["note"].as_str().is_some_and(|note| !note.is_empty()),
        "web_search returned an empty list without a note: {searched}"
    );
    assert!(
        found["note"].as_str().is_some_and(|note| !note.is_empty()),
        "find_news returned an empty list without a note: {found}"
    );
}

// ------------------------------------------------- what a log line may carry

#[test]
fn the_engines_debug_prints_the_host_and_never_the_template() {
    // `Engine` used to derive `Debug`, which prints the whole template — the endpoint
    // *plus* whatever the reader put in it (a market, a language, a key). One `{:?}` in a
    // log line or an error message was then enough to write the reader's key to disk. The
    // hand-written impl (`atlas-ingest/src/search.rs:100`) prints `host` and `source` only;
    // this pins that, because a derived `Debug` would still compile and still look right.
    let secret = "TOPSECRET-9000";
    let template = format!("https://search.mojeek.com/search?q={{query}}&token={secret}");
    let engine = Engine::new(&template).expect("a public template with a query slot");
    let printed = format!("{engine:?}");

    assert!(
        printed.contains("search.mojeek.com"),
        "the debug output does not name the host at all: {printed}"
    );
    for forbidden in [secret, "{query}", "/search?", "https://"] {
        assert!(
            !printed.contains(forbidden),
            "the engine's debug output carries {forbidden:?}: {printed}"
        );
    }

    // A template with a credential in its userinfo part never becomes an `Engine` at all,
    // so there is no `{:?}` that could print it: the gate refuses it first.
    assert!(
        Engine::new("https://user:hunter2@news.ycombinator.com/s?q={query}").is_none(),
        "an engine template with a userinfo credential was accepted"
    );
}
