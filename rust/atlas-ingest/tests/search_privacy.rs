//! What a search may say out loud: the host, and never the endpoint with the query on it.
//!
//! The engine is a URL template with a `{query}` slot, and the query is derived from
//! the reader's own topics. So the *substituted* URL — template plus words — is the
//! one string in this crate that must not travel: it names what the reader is reading
//! about, and the template may carry a market, a language or an API key that is the
//! reader's own configuration.
//!
//! The rules checked here, and where each is decided:
//!
//! | Rule | Decided in |
//! | --- | --- |
//! | the tool result names the **host** and whether the reader chose the engine — never the template, never the URL | `search.rs` (`Engine::report`, `Engine::host`) |
//! | a refusal or a transport failure says the **host** and the reason, never the address it failed on | `search.rs` (`web_search`, `find_news`) |
//! | the refusal for "no engine at all" names the **constant** default template (a `{query}` slot, no words in it) and the setting's **name** | `search.rs` (`no_engine_message`) |
//! | nothing in the search path prints to stdout/stderr or to the app log | this is a *source* rule: the crate has one console, its `main.rs`, and the search code is not on it |
//! | the app log records an inbound method, path, status and time — never a query string | `atlas-web/src/gate.rs` (`finish`) |
//!
//! `ATLAS_SEARCH_ENGINE` is the environment spelling of the same setting, so
//! "the value is never echoed" is the same rule as "the template is never echoed":
//! both are checked through `resolve`, the pure function the three inputs meet in.
//!
//! Nothing here catches a leak that happens *after* the tool result exists (a UI that
//! prints `payload["query"]`, which is the model's own argument and the reader's own
//! words). It pins the crate's half: what `atlas-ingest` puts into a `Value`, a
//! sentence or a file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas_ingest::model::OfflineModel;
use atlas_ingest::rules::NativeRules;
use atlas_ingest::search::{resolve, Engine, DEFAULT_TEMPLATE, ENV_KEY};
use atlas_ingest::{Config, Fetched, Fetcher, NullBrowser, Pipeline};
use atlas_store::Store;

/// The words a reader's topics produce. Nothing in this crate wrote them, and no
/// template can contain them, so finding this string in a *URL* position is a leak —
/// as is finding the template around it.
const QUERY: &str = "harbour strike talks hamburg";

/// A secret the reader put in their own template (a key, a market, a language).
const SECRET: &str = "TOPSECRET-9000";

/// Two items, so a payload has something in it.
///
/// The links carry an ordinary public host, not `publisher.example`: the URL gate refuses
/// a reserved TLD (`.example` is not a place the app may go — measured, the read tool
/// answered "url refused (private hostname is not allowed)"), so a fixture on `.example`
/// makes `web_search` report `count 0, refused_links 2` and the test would be measuring
/// the gate instead of the disclosure it is about. The assertions below are unchanged.
const FEED: &str = r#"<rss version="2.0"><channel><title>News</title>
<item><title>Harbour strike ends</title><link>https://publisher-news.com/a</link>
  <description>The port said the backlog would clear.</description>
  <pubDate>Thu, 05 Nov 2026 10:00:00 GMT</pubDate></item>
<item><title>Talks resume</title><link>https://publisher-news.com/b</link>
  <description>Talks resumed in Hamburg.</description>
  <pubDate>Thu, 05 Nov 2026 09:00:00 GMT</pubDate></item>
</channel></rss>"#;

/// A transport that records every URL it was asked for and answers with one fixed
/// response — the only way to see what left the process without a socket.
struct Spy {
    seen: Mutex<Vec<String>>,
    answer: Mutex<Fetched>,
}

impl Spy {
    fn new(answer: Fetched) -> Arc<Spy> {
        Arc::new(Spy {
            seen: Mutex::new(Vec::new()),
            answer: Mutex::new(answer),
        })
    }

    fn urls(&self) -> Vec<String> {
        self.seen.lock().expect("the spy").clone()
    }

    fn set(&self, answer: Fetched) {
        *self.answer.lock().expect("the spy") = answer;
    }
}

impl Fetcher for Spy {
    fn fetch(&self, url: &str, _max_bytes: usize) -> Fetched {
        self.seen.lock().expect("the spy").push(url.to_string());
        self.answer.lock().expect("the spy").clone()
    }
}

/// A pipeline with exactly one seam replaced and the engine the test names.
fn pipeline(spy: Arc<Spy>, template: Option<&str>) -> Arc<Pipeline> {
    let store = Arc::new(Store::open_memory().expect("an in-memory database"));
    store.init().expect("the schema");
    let cache = std::env::temp_dir().join(format!(
        "atlas-ingest-search-privacy-{}",
        std::process::id()
    ));
    let mut config = Config::for_root(std::env::temp_dir());
    config.cache_dir = cache;
    // `Config::for_root` reads `ATLAS_SEARCH_ENGINE`; a test that wants the built-in
    // default must not inherit the machine's setting, and a test that wants a
    // template states it here instead of writing to the process's environment.
    config.search_engine = template.map(|template| template.to_string());
    let rules = Arc::new(NativeRules::with_bin_dir(
        std::env::temp_dir(),
        Duration::from_secs(5),
    ));
    Arc::new(Pipeline::new(
        store,
        config,
        rules,
        Arc::new(OfflineModel::new("no model is asked in this test")),
        spy,
        Arc::new(NullBrowser),
        None,
    ))
}

/// Everything about one answer that could carry the URL: the payload, and every
/// string a caller could reasonably print from it.
fn spoken(answer: &serde_json::Value) -> String {
    answer.to_string()
}

// ---------------------------------------------------------------------------
// 1. the built-in engine: the query goes out, the host comes back
// ---------------------------------------------------------------------------

#[test]
fn the_built_in_engine_receives_the_query_and_only_its_host_comes_back() {
    let spy = Spy::new(Fetched {
        status: 200,
        content_type: "application/rss+xml".to_string(),
        body: FEED.to_string(),
        final_url: String::new(),
        ..Fetched::default()
    });
    let pipeline = pipeline(Arc::clone(&spy), None);

    let answer = pipeline.web_search(QUERY, 5);
    let text = spoken(&answer);

    // The other half of the proof: the words really did leave the process, inside
    // the built-in template. A test that only looked for absence would pass on a
    // search that never happened.
    let seen = spy.urls();
    assert_eq!(seen.len(), 1, "the engine was asked {} times: {:?}", seen.len(), seen);
    let asked = &seen[0];
    assert!(
        asked.contains("q=harbour%20strike%20talks%20hamburg"),
        "the query did not reach the engine: {:?}",
        asked
    );
    assert!(
        asked.starts_with("https://"),
        "the built-in template is not http(s): {:?}",
        asked
    );

    // The reader's side: the host, and whether they chose it.
    let engine = Engine::built_in().expect("the built-in template validates");
    assert_eq!(answer["engine"]["host"], serde_json::json!(engine.host()));
    assert_eq!(answer["engine"]["source"], serde_json::json!("default"));
    assert_eq!(answer["query"], serde_json::json!(QUERY), "the model's own argument stays visible");
    assert_eq!(answer["count"], serde_json::json!(2), "{:?}", answer);

    // And the URL itself is nowhere in the answer: not the endpoint, not the
    // parameters the template carries, not the engine's address.
    assert!(!text.contains(asked.as_str()), "the engine URL is in the tool result: {}", text);
    assert!(!text.contains("?q="), "the substituted query is in the tool result: {}", text);
    assert!(
        !text.contains(engine.template()),
        "the template is in the tool result: {}",
        text
    );
    for parameter in ["format=RSS", "setmkt=", "setlang="] {
        assert!(
            !text.contains(parameter),
            "the template's `{}` is in the tool result: {}",
            parameter,
            text
        );
    }
}

#[test]
fn find_news_reports_the_host_the_same_way() {
    let spy = Spy::new(Fetched {
        status: 200,
        content_type: "application/rss+xml".to_string(),
        body: FEED.to_string(),
        final_url: String::new(),
        ..Fetched::default()
    });
    let pipeline = pipeline(Arc::clone(&spy), None);
    let answer = pipeline.find_news(QUERY, 48, 5);
    let text = spoken(&answer);
    let seen = spy.urls();
    assert_eq!(seen.len(), 1, "the engine was asked {} times", seen.len());
    assert!(
        !text.contains(seen[0].as_str()),
        "find_news put the engine URL in the tool result: {}",
        text
    );
    assert!(!text.contains("?q="), "{}", text);
    assert_eq!(answer["engine"]["host"], serde_json::json!(Engine::built_in().expect("built in").host()));
    assert_eq!(answer["engine"]["source"], serde_json::json!("default"));
    assert!(!text.contains(SECRET), "{}", text);
}

// ---------------------------------------------------------------------------
// 2. a configured template, on every path it can take
// ---------------------------------------------------------------------------

/// A configured template can carry the reader's own key. None of the three answers
/// `web_search` can give may echo it, and none may echo the substituted URL.
#[test]
fn a_configured_template_is_never_echoed_not_even_when_the_engine_fails() {
    let template = format!("https://search.example.com/?q={{query}}&apikey={}", SECRET);
    let engine = Engine::new(&template).expect("the template validates");
    let spy = Spy::new(Fetched {
        status: 200,
        content_type: "application/rss+xml".to_string(),
        body: FEED.to_string(),
        ..Fetched::default()
    });
    let pipeline = pipeline(Arc::clone(&spy), Some(&template));

    // (a) the engine answered.
    let answered = pipeline.web_search(QUERY, 5);
    let text = spoken(&answered);
    assert_eq!(answered["engine"]["host"], serde_json::json!("search.example.com"));
    assert_eq!(answered["engine"]["source"], serde_json::json!("configured"));
    for leak in [SECRET, "?q=", "search.example.com/?q", "apikey"] {
        assert!(!text.contains(leak), "the answered result carries `{}`: {}", leak, text);
    }

    // (b) the gate refused it.
    spy.set(Fetched {
        refused: "the host resolves to a private address".to_string(),
        ..Fetched::default()
    });
    let refused = pipeline.web_search(QUERY, 5);
    let text = spoken(&refused);
    assert!(
        refused["error"].as_str().unwrap_or("").contains("search.example.com"),
        "the refusal must still say which engine: {:?}",
        refused
    );
    for leak in [SECRET, "?q=", "apikey=", "harbour%20strike"] {
        assert!(!text.contains(leak), "the refusal carries `{}`: {}", leak, text);
    }

    // (c) the transport failed.
    spy.set(Fetched {
        error: "timed out before the page was read".to_string(),
        ..Fetched::default()
    });
    let failed = pipeline.web_search(QUERY, 5);
    let text = spoken(&failed);
    for leak in [SECRET, "?q=", "apikey=", "harbour%20strike"] {
        assert!(!text.contains(leak), "the failure carries `{}`: {}", leak, text);
    }

    // The template is still the one that was fetched, so (a) measured something.
    let asked = spy.urls();
    assert!(
        asked.iter().all(|url| url.contains(&format!("apikey={}", SECRET))),
        "the configured template was not used: {:?}",
        asked
    );
    let _ = engine;
}

/// An engine that does not validate falls back to the built-in one, and the answer
/// says so — the reader's broken template is not echoed either.
#[test]
fn an_invalid_configured_template_is_not_echoed_while_the_default_answers() {
    let broken = format!("https://search.example.com/no-query-slot?apikey={}", SECRET);
    let spy = Spy::new(Fetched {
        status: 200,
        content_type: "application/rss+xml".to_string(),
        body: FEED.to_string(),
        ..Fetched::default()
    });
    let pipeline = pipeline(Arc::clone(&spy), Some(&broken));
    let answer = pipeline.web_search(QUERY, 3);
    let text = spoken(&answer);
    assert_eq!(answer["engine"]["source"], serde_json::json!("default"), "{:?}", answer);
    assert_eq!(answer["engine"]["host"], serde_json::json!(Engine::built_in().expect("built in").host()));
    assert!(!text.contains(SECRET), "the broken template was echoed: {}", text);
}

// ---------------------------------------------------------------------------
// 3. the setting's name, and never its value
// ---------------------------------------------------------------------------

/// `ATLAS_SEARCH_ENGINE` is one of the three inputs `resolve` takes. The value that
/// came from it is what must not surface — not in the tool result, not in the
/// sentence for "no engine at all", which names the *constant* default instead.
#[test]
fn the_environment_value_is_resolved_and_never_echoed() {
    let template = format!("https://env.example.com/search?q={{query}}&key={}", SECRET);
    let engine = resolve(None, None, Some(&template)).expect("the env value validates");
    // The env value wins over nothing configured, and the result knows where it came
    // from — the reader can tell their own engine ran.
    assert_eq!(engine.source().label(), "configured");
    let report = engine.report().to_string();
    assert!(!report.contains(SECRET), "the engine report echoes the value: {}", report);
    assert!(!report.contains("?q="), "the engine report carries the URL: {}", report);
    assert!(report.contains("env.example.com"), "the report names the host: {}", report);

    // The one sentence that could have quoted a setting: it names the setting and the
    // built-in constant, with the `{query}` slot still empty.
    let pipeline = pipeline(Spy::new(Fetched::default()), None);
    let message = pipeline.no_engine_message();
    assert!(message.contains(ENV_KEY), "the sentence must name the variable: {}", message);
    assert!(
        message.contains(DEFAULT_TEMPLATE),
        "the sentence must name the built-in template: {}",
        message
    );
    assert!(!message.contains(SECRET), "the sentence echoes a configured value: {}", message);
    assert!(
        !message.contains("harbour"),
        "the sentence carries words a reader searched for: {}",
        message
    );
}

// ---------------------------------------------------------------------------
// 4. the source rule: nothing in the search path prints
// ---------------------------------------------------------------------------

/// Every shipped file of a crate, by path.
fn sources(crate_dir: &str) -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<std::path::PathBuf> =
            entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((path.display().to_string(), text));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(crate_dir)
        .join("src");
    let mut out = Vec::new();
    walk(&root, &mut out);
    assert!(out.len() > 5, "the scan read only {} files of {}", out.len(), crate_dir);
    out
}

/// A file's code, with every `#[cfg(test)] mod …` cut away — a fixture's own
/// `println!` is not a shipped writer.
fn code_of(text: &str) -> String {
    let mut out = String::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim() == "#[cfg(test)]" {
            let next = text
                .lines()
                .skip(index + 1)
                .find(|line| !line.trim().is_empty() && !line.trim().starts_with("//"));
            if next.map(|line| line.trim().starts_with("mod ")).unwrap_or(false) {
                break;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Nothing in the search path prints, so there is no line a URL could be logged on.
///
/// This is the test that fails if the URL is logged: add `eprintln!("fetching
/// {url}")` to `search.rs` — or move the fetch into a helper that logs, or print the
/// pipeline's `Debug` — and one of the assertions below names the file and the line.
/// `main.rs` is the crate's one console (the argv/stdin/stdout contract), and it is
/// not on this path: it prints the JSON the pipeline returned, which is checked in
/// section 1–3 above. The web face's request log — the other writer this file used to
/// inspect — went with `atlas-web`, so this crate's console is now the app's only one.
#[test]
fn no_writer_in_the_search_path_prints_a_url() {
    let files = sources("atlas-ingest");
    let mut prints = Vec::new();
    for (path, text) in &files {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name == "main.rs" {
            continue;
        }
        // A test body is also a *file* here (`src/tests.rs`, `src/rules/tests.rs`,
        // reached by `mod tests;`), and `code_of` can only cut a module written
        // inside the file it reads. Such a file is skipped when it really holds
        // `#[test]`s: a test's own `println!` is read by whoever runs the suite, not
        // by a reader of the app.
        if name == "tests.rs" {
            assert!(
                text.contains("#[test]"),
                "{} is named tests.rs but holds no test — it is source, and it is scanned",
                path
            );
            continue;
        }
        for (number, line) in code_of(text).lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for needle in ["println!", "eprintln!", "print!", "eprint!", "dbg!"] {
                if line.contains(needle) {
                    prints.push(format!("{}:{} — {}", path, number + 1, trimmed));
                }
            }
        }
    }
    assert!(
        prints.is_empty(),
        "a library module of atlas-ingest prints; a print is unredacted, unbounded and \
         cannot be turned off, and the search URL is one of the strings this crate \
         handles:\n{}",
        prints.join("\n")
    );

    // And the log the app writes: none, since `atlas-web` was retired with the web face.
    // The assertions that stood here inspected `atlas-web/src/gate.rs` — the one place a
    // request path reached the log — and there is no such file, no inbound request and no
    // request log any more, so nothing on this path can carry a search URL to a writer.
    // What still matters is what is above: no library module of this crate prints.

    // `atlas-net` is the transport the search goes through: it has a console in its
    // own `main.rs` (the debug CLI) and nowhere else.
    let net = sources("atlas-net");
    let mut net_prints = Vec::new();
    for (path, text) in &net {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name == "main.rs" {
            continue;
        }
        // A test body in its own file, as above: `println!` there is a measurement
        // for whoever runs the suite.
        if name == "tests.rs" {
            assert!(
                text.contains("#[test]"),
                "{} is named tests.rs but holds no test — it is source, and it is scanned",
                path
            );
            continue;
        }
        for (number, line) in code_of(text).lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for needle in ["println!", "eprintln!", "print!", "eprint!"] {
                if line.contains(needle) {
                    net_prints.push(format!("{}:{} — {}", path, number + 1, trimmed));
                }
            }
        }
    }
    assert!(
        net_prints.is_empty(),
        "atlas-net prints outside its CLI, and it is the crate that holds the search \
         URL:\n{}",
        net_prints.join("\n")
    );
}
