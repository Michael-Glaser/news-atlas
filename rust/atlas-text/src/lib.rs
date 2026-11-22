//! ATLAS text core — the string decisions every other crate depends on.
//!
//! One JSON object in, one JSON object out, exactly like `atlas-guard`:
//!
//! ```text
//! echo '{"text":"  Hello\u0000 World "}' | atlas-text clean_text
//! {"text":"Hello World","removed":1,"truncated":false}
//! ```
//!
//! Ops: `clean_text`, `tokenize`, `clean_title`, `publisher`, `norm_url`.
//! No dependencies, no network, no filesystem: std only.
//!
//! Five operations, five files, and no operation knows about another:
//!
//! * `clean`      — which characters and which layout may be stored.
//! * `tokens`     — which words identify a story.
//! * `title`      — the headline without the outlet's tail.
//! * `publisher`  — a presentable outlet name out of a URL.
//! * `url`        — the canonical form of a URL, for duplicate detection.
//!
//! `envelope` is the one request/answer shape (`run`: `argv`'s word plus stdin →
//! one JSON object, or `{"error": …}`), `json` is the little parsing that shape
//! needs, and `cli` is argv/stdin/stdout around it. `main.rs` is only the call
//! into [`main_entry`].
//!
//! [`run`] is public on purpose: it is the same function the binary executes, so
//! another crate in this workspace asks a text question in its own process
//! instead of starting one — `atlas-ingest`'s `rules::inproc` does, for every
//! `clean_text`, `tokenize`, `clean_title`, `publisher` and `norm_url` a stored
//! article needs. A caller that re-implemented the envelope to do that would be
//! the second code path this layout exists to prevent, so the caps the envelope
//! applies ([`MAX_INPUT`], and `max_words` via [`MAX_WORDS`] /
//! [`DEFAULT_MAX_WORDS`]) are public here rather than restated there.

// What may be stored at all: dropped characters, collapsed layout, the size cap.
pub mod clean;
// Which words identify a story: stopwords dropped, markup gone, deduplicated.
pub mod tokens;
// The headline without the outlet's suffix.
pub mod title;
// A presentable outlet name out of a URL host.
pub mod publisher;
// The canonical form of a URL, for duplicate detection.
pub mod url;

// One request in, one JSON object out: the shape every answer has.
mod envelope;
// The little of JSON the request and the answer touch.
mod json;
// The process boundary: argv, stdin, stdout and the exit code.
mod cli;

pub use clean::{clean_text, MAX_FIELD};
pub use cli::main_entry;
pub use envelope::{refusal, run, MAX_INPUT};
pub use publisher::publisher;
pub use title::clean_title;
pub use tokens::{tokenize, DEFAULT_MAX_WORDS, MAX_WORDS};
pub use url::norm_url;
