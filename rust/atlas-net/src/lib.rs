//! atlas-net — the one place in ATLAS where a request leaves this machine.
//!
//! This crate replaces the transport behind `collector.fetch_page_http` and
//! `collector.read_full_article`: gate a URL, follow redirects *through the gate*,
//! read at most N bytes, decode them, hand the text back. It never writes a file,
//! keeps no cookies, never follows a hop to a non-http(s) scheme, and never asks
//! for a compressed body.
//!
//! Everything security-relevant is delegated:
//!
//! * the URL verdict is `atlas_guard::urlsafe::check` — the same function
//!   `server/guard.py` calls, so at most a *slightly* shorter list of refusals is
//!   possible here, never a longer one;
//! * this crate adds exactly two transport policies of its own, both ported from
//!   `server/collector.py`: [`BLOCKED_PORTS`] (a page must not make the app knock
//!   on ssh) and [`MAX_REDIRECTS`] (a redirect loop is a refusal, not a hang).
//!
//! # The map
//!
//! * [`gate`] — the verdict and its plumbing: the canonical request line, the
//!   port policy, and the DNS answer check. No policy of its own.
//! * [`hop`] — the redirect decision: resolve one `Location` and gate it before
//!   it is requested, inheriting the origin's class and never escalating it.
//! * `resolver` — the address decision where the connection is made: a name's
//!   answer is filtered inside the transport, so a rebinding answer cannot be the
//!   one dialled.
//! * [`fetch`] — the request: client, redirects in order, one decode call.
//! * [`limits`] — how big a fetch may be, and what counts as a page rather than
//!   a file.
//! * [`useragent`] — who the request says it is.
//! * `budget` — the one clock a fetch runs against, so redirects cannot buy a
//!   fresh timeout each.
//! * `reason` — what a failure says to the reader: which phase timed out, that a
//!   certificate was refused, which status came back.
//! * [`charset`] — which charset a response is in, and bytes → text.
//! * [`page`] — the result type: refusal, transport failure, or a page.
//! * `wire` — the JSON answer the `atlas-net` binary prints for one subcommand.
//!
//! Ported behaviour is marked where it matters:
//!
//! * [`gate`]            ← `collector.url_gate()` (native path) + `BLOCKED_PORTS`
//! * [`Client::fetch`]   ← `collector.fetch_page_http()` (`url_gate`, `GatedRedirect`,
//!                         `_checked_final`, `_document_type_ok`, byte cap, charset)
//! * [`check_hop`]       ← `collector.GatedRedirect.redirect_request()`
//! * [`detect_charset`]  ← `re.search(rb'charset=["\']?([\w-]+)', data[:4000])`
//! * [`read_capped`]     ← `data = r.read(max_bytes + 1)[:max_bytes]`
//!
//! Deliberate differences, all of them documented at the point of difference:
//! `Accept-Encoding: identity` (Python asked for gzip), no decompression of our
//! own *and none in the transport either* — the workspace builds `ureq` without its
//! `gzip` feature, so a compressed answer arrives with its `Content-Encoding` intact
//! and `fetch::exchange` refuses it by name — and a decode that cannot panic on
//! invalid bytes.
//!
//! ```text
//! atlas-net gate   <<< '{"url":"https://example.com/a"}'
//! atlas-net fetch  <<< '{"url":"https://example.com/a","max_bytes":600000}'
//! ```

#![forbid(unsafe_code)]

pub mod charset;
pub mod fetch;
pub mod gate;
pub mod hop;
pub mod limits;
pub mod page;
pub mod resolver;
pub mod useragent;
pub mod wire;

// Two decisions that are not part of the public surface: the fetch's clock
// (`budget`) and the wording of a failure (`reason`). Both are used by `fetch`
// alone, so neither is re-exported — a caller reads [`Page::error`] instead.
mod budget;
mod reason;

#[cfg(test)]
mod server;
#[cfg(test)]
mod attacks;
#[cfg(test)]
mod tests;

// Every path this crate published before the split, re-exported at the crate root
// so no caller needs a single edit.
pub use charset::{decode, detect_charset, META_SCAN_BYTES};
pub use fetch::{Client, TIMEOUT_CONNECT, TIMEOUT_TOTAL};
pub use gate::{
    approved_addresses, approved_addresses_with, gate, resolve_gate, Addresses, Verdict,
    BLOCKED_PORTS, MAX_URL_CHARS,
};
pub use hop::{check_hop, check_hop_from, same_origin, MAX_REDIRECTS};
pub use limits::{
    clamp_bytes, is_document_type, read_capped, DEFAULT_MAX_BYTES, HARD_MAX_BYTES,
    MAX_HEADER_BYTES,
};
pub use page::Page;
pub use resolver::{Class, Lookup, SystemLookup};
pub use useragent::user_agent;
