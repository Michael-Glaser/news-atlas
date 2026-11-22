//! The page's keys against the agent's gate — asked here because this crate is the one that
//! has both halves in reach: `serde_json` to build the payload, and `atlas-config` to judge it.
//!
//! The chain the Settings page depends on is three links, and every one of them can rename a
//! key on the way:
//!
//! 1. the page writes `{"settings": {…}, "why": "…"}` with only the keys the reader changed
//!    (`atlas-desktop/src/requests.rs:165`, built from `changes()` in
//!    `views/settings.rs:869`);
//! 2. the agent hands that map to `atlas_config::model_settings`
//!    (`atlas-monitor/src/settings_set.rs:92`), which refuses an unknown key *by name* and
//!    clamps a known one to its SPEC row;
//! 3. `atlas_config::Config::save` writes it, and `atlas-launcher`'s `llama::argv` turns it
//!    into the flags — the argv half is pinned by
//!    `atlas-launcher/tests/settings_reach.rs::a_model_named_in_settings_is_the_file_the_server_is_started_with`.
//!
//! This file is link 2, from the page's side: every name the page can send, the answer the
//! agent gives it, and the one name the brief calls `model_path`.

use serde_json::{json, Map, Value};

/// A project root with `models/` and one model file in it; the crate's own scratch helper.
fn root_with_a_model(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "atlas-settings-contract-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("models")).expect("models/");
    let model = root.join("models").join("local.gguf");
    std::fs::write(&model, b"GGUF a stand-in for the weights").expect("a model");
    (root, model)
}

/// The six names `atlas-desktop/src/views/settings.rs:542-547` writes, with a value inside
/// each one's range.
fn page_keys(model: &std::path::Path) -> Vec<(&'static str, Value)> {
    vec![
        ("model_local", json!(model.to_string_lossy())),
        ("model_context", json!(65_536)),
        ("model_reasoning", json!(12_288)),
        ("model_answer", json!(4_096)),
        ("model_gpu_layers", json!(7)),
        ("model_threads", json!(4)),
    ]
}

/// Every key the page can send is a key the agent applies, unchanged and under its own name:
/// nothing is renamed on the way to `data/config.json`, and nothing is dropped.
#[test]
fn every_key_the_settings_page_sends_the_agent_applies() {
    let (root, model) = root_with_a_model("keys");
    for (key, value) in page_keys(&model) {
        let mut settings: Map<String, Value> = Map::new();
        settings.insert(key.to_string(), value);
        // The exact function the agent's `settings_set` arm calls.
        let overrides = atlas_config::model_settings(&settings, &root)
            .unwrap_or_else(|sentence| panic!("the agent refuses {key}: {sentence}"));
        assert_eq!(overrides.map.len(), 1, "{key} was dropped: {:?}", overrides.map);
        assert!(
            overrides.map.contains_key(key),
            "{key} was renamed to {:?}",
            overrides.map.keys().collect::<Vec<_>>()
        );
        assert!(overrides.notes.is_empty(), "a value inside its range was noted: {:?}", overrides.notes);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// All six in one press — the shape a reader who changed several fields produces — reach the
/// file in one save, still under their own names.
#[test]
fn a_press_that_changes_six_keys_writes_six_keys() {
    let (root, model) = root_with_a_model("six");
    let mut settings: Map<String, Value> = Map::new();
    for (key, value) in page_keys(&model) {
        settings.insert(key.to_string(), value);
    }
    let overrides = atlas_config::model_settings(&settings, &root).expect("all six are settable");
    assert_eq!(overrides.map.len(), 6, "{:?}", overrides.map);
    let saved = atlas_config::Config::save(&atlas_config::Paths::at(&root), &overrides.map)
        .expect("the settings file is writable");
    assert_eq!(saved.model_context, 65_536);
    assert_eq!(saved.model_reasoning, 12_288);
    assert_eq!(saved.model_answer, 4_096);
    assert_eq!((saved.model_gpu_layers, saved.model_threads), (7, 4));
    // The stored path is the resolved one (macOS resolves /var to /private/var through the
    // temp directory), so the two are compared as the files they name, not as text.
    assert_eq!(
        std::fs::canonicalize(&saved.model_local).expect("the saved path resolves"),
        std::fs::canonicalize(&model).expect("the model resolves"),
        "the saved model_local is not the file the page sent"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `model_path` is not a setting: the agent refuses it by name and names the one that works.
/// The page sends `model_local`, so nothing is refused in practice — this test is what makes
/// that sentence true rather than assumed.
#[test]
fn model_path_is_refused_by_name_and_model_local_is_named_in_the_fix() {
    let (root, model) = root_with_a_model("model-path");
    let mut settings: Map<String, Value> = Map::new();
    settings.insert("model_path".to_string(), json!(model.to_string_lossy()));
    let sentence = atlas_config::model_settings(&settings, &root)
        .expect_err("model_path must not be accepted");
    assert!(sentence.contains("model_path"), "{sentence}");
    assert!(sentence.contains("model_local"), "the working name is not offered: {sentence}");
    assert!(sentence.contains("nothing is dropped silently"), "{sentence}");
    // And the file was not written by the refusal.
    assert!(!atlas_config::Paths::at(&root).config_file.exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// A number outside its range is clamped, not refused, and the clamp is a note — the shape
/// the page's own field check relies on for a value typed into the *file* by hand.
#[test]
fn a_value_outside_its_range_is_clamped_with_a_note() {
    let (root, _) = root_with_a_model("clamped");
    let mut settings: Map<String, Value> = Map::new();
    settings.insert("model_context".to_string(), json!(999_999));
    let overrides = atlas_config::model_settings(&settings, &root).expect("clamped, not refused");
    assert_eq!(overrides.map["model_context"], json!(262_144));
    assert_eq!(overrides.notes.len(), 1, "{:?}", overrides.notes);
    assert!(overrides.notes[0].contains("262144"), "{:?}", overrides.notes);
    let _ = std::fs::remove_dir_all(&root);
}
