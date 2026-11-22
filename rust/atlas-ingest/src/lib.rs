//! atlas-ingest — the read/classify/store pipeline, in Rust.
//!
//! This crate is the port of `server/ingest.py` (1 068 lines) and
//! `server/enrichment.py` (569 lines). It is the part of ATLAS that decides what
//! becomes part of the archive: the AI searches, opens a page, reads it,
//! classifies it — and only then is anything written.
//!
//! # The rule this crate exists for
//!
//! Nothing here collects on its own. There is no timer, no feed poller, no link
//! discovery, no background thread. [`Pipeline::research`] runs when it is
//! called and returns when the model answers; every other entry point is a tool
//! the agent loop
//! ([`atlas_ai::agent`]) invokes through [`atlas_ai::ToolHandler`].
//!
//! # Where the decisions live
//!
//! | Decision | Owner | Called from |
//! | --- | --- | --- |
//! | may this URL be requested | `atlas-guard` (`urlsafe`) | [`gate`] |
//! | redirect, byte cap, charset | `atlas-net` | [`fetcher::NetFetcher`] |
//! | JS-only pages | nobody: the seam is [`fetcher::NullBrowser`] | [`fetcher::BrowserReader`] |
//! | HTML → text | `atlas-core` (`extract`) via [`rules::Rules`] | [`read`] |
//! | URL canonical form, publisher, title | `atlas-text` via [`rules::Rules`] | [`read`], [`save`] |
//! | place | `atlas-places` via [`rules::Rules`] | [`save`], [`enrich`] |
//! | topic match | `atlas-topics` via [`rules::Rules`] | [`save`] |
//! | 72 h window and future dates | `atlas-dates` via [`rules::Rules`] | [`save`], [`handlers`] |
//! | duplicates | `atlas-store` | [`save`] |
//! | entities, edges | `atlas-graph` | [`save`], [`enrich`] |
//! | news or not | the local model, `atlas-ai`'s verdict prompt | [`model::Model`] |
//! | clean text, cap, tags | `atlas-guard` (`cleantext`) + this crate | [`quality`], [`tags`] |
//!
//! The engines in rows 4–8 answer over the workspace's stdin/stdout JSON
//! contract — one op, one object in, one object out — and the transport is
//! [`rules::NativeRules`]'s choice, not a second rule: `atlas-places`,
//! `atlas-topics` and `atlas-dates` are asked **in this process** through the very
//! `run(cmd, raw)` their binaries call — as are `atlas-text` and `atlas-core`,
//! whose envelopes are the `run` in their own crate (`atlas-text/src/envelope.rs`,
//! `atlas-core/src/entry.rs`) and who therefore need no child either. A spawn per
//! question cost 2–4 ms for tens of microseconds of work: one saved article used
//! to start 15 processes for its 18 questions and now starts none
//! (`tests/engines.rs` prints both numbers).
//! `rules::tests` compares the two transports, byte for byte and including a
//! refusal, so they cannot drift. **There is no fallback**: when an engine is
//! asked as a process and its binary is missing, the call fails with the command
//! that fixes it (`cargo build --release`) instead of quietly answering something
//! else.
//!
//! # What a page may never do
//!
//! * Page text and model output are untrusted: both are cleaned, capped and
//!   validated before they reach a column, a tool result or a prompt.
//! * Instruction-injection markers found in a page are **flagged in the note and
//!   kept in the text** — the text is evidence, and the model is told (in
//!   `atlas-ai`'s system prompt) that everything between the markers is data.
//! * A wall, a consent screen or a navigation dump never enters the text cache.
//!   One predicate decides that ([`quality::content_junk`]), for reading, for
//!   storing and for repairing — so what is refused once is refused everywhere.
//! * Nothing page-shaped is ever logged. Errors carry a reason, counts and ids;
//!   never a title, a summary, a body or a prompt.
//!
//! # Only current reporting
//!
//! The 72-hour window and the future-date rejection are enforced once, in
//! [`save`], through `atlas-dates`: a page older than the window is refused, and
//! a page dated in the future is refused with `reason: skew` (a future-dated item
//! is not current, however convenient it is at the top of a newest-first list).
//!
//! # Wiring it
//!
//! ```no_run
//! use std::sync::Arc;
//! use atlas_ingest::{Pipeline, Config};
//! use atlas_store::Store;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Arc::new(Store::open("data/atlas.db")?);
//! store.init()?;
//! let cfg = Config::for_root(".");
//! let pipeline = Pipeline::native(store, cfg)?;   // wires every seam for real
//! let read = pipeline.read_article("https://example.com/a", "");
//! println!("{} chars cached", read.chars);
//! # Ok(())
//! # }
//! ```
//!
//! `server/ingestcore.py` is the bridge, in the shape of `server/textcore.py`:
//! it runs `rust/target/release/atlas-ingest` with one subcommand and reads one
//! JSON object back.

#![forbid(unsafe_code)]

pub mod brief;
pub mod cache;
pub mod entities;
pub mod enrich;
pub mod fetcher;
pub mod gate;
pub mod handlers;
pub mod hash;
pub mod model;
pub mod quality;
pub mod read;
pub mod research;
pub mod rules;
pub mod save;
pub mod search;
pub mod tags;
pub mod titles;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas_store::Store;

pub use cache::{CacheStats, TextCache};
pub use enrich::Receipt;
pub use fetcher::{BrowserPage, BrowserReader, Fetched, Fetcher, NetFetcher, NullBrowser};
pub use gate::{article_url_refusal, gate_url, host_of, is_article_url, is_blocked_host, same_page, url_error};
pub use model::{AiModel, Answer, Model};
pub use quality::{clip, content_junk, injection_flags, preview_of};
pub use read::ReadResult;
pub use rules::{
    ArticleText, Locate, NativeRules, Parsed, Rules, RulesError, TopicMatch, TopicRow, Window,
    PLACE_BODY_CHARS, TOPIC_BODY_CHARS,
};
pub use save::{Dated, SaveResult};
pub use search::{Engine, SearchHit};
pub use titles::{norm_title, same_title, title_tokens};

/// A cached page must be at least this long to be article text
/// (`ingest.MIN_CHARS`).
pub const MIN_CHARS: usize = 700;

/// How much of a page is kept — memory, cache file and stored `content`
/// (`ingest.MAX_CACHE_CHARS`, also `content[:20000]` in `save_article`).
pub const MAX_CACHE_CHARS: usize = 20_000;

/// The preview a model gets back from `read_article` (`MAX_PREVIEW_CHARS`).
pub const MAX_PREVIEW_CHARS: usize = 1_200;

/// Whole paragraphs in that preview (`PREVIEW_PARAS`).
pub const PREVIEW_PARAS: usize = 3;

/// Pages kept in memory for `save_article` (`READ_CACHE_MAX`).
pub const READ_CACHE_MAX: usize = 20;

/// Cache files kept on disk (`CACHE_KEEP`).
pub const CACHE_KEEP: usize = 200;

/// Tags one article may carry (`ingest.MAX_TAGS`).
pub const MAX_TAGS: usize = 12;

/// `ingest.MAX_AGE_H` — the ceiling of the ingest window in hours. The value the
/// archive itself uses comes from [`atlas_store::Store::window_hours`], so the
/// 72-hour rule is not restated here; this is only the bound a tool argument is
/// clamped to.
pub const MAX_AGE_H: i64 = 72;

/// Everything the pipeline needs to know about the machine it runs on.
#[derive(Debug, Clone)]
pub struct Config {
    /// The application root (`config.ROOT`): `data/` hangs off it.
    pub root: PathBuf,
    /// Memory + disk text cache (`ingest.CACHE_DIR`, `<root>/data/cache/read`).
    pub cache_dir: PathBuf,
    /// Where `atlas-text`, `atlas-core`, … were built (`rust/target/release`).
    pub bin_dir: PathBuf,
    /// Pages held in memory.
    pub read_cache_max: usize,
    /// Cache files kept on disk; older ones are pruned on every write.
    pub cache_keep: usize,
    /// Bytes a page fetch may keep.
    pub max_bytes: usize,
    /// Characters of a page that are kept at all.
    pub max_chars: usize,
    /// The search engine template (see [`search::Engine`]). `None` means the reader
    /// configured none, and [`search::DEFAULT_TEMPLATE`] answers instead: a first
    /// run must be able to search, so "unset" is a default and not a refusal. The
    /// tool result names which of the two ran.
    pub search_engine: Option<String>,
    /// Ceiling for one engine subprocess.
    pub rules_timeout: Duration,
    /// Ceiling for one browser read.
    pub browser_timeout: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config::for_root(".")
    }
}

impl Config {
    /// The layout `server/config.py` uses: `<root>/data/cache/read` and
    /// `<root>/rust/target/release`.
    pub fn for_root(root: impl AsRef<Path>) -> Config {
        let root = root.as_ref().to_path_buf();
        Config {
            cache_dir: root.join("data").join("cache").join("read"),
            bin_dir: root.join("rust").join("target").join("release"),
            root,
            read_cache_max: READ_CACHE_MAX,
            cache_keep: CACHE_KEEP,
            max_bytes: atlas_net::DEFAULT_MAX_BYTES,
            max_chars: MAX_CACHE_CHARS,
            search_engine: std::env::var("ATLAS_SEARCH_ENGINE").ok(),
            rules_timeout: Duration::from_secs(15),
            browser_timeout: Duration::from_secs(30),
        }
    }

    /// Say where the engine binary of `name` lives. The suffix is
    /// `atlas-platform`'s answer, so `.exe` exists exactly when the platform
    /// needs it.
    pub fn bin(&self, name: &str) -> PathBuf {
        atlas_platform::host::app_binary(&self.bin_dir, name)
    }
}

/// Why the pipeline could not do the thing it was asked to do.
///
/// The message is a reason, never page content: it is built from a crate's
/// reason string (already scrubbed) or from a count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineError {
    message: String,
}

impl PipelineError {
    pub fn new(message: impl Into<String>) -> PipelineError {
        PipelineError {
            message: scrub(&message.into()),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PipelineError {}

impl From<atlas_store::StoreError> for PipelineError {
    fn from(error: atlas_store::StoreError) -> PipelineError {
        PipelineError::new(error.message())
    }
}

impl From<RulesError> for PipelineError {
    fn from(error: RulesError) -> PipelineError {
        PipelineError::new(error.message())
    }
}

/// One line, no control characters, capped: what an error may say.
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(400));
    for ch in text.chars() {
        if out.chars().count() >= 400 {
            break;
        }
        if ch.is_control() || atlas_guard::cleantext::is_invisible(ch) {
            out.push(' ');
            continue;
        }
        out.push(ch);
    }
    let collapsed = out.split_whitespace().collect::<Vec<&str>>().join(" ");
    if collapsed.is_empty() {
        "no reason given".to_string()
    } else {
        collapsed
    }
}

/// The pipeline: every seam in one place, so the same guard cannot exist twice.
///
/// Cheap to share (`Arc<Pipeline>`) — the tool handlers hold one. The store, the
/// rule engines, the model and the transport are all behind `Arc<dyn …>`, which
/// is also what lets the tests swap each one for a fake.
pub struct Pipeline {
    store: Arc<Store>,
    rules: Arc<dyn Rules>,
    model: Arc<dyn Model>,
    net: Arc<dyn Fetcher>,
    browser: Arc<dyn BrowserReader>,
    agent: Option<Arc<dyn atlas_ai::LlamaClient + Send + Sync>>,
    cache: Mutex<TextCache>,
    graph: Mutex<atlas_graph::graphdb::Caches>,
    config: Config,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("cache_dir", &self.config.cache_dir)
            .field("search_engine", &self.config.search_engine.is_some())
            .field("agent", &self.agent.is_some())
            .finish()
    }
}

/// The one clock. A test sets a fixed `now`; nothing else in here reads
/// `SystemTime` directly, so a freshness test cannot be flaky.
pub trait Clock: Send + Sync {
    /// Seconds since the Unix epoch.
    fn now_secs(&self) -> i64;
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> i64 {
        atlas_store::clock::epoch_secs_now()
    }
}

impl Pipeline {
    /// Build a pipeline from explicit seams — what a test does, and what the
    /// library does when a caller wires its own transport.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<Store>,
        config: Config,
        rules: Arc<dyn Rules>,
        model: Arc<dyn Model>,
        net: Arc<dyn Fetcher>,
        browser: Arc<dyn BrowserReader>,
        agent: Option<Arc<dyn atlas_ai::LlamaClient + Send + Sync>>,
    ) -> Pipeline {
        let cache = TextCache::new(
            config.cache_dir.clone(),
            config.read_cache_max,
            config.cache_keep,
        );
        Pipeline {
            store,
            rules,
            model,
            net,
            browser,
            agent,
            cache: Mutex::new(cache),
            graph: Mutex::new(atlas_graph::graphdb::Caches::new()),
            config,
            clock: Arc::new(SystemClock),
        }
    }

    /// Pin the clock (tests, and a caller replaying a fixed window).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Pipeline {
        self.clock = clock;
        self
    }

    /// The production wiring: `atlas-net`, the engine binaries in `config.bin_dir`, and
    /// `atlas-ai`'s `HttpLlamaClient`.
    ///
    /// **The browser seam is [`NullBrowser`] here, and that is the decision, not a
    /// placeholder.** This pipeline used to be wired with a local Chrome spoken to over
    /// the DevTools protocol; Chrome is not a Rust binary and it can only be driven
    /// through its remote-control flag on an ephemeral *listening* port, which this
    /// product does not open. A reader that cannot render a page over plain HTTP now
    /// says so — [`read`]'s JS-only sentence — instead of starting a browser. A caller
    /// that really wants a browser passes its own [`BrowserReader`] to [`Pipeline::new`];
    /// nothing in this crate can construct one.
    ///
    /// The flag is deliberately not spelled in this file: the reachability audit reads a
    /// line that names it as a browser launch, comments included, and that is the rule
    /// working. `atlas-browser` is where the spelling lives.
    ///
    /// Fails — it never degrades — when a binary this pipeline needs is not
    /// built, because a missing rule is not a reason to guess.
    pub fn native(store: Arc<Store>, config: Config) -> Result<Pipeline, PipelineError> {
        let rules = NativeRules::discover(&config)?;
        // The model's deadline is its own, not the rules' — `ENRICH_TIMEOUT`, thinking
        // included. It used to be `config.rules_timeout` (15 s), so a thinking enrichment
        // was cut off by the transport after 15 s and the article fell back to the
        // heuristic: the one constant that existed for this was never read.
        let model = match AiModel::from_env(model::ENRICH_TIMEOUT.max(config.rules_timeout)) {
            Ok(model) => Arc::new(model) as Arc<dyn Model>,
            Err(reason) => Arc::new(model::OfflineModel::new(&reason)) as Arc<dyn Model>,
        };
        let agent = match atlas_ai::HttpLlamaClient::new(
            &endpoint_string(),
            &model_id(),
            atlas_ai::DEFAULT_TIMEOUT,
        ) {
            Ok(client) => Some(Arc::new(client) as Arc<dyn atlas_ai::LlamaClient + Send + Sync>),
            Err(_) => None,
        };
        Ok(Pipeline::new(
            store,
            config,
            Arc::new(rules),
            model,
            Arc::new(NetFetcher::new()),
            // HTTP only. No Chrome, so no remote-control port and no listener in the
            // agent process: a page that needs JavaScript is a sentence the model can
            // act on, not a browser this product starts.
            Arc::new(NullBrowser),
            agent,
        ))
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn rules(&self) -> &Arc<dyn Rules> {
        &self.rules
    }

    pub fn model(&self) -> &Arc<dyn Model> {
        &self.model
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn agent(&self) -> Option<&Arc<dyn atlas_ai::LlamaClient + Send + Sync>> {
        self.agent.as_ref()
    }

    /// Seconds since the epoch, from the pipeline's clock.
    pub fn now_secs(&self) -> i64 {
        self.clock.now_secs()
    }

    /// `db.now_iso()` — the timestamp format every column already holds.
    pub fn now_iso(&self) -> String {
        atlas_store::clock::iso_from_epoch(self.now_secs())
    }

    /// The ISO cutoff `hours` ago, computed from the pipeline's clock.
    pub fn iso_ago(&self, hours: i64) -> String {
        atlas_store::clock::iso_from_epoch(self.now_secs() - hours.max(0) * 3600)
    }

    /// The archive's own window in hours (`Window_HOURS`, 72 by default), clamped
    /// to [`MAX_AGE_H`]. One place decides how far back "current" reaches.
    pub fn window_hours(&self) -> i64 {
        self.store.window_hours().clamp(1, MAX_AGE_H)
    }

    /// The cached full text for a normalised URL key (memory, then disk).
    pub fn text_cache_get(&self, key: &str) -> Option<String> {
        self.cache.lock().ok().and_then(|mut cache| cache.get(key))
    }

    /// How many pages are held in memory right now.
    pub fn text_cache_len(&self) -> usize {
        self.cache.lock().map(|cache| cache.len()).unwrap_or(0)
    }

    /// What the cache did — counts only, for a status line or a benchmark.
    pub fn text_cache_stats(&self) -> CacheStats {
        self.cache
            .lock()
            .map(|cache| cache.stats())
            .unwrap_or_default()
    }
}

/// The default port when nothing says otherwise. `atlas-config`'s own default, so the
/// pipeline and the launcher cannot disagree about an unconfigured machine.
pub const DEFAULT_LLAMA_PORT: u16 = 8080;

/// The endpoint the local model answers on — resolved in the same order the window uses,
/// because two answers to "where is my model" is how a pipeline silently talks to a port
/// nobody is on.
///
/// 1. `ATLAS_MODEL_URL`, a full loopback URL, for a caller that wants a different server;
/// 2. `ATLAS_LLAMA_PORT`, the number alone;
/// 3. `data/config.json`'s `llama_port`, the file `atlas-setup` writes and the window
///    reads. **This was missing**: `data/config.json` said `8081`, this function answered
///    `8080`, and every classification and enrichment request in the CLI went to a closed
///    port — so the model was never asked, and the honest fallback (a heuristic receipt
///    with `model: false`) was what the archive received. Measured: `enrich` on five
///    articles answered `"model": false` while the server on 8081 answered a direct POST
///    in 0.67 s;
/// 4. [`DEFAULT_LLAMA_PORT`].
///
/// The host is never configurable: it is this machine's loopback literal, spelled here.
pub fn endpoint_string() -> String {
    if let Some(url) = std::env::var("ATLAS_MODEL_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return url;
    }
    let port = std::env::var("ATLAS_LLAMA_PORT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .filter(|port| *port > 0)
        .or_else(configured_llama_port)
        .unwrap_or(DEFAULT_LLAMA_PORT);
    format!("http://127.0.0.1:{port}")
}

/// `data/config.json`'s `llama_port`, read the way the window reads it: an integer in
/// `1..=65535`, nothing guessed, and a file that is missing or unusable is simply absent.
///
/// The root is `ATLAS_ROOT` when a caller named one (the CLI does), else the working
/// directory — the same rule the rest of this binary uses.
fn configured_llama_port() -> Option<u16> {
    let root = std::env::var("ATLAS_ROOT").unwrap_or_else(|_| ".".to_string());
    let path = std::path::Path::new(&root).join("data").join("config.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let port = value.get("llama_port")?.as_u64()?;
    u16::try_from(port).ok().filter(|port| *port > 0)
}

/// The model id `atlas-ai` asks for; `local` matches the server's single model.
pub fn model_id() -> String {
    std::env::var("ATLAS_MODEL_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod brief_tests;
#[cfg(test)]
mod junk_tests;
