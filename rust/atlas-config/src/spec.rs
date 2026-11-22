//! The SPEC table, the `ATLAS_*` variables, the model metadata, and the
//! loopback invariant.
//!
//! One table decides what a setting is: its type, its range and its default.
//! Nothing reads a setting by name at run time without going through it.
//!
//! The table is also the only place the *words* about a setting come from
//! ([`range_text`], [`requirement_text`], [`default_text`], [`unusable_kind`]):
//! a message that says "write a whole number from 1024 to 65535" reads the
//! number out of the row instead of repeating it in a second sentence that can
//! drift. Coercing a value into that range is [`crate::coerce`]'s job.

use serde_json::{Number, Value};

use crate::value::value_as_text;

// ---------------------------------------------------------------------------
// model, geo and toolchain metadata
// ---------------------------------------------------------------------------

/// The GGUF repository the model comes from. A printed hint, never a fetch.
pub const MODEL_REPO: &str =
    "DavidAU/Qwen3.5-9B-The-Defiant-Fable-Uncensored-Heretic-NEO-IMATRIX-MAX-MTP-GGUF";

/// The quantised file inside that repository: IQ4_XS *with* MTP.
pub const MODEL_FILE: &str = "Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-MTP-IQ4_XS.gguf";

/// The name the local copy gets under `models/`.
pub const MODEL_LOCAL_NAME: &str = "Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf";

/// Reference geodata, shipped in `public/vendor` — the app reads the file, it
/// never fetches it. Kept as constants because the download audit knows these
/// tokens and a bare URL is a printed hint, not an action.
pub const GEO_URL: &str =
    "https://raw.githubusercontent.com/johan/world.geo.json/master/countries.geo.json";
pub const GEO_MAP_URL: &str = "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_50m_admin_0_countries.geojson";
pub const LLAMACPP_REPO: &str = "https://github.com/ggml-org/llama.cpp.git";
pub const LLAMACPP_RELEASES_API: &str =
    "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest";

/// The download URL for the model, built from repo + file the way the Python
/// constant was. Printing it is allowed; fetching it at run time is not.
pub fn default_model_url() -> String {
    format!(
        "https://huggingface.co/{}/resolve/main/{}",
        MODEL_REPO, MODEL_FILE
    )
}

// ---------------------------------------------------------------------------
// the SPEC table
// ---------------------------------------------------------------------------

/// What a numeric setting is: a port-like integer or a fractional interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    Float,
}

/// One row of the SPEC table. `low`/`high` are inclusive, exactly as in
/// `server/config.py`; a value outside them is clamped, never rejected.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub key: &'static str,
    pub kind: Kind,
    pub low: f64,
    pub high: f64,
    pub default: f64,
}

impl Spec {
    /// The typed default, as the JSON number a config file would carry.
    pub fn default_value(&self) -> Value {
        number_value(*self, self.default)
    }
}

/// The SPEC table, in the Python order, then the model server's own numbers.
///
/// The five `model_*` rows are settings like the ports: validated here, clamped with a
/// note, read by `atlas start` — and, because the window draws the argv this table
/// produces and boots the server from that same argv, a value and the flag it becomes
/// cannot disagree. The builder itself lives in `atlas-launcher`, which is the crate that
/// owns the child process; this table only says what a value may be. Two of them (`model_reasoning`, `model_answer`) are not llama.cpp flags at
/// all: they are the agent's token budgets, enforced on the request. They live in the same
/// table because a person setting up this app thinks of them as one panel, and because one
/// table is one place to validate.
pub const SPEC: [Spec; 12] = [
    Spec { key: "app_port", kind: Kind::Int, low: 1024.0, high: 65535.0, default: 8787.0 },
    Spec { key: "llama_port", kind: Kind::Int, low: 1024.0, high: 65535.0, default: 8080.0 },
    Spec { key: "cdp_port", kind: Kind::Int, low: 1024.0, high: 65535.0, default: 9222.0 },
    Spec { key: "collect_interval_min", kind: Kind::Float, low: 0.5, high: 1440.0, default: 5.0 },
    Spec { key: "max_age_hours", kind: Kind::Int, low: 1.0, high: 8760.0, default: 72.0 },
    Spec { key: "window_hours", kind: Kind::Int, low: 1.0, high: 8760.0, default: 72.0 },
    Spec { key: "retention_days", kind: Kind::Int, low: 1.0, high: 3650.0, default: 21.0 },
    // The context window handed to llama.cpp as `-c`. 262 144 is the largest KV cache a
    // current build can be asked for on a 24 GB card; below 1024 there is no room for the
    // tool schemas, so a smaller number is a model that cannot answer at all.
    Spec { key: "model_context", kind: Kind::Int, low: 1024.0, high: 262144.0, default: 32768.0 },
    Spec { key: "model_reasoning", kind: Kind::Int, low: 0.0, high: 32768.0, default: 8192.0 },
    Spec { key: "model_answer", kind: Kind::Int, low: 256.0, high: 32768.0, default: 8192.0 },
    // `-ngl`: -1 is "llama.cpp decides", 0 is CPU-only, and the high end is more layers
    // than any GGUF this app serves has.
    Spec { key: "model_gpu_layers", kind: Kind::Int, low: -1.0, high: 999.0, default: -1.0 },
    // `-t`: 0 is "llama.cpp decides" (one thread per physical core).
    Spec { key: "model_threads", kind: Kind::Int, low: 0.0, high: 256.0, default: 0.0 },
];

/// `key` → environment variable. An empty variable counts as unset.
pub const ENV: [(&str, &str); 14] = [
    ("app_port", "ATLAS_PORT"),
    ("llama_port", "ATLAS_LLAMA_PORT"),
    ("cdp_port", "ATLAS_CDP_PORT"),
    ("collect_interval_min", "ATLAS_COLLECT_MIN"),
    ("max_age_hours", "ATLAS_MAX_AGE_H"),
    ("window_hours", "ATLAS_WINDOW_H"),
    ("retention_days", "ATLAS_RETENTION_DAYS"),
    ("model_context", "ATLAS_MODEL_CONTEXT"),
    ("model_reasoning", "ATLAS_MODEL_REASONING"),
    ("model_answer", "ATLAS_MODEL_ANSWER"),
    ("model_gpu_layers", "ATLAS_MODEL_GPU_LAYERS"),
    ("model_threads", "ATLAS_MODEL_THREADS"),
    ("browser_path", "ATLAS_BROWSER"),
    ("model_local", "ATLAS_MODEL"),
];

/// Non-numeric settings: model metadata and the browser override.
pub const TEXT_KEYS: [&str; 5] = [
    "model_repo",
    "model_file",
    "model_url",
    "model_local",
    "browser_path",
];

/// Look one row of the SPEC table up by name.
pub fn spec_of(key: &str) -> Option<&'static Spec> {
    SPEC.iter().find(|spec| spec.key == key)
}

/// The environment variable that overrides `key`, when there is one.
pub fn env_name(key: &str) -> Option<&'static str> {
    ENV.iter()
        .find(|(name, _)| *name == key)
        .map(|(_, variable)| *variable)
}

/// Every setting in table order: the numeric ones first, then the text ones.
pub fn key_order() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = SPEC.iter().map(|spec| spec.key).collect();
    keys.extend(TEXT_KEYS.iter().copied());
    keys
}

/// A typed number, as JSON.
pub(crate) fn number_value(spec: Spec, value: f64) -> Value {
    match spec.kind {
        Kind::Int => Value::Number(Number::from(value as i64)),
        Kind::Float => Value::Number(Number::from_f64(value).unwrap_or_else(|| Number::from(0))),
    }
}

// ---------------------------------------------------------------------------
// the words about a setting
// ---------------------------------------------------------------------------

/// One number as the text a person reads: `8787`, not `8787.0`.
pub fn number_text(spec: &Spec, value: f64) -> String {
    match spec.kind {
        Kind::Int => format!("{}", value as i64),
        Kind::Float => format!("{}", value),
    }
}

/// The row's default, as a person reads it (`8787`, `5`).
pub fn default_text(spec: &Spec) -> String {
    number_text(spec, spec.default)
}

/// The row's range as a sentence fragment: `1024 to 65535`.
pub fn range_text(spec: &Spec) -> String {
    format!(
        "{} to {}",
        number_text(spec, spec.low),
        number_text(spec, spec.high)
    )
}

/// What a person types to satisfy this row, in the row's own terms:
/// `write a whole number from 1024 to 65535`,
/// `write a number from 0.5 to 1440, fractions allowed`.
pub fn requirement_text(spec: &Spec) -> String {
    match spec.kind {
        Kind::Int => format!("write a whole number from {}", range_text(spec)),
        Kind::Float => format!("write a number from {}, fractions allowed", range_text(spec)),
    }
}

/// Why a value cannot be used, in the setting's own words: `is not a usable
/// port` for a port, `is not a usable number` for everything else.
pub fn unusable_kind(spec: &Spec) -> &'static str {
    match spec.kind {
        Kind::Int if spec.key.ends_with("_port") => "is not a usable port",
        _ => "is not a usable number",
    }
}

// ---------------------------------------------------------------------------
// the loopback invariant
// ---------------------------------------------------------------------------

/// The only address ATLAS ever listens on. Not a setting: a constant.
pub const BIND_HOST: &str = "127.0.0.1";

/// Names that cannot be reached from another machine.
pub const LOOPBACK_HOSTS: [&str; 4] = ["127.0.0.1", "::1", "localhost", "ip6-localhost"];

/// Settings that would change what an engine binds to, if a hostile or legacy
/// file ever carried one. None of them is a real ATLAS setting — they exist so
/// such a file is *caught* instead of honoured.
pub const HOST_KEYS: [&str; 12] = [
    "host",
    "bind",
    "bind_host",
    "listen",
    "listen_host",
    "http_host",
    "server_host",
    "cdp_host",
    "chrome_host",
    "llama_host",
    "remote_debugging_address",
    "address",
];

/// Addresses that listen on every interface. Listed so a message can explain
/// itself; the verdict comes from `atlas_guard::address::check`.
pub const WILDCARD_HOSTS: [&str; 5] = ["0.0.0.0", "::", "*", "0", ""];

/// True only for an address that cannot be reached from another machine.
///
/// The decision is [`atlas_guard::address::check`]'s — the same function the
/// listener audit and the browser's debug-port check ask — so the loopback rule
/// exists exactly once. It trims, lowercases, strips `[...]` and a `:port`
/// suffix, and answers `false` for anything it cannot prove is loopback,
/// including a host name it does not know.
pub fn is_loopback(host: &str) -> bool {
    atlas_guard::address::check(host).loopback
}

/// The `ATLAS_*` variable that would carry a host key: `host` → `ATLAS_HOST`.
pub fn host_key_env(key: &str) -> String {
    format!("ATLAS_{}", key.to_ascii_uppercase())
}

/// A key name reduced to the shape a bind address is spelled in: lower case, with
/// `_`, `-` and spaces dropped. `Host`, `HOST`, `bind_host`, `Bind-Host` and
/// `bindhost` are one key to this rule, which is the point: `HOST_KEYS` is a
/// table of *names*, and a file that spells one of them differently is still
/// asking ATLAS to listen somewhere. Matching the table by exact string let
/// `{"Host": "0.0.0.0"}` through as an unknown key — republished by `values_json`
/// and, worse, handed to any reader that case-folds JSON keys.
pub fn host_key_shape(key: &str) -> String {
    key.chars()
        .filter(|ch| !matches!(ch, '_' | '-' | ' '))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// True when `key` names a bind address in any spelling ATLAS can read.
///
/// The `ATLAS_` prefix is stripped first, so the same rule answers for a
/// `data/config.json` key and for an environment variable (`ATLAS_BIND_HOST`).
pub fn is_host_key(key: &str) -> bool {
    let bare = key
        .strip_prefix("ATLAS_")
        .or_else(|| key.strip_prefix("atlas_"))
        .unwrap_or(key);
    let shape = host_key_shape(bare);
    HOST_KEYS.iter().any(|known| host_key_shape(known) == shape)
}

// ---------------------------------------------------------------------------
// number coercion moved to `crate::coerce`
// ---------------------------------------------------------------------------
// The clamp-and-round rule for one number lives in `coerce.rs` (one concern per
// file, and it is the part that also has to say *that* it moved a value). What
// stays here is the table it reads and the words a message is built from.

/// The raw text a value arrived as, for a [`crate::Problem`] that has to show
/// the caller what it sent.
pub(crate) fn display_value(raw: Option<&Value>) -> Option<String> {
    match raw {
        None | Some(Value::Null) => None,
        Some(value) => Some(value_as_text(Some(value))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use crate::config::Config;
    // The clamp-and-round rule moved to its own module; these tests still drive
    // it through the table, which is what a caller sees.
    use crate::coerce::coerce_number;

#[test]
fn every_row_says_its_own_range_and_requirement() {
    // The words a message is built from are the table's, not a second copy of
    // the numbers: change a range here and the sentence changes with it.
    let port = spec_of("app_port").unwrap();
    assert_eq!(range_text(port), "1024 to 65535");
    assert_eq!(requirement_text(port), "write a whole number from 1024 to 65535");
    assert_eq!(default_text(port), "8787");
    assert_eq!(unusable_kind(port), "is not a usable port");

    let hours = spec_of("max_age_hours").unwrap();
    assert_eq!(range_text(hours), "1 to 8760");
    assert_eq!(unusable_kind(hours), "is not a usable number");
    assert_eq!(default_text(hours), "72");

    let interval = spec_of("collect_interval_min").unwrap();
    assert_eq!(range_text(interval), "0.5 to 1440");
    assert_eq!(
        requirement_text(interval),
        "write a number from 0.5 to 1440, fractions allowed"
    );
    // A whole-number default is not printed as `5.0`.
    assert_eq!(default_text(interval), "5");
    assert_eq!(unusable_kind(interval), "is not a usable number");

    for spec in SPEC.iter() {
        assert!(!range_text(spec).is_empty(), "{}", spec.key);
        assert!(requirement_text(spec).contains(&range_text(spec)), "{}", spec.key);
    }
}

#[test]
fn spec_table_matches_the_agreed_table() {
    // The first seven rows are the Python table this crate was ported from; the five
    // `model_*` rows are the shared settings spec (context, the two token budgets, the GPU
    // layers and the thread count). One row per setting, and a row added here is a row a
    // config file may carry — so this count is deliberate, not incidental.
    assert_eq!(SPEC.len(), 12);
    let rows: Vec<(&str, Kind, f64, f64, f64)> = SPEC
        .iter()
        .map(|spec| (spec.key, spec.kind, spec.low, spec.high, spec.default))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("app_port", Kind::Int, 1024.0, 65535.0, 8787.0),
            ("llama_port", Kind::Int, 1024.0, 65535.0, 8080.0),
            ("cdp_port", Kind::Int, 1024.0, 65535.0, 9222.0),
            ("collect_interval_min", Kind::Float, 0.5, 1440.0, 5.0),
            ("max_age_hours", Kind::Int, 1.0, 8760.0, 72.0),
            ("window_hours", Kind::Int, 1.0, 8760.0, 72.0),
            ("retention_days", Kind::Int, 1.0, 3650.0, 21.0),
            ("model_context", Kind::Int, 1024.0, 262144.0, 32768.0),
            ("model_reasoning", Kind::Int, 0.0, 32768.0, 8192.0),
            ("model_answer", Kind::Int, 256.0, 32768.0, 8192.0),
            ("model_gpu_layers", Kind::Int, -1.0, 999.0, -1.0),
            ("model_threads", Kind::Int, 0.0, 256.0, 0.0),
        ]
    );
    for spec in SPEC.iter() {
        assert!(spec.low < spec.high, "{}", spec.key);
        assert!(
            spec.default >= spec.low && spec.default <= spec.high,
            "{}",
            spec.key
        );
        assert_eq!(spec_of(spec.key).map(|found| found.key), Some(spec.key));
    }
    assert!(spec_of("nonsense").is_none());
}

#[test]
fn env_map_covers_the_python_variables() {
    assert_eq!(env_name("app_port"), Some("ATLAS_PORT"));
    assert_eq!(env_name("llama_port"), Some("ATLAS_LLAMA_PORT"));
    assert_eq!(env_name("cdp_port"), Some("ATLAS_CDP_PORT"));
    assert_eq!(env_name("collect_interval_min"), Some("ATLAS_COLLECT_MIN"));
    assert_eq!(env_name("max_age_hours"), Some("ATLAS_MAX_AGE_H"));
    assert_eq!(env_name("window_hours"), Some("ATLAS_WINDOW_H"));
    assert_eq!(env_name("retention_days"), Some("ATLAS_RETENTION_DAYS"));
    assert_eq!(env_name("browser_path"), Some("ATLAS_BROWSER"));
    assert_eq!(env_name("model_local"), Some("ATLAS_MODEL"));
    assert_eq!(env_name("model_context"), Some("ATLAS_MODEL_CONTEXT"));
    assert_eq!(env_name("model_reasoning"), Some("ATLAS_MODEL_REASONING"));
    assert_eq!(env_name("model_answer"), Some("ATLAS_MODEL_ANSWER"));
    assert_eq!(env_name("model_gpu_layers"), Some("ATLAS_MODEL_GPU_LAYERS"));
    assert_eq!(env_name("model_threads"), Some("ATLAS_MODEL_THREADS"));
    assert_eq!(env_name("model_repo"), None);
    // 12 numeric rows + 5 text keys.
    assert_eq!(key_order().len(), 17);
}

#[test]
fn every_spec_key_clamps_at_its_low_and_its_high_plus_off_by_one() {
    for spec in SPEC.iter() {
        let below = Value::from(spec.low - 1.0);
        let at_low = Value::from(spec.low);
        let near_low = Value::from(spec.low - 0.25);
        let at_high = Value::from(spec.high);
        let above = Value::from(spec.high + 1000.0);
        let near_high = Value::from(spec.high + 0.25);

        assert_eq!(coerce_number(spec, Some(&below)), Some(spec.low), "{} below", spec.key);
        assert_eq!(coerce_number(spec, Some(&at_low)), Some(spec.low), "{} low", spec.key);
        assert_eq!(coerce_number(spec, Some(&near_low)), Some(spec.low), "{} near low", spec.key);
        assert_eq!(coerce_number(spec, Some(&at_high)), Some(spec.high), "{} high", spec.key);
        assert_eq!(coerce_number(spec, Some(&above)), Some(spec.high), "{} above", spec.key);
        assert_eq!(coerce_number(spec, Some(&near_high)), Some(spec.high), "{} near high", spec.key);

        // The same clamping through both doors: the file and the environment.
        let from_file = build_with(&[], &format!("{{\"{}\": {}}}", spec.key, spec.high + 1000.0));
        assert_eq!(from_file.number(spec.key), Some(spec.high), "{} file", spec.key);
        let below_env = (spec.low - 1.0).to_string();
        let from_env = build_with(&[(env_name(spec.key).unwrap(), below_env.as_str())], "{}");
        assert_eq!(from_env.number(spec.key), Some(spec.low), "{} env", spec.key);
    }
}

#[test]
fn integers_round_half_to_even_and_floats_to_three_places() {
    let port = spec_of("app_port").unwrap();
    assert_eq!(coerce_number(port, Some(&Value::from(2000.5))), Some(2000.0));
    assert_eq!(coerce_number(port, Some(&Value::from(2001.5))), Some(2002.0));
    let interval = spec_of("collect_interval_min").unwrap();
    assert_eq!(coerce_number(interval, Some(&Value::from(2.00049))), Some(2.0));
    assert_eq!(coerce_number(interval, Some(&Value::from(2.5009))), Some(2.501));
    // A boolean is not a number: `true` must not become port 1024.
    assert_eq!(coerce_number(port, Some(&Value::Bool(true))), None);
    assert_eq!(coerce_number(port, Some(&Value::Null)), None);
    assert_eq!(coerce_number(port, None), None);
    assert_eq!(coerce_number(port, Some(&Value::String(" 8081 ".into()))), Some(8081.0));
    assert_eq!(coerce_number(port, Some(&Value::String("banana".into()))), None);
    assert_eq!(coerce_number(port, Some(&Value::String("inf".into()))), None);
    assert_eq!(coerce_number(port, Some(&Value::Array(vec![]))), None);
}

#[test]
fn a_bad_type_falls_back_to_the_default_and_is_reported() {
    let config = build_with(&[], "{\"app_port\": \"banana\", \"retention_days\": []}");
    assert_eq!(config.app_port, 8787);
    assert_eq!(config.retention_days, 21);
    let problems = config.validate();
    assert_eq!(problems.len(), 2, "{:?}", problems);
    assert_eq!(problems[0].key, "app_port");
    assert_eq!(problems[0].origin, "data/config.json:app_port");
    assert_eq!(problems[0].value.as_deref(), Some("banana"));
    assert!(problems[0].message().contains("is not a usable port"));
    assert_eq!(problems[1].key, "retention_days");
    assert!(problems[1].message().contains("is not a usable number"));
}


#[test]
fn loopback_names_are_the_ones_that_cannot_be_reached() {
    for host in [
        "127.0.0.1",
        "127.0.0.2",
        "::1",
        "[::1]",
        "localhost",
        "LOCALHOST",
        "ip6-localhost",
    ] {
        assert!(is_loopback(host), "{} must be loopback", host);
    }
    for host in [
        "0.0.0.0",
        "::",
        "*",
        "0",
        "",
        "192.168.1.10",
        "10.0.0.1",
        "8.8.8.8",
        "example.com",
        "127.0.0.1.example.com",
        "[2001:db8::1]",
    ] {
        assert!(!is_loopback(host), "{} must not be loopback", host);
    }
}

#[test]
fn a_non_loopback_host_in_the_file_is_a_problem_not_a_panic() {
    let dir = TempDir::new("host-file");
    dir.write("data/config.json", r#"{"host": "0.0.0.0", "app_port": 9001}"#);
    let config = load_env_free(&dir).expect("load must not panic");
    let problems = config.validate();
    assert_eq!(problems.len(), 1, "{:?}", problems);
    assert_eq!(problems[0].key, "host");
    assert_eq!(problems[0].origin, "data/config.json:host");
    assert_eq!(problems[0].value.as_deref(), Some("0.0.0.0"));
    assert_eq!(
        problems[0].message(),
        "data/config.json:host='0.0.0.0' is not a loopback address (ATLAS binds 127.0.0.1 only)"
    );
    // The host key is never honoured: it does not survive into the settings.
    assert!(!config.extra.contains_key("host"));
    assert_eq!(config.app_port, 9001);
}

#[test]
fn a_non_loopback_host_in_the_environment_is_a_problem() {
    let dir = TempDir::new("host-env");
    let env = env_pairs(&[("ATLAS_HOST", "0.0.0.0"), ("ATLAS_CDP_HOST", "192.168.1.4")]);
    let config = Config::build(&dir.at(), &env, &empty());
    let problems = config.validate();
    assert_eq!(problems.len(), 2, "{:?}", problems);
    assert_eq!(problems[0].origin, "env:ATLAS_HOST");
    assert!(problems[0].message().starts_with("env:ATLAS_HOST='0.0.0.0'"));
    assert_eq!(problems[1].origin, "env:ATLAS_CDP_HOST");
    assert_eq!(host_key_env("host"), "ATLAS_HOST");
}

#[test]
fn every_host_key_is_checked_in_both_places() {
    assert_eq!(HOST_KEYS.len(), 12);
    for key in HOST_KEYS {
        let variable = host_key_env(key);
        let dir = TempDir::new("host-any");
        let env = env_pairs(&[(variable.as_str(), "10.1.2.3")]);
        let config = Config::build(&dir.at(), &env, &empty());
        assert_eq!(config.validate().len(), 1, "env {}", key);
        let doomed = build_with(&[], &format!("{{\"{}\": \"10.1.2.3\"}}", key));
        assert_eq!(doomed.validate().len(), 1, "file {}", key);
        // A loopback value for a host key is tolerated (and still dropped).
        let allowed = build_with(&[], &format!("{{\"{}\": \"127.0.0.1\"}}", key));
        assert!(allowed.validate().is_empty(), "file {}", key);
    }
}

#[test]
fn host_keys_are_never_persisted() {
    let dir = TempDir::new("host-save");
    let mut overrides = serde_json::Map::new();
    overrides.insert("host".to_string(), Value::String("0.0.0.0".into()));
    overrides.insert("bind_host".to_string(), Value::String("::".into()));
    overrides.insert("app_port".to_string(), Value::from(9401));
    let config = Config::save(&dir.at(), &overrides).expect("save");
    assert!(config.validate().is_empty());
    let on_disk = std::fs::read_to_string(dir.at().config_file).expect("file");
    assert!(!on_disk.contains("host"), "{}", on_disk);
    assert!(on_disk.contains("9401"));
}

#[test]
fn bind_host_is_a_constant_and_never_a_setting() {
    assert_eq!(BIND_HOST, "127.0.0.1");
    assert!(is_loopback(BIND_HOST));
    assert!(HOST_KEYS.iter().all(|key| spec_of(key).is_none()));
    assert!(HOST_KEYS.iter().all(|key| env_name(key).is_none()));
    assert_eq!(WILDCARD_HOSTS.len(), 5);
    assert_eq!(LOOPBACK_HOSTS.len(), 4);
}

#[test]
fn a_strict_load_refuses_a_non_loopback_host() {
    let dir = TempDir::new("strict");
    dir.write("data/config.json", r#"{"listen_host": "0.0.0.0"}"#);
    let error = Config::load_strict_at(&dir.at()).expect_err("boot must refuse");
    assert_eq!(error.key(), Some("listen_host"));
    assert_eq!(error.origin().as_deref(), Some("data/config.json:listen_host"));
    assert_eq!(error.problems().len(), 1);
}

}
