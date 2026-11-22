//! atlas-search — the search the AI uses to find news.
//!
//! The user's words are the requirement: **the AI decides what it searches for.**
//! So this is a tool, not a crawler. The model produces a query; this crate asks
//! one search endpoint for it and answers with what the engine said — title,
//! link, publisher, date, snippet — and the model decides what is relevant and
//! what to open next.
//!
//! # What it deliberately does not do
//!
//! * **No publisher allowlist and no news-host blocklist.** The only links this
//!   crate refuses are the ones `atlas-guard` refuses for safety (loopback,
//!   metadata, service ports) plus the engine's own navigation, which is not a
//!   publisher but the page's furniture.
//! * **No news-word filter.** `atlas-ingest`'s older search dropped encyclopaedia
//!   and social hosts by name; this crate ranks by relevance to the query and
//!   leaves the judgement to the local model.
//! * **No crawl, no timer, no queue.** Every entry point is called by
//!   `atlas-ai`'s agent loop and returns when the answer is ready.
//!
//! # Where the decisions live
//!
//! | Decision | Owner | Called from |
//! | --- | --- | --- |
//! | may this link be opened | `atlas-guard` (`urlsafe`) | [`link`], via `atlas-net` |
//! | redirect, byte cap, charset, DNS check | `atlas-net` | [`fetch::Http`] |
//! | HTML tag/text scan | `atlas-core` (`htmlscan`) | [`html`] |
//! | relevance order | `atlas-core` (`similar`) | [`rank`] |
//! | URL canonical form, publisher, headline tail | `atlas-text` via [`rules::Rules`] | [`extract`] |
//! | is this date inside the window | `atlas-dates` via [`rules::Rules`] | [`extract`] |
//! | what is relevant, what to open | the local model | — |
//!
//! # The map
//!
//! * [`engine`] — the configured endpoint: one URL template per surface, no key,
//!   no account, and the query the model wrote, percent-encoded once.
//! * [`fetch`] — one page over `atlas-net`, behind a seam a test can replace.
//! * [`html`] — the structural scan of a result page: blocks, text segments,
//!   anchors. No engine selectors, no vocabulary.
//! * [`cards`] — anchors to result cards: title, snippet, the date the engine
//!   printed.
//! * [`link`] — resolving a link, unwrapping the engine's redirect wrapper,
//!   dropping its navigation, and the coarse index that decides when two links
//!   need `atlas-text` to settle whether they are the same.
//! * [`dates`] — *where* a date might be written on a card. Reading it is
//!   `atlas-dates`' job.
//! * [`rank`] — relevance order, through `atlas-core`'s similarity primitive.
//! * [`hit`] — one answer row and its JSON.
//! * [`extract`] — cards to hits: gate, dedupe, publisher, title, date.
//! * [`rules`] — the `atlas-text` / `atlas-dates` seam, and the process
//!   implementation the binary uses.
//! * [`clock`] — "now", so a test can be a fixed day.
//! * [`search`] — the three operations: [`search::Search::web_search`],
//!   [`search::Search::news_for`], [`search::Search::find_news`].
//!
//! # Trust
//!
//! Everything on a result page is untrusted: the title, the snippet, the date,
//! the link. Text is stripped of control and bidi characters
//! (`atlas-guard`'s `cleantext`) and capped before it leaves this crate; every
//! link is gated before it is handed on. Nothing here logs a page, a title or a
//! query: errors carry reasons and counts.
//!
//! ```text
//! atlas-search web_search <<< '{"query":"chip export rules","max_results":6}'
//! atlas-search news_for   <<< '{"query":"chip export rules","since":1795251600,"limit":8}'
//! atlas-search find_news  <<< '{"query":"chip export rules","hours":48,"limit":8}'
//! ```

#![forbid(unsafe_code)]

pub mod cards;
pub mod clock;
pub mod dates;
pub mod engine;
pub mod extract;
pub mod fetch;
pub mod hit;
pub mod html;
pub mod link;
pub mod rank;
pub mod rules;
pub mod search;

#[cfg(test)]
mod doubles;
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_engines;

pub use cards::Card;
pub use clock::{Clock, Fixed, System};
pub use engine::Engine;
pub use extract::{Candidate, Outcome};
pub use fetch::{Answer, Fetcher, Http};
pub use hit::Hit;
pub use rules::{Engines, Rules, Window};
pub use search::Search;

/// The three tool names this crate answers to, as `atlas-ai`'s specs spell them.
pub const TOOLS: &[&str] = &["web_search", "news_for", "find_news"];

/// Page bytes read at all: a result page is a list, not a corpus.
pub const MAX_PAGE_BYTES: usize = 1_500_000;

/// The most results one call may return, whatever the model asks for.
pub const MAX_RESULTS: usize = 40;

/// Results kept on an ordinary page — the tool result a model reads.
pub const DEFAULT_RESULTS: usize = 10;

/// The longest a query may be, in characters (a query is not a paragraph).
pub const MAX_QUERY_CHARS: usize = 200;

/// Untrusted text, made safe for one line of a tool result: control and
/// invisible characters out, whitespace collapsed, capped at `max_chars`.
///
/// This is `atlas-guard`'s cleaning with a cap on top, so a page cannot put a
/// bidi override or a 40 kB "title" in front of the model.
pub(crate) fn untrusted(text: &str, max_chars: usize) -> String {
    let clean = atlas_guard::cleantext::inline(text);
    let collapsed = atlas_core::text::collapse_ws(&clean);
    hit::cap(&collapsed, max_chars)
}

/// A reason, safe to put in a tool result: control characters out, no page text.
pub(crate) fn scrub(reason: &str) -> String {
    hit::cap(&atlas_guard::cleantext::inline(reason), 200)
}
