//! The `settings_set` arm that owns `data/config.json`.
//!
//! The window writes no file — its archive handle is read-only — so a setting the reader
//! changes there is a row in `data/requests.db`. This module is the half that turns such a row
//! into a settings file, and it is deliberately one call deep:
//!
//! * a payload carrying a credential is refused **before** anything is touched, with the same
//!   sentence both queue doors use (`atlas_store::requests::CREDENTIAL_REFUSAL`) — the
//!   archive's own corpus shows how easily a token ends up inside a question, and this is the
//!   door that would otherwise copy it into `data/config.json`;
//! * every key is validated by [`atlas_config::model_settings`], the same code the CLI runs:
//!   an unknown key is refused *by name* (nothing is dropped silently), a number out of range
//!   is clamped by its SPEC row and the clamp is published through [`atlas_config::warn`], and
//!   `model_local` is put through the path rules before it can become an argv;
//! * the write is [`atlas_config::Config::save`] — atomic (temp file + rename), `0600` in a
//!   `0700` `data/`, refusing a symlinked `config.json` — so there is one writer of that file
//!   in the product, not two;
//! * the row is finished with whatever sentence came back: success stores the count, and a
//!   refusal stores the sentence. A request is an intent, so a refusal is the answer.
//!
//! It does not: start llama.cpp, build the argv (one builder, in `atlas-launcher`), download
//! anything, or decide which keys exist (`atlas-config`'s SPEC table does).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::serve::Acted;

/// The project root this process is working on, from the store it opened.
///
/// `data/atlas.db` → the directory above `data/`, which is the root
/// [`atlas_config::Paths::at`] derives `data/config.json` from. Asked here rather than in the
/// serve loop so the derivation has one spelling and a test: a wrong root would write a
/// settings file next to the wrong archive, and the app would keep reading the old one.
pub fn root_of(store: &atlas_store::Store) -> Option<PathBuf> {
    store
        .path()
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Apply the model settings in one request, or answer with the sentence that refused it.
///
/// `None` means "this request is not about `data/config.json`": either it has no `settings`
/// object at all (the legacy flat shape, which the archive's `search_engine` branch reads) or
/// it names only `search_engine`, which lives in the archive's `meta` table and is written by
/// the search tool's own gate.
pub fn apply(payload: &Value, root: &Path) -> Option<Acted> {
    apply_with_store(payload, root, None)
}

/// [`apply`], with the archive the window reads.
///
/// The window has no access to `data/config.json` — its handle is read-only, and
/// `tests/disclosure.rs` keeps a path off every page — so the numbers it must send on a chat
/// request travel the only way it can see them: the archive's `meta` table, the same table
/// `search_engine` already uses. Mirroring is not a second source of truth: it is written in
/// the same call that saves the file, so the two cannot disagree, and a reader who edits the
/// file by hand still gets the shipped numbers in the window until the next save (the page
/// says so).
pub fn apply_with_store(
    payload: &Value,
    root: &Path,
    store: Option<&atlas_store::Store>,
) -> Option<Acted> {
    let settings = match payload.get("settings") {
        None => return None,
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Some(refused(
                "\"settings\" has to be an object of key/value pairs, like {\"settings\": \
                 {\"model_context\": 65536}} — nothing was written",
            ))
        }
    };
    if !atlas_config::is_settings_request(settings) {
        return None;
    }
    // Refuse, never rewrite: a repaired payload would write a setting the reader did not ask
    // for, and a credential that arrived in one has to be removed at its source. Both of
    // guard's answers are asked, because a key next to the reader's own words is a value
    // (`carries_a_credential`) while a named assignment is caught either way
    // (`credential_in`) — the union is what the store's door does with the same text.
    let text = payload.to_string();
    if atlas_guard::redact::carries_a_credential(&text)
        || atlas_guard::redact::credential_in(&text).is_some()
    {
        return Some(refused(atlas_store::requests::CREDENTIAL_REFUSAL));
    }
    let overrides = match atlas_config::model_settings(settings, root) {
        Ok(overrides) => overrides,
        Err(sentence) => return Some(refused(&sentence)),
    };
    // A clamp is not a refusal, so it is not the row's error: it is a hardening note, published
    // where every other note in this app is (`atlas_config::warnings`, echoed on stderr) — the
    // agent's stdout stays one JSON line per action.
    for note in &overrides.notes {
        atlas_config::warn(note);
    }
    let paths = atlas_config::Paths::at(root);
    match atlas_config::Config::save(&paths, &overrides.map) {
        Ok(_) => {
            if let Some(store) = store {
                mirror(&overrides.map, store);
            }
            Some(Acted {
                stored: overrides.map.len(),
                ..Acted::default()
            })
        }
        Err(error) => Some(refused(&error.to_string())),
    }
}

/// The keys the model server needs mirrored into the archive, for the window that cannot
/// read the settings file.
///
/// Only numbers: `model_local` is a path and never goes into the archive (a page must not be
/// able to name somebody's disk, and the window does not need it — the server already has the
/// model open). A value that would not fit an `i64` is not a number this app writes.
const MIRRORED: [&str; 3] = ["model_context", "model_reasoning", "model_answer"];

/// Write the three numbers into `meta`, ignoring a failure: a mirror that could not be
/// written must not turn a saved setting into a refused one. The window falls back to the
/// shipped numbers and the next save rewrites it.
fn mirror(map: &Map<String, Value>, store: &atlas_store::Store) {
    for key in MIRRORED {
        let Some(value) = map.get(key) else {
            continue;
        };
        let number = match value {
            Value::Number(number) => number.as_i64(),
            _ => None,
        };
        if let Some(number) = number {
            let _ = store.set_meta(key, &number.to_string());
        }
    }
}

/// One finished row that refused, with the sentence a reader can act on.
fn refused(sentence: &str) -> Acted {
    Acted {
        error: Some(sentence.to_string()),
        ..Acted::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    /// A project root with `models/` and one model file in it, and nothing else.
    fn project(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "atlas-monitor-settings-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::create_dir_all(root.join("data")).expect("data");
        std::fs::write(root.join("models/local.gguf"), b"GGUF").expect("model");
        root
    }

    fn stored(root: &Path) -> serde_json::Value {
        let text = std::fs::read_to_string(root.join("data/config.json")).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
    }

    #[test]
    fn a_settings_request_is_written_through_the_config_path_and_nothing_else_changes() {
        let root = project("write");
        let model = root.join("models/local.gguf");
        let payload = json!({
            "settings": {
                "model_local": model.to_string_lossy(),
                "model_context": 65536,
                "model_answer": 4096
            },
            "why": "the model has to hold a whole article"
        });
        let acted = apply(&payload, &root).expect("handled");
        assert_eq!(acted.error, None);
        assert_eq!(acted.stored, 3);
        let saved = stored(&root);
        assert_eq!(saved["model_context"], json!(65536));
        assert_eq!(saved["model_answer"], json!(4096));
        assert!(
            saved["model_local"]
                .as_str()
                .unwrap_or_default()
                .ends_with("models/local.gguf"),
            "{saved}"
        );
        // The file the app will read is the file the CLI would read, and it is 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(root.join("data/config.json"))
                .expect("config")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "config.json is {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_payload_carrying_a_credential_is_refused_and_nothing_is_written() {
        let root = project("credential");
        for payload in [
            json!({
                "settings": {"model_context": 8192},
                "why": "my key is sk-live-abcdefghijklmnop and it keeps failing"
            }),
            json!({
                "settings": {"model_context": "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq"}
            }),
        ] {
            let acted = apply(&payload, &root).expect("handled");
            assert_eq!(
                acted.error.as_deref(),
                Some(atlas_store::requests::CREDENTIAL_REFUSAL),
                "{payload}"
            );
            assert!(!root.join("data/config.json").exists(), "a refusal wrote a file");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unknown_key_is_refused_by_name_and_the_row_carries_the_sentence() {
        let root = project("unknown");
        let payload = json!({"settings": {"model_contex": 8192}});
        let acted = apply(&payload, &root).expect("handled");
        let sentence = acted.error.expect("refused");
        assert!(sentence.contains("\"model_contex\""), "{sentence}");
        assert!(sentence.contains("nothing is dropped silently"), "{sentence}");
        assert!(!root.join("data/config.json").exists(), "a refusal wrote a file");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_number_out_of_range_is_clamped_and_the_note_is_published() {
        let root = project("clamp");
        let payload = json!({"settings": {"model_context": 999_999}});
        let acted = apply(&payload, &root).expect("handled");
        assert_eq!(acted.error, None, "a clamp is not a refusal");
        assert_eq!(stored(&root)["model_context"], json!(262_144));
        let notes = atlas_config::warnings();
        assert!(
            notes.iter().any(|note| note.contains("model_context")
                && note.contains("262144")
                && note.contains("1024 to 262144")),
            "{notes:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_word_where_a_number_belongs_is_refused_and_the_file_keeps_its_bytes() {
        // A written settings file is the case that matters: a reader who already changed one
        // number must not lose it because the next request carried a word. The refusal says
        // which key and which range, and the file is compared byte for byte.
        let root = project("word");
        let first = json!({"settings": {"model_context": 65536}});
        assert_eq!(apply(&first, &root).expect("handled").error, None);
        let before = std::fs::read(root.join("data/config.json")).expect("config");
        let payload = json!({"settings": {"model_answer": "lots"}});
        let sentence = apply(&payload, &root).expect("handled").error.expect("refused");
        assert!(
            sentence.contains("model_answer = \"lots\" is not usable"),
            "{sentence}"
        );
        assert!(sentence.contains("256 to 32768"), "the range is named: {sentence}");
        assert_eq!(
            std::fs::read(root.join("data/config.json")).expect("config"),
            before,
            "a refusal rewrote the settings file"
        );
        assert_eq!(stored(&root)["model_context"], json!(65536), "the first number survived");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_search_engine_request_is_not_this_arms_business() {
        // It lives in the archive's `meta` table and is written by the search tool's own gate;
        // this arm must not write it into `config.json` behind that gate's back.
        let root = project("engine");
        let payload = json!({"settings": {"search_engine": "https://s.example/?q={query}"}});
        assert!(apply(&payload, &root).is_none());
        assert!(apply(&json!({"key": "search_engine", "value": "x"}), &root).is_none());
        assert!(!root.join("data/config.json").exists());
        // A mixed request is refused rather than half-applied.
        let mixed = json!({
            "settings": {
                "model_context": 8192,
                "search_engine": "https://s.example/?q={query}"
            }
        });
        let acted = apply(&mixed, &root).expect("handled");
        let sentence = acted.error.expect("refused");
        assert!(sentence.contains("meta table"), "{sentence}");
        assert!(!root.join("data/config.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_root_comes_from_the_store_and_is_the_one_the_settings_file_lives_beside() {
        // The seam the serve loop uses: the archive is at `<root>/data/atlas.db`, so the root
        // is the directory above `data/`. Measured through a store this test really opens, and
        // checked against the layout `atlas-config` would use for the same root.
        let root = project("root");
        let store = atlas_store::Store::open(&root.join("data/atlas.db")).expect("store");
        let derived = root_of(&store).expect("a root");
        assert_eq!(derived, root, "the derived root is not the project root");
        assert_eq!(
            atlas_config::Paths::at(&derived).config_file,
            root.join("data/config.json"),
            "the settings file is not beside the archive"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_settings_member_that_is_not_an_object_is_refused_with_the_shape_it_wants() {
        let root = project("shape");
        let acted = apply(&json!({"settings": "model_context=8192"}), &root).expect("handled");
        let sentence = acted.error.expect("refused");
        assert!(sentence.contains("has to be an object"), "{sentence}");
        assert!(!root.join("data/config.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
    /// The window cannot read `data/config.json`, so the three numbers it must send on a chat
    /// request are mirrored into the archive in the same call that saves the file. Without
    /// this the page kept asking for its shipped 8 192 tokens whatever the reader set.
    #[test]
    fn the_numbers_a_chat_request_needs_are_mirrored_into_the_archive() {
        let root = project("mirror");
        let store = atlas_store::Store::open(root.join("data/atlas.db")).expect("archive");
        store.init().expect("the archive's schema");

        let acted = apply_with_store(
            &json!({"settings": {"model_reasoning": 12288, "model_answer": 4096}}),
            &root,
            Some(&store),
        )
        .expect("handled");
        assert_eq!(acted.error, None, "{:?}", acted.error);
        assert_eq!(store.get_meta("model_reasoning").ok().flatten().as_deref(), Some("12288"));
        assert_eq!(store.get_meta("model_answer").ok().flatten().as_deref(), Some("4096"));
        assert_eq!(
            store.get_meta("model_context").ok().flatten(),
            None,
            "a key the request did not name is not invented"
        );

        // A path is never mirrored: a page must not be able to name somebody's disk. The
        // absolute path the tool accepts is the one that would leak, so it is the one tested.
        let file = root.join("models/local.gguf");
        let acted = apply_with_store(
            &json!({"settings": {"model_local": file.to_string_lossy()}}),
            &root,
            Some(&store),
        )
        .expect("handled");
        assert_eq!(acted.error, None, "{:?}", acted.error);
        assert_eq!(store.get_meta("model_local").ok().flatten(), None);
    }

}
