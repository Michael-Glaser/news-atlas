//! atlas-guard — the ATLAS security core: one JSON object in on stdin, one out.
//!
//! Every security decision the app makes about untrusted input lives here, in
//! Rust, and nowhere else. Six subcommands, each a pure function of its input:
//!
//! * `url`           — the SSRF gate: scheme/host/port/IP checks before a fetch.
//! * `clean_text`    — strip control and invisible characters, normalise layout.
//! * `sanitize_html` — HTML to plain paragraphs, before any model sees the page.
//! * `token`         — normalise and vet a tag/place/topic/search term.
//! * `redact`        — drop secrets and e-mail addresses before logging or prompting.
//! * `address`       — is a listening address loopback? The listener audit and the
//!                     browser's debug-port check ask this instead of deciding it
//!                     themselves.
//!
//! Contract: one JSON object on stdin, one JSON object on stdout, exit 0 on
//! success. A malformed request (bad JSON, missing field, oversized field)
//! prints `{"error":"..."}` and exits 1. A *verdict* is not an error: `url` and
//! `token` answer `{"ok":false,"reason":"..."}` with exit 0 so the caller can
//! act on the reason.
//!
//! No `unsafe`, no crate dependencies, no network, no globals: the binary only
//! reads stdin and writes stdout. Every rule below is written down in
//! docs/SECURITY-RUST.md, and this crate is the only implementation of it.

#![forbid(unsafe_code)]

// The modules, in the order a request travels through them: parse it, decode its
// entities, clean it, sanitise it, vet it, redact it, gate a URL, audit a
// listener — and the two that hold the field contract and the process boundary.

/// A minimal JSON parser and writer, plus the constructors an answer is built from.
pub mod json;
/// HTML entities to characters, shared by `cleantext` and `sanitize`.
pub mod text;
/// What may be stored at all: dropped characters, collapsed layout, truncation.
pub mod cleantext;
/// One HTML page to plain paragraphs, before any model sees it.
pub mod sanitize;
/// Whether a tag / place / topic / search value may be used as a value.
pub mod token;
/// Secrets and e-mail addresses out, before anything is logged or prompted.
pub mod redact;
/// The SSRF gate: scheme, host, port and IP range checks before a fetch.
pub mod urlsafe;
/// Is a listening address loopback, or reachable from another machine?
pub mod address;
/// A URL that already holds the gate's answer: the value the transports take
/// instead of a `&str`, so a request cannot skip the gate.
pub mod gated;
/// Deterministic mutated input for the fuzz tests: no crate, no network, same
/// bytes on every machine.
pub mod mutate;
/// What counts as shipped code in `src/*.rs`, so a structural claim can be a
/// test instead of a promise.
pub mod source;

/// IP literals and which of them are private: the gate's and the audit's arithmetic.
mod ipaddr;
/// What a host is — an address, a name, or a refusal — for the gate above.
mod hostname;
/// PEM private-key blocks, the one credential shape a log line keeps verbatim.
mod pemkey;
/// RFC 3492 punycode decoding, so an encoded name cannot hide from the blocklist.
mod punycode;

/// The process boundary: argv, stdin, stdout and the exit code.
mod entry;
/// The request field contract: reading a field, the size cap, a character prefix.
mod field;

pub use entry::main_entry;
pub use gated::Gated;

#[cfg(test)]
pub(crate) mod tests;
