//! Cross-module tests, and the fixtures every unit test in this crate shares.
//!
//! The promises in this file need several modules at once — a file on disk, the
//! environment, the SPEC table and the value coercion all decide one loaded
//! setting — so they live here. Everything that tests one module sits in that
//! module's own `#[cfg(test)] mod tests`, and the fixtures below are what they
//! share.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use crate::config::Config;
use crate::problem::ConfigError;
use crate::stored::read_stored;
use crate::Paths;
use crate::spec::{default_model_url, MODEL_FILE, MODEL_LOCAL_NAME, MODEL_REPO, SPEC};
use crate::ports::{free_port, load_managed_at};
use crate::modes::{permissions, posix};
use crate::spawn::key_is_secret;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct TempDir {
    pub(crate) path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(tag: &str) -> TempDir {
        let serial = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "atlas-config-{}-{}-{}",
            std::process::id(),
            tag,
            serial
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir { path }
    }

    pub(crate) fn at(&self) -> Paths {
        Paths::at(&self.path)
    }

    pub(crate) fn write(&self, relative: &str, body: &str) {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, body).expect("write fixture");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn env_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

pub(crate) fn stored(json: &str) -> serde_json::Map<String, Value> {
    match serde_json::from_str::<Value>(json).expect("json") {
        Value::Object(map) => map,
        other => panic!("expected an object, got {}", other),
    }
}

pub(crate) fn empty() -> serde_json::Map<String, Value> {
    serde_json::Map::new()
}

/// Build without reading the real environment, so a test never depends on the
/// shell it happens to run in.
pub(crate) fn build_with(env: &[(&str, &str)], json: &str) -> Config {
    let dir = TempDir::new("build");
    Config::build(&dir.at(), &env_pairs(env), &stored(json))
}

pub(crate) fn load_env_free(dir: &TempDir) -> Result<Config, ConfigError> {
    let file = read_stored(&dir.at())?;
    Ok(Config::build(&dir.at(), &[], &file))
}

#[test]
fn defaults_apply_when_there_is_no_file_and_no_environment() {
    let config = build_with(&[], "{}");
    assert_eq!(config.app_port, 8787);
    assert_eq!(config.llama_port, 8080);
    assert_eq!(config.cdp_port, 9222);
    assert_eq!(config.collect_interval_min, 5.0);
    assert_eq!(config.max_age_hours, 72);
    assert_eq!(config.window_hours, 72);
    assert_eq!(config.retention_days, 21);
    assert_eq!(config.model_repo, MODEL_REPO);
    assert_eq!(config.model_file, MODEL_FILE);
    assert_eq!(config.model_url, default_model_url());
    assert!(config.model_local.ends_with(MODEL_LOCAL_NAME));
    assert_eq!(config.browser_path, "");
    assert_eq!(config.source_of("app_port"), "default");
    assert!(config.validate().is_empty());
}

#[test]
fn precedence_is_environment_then_file_then_default() {
    let dir = TempDir::new("precedence");
    dir.write(
        "data/config.json",
        r#"{"app_port": 9001, "llama_port": 9002, "window_hours": 12}"#,
    );
    let env = env_pairs(&[("ATLAS_PORT", "9101"), ("ATLAS_COLLECT_MIN", "2.5")]);
    let file = read_stored(&dir.at()).expect("stored");
    let config = Config::build(&dir.at(), &env, &file);
    assert_eq!(config.app_port, 9101);
    assert_eq!(config.source_of("app_port"), "env");
    assert_eq!(config.llama_port, 9002);
    assert_eq!(config.source_of("llama_port"), "config");
    assert_eq!(config.window_hours, 12);
    assert_eq!(config.collect_interval_min, 2.5);
    assert_eq!(config.source_of("collect_interval_min"), "env");
    assert_eq!(config.cdp_port, 9222);
    assert_eq!(config.source_of("cdp_port"), "default");
    assert!(config.validate().is_empty());
}

#[test]
fn an_empty_environment_variable_counts_as_unset() {
    let dir = TempDir::new("empty-env");
    dir.write("data/config.json", r#"{"app_port": 9001}"#);
    let env = env_pairs(&[("ATLAS_PORT", "   ")]);
    let config = Config::build(&dir.at(), &env, &read_stored(&dir.at()).unwrap());
    assert_eq!(config.app_port, 9001);
    assert_eq!(config.source_of("app_port"), "config");
}


#[test]
fn unknown_keys_survive_and_are_not_a_problem() {
    let dir = TempDir::new("unknown");
    dir.write(
        "data/config.json",
        r#"{"app_port": 9001, "future_setting": {"nested": [1, 2]}, "another": "x"}"#,
    );
    let config = load_env_free(&dir).expect("load");
    assert_eq!(config.extra.len(), 2);
    assert!(config.extra.contains_key("future_setting"));
    assert!(config.validate().is_empty());
    assert!(config.values_json().get("future_setting").is_some());
    assert_eq!(
        config.extra.keys().cloned().collect::<Vec<_>>(),
        vec!["another".to_string(), "future_setting".to_string()]
    );
}

#[test]
fn credential_shaped_keys_are_never_stored() {
    assert!(key_is_secret("api_key"));
    assert!(key_is_secret("APIKEY"));
    assert!(key_is_secret("hf_token"));
    assert!(key_is_secret("refresh-token"));
    assert!(key_is_secret("client_secret"));
    assert!(key_is_secret("password"));
    assert!(key_is_secret("passphrase"));
    assert!(key_is_secret("session_id"));
    assert!(key_is_secret("authorization"));
    assert!(key_is_secret("access-key"));
    assert!(key_is_secret("private_key"));
    assert!(!key_is_secret("app_port"));
    assert!(!key_is_secret("model_local"));
    assert!(!key_is_secret(""));

    let dir = TempDir::new("secrets");
    let mut overrides = serde_json::Map::new();
    overrides.insert("api_key".to_string(), Value::String("sk-live-123456".into()));
    overrides.insert("retention_days".to_string(), Value::from(30));
    let config = Config::save(&dir.at(), &overrides).expect("save");
    assert_eq!(config.retention_days, 30);
    let on_disk = std::fs::read_to_string(dir.at().config_file).expect("file");
    assert!(!on_disk.contains("sk-live-123456"), "the secret reached the disk");
    assert!(!on_disk.contains("api_key"));
}

#[test]
fn a_credential_in_the_settings_file_is_never_used_or_published() {
    // `save` refused to *write* a credential-shaped key, but a file that already
    // had one — hand-edited, restored from a backup, written by a newer binary —
    // was kept as an unknown key and republished by `values_json`, which is what
    // `atlas-config config` prints. The name and the reason are reported; the
    // value is not, anywhere.
    let dir = TempDir::new("secret-file");
    dir.write(
        "data/config.json",
        r#"{"app_port": 9001, "hf_token": "hf_SUPERSECRET123", "future_setting": 7}"#,
    );
    let config = load_env_free(&dir).expect("load");
    assert_eq!(config.app_port, 9001, "the rest of the file still applies");
    assert!(!config.extra.contains_key("hf_token"), "{:?}", config.extra);
    assert!(
        config.extra.contains_key("future_setting"),
        "a harmless unknown key still survives"
    );
    let values = config.values_json().to_string();
    assert!(!values.contains("hf_token"), "{}", values);
    assert!(!values.contains("SUPERSECRET123"), "{}", values);
    assert!(!config.describe().to_string().contains("SUPERSECRET123"));
    assert!(!config.security_report().to_string().contains("SUPERSECRET123"));
    // Not silent either: the key is named, so a person can go and remove it.
    assert!(
        crate::warnings()
            .iter()
            .any(|note| note.contains("hf_token") && !note.contains("SUPERSECRET123")),
        "{:?}",
        crate::warnings()
    );
    // And the next write is what finally takes it out of the file.
    assert!(std::fs::read_to_string(dir.at().config_file)
        .expect("file")
        .contains("SUPERSECRET123"));
    Config::save(&dir.at(), &serde_json::Map::new()).expect("save");
    assert!(!std::fs::read_to_string(dir.at().config_file)
        .expect("file")
        .contains("SUPERSECRET123"));
}

#[test]
fn a_credential_a_caller_puts_in_extra_is_not_published() {
    // The second door. `build` refuses to keep a credential-shaped key, but
    // `Config`'s fields are public, so a caller can hand this crate a Config whose
    // `extra` holds anything — and `values_json` is what gets printed.
    let mut config = build_with(&[], "{}");
    config
        .extra
        .insert("gh_token".to_string(), Value::String("ghp_x".into()));
    assert!(config.values_json().get("gh_token").is_none());
    assert!(!config.describe().to_string().contains("ghp_x"));
}

#[test]
fn a_host_key_in_any_spelling_is_dropped_and_named() {
    // `HOST_KEYS` is a table of *names*, and the drop was an exact string match:
    // `{"Host": "0.0.0.0"}` was not caught, so it was forwarded as an unknown key
    // into `extra` and republished by `values_json`. The shape decides now — case,
    // `_`, `-` and spaces do not make it a different key.
    let dir = TempDir::new("host-spellings");
    dir.write(
        "data/config.json",
        r#"{"Host": "0.0.0.0", "BIND-HOST": "::", "bindhost": "10.1.2.3"}"#,
    );
    let config = load_env_free(&dir).expect("load");
    for key in ["Host", "BIND-HOST", "bindhost"] {
        assert!(!config.extra.contains_key(key), "{} survived", key);
        assert!(!config.values_json().to_string().contains(key), "{} published", key);
    }
    let notes = config.notes.join("\n");
    for place in [
        "data/config.json:Host='0.0.0.0'",
        "data/config.json:BIND-HOST='::'",
        "data/config.json:bindhost='10.1.2.3'",
    ] {
        assert!(notes.contains(place), "{} missing from {}", place, notes);
    }
    // Each one is a problem too, so `validate`/boot refuses instead of ignoring.
    let problems = config.validate();
    assert_eq!(problems.len(), 3, "{:?}", problems);
    assert_eq!(
        problems.iter().map(|problem| problem.key.as_str()).collect::<Vec<_>>(),
        vec!["host", "bind_host", "bind_host"],
        "the problem names the setting, and the origin names the spelling: {:?}",
        problems
    );
    // The environment half is the same rule: `ATLAS_` + any spelling.
    let env = env_pairs(&[("ATLAS_Bind-Host", "192.168.1.4")]);
    let config = Config::build(&dir.at(), &env, &empty());
    assert_eq!(config.validate().len(), 1, "{:?}", config.validate());
    assert_eq!(config.validate()[0].origin, "env:ATLAS_Bind-Host");
}

#[test]
fn an_unknown_key_cannot_choose_where_atlas_listens() {
    // The promise the table exists for: no key outside it — however a newer or
    // hostile file spells it — reaches the bind address or a port. The bind is a
    // constant (`BIND_HOST`) and every number comes from a SPEC row, so an unknown
    // key is inert by construction; this pins the construction.
    let dir = TempDir::new("host-smuggle");
    dir.write(
        "data/config.json",
        r#"{"bind_addr": "0.0.0.0", "port": 80, "app_port ": 9009, "listen_on": "0.0.0.0",
            "hosts": ["0.0.0.0"], "bind": {"address": "0.0.0.0"}, "ADDRESS": "::"}"#,
    );
    let config = load_env_free(&dir).expect("load");
    assert_eq!(crate::BIND_HOST, "127.0.0.1");
    assert_eq!(config.app_port, 8787, "an unknown key chose a port");
    for (key, value) in [
        ("app_port", 8787.0),
        ("llama_port", 8080.0),
        ("cdp_port", 9222.0),
    ] {
        assert_eq!(config.number(key), Some(value), "{}", key);
    }
    let report = config.security_report();
    assert_eq!(
        report.get("bind_host").and_then(Value::as_str),
        Some("127.0.0.1")
    );
    assert_eq!(report.get("loopback_only").and_then(Value::as_bool), Some(true));
    // `address` is a host key and is dropped; `bind_addr`, `port` and `listen_on`
    // are simply unknown to this build — forwarded, and read by nothing, which is
    // why the two assertions above still hold.
    assert!(!config.extra.contains_key("ADDRESS"));
    assert!(!config.extra.contains_key("bind"));
    assert!(config.extra.contains_key("bind_addr"));
}


#[test]
fn save_writes_resolved_settings_and_a_0600_file() {
    let dir = TempDir::new("save");
    let mut overrides = serde_json::Map::new();
    overrides.insert("app_port".to_string(), Value::from(9401));
    overrides.insert("collect_interval_min".to_string(), Value::from(2.5));
    overrides.insert("model_local".to_string(), Value::String("models/a.gguf".into()));
    overrides.insert("nonsense_port".to_string(), Value::String("x".into()));
    Config::save(&dir.at(), &overrides).expect("save");

    let reloaded = load_env_free(&dir).expect("reload");
    assert_eq!(reloaded.app_port, 9401);
    assert_eq!(reloaded.collect_interval_min, 2.5);
    assert_eq!(reloaded.source_of("app_port"), "config");
    assert_eq!(
        reloaded.model_local,
        format!("{}/models/a.gguf", crate::paths::show(&dir.path))
    );
    assert!(reloaded.extra.contains_key("nonsense_port"));
    if posix() {
        assert_eq!(permissions(&crate::paths::show(&dir.at().config_file)).mode, "0o600");
        assert_eq!(permissions(&crate::paths::show(&dir.at().data_dir)).mode, "0o700");
    }
}

#[test]
fn save_drops_a_junk_number_instead_of_storing_it() {
    let dir = TempDir::new("save-junk");
    let mut overrides = serde_json::Map::new();
    overrides.insert("app_port".to_string(), Value::String("banana".into()));
    Config::save(&dir.at(), &overrides).expect("save");
    let on_disk = std::fs::read_to_string(dir.at().config_file).expect("file");
    assert!(!on_disk.contains("banana"));
    let reloaded = load_env_free(&dir).expect("reload");
    assert_eq!(reloaded.app_port, 8787);
    assert!(reloaded.validate().is_empty());
}

#[test]
fn a_never_stored_port_is_picked_once_and_persisted() {
    let dir = TempDir::new("managed");
    let config = load_managed_at(&dir.at()).expect("managed");
    assert_eq!(config.source_of("app_port"), "auto");
    assert!(config.app_port >= 1024);
    let file = read_stored(&dir.at()).expect("stored");
    assert_eq!(
        file.get("app_port").and_then(Value::as_u64),
        Some(config.app_port as u64)
    );
    // The second load never re-rolls it.
    let again = load_managed_at(&dir.at()).expect("managed");
    assert_eq!(again.app_port, config.app_port);
    assert_eq!(again.source_of("app_port"), "config");
}

#[test]
fn a_stored_port_is_clamped_in_place_and_never_re_rolled() {
    let dir = TempDir::new("managed-clamp");
    dir.write("data/config.json", r#"{"app_port": 80, "llama_port": 99999}"#);
    let config = load_managed_at(&dir.at()).expect("managed");
    assert_eq!(config.app_port, 1024);
    assert_eq!(config.llama_port, 65535);
    let file = read_stored(&dir.at()).expect("stored");
    assert_eq!(file.get("app_port").and_then(Value::as_u64), Some(1024));
    assert_eq!(file.get("llama_port").and_then(Value::as_u64), Some(65535));
}

#[test]
fn free_port_answers_a_port_that_can_be_bound_on_loopback() {
    let port = free_port(8787);
    assert!(port >= 1024);
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
    // A port that is already taken is skipped, not handed out twice.
    let held = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let taken = held.local_addr().expect("addr").port();
    assert_ne!(free_port(taken), taken);
}

#[test]
fn the_describe_shape_carries_values_sources_and_files() {
    let dir = TempDir::new("describe");
    dir.write("data/config.json", r#"{"app_port": 9301, "host": "0.0.0.0"}"#);
    let config = load_env_free(&dir).expect("load");
    let described = config.describe();
    assert_eq!(
        described
            .get("values")
            .and_then(|values| values.get("app_port"))
            .and_then(Value::as_u64),
        Some(9301)
    );
    assert_eq!(
        described
            .get("sources")
            .and_then(|sources| sources.get("app_port"))
            .and_then(Value::as_str),
        Some("config")
    );
    assert_eq!(
        described
            .get("sources")
            .and_then(|sources| sources.get("window_hours"))
            .and_then(Value::as_str),
        Some("default")
    );
    let files = described
        .get("files")
        .and_then(Value::as_object)
        .expect("files");
    for key in [
        "model",
        "model_resolved",
        "browser",
        "geo",
        "geo_map",
        "llama_server",
    ] {
        assert!(files.contains_key(key), "{}", key);
        assert_eq!(
            files.get(key).and_then(|entry| entry.get("exists")),
            Some(&Value::Bool(false)),
            "{}",
            key
        );
    }
    let security = described
        .get("security")
        .and_then(Value::as_object)
        .expect("security");
    assert_eq!(
        security.get("bind_host").and_then(Value::as_str),
        Some("127.0.0.1")
    );
    assert_eq!(security.get("loopback_only").and_then(Value::as_bool), Some(true));
    assert_eq!(security.get("shell").and_then(Value::as_bool), Some(false));
    assert!(security.get("env_allowlist").and_then(Value::as_array).is_some());
    assert!(security.get("permissions").is_some());
    // `/api/security` publishes this block on its own, without `describe`'s
    // root-level `problems`, so a refused host key has to travel inside it.
    let security_problems = security
        .get("problems")
        .and_then(Value::as_array)
        .expect("security.problems");
    assert_eq!(security_problems.len(), 1, "{:?}", security_problems);
    assert!(security_problems[0]
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("0.0.0.0"));
    // Every file ATLAS writes into `data/` is checked, the logs included:
    // `harden_data_files` chmods them, so a report that skipped them said "ok"
    // about files it never looked at.
    let entries = security
        .get("permissions")
        .and_then(|permissions| permissions.get("entries"))
        .and_then(Value::as_array)
        .expect("permission entries");
    let listed: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.get("path").and_then(Value::as_str))
        .collect();
    for path in [&dir.at().log_app, &dir.at().log_llama, &dir.at().log_chrome] {
        let text = crate::paths::show(path);
        assert!(listed.contains(&text.as_str()), "{} is not reported", text);
    }
    assert_eq!(
        described.get("problems").and_then(Value::as_array).map(|list| list.len()),
        Some(1)
    );
}

#[test]
fn the_report_publishes_the_mode_it_read_and_the_mode_it_wants() {
    // `/api/security` published `data_mode: "0o700"` and `file_mode: "0o600"` as
    // constants, so a `data/` at 0777 was reported with the two numbers ATLAS
    // hoped for beside entries that said 0o777. `mode` is a measurement now: the
    // wanted numbers moved to `target_modes`, where they cannot be misread.
    if !posix() {
        return; // Windows reports `windows-acl` and has no mode to read.
    }
    let dir = TempDir::new("report-modes");
    let paths = dir.at();
    crate::ensure_dir(&paths.data_dir);
    std::fs::write(&paths.config_file, "{}").expect("config");
    crate::ensure_file(&paths.config_file);
    std::fs::write(&paths.db, "x").expect("db");
    crate::ensure_file(&paths.db);

    let config = load_env_free(&dir).expect("load");
    let report = config.security_report();
    let permissions = report.get("permissions").expect("permissions");
    assert_eq!(permissions.get("ok").and_then(Value::as_bool), Some(true), "{:?}", permissions);
    assert_eq!(report.get("data_mode").and_then(Value::as_str), Some("0o700"));
    assert_eq!(report.get("file_mode").and_then(Value::as_str), Some("0o600"));
    assert_eq!(
        report.get("target_modes").and_then(|modes| modes.get("data")).and_then(Value::as_str),
        Some("0o700")
    );
    assert_eq!(
        permissions.get("basis").and_then(Value::as_str),
        Some("posix-mode")
    );
    assert_eq!(
        permissions.get("acl_inspected").and_then(Value::as_bool),
        Some(true),
        "a POSIX mode was read, so it was inspected"
    );

    // Loosen the directory: the number in the report follows the file system, and
    // `ok` stops being true. The target is still 0700 — that is a wish, and it is
    // labelled as one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&paths.data_dir, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let report = load_env_free(&dir).expect("load").security_report();
        assert_eq!(
            report.get("data_mode").and_then(Value::as_str),
            Some("0o755"),
            "the report did not read the mode it published"
        );
        assert_eq!(
            report.get("permissions").and_then(|p| p.get("ok")).and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            report.get("target_modes").and_then(|m| m.get("data")).and_then(Value::as_str),
            Some("0o700")
        );
    }
}

#[test]
fn a_report_about_a_filesystem_it_did_not_read_is_not_ok() {
    // Nothing exists yet: eight entries with `exists: false` and a top-level
    // `"ok": true`. That is a clean bill of health for files nobody looked at.
    let dir = TempDir::new("report-empty");
    let config = load_env_free(&dir).expect("load");
    let permissions = config
        .security_report()
        .get("permissions")
        .cloned()
        .expect("permissions");
    assert_eq!(
        permissions.get("ok").and_then(Value::as_bool),
        Some(false),
        "{:?}",
        permissions
    );
    let entries = permissions.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
    assert!(entries.iter().all(|entry| entry.get("exists").and_then(Value::as_bool) == Some(false)));
    assert!(entries
        .iter()
        .all(|entry| entry.get("mode").and_then(Value::as_str) == Some("")));
}

#[cfg(unix)]
#[test]
fn a_data_directory_linked_out_of_the_project_is_reported_as_redirected() {
    // `data/` is the archive's home: it is not a setting, and `ATLAS_DATA_DIR` is
    // the only thing that moves it. A symlink planted at `data/` moves the
    // database, the settings and the logs outside the project while every path the
    // app publishes still reads `<root>/data` — and the mode the report printed
    // was the *target's*, under this project's name. The report says both things
    // now, and `ok` cannot be true while it is happening.
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("report-redirect");
    let outside = TempDir::new("report-redirect-outside");
    std::fs::set_permissions(&outside.path, std::fs::Permissions::from_mode(0o700))
        .expect("chmod");
    std::fs::write(outside.path.join("atlas.db"), "x").expect("db");
    let paths = dir.at();
    std::os::unix::fs::symlink(&outside.path, &paths.data_dir).expect("symlink");

    let config = load_env_free(&dir).expect("load");
    let permissions = config
        .security_report()
        .get("permissions")
        .cloned()
        .expect("permissions");
    assert_eq!(
        permissions.get("data_inside_project").and_then(Value::as_bool),
        Some(false),
        "the archive left the project and the report did not say so: {:?}",
        permissions
    );
    assert_eq!(
        permissions.get("ok").and_then(Value::as_bool),
        Some(false),
        "a redirected data directory cannot be ok"
    );
    let dir_entry = permissions
        .get("entries")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .cloned()
        .expect("first entry");
    assert_eq!(
        dir_entry.get("path").and_then(Value::as_str),
        Some(crate::paths::show(&paths.data_dir).as_str())
    );
    assert!(
        dir_entry.get("link").and_then(Value::as_str).is_some(),
        "the entry does not say it is a link: {:?}",
        dir_entry
    );

    // The same layout with a real `data/` is inside the project and fine.
    let dir = TempDir::new("report-real");
    crate::ensure_dir(&dir.at().data_dir);
    std::fs::write(&dir.at().db, "x").expect("db");
    crate::ensure_file(&dir.at().db);
    let permissions = load_env_free(&dir)
        .expect("load")
        .security_report()
        .get("permissions")
        .cloned()
        .expect("permissions");
    assert_eq!(
        permissions.get("data_inside_project").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(permissions.get("ok").and_then(Value::as_bool), Some(true), "{:?}", permissions);
}

#[test]
fn a_settings_file_written_by_the_old_app_still_loads() {
    // The Python app wrote this shape — `json.dump` of the SPEC table, floats
    // where an integer setting lives, text where a path lives, `null` for "not
    // set". It has to load as-is: no key renamed, no value lost, nothing
    // reported as a problem, and the ports still integers in range.
    let dir = TempDir::new("legacy-file");
    dir.write(
        "data/config.json",
        r#"{
  "app_port": 8787,
  "llama_port": 8080.0,
  "cdp_port": 9222,
  "collect_interval_min": 5,
  "max_age_hours": 72,
  "window_hours": 72,
  "retention_days": 21,
  "model_repo": "DavidAU/Qwen3.5-9B-The-Defiant-Fable-Uncensored-Heretic-NEO-IMATRIX-MAX-MTP-GGUF",
  "model_file": "old-name.gguf",
  "model_url": "https://example.invalid/old-name.gguf",
  "model_local": null,
  "browser_path": ""
}"#,
    );
    let config = load_env_free(&dir).expect("an old settings file must load");
    assert_eq!(config.app_port, 8787);
    assert_eq!(config.llama_port, 8080);
    assert_eq!(config.cdp_port, 9222);
    assert_eq!(config.collect_interval_min, 5.0);
    assert_eq!(config.max_age_hours, 72);
    assert_eq!(config.window_hours, 72);
    assert_eq!(config.retention_days, 21);
    // A `null` model path means "the pinned name", not a problem.
    assert!(config.model_local.ends_with(MODEL_LOCAL_NAME));
    assert_eq!(config.browser_path, "");
    assert_eq!(config.model_file, "old-name.gguf");
    assert_eq!(config.sources.get("app_port").map(|_| config.source_of("app_port")), Some("config".to_string()));
    assert!(config.validate().is_empty(), "{:?}", config.validate());
    assert!(config.notes.is_empty(), "an old file needs no note: {:?}", config.notes);
    // And it survives a save unchanged: an upgrade must not rewrite the file
    // into something the next load reads differently.
    let saved = Config::save(&dir.at(), &serde_json::Map::new()).expect("save");
    assert_eq!(saved.app_port, 8787);
    assert_eq!(saved.llama_port, 8080);
    assert_eq!(saved.collect_interval_min, 5.0);
    assert_eq!(saved.model_file, "old-name.gguf");
}

#[test]
fn a_clamped_value_is_reported_and_not_silently_changed() {
    // Python clamped and said nothing; this crate's rule is that nothing is
    // silent. A value that is a number but out of range still runs — it is a
    // note, not a refusal — and the note names the place, the value written,
    // the range and the value in force.
    let dir = TempDir::new("clamp-note");
    dir.write("data/config.json", r#"{"app_port": 80, "retention_days": 99999}"#);
    let config = load_env_free(&dir).expect("load");
    assert_eq!(config.app_port, 1024);
    assert_eq!(config.retention_days, 3650);
    assert!(config.validate().is_empty(), "clamping is not a refusal");
    let notes = config.notes.join("\n");
    assert!(
        notes.contains("data/config.json:app_port=80 is outside 1024 to 65535; ATLAS is using 1024"),
        "{}",
        notes
    );
    assert!(
        notes.contains("data/config.json:retention_days=99999 is outside 1 to 3650; ATLAS is using 3650"),
        "{}",
        notes
    );
    // The same through the environment, named as the environment.
    let env = env_pairs(&[("ATLAS_MAX_AGE_H", "100000")]);
    let config = Config::build(&dir.at(), &env, &empty());
    assert_eq!(config.max_age_hours, 8760);
    assert!(
        config
            .notes
            .iter()
            .any(|note| note.starts_with("env:ATLAS_MAX_AGE_H=100000")),
        "{:?}",
        config.notes
    );
    // `describe` publishes them, so `atlas-config config` and a status page can
    // answer "why is it 1024 when the file says 80".
    let described = config.describe();
    assert!(described
        .get("notes")
        .and_then(Value::as_array)
        .map(|notes| !notes.is_empty())
        .unwrap_or(false));
}

#[test]
fn no_port_setting_is_ever_a_fraction_or_outside_its_range() {
    // "ports are integers in range" for every hostile shape a file or a shell
    // can carry: a fraction, a string, a huge number, a negative one, a boolean,
    // `null`, a structure, and NaN spelled as text.
    let hostile = [
        r#"{"app_port": 8787.5, "llama_port": 99999, "cdp_port": -5}"#,
        r#"{"app_port": "8081", "llama_port": " 8082 ", "cdp_port": "banana"}"#,
        r#"{"app_port": 1e9, "llama_port": true, "cdp_port": null}"#,
        r#"{"app_port": [], "llama_port": {}, "cdp_port": "nan"}"#,
        r#"{"app_port": "inf", "llama_port": -1, "cdp_port": 65535.5}"#,
    ];
    for json in hostile {
        let config = build_with(&[], json);
        let described = config.values_json();
        for spec in SPEC.iter() {
            let value = config.number(spec.key).unwrap_or_else(|| panic!("{}", spec.key));
            assert!(
                (spec.low..=spec.high).contains(&value),
                "{}={} outside {}..={} for {}",
                spec.key,
                value,
                spec.low,
                spec.high,
                json
            );
            let emitted = described.get(spec.key).expect("values_json");
            if spec.kind == crate::spec::Kind::Int {
                // A port (and every other whole-number row) is an integer in
                // the JSON a caller parses, not `8080.0` and not `8080.5`.
                assert!(
                    emitted.is_i64() || emitted.is_u64(),
                    "{} is not an integer in the JSON: {} ({})",
                    spec.key,
                    emitted,
                    json
                );
            }
            assert_eq!(
                emitted.as_f64(),
                Some(value),
                "{} disagrees with the typed field ({})",
                spec.key,
                json
            );
        }
    }
}

#[test]
fn save_keeps_a_key_from_a_newer_file_and_says_what_it_dropped() {
    // Round trip: an unknown key written by a newer binary is forwarded, a
    // credential-shaped one is refused by name, and a number that cannot be used
    // is dropped *out loud* — otherwise a setting would "revert on its own"
    // after a save.
    let dir = TempDir::new("round-trip");
    dir.write(
        "data/config.json",
        r#"{"app_port": 9001, "future_setting": {"nested": [1, 2]}, "api_key": "sk-secret"}"#,
    );
    let mut overrides = serde_json::Map::new();
    overrides.insert("llama_port".to_string(), Value::from(9101));
    overrides.insert("window_hours".to_string(), Value::String("48".into()));
    overrides.insert("retention_days".to_string(), Value::String("banana".into()));
    let saved = Config::save(&dir.at(), &overrides).expect("save");
    assert_eq!(saved.llama_port, 9101);
    assert_eq!(saved.window_hours, 48);
    let on_disk = std::fs::read_to_string(dir.at().config_file).expect("file");
    let parsed: Value = serde_json::from_str(&on_disk).expect("json");
    assert_eq!(
        parsed.get("future_setting"),
        Some(&Value::Object(
            [("nested".to_string(), Value::Array(vec![Value::from(1), Value::from(2)]))]
                .into_iter()
                .collect()
        )),
        "an unknown key was lost: {}",
        on_disk
    );
    assert!(!on_disk.contains("sk-secret"), "a credential reached the disk");
    assert!(!on_disk.contains("banana"), "junk was stored: {}", on_disk);
    assert_eq!(
        parsed.get("retention_days"),
        None,
        "the dropped key is gone, not silently replaced: {}",
        on_disk
    );
    // The drop is a sentence on the config that was returned, so a person can
    // see why the setting reverted instead of watching it "reset on its own".
    assert!(
        saved
            .notes
            .iter()
            .any(|note| note.contains("data/config.json:retention_days=banana was not stored")
                && note.contains("write a whole number from 1 to 3650")),
        "{:?}",
        saved.notes
    );
    // And the reload agrees with what was written.
    let reloaded = load_env_free(&dir).expect("reload");
    assert_eq!(reloaded.retention_days, 21);
    assert_eq!(reloaded.window_hours, 48);
    assert!(reloaded.extra.contains_key("future_setting"));
}

#[test]
fn a_host_key_in_the_environment_is_named_as_the_environment() {
    // "ignoring '0.0.0.0' from the configuration" sent a person to a file that
    // does not carry the value — and an `ATLAS_HOST` in the shell produced no
    // note at all, because the old loop only looked at what the file held. The
    // note names the place the value came from.
    let dir = TempDir::new("host-note");
    let env = env_pairs(&[("ATLAS_HOST", "0.0.0.0")]);
    let config = Config::build(&dir.at(), &env, &empty());
    assert_eq!(
        config.notes,
        vec![
            "env:ATLAS_HOST='0.0.0.0' is not loopback and was ignored: ATLAS binds 127.0.0.1 only"
                .to_string()
        ],
        "the environment half of the host-key rule"
    );
    // And the file case names the file, on its own.
    let config = build_with(&[], r#"{"listen_host": "192.168.1.4"}"#);
    assert_eq!(config.validate().len(), 1);
    assert_eq!(
        config.notes,
        vec![
            "data/config.json:listen_host='192.168.1.4' is not loopback and was ignored: \
             ATLAS binds 127.0.0.1 only"
                .to_string()
        ]
    );
    // A loopback host key is still not a setting: it is dropped, and said.
    let config = build_with(&[], r#"{"host": "localhost"}"#);
    assert!(config.validate().is_empty());
    assert_eq!(
        config.notes,
        vec![
            "data/config.json:host='localhost' was ignored: ATLAS has no bind-address setting \
             (it binds 127.0.0.1 only)"
                .to_string()
        ]
    );
}
