//! `ATLAS_DATA_DIR` is the data directory, or it is not set — `Paths` included.
//!
//! An integration test, not a unit one, because the rule is about this *process's*
//! environment: `ATLAS_DATA_DIR` is a process-wide variable, and a test that sets
//! it inside the unit-test binary would be racing every other test there that
//! discovers the project root. A file here is a binary of its own (cargo builds
//! `tests/*.rs` separately), so the variable has exactly one reader.
//!
//! What it pins: `Home::discover()` honours the override, and `Paths::discover()`
//! answered `root.join("data")` anyway. Measured before the fix, with
//! `ATLAS_DATA_DIR=<tmp>/atlas-archive`: `Home::discover().data_dir` was
//! `<tmp>/atlas-archive` and `Paths::discover().data_dir` was `<tmp>/data` — every
//! log, the database and `config.json` written into a directory nobody named, and
//! no note anywhere saying so.

use std::path::{Path, PathBuf};

/// A scratch directory that does not exist yet, unique to this process.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "atlas-config-override-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_data_dir_whose_last_component_is_not_data_is_the_one_paths_use() {
    let home = scratch("paths");
    // The override names a directory called `atlas-archive`, not `data`: that is
    // the case the old `Paths::at(home.root)` could not represent.
    let archive = home.join("atlas-archive");
    std::fs::create_dir_all(&archive).expect("the archive the environment names");
    std::env::set_var("ATLAS_DATA_DIR", &archive);

    let paths = atlas_config::Paths::discover();

    assert_eq!(paths.data_dir, archive);
    // Every file this crate derives from it follows, so a caller cannot be
    // handed the override by one key and `<root>/data` by the next.
    assert_eq!(paths.config_file, archive.join("config.json"));
    assert_eq!(paths.db, archive.join("atlas.db"));
    assert_eq!(paths.db_wal, archive.join("atlas.db-wal"));
    assert_eq!(paths.db_shm, archive.join("atlas.db-shm"));
    assert_eq!(paths.log_app, archive.join("app.log"));
    assert_eq!(paths.log_llama, archive.join("llama.log"));
    assert_eq!(paths.log_chrome, archive.join("chrome.log"));
    assert_eq!(paths.chrome_profile, archive.join("chrome-profile"));
    // The root is the override's parent — `atlas-platform`'s rule — so the
    // read-only halves of the layout still resolve against the project.
    assert_eq!(paths.root, home);
    assert_eq!(paths.models_dir, home.join("models"));
    assert!(
        paths.llama_local.starts_with(home.join("bin")),
        "{}",
        paths.llama_local.display()
    );
    // And the JSON the `paths` subcommand publishes says the same thing.
    let published = paths.to_json();
    assert_eq!(
        published.get("data_dir").and_then(|value| value.as_str()),
        Some(atlas_config::Paths::discover().data_dir.to_string_lossy().as_ref())
    );

    // An empty value is "not set" (`atlas_platform::location::data_dir_override`),
    // so discovery falls back to the walk — the other half of the rule.
    std::env::remove_var("ATLAS_DATA_DIR");
    let discovered = atlas_config::Paths::discover();
    assert!(
        discovered.data_dir.ends_with(Path::new("data")),
        "with no override the archive is <root>/data, not {}",
        discovered.data_dir.display()
    );

    let _ = std::fs::remove_dir_all(&home);
}
