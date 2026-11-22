//! atlas-core — a small, dependency-free JSON-in / JSON-out CLI for ATLAS.
//!
//! Three subcommands, all of which read exactly one JSON object from stdin and
//! write exactly one JSON object to stdout:
//!
//! * `extract`  — pull readable article text out of a full HTML page.
//! * `entities` — count proper-noun phrases and known names in a text.
//! * `similar`  — cosine similarity over sparse token vectors.
//!
//! On bad input the program writes `{"error":"..."}` to stdout and exits 1.
//!
//! The same decisions are the library below, split by concern in the order a
//! page travels through it: `json` reads a request and writes an answer, `text`
//! decodes entities and collapses whitespace, `htmlscan` walks a page once,
//! `extract` chooses the body, `entities` counts names, `similar` scores
//! documents against each other. `main.rs` is only argv/stdin/stdout wiring.

/// A minimal JSON parser and writer, plus the two number formats an answer uses.
pub mod json;
/// Entity decoding, whitespace collapsing, word counting and line filtering.
pub mod text;
/// The tag scanner: one pass over a page, with candidate ranges as its answer.
pub mod htmlscan;
/// The body chooser: candidates in, the best article text out.
pub mod extract;
/// Proper-noun phrases, counted and ordered.
pub mod entities;
/// Cosine similarity over sparse token vectors: document pairs out.
pub mod similar;

/// The process boundary: argv, stdin, stdout and the exit code.
mod entry;

/// The process boundary's program, [`entry::run`]: one op plus one JSON object →
/// one JSON object, or `{"error": …}`.
///
/// Public because it is the same function `main` runs, so a caller in this
/// workspace (atlas-ingest's `rules::inproc`) answers `extract` and `entities` in
/// its own process instead of spawning this binary — with the binary's bytes, not
/// with a second envelope.
pub use entry::main_entry;
pub use entry::run;
