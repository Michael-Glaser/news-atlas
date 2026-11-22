//! The three numbers in Settings reach the process that spends them — end to end, in a temp
//! root, without starting anything.
//!
//! The shipped path is four hops and every one of them can swallow a value:
//!
//! 1. the window writes no file (its archive handle is read-only), so a changed setting is a
//!    `settings_set` row in `data/requests.db` (`atlas-desktop/src/requests.rs:159`);
//! 2. `atlas-monitor/src/settings_set.rs` applies it with `atlas_config::model_settings` and
//!    `Config::save` into `data/config.json`, and mirrors the three numbers into the
//!    archive's `meta` table **in the same call** (`MIRRORED`, `settings_set.rs:123`);
//! 3. `atlas start` reads `data/config.json` (`start.rs:123`, `load_managed_at`),
//!    `model::resolve` turns `model_local` into the file to serve (`model.rs:108`) and
//!    `llama::argv` puts it after `-m` and `model_context` after `-c`
//!    (`llama.rs:181-187`);
//! 4. the two budget numbers are not llama.cpp flags: llama.cpp takes one `max_tokens` per
//!    request, so `atlas-ai/src/budget.rs:129` (`Budgets::from_config`) reads
//!    `model_reasoning` and `model_answer` from the same file, and the window reads the
//!    mirrored pair out of `meta` (`atlas-desktop/src/app.rs:782`).
//!
//! The defect these tests exist for is the value that **stops at the archive**: the window's
//! write lands in `meta`, nothing copies it into `data/config.json`, and the next `atlas
//! start` loads the shipped 32768 for `-c` while the Settings page shows the reader's number.
//! Hop 1→2 is runtime-covered by the monitor's own test
//! (`settings_set::tests::a_settings_request_is_written_through_the_config_path_and_nothing_else_changes`,
//! run with `cargo test -p atlas-monitor settings_set`); this file covers 2→3 at runtime and
//! pins the join in the monitor's shipped source, because a launcher test cannot link the
//! monitor (that would be a dependency edge the launcher must not have).

use std::path::{Path, PathBuf};

use atlas_config::Paths;
use atlas_launcher::llama;

/// The numbers this machine runs today, and the ones the report quotes.
const CONTEXT: u32 = 32_768;
const REASONING: u32 = 8_192;
const ANSWER: u32 = 8_192;

/// A temp project root with a `models/` directory and one model file in it.
struct Root {
    path: PathBuf,
}

impl Root {
    fn new(tag: &str) -> Root {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "atlas-settings-reach-{}-{}-{}",
            tag,
            std::process::id(),
            stamp
        ));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::create_dir_all(path.join("data"));
        let _ = std::fs::create_dir_all(path.join("models"));
        let _ = std::fs::write(path.join("models/local.gguf"), b"GGUF");
        Root { path }
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The file `settings_set::apply_with_store` leaves behind: the three numbers and the port.
///
/// Written as text because this crate does not link `serde_json` (the launcher builds argv,
/// it does not parse settings — `Config` owns that), and adding a dependency to a crate to
/// test another one is how a boundary rots. What is written here is exactly what the monitor's
/// `Config::save` writes for these keys.
fn write_settings(root: &Path, context: u32, reasoning: u32, answer: u32) -> Paths {
    let paths = Paths::at(root);
    let text = format!(
        "{{\n  \"llama_port\": 8081,\n  \"model_context\": {context},\n  \
         \"model_reasoning\": {reasoning},\n  \"model_answer\": {answer}\n}}\n"
    );
    std::fs::write(&paths.config_file, text).expect("data/config.json is writable");
    paths
}

/// The value after a flag in an argv, if the flag is there.
fn value_of<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
}

#[test]
fn the_context_in_settings_reaches_the_model_server_argv() {
    let root = Root::new("argv");
    let paths = write_settings(&root.path, CONTEXT, REASONING, ANSWER);

    // The load the boot performs, not a hand-built `Config`: this is `start.rs:123`.
    let config = atlas_config::load_managed_at(&paths).expect("the boot's config load");
    assert_eq!(
        (
            config.model_context,
            config.model_reasoning,
            config.model_answer
        ),
        (CONTEXT, REASONING, ANSWER),
        "the numbers did not survive data/config.json"
    );

    let argv = llama::argv(&config, &paths.model_file, config.llama_port, "");
    let context = CONTEXT.to_string();
    let port = config.llama_port.to_string();
    assert_eq!(
        value_of(&argv, "-c"),
        Some(context.as_str()),
        "the model server is not started with the reader's context: {argv:?}"
    );
    assert_eq!(value_of(&argv, "--host"), Some("127.0.0.1"), "{argv:?}");
    assert_eq!(value_of(&argv, "--port"), Some(port.as_str()), "{argv:?}");
    assert_eq!(value_of(&argv, "-np"), Some("1"), "{argv:?}");

    // And it is the file, not a constant: a second, smaller context must reach the argv and
    // take the first one with it.
    let paths = write_settings(&root.path, 16_384, REASONING, ANSWER);
    let config = atlas_config::load_managed_at(&paths).expect("the boot's config load");
    let argv = llama::argv(&config, &paths.model_file, config.llama_port, "");
    assert_eq!(value_of(&argv, "-c"), Some("16384"), "{argv:?}");
    assert!(
        !argv.iter().any(|arg| arg == "32768"),
        "the old context is still on the command line: {argv:?}"
    );
}

#[test]
fn the_two_budget_numbers_reach_the_places_they_are_spent() {
    // llama.cpp takes one `max_tokens` per request, so the thinking and answer budgets are
    // spent by the caller: `atlas-ai/src/budget.rs` reads `model_reasoning` and
    // `model_answer` out of the same `data/config.json` for a request, and the window reads
    // the mirrored pair out of `meta` (`atlas-desktop/src/app.rs:782`). Both halves are
    // pinned here; what is *not* claimed is that they appear in the argv, because llama.cpp
    // has no such flag and a value invented into one would be a request it misreads.
    let root = Root::new("budgets");
    let paths = write_settings(&root.path, CONTEXT, REASONING, ANSWER);
    let config = atlas_config::load_managed_at(&paths).expect("the boot's config load");
    assert_eq!(
        (config.model_reasoning, config.model_answer),
        (REASONING, ANSWER),
        "the budget numbers did not survive data/config.json"
    );

    // The mirror the monitor writes, read back the way the window reads it.
    let store = atlas_store::Store::open(&paths.db).expect("the archive opens");
    store.init().expect("the archive's schema");
    for (key, value) in [
        ("model_context", CONTEXT),
        ("model_reasoning", REASONING),
        ("model_answer", ANSWER),
    ] {
        store
            .set_meta(key, &value.to_string())
            .expect("meta is writable in a temp root");
        assert_eq!(
            store.get_meta(key).expect("meta is readable"),
            Some(value.to_string()),
            "the window would show a different {key} than the file holds"
        );
    }

    let before = llama::argv(&config, &paths.model_file, config.llama_port, "");
    let mut changed = config.clone();
    changed.model_reasoning = 0;
    changed.model_answer = 256;
    let after = llama::argv(&changed, &paths.model_file, changed.llama_port, "");
    assert_eq!(
        before, after,
        "a budget the model server cannot honour changed its command line"
    );
    for flag in ["-n", "--n-predict", "--reasoning", "--answer"] {
        assert!(
            value_of(&before, flag).is_none(),
            "the model server is started with {flag}: {before:?}"
        );
    }
}

#[test]
fn the_monitor_writes_the_file_and_the_mirror_in_one_call() {
    // The regression this pins: the window's number lands in `meta` and nothing copies it
    // into `data/config.json`, so the next start loads the shipped context while Settings
    // shows the reader's. The archive is the mirror, not a second source of truth, and both
    // writes have to stay in `apply_with_store` — in this order, so a failed save cannot
    // leave the window ahead of the boot.
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates live in rust/");
    let source = std::fs::read_to_string(crates.join("atlas-monitor/src/settings_set.rs"))
        .expect("the monitor's settings arm is readable");
    let body = source
        .split_once("pub fn apply_with_store(")
        .map(|(_, rest)| rest)
        .expect("settings_set::apply_with_store exists");

    let save = body.find("atlas_config::Config::save(").expect(
        "apply_with_store no longer writes data/config.json: the value stops at the archive",
    );
    let mirror = body
        .find("store.set_meta(")
        .expect("apply_with_store no longer mirrors the numbers into the archive");
    assert!(
        save < mirror,
        "the mirror runs before the file is written, so a failed save leaves meta ahead of \
         data/config.json"
    );
    assert!(
        source.contains(
            "const MIRRORED: [&str; 3] = [\"model_context\", \"model_reasoning\", \"model_answer\"];"
        ),
        "the mirrored set changed; the window reads those three names"
    );
    assert!(
        body.contains("atlas_config::model_settings("),
        "the monitor stopped asking atlas-config which keys exist and what they may be"
    );

    // And the consumer reads the file, never the archive: a boot that took `-c` from `meta`
    // would answer this machine's window and nothing else, and would restart with a stale
    // number after the reader closed Settings.
    let boot = std::fs::read_to_string(crates.join("atlas-launcher/src/start.rs"))
        .expect("the boot is readable");
    assert!(
        boot.contains("atlas_config::load_managed_at(&paths)"),
        "the boot no longer reads data/config.json"
    );
    assert!(
        !boot.contains("get_meta("),
        "the boot reads a settings number out of the archive: data/config.json is the one \
         source the argv may come from"
    );
}

/// The file a reader picked in Settings is the file the model server is started with: the
/// `model_local` in `data/config.json` reaches `-m`, and the reader's context reaches `-c`
/// beside it.
#[test]
fn a_model_named_in_settings_is_the_file_the_server_is_started_with() {
    let root = Root::new("fallen");
    // A second model, standing in for the one an import brought in.
    let chosen = root.path.join("models/chosen.gguf");
    std::fs::write(&chosen, b"GGUF a different file, chosen by the reader").expect("a model");

    let paths = Paths::at(&root.path);
    let body = format!(
        "{{\n  \"llama_port\": 8081,\n  \"model_local\": {:?},\n  \"model_context\": 65536,\n           \"model_gpu_layers\": 7,\n  \"model_threads\": 4\n}}\n",
        chosen.to_string_lossy()
    );
    std::fs::write(&paths.config_file, body).expect("data/config.json is writable");

    // The boot's own two steps: the config load, then the resolver.
    let config = atlas_config::load_managed_at(&paths).expect("the boot's config load");
    assert_eq!(config.model_local, chosen.to_string_lossy(), "the path did not survive the load");
    let model = atlas_launcher::model::resolve(&paths, &config).expect("the chosen model resolves");
    assert_eq!(model.path, chosen, "the resolver did not pick the reader's file");

    let argv = llama::argv(&config, &model.path, config.llama_port, "");
    // The whole head of the line, so the report can quote it: the file the reader picked is
    // what `-m` names, and the number they typed is what `-c` names.
    assert_eq!(
        &argv[..8],
        [
            "-m".to_string(),
            chosen.display().to_string(),
            "-c".to_string(),
            "65536".to_string(),
            "-ngl".to_string(),
            "7".to_string(),
            "-t".to_string(),
            "4".to_string(),
        ],
        "the command line is not the one the page previewed: {argv:?}"
    );
    assert_eq!(value_of(&argv, "-c"), Some("65536"), "{argv:?}");
    assert_eq!(value_of(&argv, "-ngl"), Some("7"), "{argv:?}");
    assert_eq!(value_of(&argv, "-t"), Some("4"), "{argv:?}");
    assert_eq!(value_of(&argv, "--host"), Some("127.0.0.1"), "{argv:?}");

    // The sentinels mean "llama.cpp decides", so the flags are *left out* rather than passed
    // as -1 / 0: llama.cpp has no -1 layers and no 0 threads.
    let plain = format!(
        "{{\n  \"llama_port\": 8081,\n  \"model_local\": {:?},\n  \"model_context\": 16384\n}}\n",
        chosen.to_string_lossy()
    );
    std::fs::write(&paths.config_file, plain).expect("a settings file without the two sentinels");
    let config = atlas_config::load_managed_at(&paths).expect("a second load");
    assert_eq!((config.model_gpu_layers, config.model_threads), (-1, 0));
    let argv = llama::argv(&config, &chosen, config.llama_port, "");
    assert_eq!(value_of(&argv, "-c"), Some("16384"), "{argv:?}");
    for absent in ["-ngl", "-t"] {
        assert!(
            value_of(&argv, absent).is_none(),
            "{absent} is passed for a sentinel, which llama.cpp cannot take: {argv:?}"
        );
    }

    // And the choice is a setting, not a constant: naming the other file in `models/` moves
    // `-m` to it and takes the previous one off the line. (The file has to exist: the agent's
    // gate refuses a `model_local` that is not a regular `.gguf` in `models/`, which is why
    // the reader picks from the list the page read off the disk.)
    let shipped = root.path.join("models/local.gguf");
    std::fs::write(
        &paths.config_file,
        format!(
            "{{\n  \"llama_port\": 8081,\n  \"model_local\": {:?}\n}}\n",
            shipped.to_string_lossy()
        ),
    )
    .expect("a third settings file");
    let config = atlas_config::load_managed_at(&paths).expect("a third load");
    let model = atlas_launcher::model::resolve(&paths, &config).expect("the shipped file resolves");
    let argv = llama::argv(&config, &model.path, config.llama_port, "");
    assert_eq!(
        value_of(&argv, "-m"),
        Some(shipped.to_string_lossy().as_ref()),
        "the old file is still on the command line: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg.ends_with("chosen.gguf")),
        "the previous model is still on the command line: {argv:?}"
    );
}
