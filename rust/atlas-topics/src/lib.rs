//! `atlas-topics` — the topic gate from `server/topics.py`, in Rust.
//!
//! One JSON object in on stdin, one JSON object out on stdout:
//!
//! ```text
//! atlas-topics match   <<< '{"topics":[…],"article":{…}}'
//! atlas-topics rank    <<< '{"topic":{…},"articles":[…]}'
//! atlas-topics score   <<< '{"topic":{…},"article":{…}}'
//! ```
//!
//! Subcommands:
//!
//! * `match`  — the port of `topics.match_article(title, summary, content)`: every
//!   active topic the article belongs to, best match first, plus one row per
//!   topic that did not make it, saying whether an exclude term blocked it
//!   (and which one) or how many keywords short it fell.
//! * `rank`   — the same scoring for one topic over a batch of articles (needles
//!   built once, one folded body per article), ordered like the Python list.
//! * `score`  — the bare relevance of a single (topic, article) pair, for the
//!   monitor's cheap re-checks.
//!
//! What decides a match, exactly as in Python: the user's own keyword list, the
//! article's title (3.0), summary (1.5) and body (0.5), a topic `min_matches`
//! threshold and optional exclude terms. There is no news vocabulary, no
//! publisher list and no word blocklist in this file — relevance comes from the
//! topic the user wrote. Extracted entities and tags are accepted as *evidence*
//! only: they are reported next to the terms that matched and can never create,
//! raise or suppress a match (Python never scores them either).
//!
//! Fidelity notes, ported on purpose and not "fixed":
//!
//! * `fold` reproduces `_fold`: lowercase, then the `_FOLD` substitutions, then
//!   NFKD with combining marks dropped (the decomposition is a table for Latin,
//!   Greek and Cyrillic, see [`fold::text`]; Vietnamese stacked tones, Hangul
//!   syllables and halfwidth katakana keep their shape — the same fold runs on
//!   the keyword and on the text, so matching stays consistent).
//! * There is no stemmer: keywords get the same cheap plural/verb endings the
//!   Python pattern builder allows (`rate` → `rate|rates|rated|rating`).
//! * `// BUG:` comments mark behaviour that is faithfully wrong in Python.
//!
//! Two departures from the Python, both of which widen a match and neither of
//! which invents vocabulary:
//!
//! * the two Greek sigmas fold together (`ς` → `σ`), because `str.lower()` only
//!   unifies them when it has the uppercase form to work from, so a keyword
//!   typed with `σ` missed every headline written with `ς`;
//! * the fold is a fixed point (see [`fold::text`]), so a letter typed as one
//!   precomposed codepoint and the same letter typed as another land on the
//!   same form instead of on `æ` and `ae`.
//!
//! ## The modules, and what each one decides
//!
//! * [`chars`]    — Python's `\s`/`\w` classes and the `[:n]`/`join(split())`
//!   primitives, spelled out once.
//! * [`fold`]     — case and accent folding: the one canonical form a keyword and
//!   a text are compared in.
//! * [`terms`]    — the user's keywords compiled: split, de-duplicated, stemmed
//!   into families, and turned into a matcher that is run without backtracking.
//! * [`model`]    — what the payload means: topics, article, folded fields, the
//!   two content windows, `min_matches`, and the evidence-only name lists.
//! * [`score`]    — what a keyword is worth (3.0/1.5/0.5, +0.25), the threshold,
//!   the exclude gate, and the bare `score` answer for one pair.
//! * [`matching`] — the `match` answer: every topic an article belongs to.
//! * [`ranking`]  — the `rank` answer: one topic over a batch of articles.
//! * [`wire`]     — the two JSON fragments every answer shares.

#![forbid(unsafe_code)]

pub(crate) mod chars;
pub(crate) mod fold;
pub mod matching;
pub(crate) mod model;
pub mod ranking;
pub mod score;
pub(crate) mod terms;
pub(crate) mod wire;
#[cfg(test)]
mod tests;

use atlas_lib::cli;

pub use crate::matching::match_article;
pub use crate::ranking::rank;
pub use crate::score::score;

/// Dispatch one subcommand. `raw` is the whole of stdin.
pub fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input = cli::input_object(raw)?;
    match cmd {
        "match" => match_article(&input),
        "rank" => rank(&input),
        "score" => score(&input),
        other => Err(format!(
            "unknown subcommand: {} (expected: match | rank | score)",
            other
        )),
    }
}
