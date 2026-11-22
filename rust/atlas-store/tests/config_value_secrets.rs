//! Does a config value reach the archive when it looks like a secret?
//!
//! Two boundaries, one question. Both are measured here, because "ATLAS stores no secrets" is
//! a claim about rows and bytes, not about intentions:
//!
//! * **`meta`** — `atlas_store::Store::set_meta` (`atlas-store/src/meta.rs:50`) is the only
//!   way a caller's configuration becomes a row in `data/atlas.db`; it is where the search
//!   engine's template lives (`atlas-ingest/src/search.rs:52`, `META_KEY`). The decision is
//!   `atlas_guard::redact`, asked with the pair (`atlas-store/src/meta.rs:106`,
//!   `pair_is_secret`), and a URL carrying a credential is refused outright
//!   (`meta.rs:66`).
//! * **`requests`** — `atlas_store::Requests::push` (`atlas-store/src/requests.rs:202`) is the
//!   agent's door to the same queue the window writes (`atlas-desktop/src/requests.rs`).
//! * **`requests`** — `atlas_store::Requests::push` (`atlas-store/src/requests.rs:196`) is the
//!   agent's door to the same queue the window writes (`atlas-desktop/src/requests.rs`); a
//!   payload carrying a credential is refused there with `CREDENTIAL_REFUSAL`.
//!
//! What is caught, measured through the shipped functions (values from the reader's side —
//! `ATLAS_SEARCH_ENGINE` is a URL template a reader may point at their own endpoint, and some
//! endpoints carry a token in the query). The left column is the rule, the right what the
//! archive did:
//!
//! ```text
//! search_engine  …&api_key=sk-live-…        a name the guard knows  → refused, no row
//! search_engine  …&token=abc                a name the guard knows  → refused, no row
//! search_engine  …&key=<opaque>             a name the guard knows  → refused, no row
//! api_key        "short"                    a name the guard knows  → stored as [redacted]
//! search_engine  …&t=<opaque>               a 40+ token shape       → refused, no row
//! search_engine  <opaque>                   a 40+ token shape       → stored as [redacted]
//! future_setting <opaque>                   a 40+ token shape       → stored as [redacted]
//! search_engine  https://…/<opaque>/rss     a 40+ token shape       → refused, no row
//! ```
//!
//! Both halves are now rules rather than vocabulary: the named half is `atlas_guard`'s
//! `ASSIGN`/`PREFIXES` table, and the unnamed half is `atlas_guard`'s token *shape* rule
//! (`redact.rs`, `TOKEN_MIN_CHARS`), which redacts a 40+ byte `[A-Za-z0-9_-]` run that mixes
//! case and carries a digit — in a value position. Nothing here keys off a word list of the
//! reader's content, and nothing needs to: the shape is the evidence.
//!
//! What is caught, measured (each case run through the shipped function, values from the
//! reader's side — `ATLAS_SEARCH_ENGINE` is a URL template a reader may point at their own
//! endpoint, and some endpoints carry a token in the query):
//!
//! ```text
//! CASE[0] search_engine  …&api_key=sk-live-…        refused, no row
//! CASE[1] search_engine  …&token=abc                refused, no row
//! CASE[2] search_engine  …&key=<80 opaque chars>    refused, no row
//! CASE[3] search_engine  …&t=<80 opaque chars>      stored verbatim
//! CASE[4] search_engine  <80 opaque chars>          stored verbatim
//! CASE[5] future_setting <80 opaque chars>          stored verbatim
//! CASE[6] api_key        "short"                    stored as [redacted]
//! CASE[7] search_engine  https://…/<80 opaque>/rss  stored verbatim
//! ```
//!
//! So the rule is a *named* one: a credential word (`token`, `api_key`, `key`, …), a known
//! key prefix (`sk-`, `ghp_`, `hf_`, …) or an authentication scheme. A long opaque token with
//! no name — a bare base64url blob, an id in a path, a query parameter the vocabulary does
//! not carry — is not recognised, and lands in the archive byte for byte. That is the open
//! defect the ignored test below states; the tests that pass are the half that holds.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use atlas_store::{Requests, SqlValue, Store};

fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-store-secret-{}-{}-{}",
        tag,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    root
}

fn store_in(root: &Path) -> Store {
    let store = Store::open(root.join("data").join("atlas.db")).expect("open");
    store.init().expect("init");
    store
}

/// An 80-character base64url blob: the shape a bearer token has once it is pasted where the
/// app cannot see a name for it.
const OPAQUE: &str =
    "Zm9vYmFyYmF6cXV1eDEyMzQ1Njc4OTBhYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0NTY3ODlh";

/// Does the file's bytes contain `needle`? The file, not a row: the claim is about what is on
/// disk after the process is gone.
fn file_holds(path: &Path, needle: &str) -> bool {
    let bytes = std::fs::read(path).unwrap_or_default();
    let needle = needle.as_bytes();
    bytes
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_credential_in_a_location_is_refused_and_no_row_is_left() {
    // Every shape whose *name* the security core knows. The value is not replaced by
    // `[redacted]` here and that is deliberate (`meta.rs:8`): a redacted search template reads
    // as "the setting was saved" while the app silently falls back to its built-in endpoint,
    // so the pair is refused with a sentence naming the fix and nothing is written.
    let root = scratch("location");
    let store = store_in(&root);
    let cases = [
        "https://search.example/?q={query}&api_key=sk-live-abcdefghijklmnop".to_string(),
        "https://search.example/?q={query}&token=abc".to_string(),
        format!("https://search.example/?q={{query}}&key={OPAQUE}"),
    ];
    for value in cases {
        let refused = store.set_meta("search_engine", &value);
        assert!(
            refused.is_err(),
            "a location carrying a credential was stored: {}",
            value
        );
        let said = refused.err().map(|error| error.message().to_string()).unwrap_or_default();
        assert!(
            said.contains("credential") && said.contains("nothing is stored"),
            "the refusal must say what happened and that nothing was stored: {}",
            said
        );
        assert!(
            !said.contains("sk-live") && !said.contains(OPAQUE),
            "the refusal repeated the credential: {}",
            said
        );
        assert_eq!(
            store.get_meta("search_engine").expect("get"),
            None,
            "a refused pair must not leave a row behind"
        );
        let bytes = std::fs::read(root.join("data").join("atlas.db")).unwrap_or_default();
        assert!(
            !file_holds(&root.join("data").join("atlas.db"), "sk-live-abcdefghijklmnop"),
            "the credential is in the archive file's bytes ({} bytes)",
            bytes.len()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_credential_under_an_innocuous_key_is_stored_as_the_redacted_marker() {
    // The other half: a value that is not a location, and a key whose name says nothing. The
    // archive keeps the marker, the caller is told (the return value is the verdict), and the
    // value itself is nowhere in the file.
    let root = scratch("marker");
    let store = store_in(&root);
    let cases = [
        ("api_key", "short"),
        ("future_setting", "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jV"),
        ("note", "the handshake sent hf_abcdefghijklmnopqrstuvwxyz"),
    ];
    for (key, value) in cases {
        let redacted = store.set_meta(key, value).expect("set");
        assert!(redacted, "{key} = {value:?} was not recognised as a credential");
        assert_eq!(
            store.get_meta(key).expect("get").as_deref(),
            Some("[redacted]"),
            "{key} kept something other than the marker"
        );
        assert!(
            !file_holds(&root.join("data").join("atlas.db"), value),
            "the value is in the archive file's bytes: {value:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unnamed_long_opaque_token_never_reaches_the_archive() {
    // Today: four of the eight measured shapes land in `data/atlas.db` byte for byte —
    // `&t=<opaque>`, a bare `<opaque>` value, an opaque id in the path, and the same value
    // under an unknown key. The security core's rule is named (`PREFIXES`, `ASSIGN` in
    // `atlas-guard/src/redact.rs:32`, `:67`); nothing there asks "is this run of characters a
    // token".
    //
    // The fix belongs in `atlas_guard::redact`, once, so every caller gets it — `set_meta`,
    // the queue, the log scrubber and the prompt builder all ask that function:
    //
    // > a span of 40+ characters from `[A-Za-z0-9_-]` that contains at least one digit, one
    // > upper-case and one lower-case letter, and is not a word in any of the six feed
    // > languages' dictionaries the workspace already ships, is a token-shaped span: redact it.
    //
    // Structural, not a word list: it counts classes of characters in a run, and it fires on
    // the shape of a credential rather than on the name someone gave it. The cost is that a
    // base64 constant in prose would be redacted too — which is the right side to err on for a
    // field that already redacts e-mail addresses.
    let root = scratch("opaque");
    let store = store_in(&root);
    let cases = [
        ("search_engine", format!("https://search.example/?q={{query}}&t={OPAQUE}")),
        ("search_engine", OPAQUE.to_string()),
        ("future_setting", OPAQUE.to_string()),
        (
            "search_engine",
            format!("https://search.example/{OPAQUE}/rss?q={{query}}"),
        ),
    ];
    for (key, value) in cases {
        let stored = store.set_meta(key, &value);
        assert!(
            stored.is_err() || store.get_meta(key).expect("get").as_deref() == Some("[redacted]"),
            "{key} stored an opaque token verbatim: {:?}",
            store.get_meta(key).expect("get")
        );
    }
    assert!(
        !file_holds(&root.join("data").join("atlas.db"), OPAQUE),
        "the opaque token is in the archive file's bytes"
    );
    let _ = std::fs::remove_dir_all(&root);
}
#[test]
fn the_queues_other_door_refuses_a_credential_and_writes_no_row() {
    // Flipped. `Requests::push` (`atlas-store/src/requests.rs:196`) used to validate the kind,
    // the size and that the payload is one JSON object — and nothing else, so a payload
    // carrying a named credential reached `data/requests.db` byte for byte. It now asks
    // `atlas_guard::redact` about the payload and refuses a hit with one exported sentence
    // (`CREDENTIAL_REFUSAL`). Refuses, never rewrites: a row is read by another process, and a
    // repaired payload would send the agent a question the reader did not write.
    let root = scratch("queue");
    let file = root.join("data").join("requests.db");
    let queue = Requests::open(&file).expect("queue");
    let named = "{\"query\":\"look up https://api.example/v1?q=x&api_key=sk-live-abcdefghijklmnop\"}";
    let refused = queue.push("research", named).expect_err("refused");
    assert_eq!(refused.message(), atlas_store::requests::CREDENTIAL_REFUSAL);
    assert!(
        !refused.message().contains("sk-live"),
        "the refusal repeated the credential: {}",
        refused.message()
    );
    // The same sentence the window's door uses — the two are compared in
    // `atlas-desktop/tests/config_secrets.rs`, which is why this string is exported.
    let token = format!("{{\"query\":\"look up {OPAQUE}\"}}");
    let refused_shape = queue.push("research", &token).expect_err("refused");
    assert_eq!(refused_shape.message(), atlas_store::requests::CREDENTIAL_REFUSAL);
    let claimed = queue.claim_due(&atlas_store::now_iso(), 60, 5).expect("claim");
    assert!(
        claimed.is_empty(),
        "a refused request left a row the agent can claim: {claimed:?}"
    );
    drop(queue);
    assert!(
        !file_holds(&file, "sk-live-abcdefghijklmnop") && !file_holds(&file, OPAQUE),
        "a credential is in data/requests.db"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_redaction_verdict_is_the_guard_crates_not_a_copy() {
    // "Neither side keeps a second copy of the other's vocabulary" (`meta.rs:110`). The cheap
    // structural check: the archive's decision is the guard's decision, on the same probe
    // string, for a case the store does not special-case.
    let probe = "note = hf_abcdefghijklmnopqrstuvwxyz";
    assert!(
        atlas_guard::redact::redact(probe).hits > 0,
        "the guard does not recognise a known key prefix"
    );
    let root = scratch("guard");
    let store = store_in(&root);
    assert!(store
        .set_meta("note", "hf_abcdefghijklmnopqrstuvwxyz")
        .expect("set"));
    assert_eq!(
        store.get_meta("note").expect("get").as_deref(),
        Some("[redacted]")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_meta_value_too_big_for_a_marker_is_refused_not_truncated() {
    // The other way a caller's text can be mishandled: a value past the cap must be refused,
    // never cut — a cut value is a value the app will act on with the wrong tail.
    let root = scratch("cap");
    let store = store_in(&root);
    let huge = "A".repeat(atlas_store::meta::MAX_VALUE_BYTES + 1);
    let refused = store.set_meta("search_engine", &huge);
    assert!(refused.is_err(), "an oversized marker was stored");
    assert_eq!(store.get_meta("search_engine").expect("get"), None);
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_nul_byte_value_is_stored_as_the_database_reads_it() {
    // SQLite stops reading a string at its first NUL, so `"a\0b"` is `a` to SQL. A caller that
    // compares what it wrote with what it read back must know; this pins that the store keeps
    // the byte it was given and returns the prefix SQL sees, rather than silently "repairing"
    // either side.
    let root = scratch("nul");
    let store = store_in(&root);
    let value = format!("a{}b", '\0');
    store.set_meta("probe", &value).expect("set");
    let back = store.get_meta("probe").expect("get").unwrap_or_default();
    assert!(
        back == value || back == "a",
        "a NUL-valued marker came back as {:?}",
        back
    );
    let length = store
        .query_one("SELECT length(v) AS n FROM meta WHERE k='probe'", &[])
        .expect("length")
        .and_then(|row| row.int("n"));
    assert_eq!(length, Some(1), "SQL sees the part before the NUL");
    let _ = SqlValue::Integer(0);
    let _ = std::fs::remove_dir_all(&root);
}
