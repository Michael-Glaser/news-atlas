//! The settings a reader may change through the queue: end to end through the shipped write.
//!
//! The unit tests in `src/checks.rs` pin each rule and its sentence. This file pins the two
//! things a rule cannot prove on its own, because they are about the *file*:
//!
//! * a value the rules accepted survives [`Config::save`] — the path in `data/config.json` is
//!   the path the rules answered, and a path outside `models/` is not quietly replaced with
//!   `""` by the resolver behind the save;
//! * the file that lands is owner-only (`0600`) in a `0700` `data/`, because a settings file a
//!   second account can read is a settings file that leaks the model path and every value in
//!   it. The mode rule itself is `atlas-platform`'s and is pinned in `config_file_modes.rs`;
//!   here it is asserted for the one write path the queue uses.

use std::path::{Path, PathBuf};

use atlas_config::{model_settings, Config, Paths};
use serde_json::{json, Map, Value};

/// A project root with `models/` and one model in it.
fn project(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-config-settings-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("models")).expect("models");
    std::fs::write(root.join("models/local.gguf"), b"GGUF").expect("model");
    root
}

fn settings(pairs: &[(&str, Value)]) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

fn stored(root: &Path) -> Value {
    let text = std::fs::read_to_string(root.join("data/config.json")).expect("config.json");
    serde_json::from_str(&text).expect("json")
}

#[test]
fn the_values_the_rules_accept_are_the_values_the_file_holds() {
    let root = project("write");
    let model = root.join("models/local.gguf");
    let overrides = model_settings(
        &settings(&[
            ("model_local", json!(model.to_string_lossy())),
            ("model_context", json!(65536)),
            ("model_reasoning", json!(16384)),
            ("model_answer", json!(4096)),
            ("model_gpu_layers", json!(24)),
            ("model_threads", json!(8)),
        ]),
        &root,
    )
    .expect("accepted");
    assert!(overrides.notes.is_empty(), "{:?}", overrides.notes);
    let config = Config::save(&Paths::at(&root), &overrides.map).expect("saved");
    let saved = stored(&root);
    // The path survives the resolver behind the save: it is still the file that was checked,
    // not "" (which is what a path outside the project becomes).
    let written = saved["model_local"].as_str().unwrap_or_default();
    assert_eq!(
        written,
        std::fs::canonicalize(&model).expect("canonical").to_string_lossy(),
        "{saved}"
    );
    assert_eq!(saved["model_context"], json!(65536));
    assert_eq!(saved["model_answer"], json!(4096));
    assert_eq!(saved["model_gpu_layers"], json!(24));
    assert_eq!(saved["model_threads"], json!(8));
    assert_eq!(saved["model_reasoning"], json!(16384));
    // And the process reading them back agrees.
    assert_eq!(config.model_context, 65536);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_model_outside_models_is_refused_before_the_file_is_touched() {
    // The documented choice from `checks.rs`: `Config::save` would resolve an outside path to
    // "" through `PathPolicy::model`, so the rule refuses it first rather than storing a
    // setting that silently means "the default model".
    let root = project("outside");
    let outside = std::env::temp_dir().join(format!("atlas-config-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).expect("outside");
    let model = outside.join("big.gguf");
    std::fs::write(&model, b"GGUF").expect("model");
    let error = model_settings(
        &settings(&[("model_local", json!(model.to_string_lossy()))]),
        &root,
    )
    .expect_err("refused");
    assert!(error.contains("has to be a file inside"), "{error}");
    assert!(
        !root.join("data/config.json").exists(),
        "a refusal wrote the settings file"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn the_queue_write_lands_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let root = project("mode");
    let model = root.join("models/local.gguf");
    let overrides = model_settings(
        &settings(&[
            ("model_local", json!(model.to_string_lossy())),
            ("model_context", json!(32768)),
        ]),
        &root,
    )
    .expect("accepted");
    Config::save(&Paths::at(&root), &overrides.map).expect("saved");
    let mode = std::fs::metadata(root.join("data/config.json"))
        .expect("config")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "config.json is {mode:o}");
    let dir_mode = std::fs::metadata(root.join("data"))
        .expect("data")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "data/ is {dir_mode:o}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_clamped_value_is_written_as_the_clamp_and_the_note_is_published() {
    let root = project("clamp");
    let overrides = model_settings(&settings(&[("model_context", json!(999_999))]), &root)
        .expect("accepted");
    assert_eq!(overrides.notes.len(), 1, "{:?}", overrides.notes);
    for note in &overrides.notes {
        atlas_config::warn(note);
    }
    Config::save(&Paths::at(&root), &overrides.map).expect("saved");
    assert_eq!(stored(&root)["model_context"], json!(262_144));
    assert!(
        atlas_config::warnings()
            .iter()
            .any(|note| note.contains("model_context") && note.contains("262144")),
        "{:?}",
        atlas_config::warnings()
    );
    let _ = std::fs::remove_dir_all(&root);
}
