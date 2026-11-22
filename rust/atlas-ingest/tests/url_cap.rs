//! §4.1's URL cap, across the three crates that carry a copy of it.
//!
//! `docs/SECURITY-PROOF.md` §7 item 3 is OPEN: "`MAX_URL_CHARS` exists more than
//! once" — `atlas-guard/src/urlsafe.rs` (private), `atlas_net::MAX_URL_CHARS` and
//! `atlas_ingest::gate::MAX_URL_CHARS`, all `2048`. The duplication is the
//! residual. What a drift would cost: a URL one layer accepts and the next refuses,
//! or one layer truncating where another does not — a two-hundred-character
//! difference is enough to make a request line to a different resource than the one
//! the gate approved.
//!
//! This file lives in `atlas-ingest` because it is the only crate that depends on
//! both of the others (`atlas-net` for the request line, `atlas-guard` for the
//! verdict). It is a pin, not a fix: the fix is to delete two of the three and have
//! both crates use `atlas_guard`'s, which `atlas-guard` must then export.

use atlas_ingest::gate::MAX_URL_CHARS as INGEST;

/// The longest URL the fetch policy looks at.
const CAP: usize = 2048;

/// A URL of exactly `chars` characters, at a public host.
fn url_of_length(chars: usize) -> String {
    let base = "https://example.com/";
    assert!(chars > base.len(), "a {}-character URL has no room for a path", chars);
    format!("{}{}", base, "a".repeat(chars - base.len()))
}

#[test]
fn the_three_copies_of_the_url_cap_are_one_number() {
    assert_eq!(INGEST, CAP, "atlas-ingest's cap moved");
    assert_eq!(atlas_net::MAX_URL_CHARS, CAP, "atlas-net's cap moved");
}

#[test]
fn the_guard_and_the_request_line_stop_at_the_same_character() {
    // The guard's copy is private, so it is measured through its own gate rather
    // than read: exactly `CAP` characters is looked at, `CAP + 1` is refused before
    // the URL is parsed.
    let at_cap = url_of_length(CAP);
    let one_over = format!("{}a", at_cap);
    assert_eq!(at_cap.chars().count(), CAP);
    assert!(
        atlas_guard::urlsafe::check(&at_cap, false).ok,
        "the guard refuses a URL of exactly its cap"
    );
    let refused = atlas_guard::urlsafe::check(&one_over, false);
    assert!(!refused.ok, "the guard accepted {} characters", CAP + 1);
    assert!(
        refused.reason.to_lowercase().contains("long"),
        "the refusal does not name the length: {}",
        refused.reason
    );

    // The layer that turns the verdict into a request line stops at the same
    // character — the point of pinning the two to each other rather than to `2048`.
    assert!(
        atlas_net::gate(&at_cap).ok,
        "atlas-net refuses a URL of exactly the cap the guard accepts"
    );
    assert!(
        !atlas_net::gate(&one_over).ok,
        "atlas-net accepted a URL {} characters long",
        CAP + 1
    );
}
