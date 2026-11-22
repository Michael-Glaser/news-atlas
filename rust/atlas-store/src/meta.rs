//! The `meta` table: small key/value markers, and the one secret check in the
//! storage path.
//!
//! Decisions: a key/value pair that is credential material is stored as `[redacted]` and the
//! caller is told so; everything else is stored verbatim. The verdict is `atlas-guard`'s,
//! asked as "is this pair a credential" ([`atlas_guard::redact::credential_in`]).
//!
//! A **URL with a credential in it** is the one case that is neither: replacing the whole
//! value with `[redacted]` would keep the row and lose the location, which reads as "the
//! setting was saved" while the app silently falls back to its built-in endpoint (measured on
//! the search template: `…&token=abc` became `[redacted]`, `set_meta` answered `true`, and
//! `atlas-ingest::search` then used its own engine). So that pair is refused with a sentence
//! naming the fix, and the caller decides what to show.
//!
//! It does not: keep a vocabulary of its own (guard owns password/secret/token names),
//! encrypt anything, or log a value. ATLAS stores no secrets at all; a redacted marker is how
//! a caller learns that.

use crate::error::{Result, StoreError};
use crate::params::SqlValue;
use crate::Store;

/// The longest meta key ATLAS stores, in bytes.
///
/// A key names a marker, not a document: `last_enrich`, `search_engine`, and
/// `enrich:<article id>` are the shapes in use, all under 40 bytes.
pub const MAX_KEY_BYTES: usize = 256;

/// The longest meta value ATLAS stores, in bytes.
///
/// The largest value any caller writes is the search engine's URL template, and the
/// gate refuses a template longer than 2 048 characters — so 8 KiB is room around the
/// biggest legitimate value, and a megabyte row is not a marker any more. Counted in
/// **Rust**, not in a SQL `CHECK(length(v) <= n)`: SQLite stops reading a string at its
/// first NUL, so a value of `"\0"` plus a megabyte is `length(v) = 0` to SQL while being
/// 1 048 577 characters to us.
pub const MAX_VALUE_BYTES: usize = 8 * 1024;

/// The key whose value is a URL template the search gate has to be able to build.
pub const ENGINE_KEY: &str = "search_engine";

/// The longest engine template ATLAS stores, in bytes.
///
/// A template is stored so a search can be built from it, and the gate judges the URL it
/// *builds* — which is never shorter than the template. Measured while the generic cap was
/// in force here: a 5 000-character template was accepted, `Engine::checked` approved it
/// (its probe is cut at 2 048), `resolve` reported `source=configured`, and every search then
/// died at the transport's own length check — a setting Settings called *yours* while the
/// agent used something else. The cap for this key is the gate's number
/// (`atlas_ingest::gate::MAX_URL_CHARS`), spelled once here because this crate must not link
/// the one that owns the gate.
pub const MAX_ENGINE_TEMPLATE_BYTES: usize = 2_048;

impl Store {
    /// Store one key/value marker (`db.py: set_meta`). A credential-shaped pair is
    /// stored as `[redacted]`: ATLAS needs no secrets. Returns whether the value
    /// was replaced.
    ///
    /// The decision is `atlas-guard`'s, asked as "is this pair a credential"
    /// ([`pair_is_secret`]). `db.py` asks `config.key_is_secret`, a regex over the key name;
    /// guard looks at the pair, which catches a token handed to an innocuous key name and
    /// misses a credential-shaped name with a short value. Neither side keeps a second copy of
    /// the other's vocabulary.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<bool> {
        if key.len() > MAX_KEY_BYTES {
            return Err(StoreError::new(format!(
                "a meta key is capped at {MAX_KEY_BYTES} bytes; this one is {}. A key names \
                 a marker, not a document.",
                key.len()
            )));
        }
        let cap = if key == ENGINE_KEY {
            MAX_ENGINE_TEMPLATE_BYTES
        } else {
            MAX_VALUE_BYTES
        };
        if value.len() > cap {
            return Err(StoreError::new(format!(
                "a meta value is capped at {cap} bytes; this one is {}. {why}",
                value.len(),
                why = if key == ENGINE_KEY {
                    "A search template has to fit in a URL the gate can judge, so it carries \
                     the gate's cap rather than the store's."
                } else {
                    "The value is a marker, not a document."
                }
            )));
        }
        let redacted = pair_is_secret(key, value);
        if redacted && is_location(value) {
            return Err(StoreError::new(format!(
                "{key} is a location with a credential in it. ATLAS stores no secrets, and \
                 storing the redacted marker instead would lose the address and quietly fall \
                 back to a default — so nothing is stored. Remove the credential from the URL \
                 and save it again; a template that needs a key belongs in the environment \
                 variable the caller documents, which is never written to the archive."
            )));
        }
        let stored = if redacted { "[redacted]" } else { value };
        self.exec(
            "INSERT INTO meta(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            &[
                SqlValue::Text(key.to_string()),
                SqlValue::Text(stored.to_string()),
            ],
        )?;
        Ok(redacted)
    }

    /// `db.py: get_meta`.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let row = self.query_one(
            "SELECT v FROM meta WHERE k=?",
            &[SqlValue::Text(key.to_string())],
        )?;
        Ok(row.and_then(|row| row.text("v").map(|text| text.to_string())))
    }
}

/// Is this key/value pair credential material?
///
/// `db.py: set_meta` asks `config.key_is_secret`, a regex over the *key name*. That regex is
/// Python and is going away, so the question is put to `atlas-guard`: is this pair a
/// credential? Its vocabulary decides — password/secret/token/api_key/authorization/bearer
/// and token-shaped values — which is broader than `db.py` for values and narrower for a key
/// name whose credential word is part of a longer word (`openai_api_key`). Either way the
/// decision lives in one place instead of two.
///
/// `credential_in`, not `redact(...).hits`: `redact` answers "what should be printed with a
/// marker in it", which includes a token inside any quoted string — and the archive's own
/// `research_result` row is workflow JSON whose every step names a URL, most of them Google
/// News links. A gate asks "is this value a credential", and measured against the archive
/// those are different questions: 10 hits by `redact`'s answer, 0 by this one
/// (`atlas-ingest/tests/redaction_corpus.rs`).
fn pair_is_secret(key: &str, value: &str) -> bool {
    let probe = format!("{} = {}", key, value);
    // Two questions, both put to `atlas-guard`: is the *pair* a named credential, and is the
    // *value* a credential on its own. The second is what catches a setting that is nothing
    // but a location with an opaque token in its path — `search_engine` is used, not quoted,
    // so its path is a key position and not a citation (`credential_in_a_setting`).
    atlas_guard::redact::credential_in(&probe).is_some()
        || atlas_guard::redact::credential_in_a_setting(value).is_some()
}

/// Is this value an address rather than a secret?
///
/// The distinction matters only for the refusal above: a token that is a token can be
/// replaced by `[redacted]` and nothing is lost, but an http(s) URL is a place the app
/// has to be able to go, so replacing it with a marker loses the setting while looking
/// like it was saved.
fn is_location(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::{MAX_KEY_BYTES, MAX_VALUE_BYTES};
    use crate::tests::*;

    #[test]
    fn meta_round_trips_and_carries_no_credential() {
        // Rewritten after an accident (a bad shell redirect truncated this file; see the
        // report). The behaviour it pins is the same one `tests/search_engine_meta.rs` pins
        // from outside the crate: a plain marker round trips, an update replaces it, and a
        // credential under a credential-shaped key is stored as the marker instead.
        let store = store();
        assert_eq!(store.get_meta("last_dedupe").expect("meta"), None);
        assert!(!store
            .set_meta("last_dedupe", "2026-11-05T10:00:00Z")
            .expect("set"));
        assert_eq!(
            store.get_meta("last_dedupe").expect("meta").as_deref(),
            Some("2026-11-05T10:00:00Z")
        );
        store
            .set_meta("last_dedupe", "2026-11-06T10:00:00Z")
            .expect("set");
        assert_eq!(
            store.get_meta("last_dedupe").expect("meta").as_deref(),
            Some("2026-11-06T10:00:00Z"),
            "an upsert replaces the marker"
        );

        // A credential: stored as the marker, and the caller is told (the return value is
        // the verdict).
        assert!(store
            .set_meta("openai_api_key", "sk-live-abcdefghijklmnop")
            .expect("set"));
        assert_eq!(
            store.get_meta("openai_api_key").expect("meta").as_deref(),
            Some("[redacted]")
        );
        // The whole table holds no credential-shaped text afterwards.
        let text: String = store
            .query("SELECT k, v FROM meta", &[])
            .expect("rows")
            .iter()
            .filter_map(|row| row.text("v").map(|value| value.to_string()))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(!text.contains("sk-live"), "{text}");
    }

    #[test]
    fn a_location_with_a_credential_is_refused_not_marked() {
        // The one pair that is neither stored nor marked: a redacted URL would look saved and
        // leave the app on its built-in endpoint, so nothing is written and the sentence names
        // the fix.
        let store = store();
        let refused = store
            .set_meta(
                "search_engine",
                "https://search.example/?q={query}&api_key=sk-live-abcdefghijklmnop",
            )
            .expect_err("refused");
        let message = refused.message();
        assert!(message.contains("credential"), "{message}");
        assert!(message.contains("nothing is stored"), "{message}");
        assert!(!message.contains("sk-live"), "the refusal repeats no value: {message}");
        assert_eq!(store.get_meta("search_engine").expect("meta"), None);

        // A location without one is stored whole.
        let plain = "https://search.example/?q={query}&format=RSS";
        assert!(!store.set_meta("search_engine", plain).expect("set"));
        assert_eq!(
            store.get_meta("search_engine").expect("meta").as_deref(),
            Some(plain)
        );
    }

    #[test]
    fn a_research_result_row_with_google_ids_in_it_is_saved() {
        // The measured row (shape, not the full 8011 characters): workflow JSON whose every
        // step names a URL, most of them Google News links whose *path* is a 235-byte
        // base64url id. `redact` marked 10 spans in the live row and `set_meta` refused it —
        // the app could no longer save its own research output. The gate asks
        // `credential_in`, and a path segment is a citation, not a credential.
        let store = store();
        let research = concat!(
            "{\"topic\":\"chips\",\"steps\":[{\"tool\":\"find_news\",\"args\":{\"url\":\"",
            "https://news.google.com/rss/articles/CBMirAFBVV95cUxNNlM4aDBCQjFpWWp6NFp3aU1wUVI2",
            "LXJLeFlFSmo3c2RiZ1hhbDgxOFJaWWtHc1JiYzBXd29mVXlsbzl3ZHF2a3RzQlZ1SFAtWVFWWXdITXpJ",
            "MXU0a1RfZFVfQ1E5aHRGYjJtMFo3Y2p0dlpFZU1qbFd3S0dNYVVZZVM3cm1Lc3hkcDd0MGJBQkVRaHE4V21V",
            "VVc1cjdVaDZkTGlCQXFrdTByQXVj?oc=5\"}},\"result\":\"12 items\"}},{\"tool\":\"read_page\",",
            "\"args\":{\"url\":\"",
            "https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/",
            "\"},\"result\":\"stored\"}}]}"
        );
        assert!(
            research.len() > 400,
            "the fixture is the shape, not a stub: {} bytes",
            research.len()
        );
        let replaced = store
            .set_meta("research_result", research)
            .expect("the row must save");
        assert!(!replaced, "it is not a credential, so nothing was redacted");
        assert_eq!(
            store.get_meta("research_result").expect("get").as_deref(),
            Some(research),
            "the row comes back word for word"
        );

        // The same row with a credential added to it is a credential, and the store still
        // keeps no secret: the pair is marked, not stored. (A *location* is refused instead,
        // because marking an address loses the setting — see the test above.)
        let with_key = format!(
            "{}{}",
            research.trim_end_matches('}'),
            ",\"api_key\":\"sk-live-abcdefghijklmnop\"}"
        );
        assert!(
            store.set_meta("research_result", &with_key).expect("set"),
            "a credential in the row is replaced by the marker"
        );
        assert_eq!(
            store.get_meta("research_result").expect("get").as_deref(),
            Some("[redacted]")
        );
    }

    #[test]
    fn the_caps_are_counted_in_rust_and_change_nothing_on_disk() {
        // `tests/search_engine_meta.rs` pins the same boundary from outside the crate, with
        // the sentence's two numbers; this is the short form, kept here so the cap cannot be
        // dropped without a failure in the module that owns it.
        let store = store();
        let key = "k".repeat(MAX_KEY_BYTES + 1);
        assert!(store.set_meta(&key, "value").is_err(), "a key over the cap");
        let value = "A".repeat(MAX_VALUE_BYTES + 1);
        assert!(
            store.set_meta("megabyte", &value).is_err(),
            "a value over the cap"
        );
        assert_eq!(store.get_meta("megabyte").expect("get"), None);
        // The boundary itself is inside: a value exactly at the cap is stored whole.
        let at_cap = "A".repeat(MAX_VALUE_BYTES);
        assert!(!store.set_meta("at_cap", &at_cap).expect("set"));
        assert_eq!(
            store.get_meta("at_cap").expect("get").as_deref(),
            Some(at_cap.as_str())
        );
    }
}
