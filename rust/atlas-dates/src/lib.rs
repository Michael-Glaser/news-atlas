//! atlas-dates — the one date/time implementation for ATLAS, in Rust.
//!
//! Timestamp reading, the 72-hour freshness window and the display age. One
//! implementation, one rule: `atlas-ingest` and `atlas-search` call this crate
//! through the workspace's stdin/stdout JSON contract, so a fix here is a fix
//! everywhere — for what a stamp *means* and for whether it is *current*.
//!
//! The one thing outside this crate that still owns a date rule of its own is the
//! wasm UI's display formatting (`atlas-ui/src/fmt.rs` parses the stored string
//! and writes "3 h ago"), because it runs in the browser with no process to call.
//! That is a display copy, and display is the only thing it may decide: freshness
//! is decided here, once, and a future-dated item is refused here.
//!
//! Wire format (see docs/RUST-CRATE-CONTRACT.md): one JSON object on stdin, one
//! JSON object on stdout.
//!
//! ```text
//! atlas-dates parse   <<< '{"ts": "Thu, 21 Nov 2026 10:00:00 +0100"}'
//! atlas-dates window  <<< '{"ts": "...", "now": 1795251600, "max_age_s": 259200}'
//! atlas-dates format  <<< '{"age_s": 14400}'
//! ```
//!
//! Subcommands
//! -----------
//! * `parse`  — `{"ts", "hint"?, "now"?}` →
//!   `{"ok": true, "epoch", "iso", "kind", "tz_offset_s", "assumed_utc",
//!     "weekday", "weekday_ok", "frac_us"}` or `{"ok": false, "reason"}`.
//!   An unparseable timestamp is a *normal answer*, not a wire error: the caller
//!   gets a reason it can log instead of a guess.
//! * `window` — `{"ts", "now"?, "max_age_s"?, "skew_s"?}` →
//!   `{"fresh", "age_s", "age_h", "reason", "note", "iso", "now",
//!     "max_age_s", "skew_s"}`. `reason` is one of `fresh`, `stale`, `skew`,
//!     `unparsed`; `age_s`/`age_h` are `null` when the stamp could not be read.
//! * `format` — `{"age_s"}` or `{"ts", "now"?}` → `{"text", "unit", "n", "age_s",
//!   "future"}`. `text` is the one relative-time string the UI shows: `now`,
//!   `4 h`, `2 d`, `in 4 h`.
//!
//! Accepted timestamp syntax (all hand-parsed, single pass, and allocation-free
//! on the ASCII path):
//!
//! * ISO-8601 / RFC-3339: `2026-12-31T23:59:59Z`, `2026-12-31T23:59:59+02:00`,
//!   `...-0500`, `...+05:30`, fractional seconds, a space instead of `T`,
//!   `2026-12-31`, `2026-12-31 10:00`, `... UTC` / `... GMT`, and an ISO date
//!   with a 12-hour clock (`2026-01-01 10:00 AM`).
//! * RFC-2822 (mail / RSS): `Thu, 21 Nov 2026 10:00:00 +0100 (CET)`, named zones
//!   (`AEST`, `MSK`, `EST`, …) and `GMT+0200`; the weekday is validated and a
//!   mismatch is reported, not silently ignored.
//! * German numeric `31.12.2026 [10:00[:00]]`, `31.12.2026 um 23:59 Uhr`.
//! * Month names in the four languages feeds are written in — English, German,
//!   French, Spanish: `21 November 2026`, `November 21, 2026`, `Nov 21 2026`,
//!   `21. November 2026`, `1er janvier 2026`, `21 de noviembre de 2026` — each
//!   optionally followed by a time (`10:00`, `10h30`) and a zone.
//! * Relative phrases: `vor 2 Stunden`, `2 hours ago`, `yesterday`, `gestern`,
//!   `just now`.
//! * Unix epoch seconds / milliseconds / microseconds / nanoseconds.
//!
//! Deliberate decisions — each one is a rule the app depends on, and each one has
//! a test in `crate::tests`:
//!
//! * A timestamp without a zone is UTC. Reading a naive stamp as the machine's
//!   local time makes the stored value change when the laptop travels, and the
//!   app stores UTC everywhere. The answer says `assumed_utc: true` so a caller
//!   can tell a naive stamp from a zoned one; an epoch number and a relative
//!   phrase report `false`, because there no zone was guessed.
//! * Unparseable input is an error with a reason. Answering *now* for a garbage
//!   feed date turns it into breaking news.
//! * A timestamp more than `skew_s` (300 s) in the future is not fresh. Its age
//!   is negative, which is below every window, so the arithmetic alone would let
//!   an article dated 2027 through every gate there is.
//! * The display label never claims a future stamp is current: beyond the same
//!   300 s it reads `in 4 h`, not `now`.
//! * A bare number that could be read two ways is refused, not guessed: `2026` is
//!   a year or an epoch, `20261231120000` is an epoch in milliseconds or the
//!   compact date-time 2026-12-31 12:00:00.
//! * Month and year units in relative phrases are fixed lengths (30 d / 365 d):
//!   they are display-grade and never feed the freshness window.
//!
//! ## The modules, and what each one decides
//!
//! * [`calendar`] — day numbers, civil dates and weekdays. Pure integer
//!   arithmetic, no clock, no parsing; the one module a reader can check by eye.
//! * [`data`] — the month and weekday tables of the four languages. Data only.
//! * [`scanners`] — the byte-level readers every family shares: digits, a clock,
//!   a zone, a word.
//! * [`epoch`] — bare numbers: seconds, ms, µs, ns, and the compact
//!   `YYYYMMDDHHMM[SS]` shape that must not be guessed.
//! * [`iso`] — the ISO-8601 shape in both directions: `iso_utc` writes it,
//!   `try_iso` reads it.
//! * [`phrases`] — dates written in words: month and weekday names, `um`/`Uhr`/
//!   `AM`/`PM`, English/French/Spanish ordinals, and the relative phrases.
//! * [`parse`] — the readers by family, their range checks and messages, and
//!   `parse_stamp`, the only way in.
//! * [`window`] — fresh / stale / skew, the 72-hour gate.
//! * [`format`] — the display age the UI shows (`now`, `5 min`, `4 h`, `2 d`,
//!   `in 4 h`).
//! * [`args`] — the arguments of a request, and the clock when "now" is absent.

#![forbid(unsafe_code)]

pub mod args;
pub mod calendar;
pub(crate) mod data;
pub(crate) mod epoch;
pub mod format;
pub(crate) mod iso;
pub mod parse;
pub(crate) mod phrases;
pub(crate) mod scanners;
pub mod window;
#[cfg(test)]
mod tests;

use atlas_lib::cli;

pub use crate::args::clock;
pub use crate::format::{cmd_format, rel_text, rel_unit};
pub use crate::parse::{cmd_parse, parse_stamp, Stamp};
pub use crate::window::{cmd_window, DEFAULT_MAX_AGE_S, DEFAULT_SKEW_S};

/// Dispatch one subcommand. Returns the JSON text to print on stdout.
pub fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input = cli::input_object(raw)?;
    match cmd {
        "parse" => cmd_parse(&input),
        "window" => cmd_window(&input),
        "format" => cmd_format(&input),
        other => Err(format!("unknown subcommand: {}", other)),
    }
}
